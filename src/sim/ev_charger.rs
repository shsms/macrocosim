//! EV charger — AC charging with command-delay + slew-rate-limited
//! ramp on the set-point, plus the same SoC-protective derate the
//! battery uses (charge taper near `soc_upper`, discharge near
//! `soc_lower` — though most chargers stay non-negative in practice).

use std::{fmt, time::Duration};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;

use crate::sim::{
    Category, MicrogridSite, SetpointError, SimulatedComponent, Telemetry,
    axis::{AxisConfig, IdleTarget, PowerAxis, StepCtx},
    bounds::VecBounds,
    decay::{SocProtect, integrate_soc_pct, sanitize_soc_pct, soc_protected_bounds},
};

#[derive(Clone, Debug)]
pub struct EvChargerConfig {
    pub rated_lower_w: f32,
    pub rated_upper_w: f32,
    pub initial_soc_pct: f32,
    pub soc_lower_pct: f32,
    pub soc_upper_pct: f32,
    pub soc_protect_margin_pct: f32,
    pub capacity_wh: f32,
    pub command_delay: Duration,
    pub ramp_rate_w_per_s: f32,
    pub stream_jitter_pct: f32,
}

impl Default for EvChargerConfig {
    fn default() -> Self {
        Self {
            rated_lower_w: 0.0,
            rated_upper_w: 22_000.0,
            initial_soc_pct: 50.0,
            soc_lower_pct: 0.0,
            soc_upper_pct: 100.0,
            soc_protect_margin_pct: 10.0,
            capacity_wh: 30_000.0,
            command_delay: Duration::from_millis(500),
            ramp_rate_w_per_s: f32::INFINITY,
            stream_jitter_pct: 0.0,
        }
    }
}

pub struct EvCharger {
    id: u64,
    name: String,
    interval: Duration,
    cfg: EvChargerConfig,
    state: Mutex<EvState>,
    /// Active (P) control path: rated band + TTL augmentations,
    /// command delay, slew ramp. The SoC-protective derate is passed
    /// in as the per-tick dynamic hook — see `tick`. `published` is
    /// unused: `aggregate_power_w` / telemetry read `actual()`.
    active: PowerAxis,
}

#[derive(Debug, Clone)]
struct EvState {
    soc_pct: f32,
    /// SoC-protected effective bounds, refreshed every tick.
    effective_lower_w: f32,
    effective_upper_w: f32,
}

impl EvCharger {
    pub fn new(id: u64, interval: Duration, cfg: EvChargerConfig) -> Self {
        Self::protect(&cfg).warn_if_overwide(&format!("ev-charger {id}"));
        let init_soc = cfg.initial_soc_pct;
        let (l, u) = soc_protected_bounds(
            cfg.rated_lower_w,
            cfg.rated_upper_w,
            init_soc,
            Self::protect(&cfg),
        );
        let active = PowerAxis::new(AxisConfig {
            rated: Some((cfg.rated_lower_w, cfg.rated_upper_w)),
            caps: None,
            command_delay: cfg.command_delay,
            ramp_rate_per_s: cfg.ramp_rate_w_per_s,
            unit: "W",
        });
        Self {
            id,
            name: format!("ev-charger-{id}"),
            interval,
            cfg,
            state: Mutex::new(EvState {
                soc_pct: init_soc,
                effective_lower_w: l,
                effective_upper_w: u,
            }),
            active,
        }
    }

    fn protect(cfg: &EvChargerConfig) -> SocProtect {
        SocProtect::new(
            cfg.soc_lower_pct,
            cfg.soc_upper_pct,
            cfg.soc_protect_margin_pct,
        )
    }

    fn refresh_bounds(&self, soc: f32) -> (f32, f32) {
        soc_protected_bounds(
            self.cfg.rated_lower_w,
            self.cfg.rated_upper_w,
            soc,
            Self::protect(&self.cfg),
        )
    }
}

impl fmt::Display for EvCharger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl SimulatedComponent for EvCharger {
    fn id(&self) -> u64 {
        self.id
    }
    fn category(&self) -> Category {
        Category::EvCharger
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
        // Same contract as Battery: lets a scenario script "car
        // arrives at 20 %" via (set-component-soc ...).
        if let Some(pct) = sanitize_soc_pct("EvCharger::set_soc_pct", pct) {
            self.state.lock().soc_pct = pct;
        }
        true
    }

    fn takes_soc_pct(&self) -> bool {
        true
    }

