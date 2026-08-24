use std::{fmt, time::Duration};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;

use crate::sim::{
    Category, MicrogridSite, SimulatedComponent, Telemetry,
    bounds::VecBounds,
    decay::{
        SocProtect, integrate_soc_pct, sanitize_soc_pct, soc_protected_bounds as decay_soc_bounds,
    },
};

/// Tunables exposed via `(make-battery :soc-protect-margin 10.0 …)`.
#[derive(Clone, Debug)]
pub struct BatteryConfig {
    pub capacity_wh: f32,
    pub initial_soc_pct: f32,
    pub soc_lower_pct: f32,
    pub soc_upper_pct: f32,
    pub voltage_v: f32,
    pub rated_lower_w: f32,
    pub rated_upper_w: f32,
    /// Width of the SoC band (in % points) where the rated DC bound is
    /// tapered toward zero. With margin = 10 and `soc_upper_pct = 90`,
    /// the charge bound starts decaying at SoC=80% and reaches 0 at
    /// SoC=90%. Same on the discharge side near `soc_lower_pct`. Set to
    /// `0.0` to disable.
    pub soc_protect_margin_pct: f32,
    pub stream_jitter_pct: f32,
}

impl Default for BatteryConfig {
    fn default() -> Self {
        Self {
            capacity_wh: 92_000.0,
            initial_soc_pct: 50.0,
            soc_lower_pct: 10.0,
            soc_upper_pct: 90.0,
            voltage_v: 800.0,
            rated_lower_w: -30_000.0,
            rated_upper_w: 30_000.0,
            soc_protect_margin_pct: 10.0,
            stream_jitter_pct: 0.0,
        }
    }
}

pub struct Battery {
    id: u64,
    name: String,
    interval: Duration,
    cfg: BatteryConfig,
    state: Mutex<BatteryState>,
}

#[derive(Debug, Clone)]
struct BatteryState {
    /// Settled active DC power in W (last-tick's clamped accumulator).
    /// Drives SoC integration — reactive doesn't move net energy.
    power_w: f32,
    /// Settled reactive component (last-tick's accumulated Q).
    /// Doesn't change SoC, but inflates dc_current / dc_power in
    /// telemetry to reflect the conductor / IGBT loading a real DC
    /// ammeter would read.
    reactive_var: f32,
    /// Per-tick accumulator for inverter pushes. `set_dc_*` adds
    /// here; `tick()` drains it, clamps the active sum, and sets
    /// `power_w` / `reactive_var`. This is what makes N inverters
    /// pushing the same battery sum correctly instead of having the
    /// last writer win.
    pending_p: f32,
    pending_q: f32,
    /// `power_w / pushed total` from the last tick — how much of what
    /// the inverters pushed the SoC envelope let through. 1.0 when
    /// nothing was pushed. Read back by the inverters for their own
    /// published power (see `SimulatedComponent::dc_accept_ratio`).
    accept_ratio: f32,
    /// State of charge in % [0, 100]. Updated each tick from
    /// `power_w * dt`. Clamped at the boundaries — without this,
    /// configs that disable the SoC-protect taper (margin = 0)
    /// could pump charge in past 100% indefinitely, then need to
    /// "discharge" the unphysical surplus before SoC moves back.
    soc_pct: f32,
    /// Cached effective DC bounds — recomputed every tick from SoC,
    /// then read by `effective_active_bounds` and the inverter.
    effective_lower_w: f32,
    effective_upper_w: f32,
}

impl Battery {
    pub fn new(id: u64, interval: Duration, cfg: BatteryConfig) -> Self {
        protect(&cfg).warn_if_overwide(&format!("battery {id}"));
        let init_soc = cfg.initial_soc_pct;
        let (l, u) = soc_protected_bounds(&cfg, init_soc);
        Self {
            id,
            name: format!("bat-{id}"),
            interval,
            cfg,
            state: Mutex::new(BatteryState {
                power_w: 0.0,
                reactive_var: 0.0,
                pending_p: 0.0,
                accept_ratio: 1.0,
                pending_q: 0.0,
                soc_pct: init_soc,
                effective_lower_w: l,
                effective_upper_w: u,
            }),
        }
    }
}

fn protect(cfg: &BatteryConfig) -> SocProtect {
    SocProtect::new(
        cfg.soc_lower_pct,
        cfg.soc_upper_pct,
        cfg.soc_protect_margin_pct,
    )
}