    fn tick(&self, _world: &MicrogridSite, now: DateTime<Utc>, dt: Duration) {
        // 1. Refresh SoC-derated bounds and snapshot them for the rest
        //    of the tick under a single lock acquisition. Splitting
        //    `(self.state.lock().lo, self.state.lock().up)` would
        //    re-enter the same parking_lot::Mutex and deadlock.
        let (soc_lo, soc_hi) = {
            let mut s = self.state.lock();
            let (l, u) = self.refresh_bounds(s.soc_pct);
            s.effective_lower_w = l;
            s.effective_upper_w = u;
            (l, u)
        };

        // 2. The axis composes rated ∩ live augmentations ∩ this
        //    per-tick SoC derate into the tracking envelope, drops
        //    expired augmentations, promotes any pending command
        //    clamped into that envelope (band-aware — a multi-band
        //    augmentation clamps into the band the setpoint sits in),
        //    and parks at 0 when the composed envelope is empty. With
        //    no command armed, `IdleTarget::Hold` leaves the ramp
        //    target untouched — 0 stays 0 even when an augmentation
        //    excludes it (e.g. a [5 kW, 22 kW] floor).
        let derate = VecBounds::single(soc_lo, soc_hi);
        let p = self.active.step(
            now,
            dt,
            StepCtx {
                other_axis: 0.0,
                dynamic: Some(&derate),
                idle: IdleTarget::Hold,
            },
        );

        // 3. Integrate SoC (shared rectangular step, same as Battery).
        let mut s = self.state.lock();
        s.soc_pct = integrate_soc_pct(s.soc_pct, p, dt, self.cfg.capacity_wh);
    }

    fn telemetry(&self, site: &MicrogridSite) -> Telemetry {
        let grid = site.grid_state();
        let p = self.active.actual();
        let s = self.state.lock().clone();
        Telemetry {
            id: self.id,
            category: Some(Category::EvCharger),
            active_power_w: Some(p),
            // The EV is a P-only AC component — it never takes a Q
            // setpoint — but the formula engine's convergence pass
            // treats an absent reactive sample as "unknown", not
            // "zero". Advertising Some(0.0) here (and, via the
            // streaming path in proto_conv.rs, an AcPowerReactive
            // sample of 0) tells it honestly that Q is settled at 0.
            reactive_power_var: Some(0.0),
            soc_pct: Some(s.soc_pct),
            soc_lower_pct: Some(self.cfg.soc_lower_pct),
            soc_upper_pct: Some(self.cfg.soc_upper_pct),
            capacity_wh: Some(self.cfg.capacity_wh),
            per_phase_voltage_v: Some(grid.voltage_per_phase),
            frequency_hz: Some(grid.frequency_hz),
            active_power_bounds: self.effective_active_bounds(),
            cable_state: Some("ev-charging-cable-locked-at-ev"),
            ..Default::default()
        }
    }

    fn set_active_setpoint(&self, power_w: f32) -> Result<(), SetpointError> {
        // Validate against rated ∩ augmentations, not SoC-derated —
        // the SoC clamp stays silent (avoids bouncing accept / reject
        // as the cell tops up). Augmentations are an explicit
        // narrowing the client just asked for and expects to take
        // effect, so they belong in the validation envelope. That is
        // exactly the axis's validation/tracking split: `accept`
        // validates against rated ∩ augmentations only, never the
        // dynamic hook.
        self.active.accept(power_w, Utc::now(), 0.0)
    }

    fn augment_active_bounds(&self, ts: DateTime<Utc>, bounds: VecBounds, lifetime: Duration) {
        self.active.augment(ts, bounds, lifetime);
    }

    fn reset_setpoint(&self) {
        // Today's reset is delay.reset + ramp.snap_to(0) — exactly
        // `PowerAxis::trip()` minus the published write. The EV never
        // reads `active.published()` (telemetry and aggregate_power_w
        // both read `actual()`), so that unused write is harmless.
        self.active.trip();
    }

    fn active_power_w(&self, _site: &MicrogridSite) -> Option<f32> {
        Some(self.active.actual())
    }

    fn aggregate_power_w(&self, _world: &MicrogridSite) -> f32 {
        self.active.actual()
    }

    fn rated_active_bounds(&self) -> Option<(f32, f32)> {
        Some((self.cfg.rated_lower_w, self.cfg.rated_upper_w))
    }

    fn effective_active_bounds(&self) -> Option<VecBounds> {
        let s = self.state.lock();
        let soc = VecBounds::single(s.effective_lower_w, s.effective_upper_w);
        drop(s);
        Some(soc.intersect(&self.active.effective_static()))
    }