fn soc_protected_bounds(cfg: &BatteryConfig, soc: f32) -> (f32, f32) {
    decay_soc_bounds(cfg.rated_lower_w, cfg.rated_upper_w, soc, protect(cfg))
}

impl fmt::Display for Battery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl SimulatedComponent for Battery {
    fn id(&self) -> u64 {
        self.id
    }
    fn category(&self) -> Category {
        Category::Battery
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn stream_interval(&self) -> Duration {
        self.interval
    }
    fn stream_jitter_pct(&self) -> f32 {
        self.cfg.stream_jitter_pct
    }

    fn set_soc_pct(&self, pct: f32) -> bool {
        // The next tick re-derives the SoC-protected bounds from the
        // new value, so no other state needs touching here.
        if let Some(pct) = sanitize_soc_pct("Battery::set_soc_pct", pct) {
            self.state.lock().soc_pct = pct;
        }
        true
    }

    fn takes_soc_pct(&self) -> bool {
        true
    }

    fn tick(&self, _world: &MicrogridSite, _now: DateTime<Utc>, dt: Duration) {
        let mut s = self.state.lock();

        // 1. Refresh SoC-derated bounds from current SoC.
        let (l, u) = soc_protected_bounds(&self.cfg, s.soc_pct);
        s.effective_lower_w = l;
        s.effective_upper_w = u;

        // 2. Drain the per-tick accumulator and clamp the active sum
        //    against the freshly-computed envelope. With one inverter
        //    this is identical to "store-then-clamp"; with N inverters
        //    sharing the bus, the clamp applies to the *total* push,
        //    not just the last writer.
        let total_p = s.pending_p;
        let total_q = s.pending_q;
        s.pending_p = 0.0;
        s.pending_q = 0.0;
        // NaN-safe clamp: std `f32::clamp` panics on a NaN bound, and
        // the bounds derive from config-supplied rated values with no
        // finiteness guarantee — a panic here kills this microgrid's
        // physics task permanently while gRPC keeps serving stale
        // telemetry. min/max propagate the finite side instead.
        s.power_w = total_p.min(s.effective_upper_w).max(s.effective_lower_w);
        s.reactive_var = total_q;
        // Inside a sane envelope (lower ≤ 0 ≤ upper) the clip keeps the
        // sign and never grows the magnitude, so the ratio is already in
        // [0, 1]; the clamp only guards a config whose envelope excludes
        // zero.
        s.accept_ratio = if total_p != 0.0 && s.power_w.is_finite() {
            (s.power_w / total_p).clamp(0.0, 1.0)
        } else {
            1.0
        };

        // 3. SoC update from settled P — the rectangular P·dt step in
        //    decay::integrate_soc_pct. Related to the per-component
        //    `EnergyWh` history metric (see history.rs `push_snapshot`), but
        //    a different quadrature: rectangular over this one fixed physics
        //    tick's `dt` here, versus trapezoidal over the variable sample
        //    gap there. Deliberately not the shared `EnergyAccum` — the
        //    per-tick rectangle is what clamps SoC each step.
        s.soc_pct = integrate_soc_pct(s.soc_pct, s.power_w, dt, self.cfg.capacity_wh);
    }

    fn telemetry(&self, _world: &MicrogridSite) -> Telemetry {
        let s = self.state.lock().clone();
        // Apparent DC magnitude with sign of P. Reactive load
        // doesn't move net energy (so SoC integrates on `power_w`
        // alone, see tick()) but it does flow through the conductors,
        // so dc_power and dc_current here reflect the apparent
        // loading a real instrument would read.
        let apparent = (s.power_w * s.power_w + s.reactive_var * s.reactive_var).sqrt();
        let signed_apparent = apparent * if s.power_w >= 0.0 { 1.0 } else { -1.0 };
        Telemetry {
            id: self.id,
            category: Some(Category::Battery),
            soc_pct: Some(s.soc_pct),
            soc_lower_pct: Some(self.cfg.soc_lower_pct),
            soc_upper_pct: Some(self.cfg.soc_upper_pct),
            capacity_wh: Some(self.cfg.capacity_wh),
            dc_voltage_v: Some(self.cfg.voltage_v),
            dc_power_w: Some(signed_apparent),
            dc_current_a: Some(if self.cfg.voltage_v != 0.0 {
                signed_apparent / self.cfg.voltage_v
            } else {
                0.0
            }),
            active_power_bounds: Some(VecBounds::single(s.effective_lower_w, s.effective_upper_w)),
            component_state: Some(crate::sim::component::power_state(s.power_w)),
            relay_state: Some("relay-closed"),
            ..Default::default()
        }
    }

    /// Battery's contribution to its parent meter — *active* DC
    /// power only. `telemetry().dc_power_w` is the *signed apparent*
    /// magnitude (√(P²+Q²) with the sign of P) so a SCADA-style
    /// instrument reads the actual conductor loading, but parent
    /// meters integrate energy and a reactive flow doesn't move
    /// joules. The split is deliberate; a control app comparing
    /// the two values via /api/telemetry vs /api/topology will see
    /// the gap whenever Q ≠ 0.
    fn aggregate_power_w(&self, _world: &MicrogridSite) -> f32 {
        self.state.lock().power_w
    }

    /// Add an inverter's active push to this tick's accumulator.
    /// The actual `power_w` value is the *total* across all parents
    /// after `tick()` clamps the accumulated sum to the SoC envelope.
    fn set_dc_power(&self, p: f32) {
        self.state.lock().pending_p += p;
    }

    /// Active+reactive variant. Both are accumulated additively so an
    /// MxN topology (multiple inverters sharing a battery) settles to
    /// the *total* push, not last-writer-wins. The active sum is
    /// clamped to the SoC envelope at `tick()` time; reactive flows
    /// through unchanged (the battery doesn't refuse Q).
    fn set_dc_active_reactive(&self, p: f32, q: f32) {
        let mut s = self.state.lock();
        s.pending_p += p;
        s.pending_q += q;
    }

    fn dc_accept_ratio(&self) -> f32 {
        self.state.lock().accept_ratio
    }

    fn aggregate_reactive_var(&self, _world: &MicrogridSite) -> f32 {
        self.state.lock().reactive_var
    }

    fn rated_active_bounds(&self) -> Option<(f32, f32)> {
        Some((self.cfg.rated_lower_w, self.cfg.rated_upper_w))
    }

    fn effective_active_bounds(&self) -> Option<VecBounds> {
        let s = self.state.lock();
        Some(VecBounds::single(s.effective_lower_w, s.effective_upper_w))
    }

    fn make_fn(&self) -> &'static str {
        "%make-battery"
    }

    fn constructor_kwargs(&self) -> Vec<(&'static str, String)> {
        let lf = |v: f32| crate::lisp::lisp_float(v as f64);
        let mut kw = vec![
            (":capacity", lf(self.cfg.capacity_wh)),
            (":initial-soc", lf(self.cfg.initial_soc_pct)),
            (":soc-lower", lf(self.cfg.soc_lower_pct)),
            (":soc-upper", lf(self.cfg.soc_upper_pct)),
            (":voltage", lf(self.cfg.voltage_v)),
            (":rated-lower", lf(self.cfg.rated_lower_w)),
            (":rated-upper", lf(self.cfg.rated_upper_w)),
            (":soc-protect-margin", lf(self.cfg.soc_protect_margin_pct)),
        ];
        if self.interval != Duration::from_millis(1000) {
            kw.push((":interval", self.interval.as_millis().to_string()));
        }
        if self.cfg.stream_jitter_pct != 0.0 {
            kw.push((":stream-jitter-pct", lf(self.cfg.stream_jitter_pct)));
        }
        kw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every construction kwarg round-trips into the rendered form,
    /// and `:interval` renders only when it departs from the 1000 ms
    /// default.
    #[test]
    fn constructor_kwargs_round_trip_battery() {
        let cfg = BatteryConfig {
            capacity_wh: 50_000.0,
            initial_soc_pct: 20.0,
            ..Default::default()
        };
        let b = Battery::new(7, Duration::from_millis(500), cfg);
        assert_eq!(b.make_fn(), "%make-battery");
        let kw = b.constructor_kwargs();
        let s = kw
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(s.contains(":capacity 50000.0"));
        assert!(s.contains(":initial-soc 20.0"));
        assert!(s.contains(":interval 500"));
        assert!(s.contains(":rated-lower -30000.0"));
    }
}