    fn make_fn(&self) -> &'static str {
        "%make-ev-charger"
    }

    fn constructor_kwargs(&self) -> Vec<(&'static str, String)> {
        let lf = crate::lisp::lisp_float32;
        let mut kw = vec![
            (":rated-lower", lf(self.cfg.rated_lower_w)),
            (":rated-upper", lf(self.cfg.rated_upper_w)),
            (":initial-soc", lf(self.cfg.initial_soc_pct)),
            (":soc-lower", lf(self.cfg.soc_lower_pct)),
            (":soc-upper", lf(self.cfg.soc_upper_pct)),
            (":soc-protect-margin", lf(self.cfg.soc_protect_margin_pct)),
            (":capacity", lf(self.cfg.capacity_wh)),
            (
                ":command-delay-ms",
                self.cfg.command_delay.as_millis().to_string(),
            ),
        ];
        if self.cfg.ramp_rate_w_per_s.is_finite() {
            kw.push((":ramp-rate", lf(self.cfg.ramp_rate_w_per_s)));
        }
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
    use crate::proto::common::metrics::Bounds;

    fn charger() -> EvCharger {
        EvCharger::new(
            300,
            Duration::from_secs(1),
            EvChargerConfig {
                rated_lower_w: 0.0,
                rated_upper_w: 22_000.0,
                soc_protect_margin_pct: 0.0,
                command_delay: Duration::ZERO,
                ramp_rate_w_per_s: f32::INFINITY,
                ..Default::default()
            },
        )
    }

    /// Augmenting the active-power bounds tightens both the
    /// validation envelope and the telemetry-reported bounds. Before
    /// the override on `augment_active_bounds` the call silently
    /// dropped — the rated bounds stayed in effect and clients saw
    /// a setpoint they thought they'd narrowed go through.
    #[test]
    fn augment_active_bounds_narrows_validation_and_telemetry() {
        let w = MicrogridSite::new();
        let ev = charger();
        ev.augment_active_bounds(
            Utc::now(),
            VecBounds(vec![Bounds {
                lower: Some(0.0),
                upper: Some(5_000.0),
            }]),
            Duration::from_secs(60),
        );

        // Effective bounds now reflect the augmentation.
        let eff = ev.effective_active_bounds().unwrap();
        assert_eq!(eff.0.len(), 1);
        assert_eq!(eff.0[0].lower, Some(0.0));
        assert_eq!(eff.0[0].upper, Some(5_000.0));

        // A setpoint inside the augmented envelope still works.
        assert!(ev.set_active_setpoint(3_000.0).is_ok());
        ev.tick(&w, Utc::now(), Duration::from_millis(100));
        assert!((ev.aggregate_power_w(&w) - 3_000.0).abs() < 1.0);

        // A setpoint outside the augmented envelope is rejected even
        // though it's still inside rated.
        assert!(matches!(
            ev.set_active_setpoint(10_000.0),
            Err(SetpointError::OutOfBounds { .. })
        ));
    }

    /// A multi-band augmentation clamps a setpoint into the band it
    /// sits in — the tick must not flatten the envelope to its first
    /// band and drag a valid later-band setpoint down.
    #[test]
    fn multi_band_augmentation_keeps_later_band_setpoints() {
        let w = MicrogridSite::new();
        let ev = charger();
        ev.augment_active_bounds(
            Utc::now(),
            VecBounds(vec![
                Bounds {
                    lower: Some(0.0),
                    upper: Some(5_000.0),
                },
                Bounds {
                    lower: Some(10_000.0),
                    upper: Some(22_000.0),
                },
            ]),
            Duration::from_secs(60),
        );
        assert!(ev.set_active_setpoint(15_000.0).is_ok());
        ev.tick(&w, Utc::now(), Duration::from_millis(100));
        assert!((ev.aggregate_power_w(&w) - 15_000.0).abs() < 1.0);
    }

    /// Once the augmentation's lifetime elapses, `tick` reaps it and
    /// the rated bounds come back in full.
    #[test]
    fn augmentation_expires_and_rated_returns() {
        let w = MicrogridSite::new();
        let ev = charger();
        let t0 = Utc::now();
        ev.augment_active_bounds(
            t0,
            VecBounds(vec![Bounds {
                lower: Some(0.0),
                upper: Some(5_000.0),
            }]),
            Duration::from_millis(50),
        );

        // Pre-expiry: narrowed.
        let eff = ev.effective_active_bounds().unwrap();
        assert_eq!(eff.0[0].upper, Some(5_000.0));

        // Tick past the lifetime — `drop_expired` reaps inside tick.
        ev.tick(
            &w,
            t0 + chrono::Duration::milliseconds(100),
            Duration::from_millis(50),
        );

        let eff = ev.effective_active_bounds().unwrap();
        assert_eq!(eff.0[0].upper, Some(22_000.0));
    }

    /// A P-only AC component must still EMIT a zero Q sample, not stay
    /// silent on it — the formula engine's convergence pass reads
    /// `reactive_power_var` for every AC component and an absent
    /// field there reads as "unknown", not "zero".
    #[test]
    fn ev_telemetry_advertises_zero_reactive() {
        let w = MicrogridSite::new();
        let ev = charger();
        let t = ev.telemetry(&w);
        assert_eq!(t.reactive_power_var, Some(0.0));
    }

    /// Every construction kwarg round-trips; `:ramp-rate` renders
    /// only when finite and `:interval` only when it departs from
    /// the 1000 ms default.
    #[test]
    fn constructor_kwargs_round_trip_ev_charger() {
        let cfg = EvChargerConfig {
            capacity_wh: 40_000.0,
            rated_upper_w: 11_000.0,
            ..Default::default()
        };
        let ev = EvCharger::new(9, Duration::from_millis(500), cfg);
        assert_eq!(ev.make_fn(), "%make-ev-charger");
        let s = ev
            .constructor_kwargs()
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(s.contains(":capacity 40000.0"));
        assert!(s.contains(":rated-upper 11000.0"));
        assert!(s.contains(":interval 500"));
        assert!(s.contains(":command-delay-ms 500"));
        assert!(!s.contains(":ramp-rate"), "infinite ramp is omitted");
    }
}
