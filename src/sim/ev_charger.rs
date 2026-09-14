//! EV charger — an AC charging point. Its power axis (command delay,
//! slew, rated band + TTL augmentations) produces the *limit* the
//! charger offers; the connected car, if any, decides what it draws
//! within that limit. See `ev_presets` for the car and the draw law.

use std::{fmt, str::FromStr, time::Duration};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;

use crate::sim::{
    AugmentError, Category, MicrogridSite, SetpointError, SimulatedComponent, Telemetry,
    axis::{AxisConfig, IdleTarget, PowerAxis, StepCtx},
    bounds::VecBounds,
    component::{KnobKind, KnobSnapshot},
    decay::sanitize_soc_pct,
    ev_presets::{ConnectedEv, EvDrawState, EvInfo, offered_current_a},
    runtime::Health,
};

/// What the charger offers when no command stands (or a TTL expired).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EvIdle {
    /// Offer nothing: the safe state the API's expiry semantics mean.
    #[default]
    Paused,
    /// Offer the full rating, as a charger with no EMS does.
    Full,
}

impl FromStr for EvIdle {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "paused" => Ok(EvIdle::Paused),
            "full" => Ok(EvIdle::Full),
            _ => Err(()),
        }
    }
}

impl fmt::Display for EvIdle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EvIdle::Paused => "paused",
            EvIdle::Full => "full",
        })
    }
}

#[derive(Clone, Debug)]
pub struct EvChargerConfig {
    pub rated_lower_w: f32,
    pub rated_upper_w: f32,
    /// 1 or 3: how many phases the charger is wired on.
    pub phases: u8,
    pub idle: EvIdle,
    pub command_delay: Duration,
    pub ramp_rate_w_per_s: f32,
    pub stream_jitter_pct: f32,
    /// Keep the armed command through a health fault and ramp back on
    /// recovery, like the solar inverter; off, the fault clears the
    /// command and recovery waits for a new one, like the battery
    /// inverter. A setpoint TTL that expires while the charger is
    /// faulted still clears the command — expiry is a control event,
    /// independent of health — so a fault outlasting the request's
    /// lifetime does not resume either way.
    pub resume_on_recovery: bool,
}

impl EvChargerConfig {
    /// What the charger offers with no command standing: nothing, or
    /// its full rating.
    fn idle_w(&self) -> f32 {
        match self.idle {
            EvIdle::Paused => 0.0,
            EvIdle::Full => self.rated_upper_w,
        }
    }
}

impl Default for EvChargerConfig {
    fn default() -> Self {
        Self {
            rated_lower_w: 0.0,
            rated_upper_w: 22_000.0,
            phases: 3,
            idle: EvIdle::Paused,
            command_delay: Duration::from_millis(500),
            ramp_rate_w_per_s: f32::INFINITY,
            stream_jitter_pct: 0.0,
            resume_on_recovery: false,
        }
    }
}

/// A charger config that answers a command the same tick: no delay,
/// no slew. The starting point for every charger test.
#[cfg(test)]
pub fn instant() -> EvChargerConfig {
    EvChargerConfig {
        command_delay: Duration::ZERO,
        ramp_rate_w_per_s: f32::INFINITY,
        ..Default::default()
    }
}

pub struct EvCharger {
    id: u64,
    name: String,
    interval: Duration,
    cfg: EvChargerConfig,
    state: Mutex<EvState>,
    /// Active (P) control path. Its `actual()` is the limit offered to
    /// the car, not the draw — the draw lives in `EvState::draw_w`.
    active: PowerAxis,
}

#[derive(Debug, Clone)]
struct EvState {
    ev: Option<ConnectedEv>,
    /// Power taken this tick.
    draw_w: f32,
    /// What the charger did with its car this tick; `Tripped` while
    /// faulted, `Paused` for an empty charger too (nothing to draw).
    last: EvDrawState,
}

impl EvCharger {
    pub fn new(id: u64, interval: Duration, cfg: EvChargerConfig) -> Self {
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
                ev: None,
                draw_w: 0.0,
                last: EvDrawState::Paused,
            }),
            active,
        }
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
        let mut s = self.state.lock();
        let Some(ev) = s.ev.as_mut() else {
            return false;
        };
        if let Some(pct) = sanitize_soc_pct("EvCharger::set_soc_pct", pct) {
            ev.soc_pct = pct;
        }
        true
    }

    fn takes_soc_pct(&self) -> bool {
        self.state.lock().ev.is_some()
    }

    fn takes_ev(&self) -> bool {
        true
    }

    fn plug_ev(&self, ev: ConnectedEv) -> Result<(), String> {
        let mut s = self.state.lock();
        if s.ev.is_some() {
            return Err("an EV is already plugged in".to_string());
        }
        s.ev = Some(ev);
        // A fresh car starts from a clean slate, the way `unplug_ev`
        // and `restore_knob` leave one: otherwise the draw and state
        // of whatever was here before are reported until the next
        // tick overwrites them.
        s.draw_w = 0.0;
        s.last = EvDrawState::Paused;
        Ok(())
    }

    fn unplug_ev(&self) -> bool {
        let mut s = self.state.lock();
        let had = s.ev.take().is_some();
        s.draw_w = 0.0;
        s.last = EvDrawState::Paused;
        had
    }

    fn ev_info(&self) -> Option<EvInfo> {
        let s = self.state.lock();
        s.ev.clone().map(|ev| EvInfo { ev, state: s.last })
    }

    fn snapshot_knob(&self, kind: KnobKind) -> Option<KnobSnapshot> {
        match kind {
            KnobKind::Ev => Some(KnobSnapshot::Ev(self.state.lock().ev.clone())),
            _ => None,
        }
    }

    fn restore_knob(&self, snap: KnobSnapshot) -> bool {
        match snap {
            KnobSnapshot::Ev(ev) => {
                let mut s = self.state.lock();
                s.ev = ev;
                s.draw_w = 0.0;
                s.last = EvDrawState::Paused;
                true
            }
            _ => false,
        }
    }

    fn tick(&self, world: &MicrogridSite, now: DateTime<Utc>, dt: Duration) {
        // 1. Own-health gate: a faulted or standby charger is offline.
        //    By default the command is cleared too, so recovery waits
        //    for a re-dispatch; with resume_on_recovery the offer
        //    collapses but the command survives.
        if world.runtime_of(self.id).health != Health::Ok {
            if self.cfg.resume_on_recovery {
                self.active.snap_output(0.0);
            } else {
                self.active.trip();
            }
            let mut s = self.state.lock();
            s.draw_w = 0.0;
            s.last = EvDrawState::Tripped;
            return;
        }

        // 2. The axis's output is the limit the charger offers: rated ∩
        //    augmentations, command delay, slew. With no command it
        //    idles at 0 (paused) or the full rating.
        let idle = IdleTarget::Value(self.cfg.idle_w());
        let limit_w = self.active.step(
            now,
            dt,
            StepCtx {
                other_axis: 0.0,
                dynamic: None,
                idle,
            },
        );

        // 3. The car decides what it takes within the offer.
        let mut s = self.state.lock();
        let offered = offered_current_a(limit_w, self.cfg.phases);
        let (draw, last) = match (s.ev.as_mut(), offered) {
            (None, _) => (0.0, EvDrawState::Paused),
            (Some(ev), _) if ev.done() => (0.0, EvDrawState::Done),
            (Some(_), None) => (0.0, EvDrawState::Paused),
            (Some(ev), Some(amps)) => {
                let p = ev.draw_w(amps, self.cfg.phases);
                ev.integrate(p, dt);
                (
                    p,
                    if p > 0.0 {
                        EvDrawState::Charging
                    } else {
                        EvDrawState::Paused
                    },
                )
            }
        };
        s.draw_w = draw;
        s.last = last;
    }

    fn telemetry(&self, site: &MicrogridSite) -> Telemetry {
        let grid = site.grid_state();
        let s = self.state.lock();
        Telemetry {
            id: self.id,
            category: Some(Category::EvCharger),
            active_power_w: Some(s.draw_w),
            // A P-only AC component: Q is settled at 0, said explicitly
            // so the stream and the UI do not read it as missing.
            reactive_power_var: Some(0.0),
            // The car's SoC rides the internal telemetry for the UI,
            // history and CSV; proto_conv drops it from the gRPC stream.
            soc_pct: s.ev.as_ref().map(|ev| ev.soc_pct),
            per_phase_voltage_v: Some(grid.voltage_per_phase),
            frequency_hz: Some(grid.frequency_hz),
            active_power_bounds: self.effective_active_bounds(),
            component_state: Some(if s.draw_w > 0.0 { "charging" } else { "ready" }),
            cable_state: Some(if s.ev.is_some() {
                "ev-charging-cable-locked-at-ev"
            } else {
                "ev-charging-cable-unplugged"
            }),
            ..Default::default()
        }
    }

    fn set_active_setpoint(&self, power_w: f32) -> Result<(), SetpointError> {
        // The limit is validated against rated ∩ augmentations; the car
        // never narrows the charger's envelope.
        self.active.accept(power_w, Utc::now(), 0.0)
    }

    fn try_augment_active_bounds(
        &self,
        ts: DateTime<Utc>,
        bounds: VecBounds,
        lifetime: Duration,
    ) -> Result<(), AugmentError> {
        self.active
            .try_augment(ts, bounds, lifetime, 0.0, None)
            .map_err(AugmentError::Disjoint)
    }

    fn augmentation_active(
        &self,
        axis: crate::timeout_tracker::SetpointAxis,
        now: DateTime<Utc>,
    ) -> bool {
        use crate::timeout_tracker::SetpointAxis;
        match axis {
            SetpointAxis::Active => self.active.augmented(now),
            SetpointAxis::Reactive => false,
        }
    }

    fn reset_setpoint(&self) {
        self.active.reset(self.cfg.idle_w());
    }

    fn active_power_w(&self, _site: &MicrogridSite) -> Option<f32> {
        Some(self.state.lock().draw_w)
    }

    fn aggregate_power_w(&self, _world: &MicrogridSite) -> f32 {
        self.state.lock().draw_w
    }

    fn rated_active_bounds(&self) -> Option<(f32, f32)> {
        Some((self.cfg.rated_lower_w, self.cfg.rated_upper_w))
    }

    fn effective_active_bounds(&self) -> Option<VecBounds> {
        Some(self.active.effective_static())
    }

    fn make_fn(&self) -> &'static str {
        "%make-ev-charger"
    }

    fn constructor_kwargs(&self) -> Vec<(&'static str, String)> {
        let lf = crate::lisp::lisp_float32;
        let mut kw = vec![
            (":rated-lower", lf(self.cfg.rated_lower_w)),
            (":rated-upper", lf(self.cfg.rated_upper_w)),
            (
                ":command-delay-ms",
                self.cfg.command_delay.as_millis().to_string(),
            ),
        ];
        if self.cfg.phases != 3 {
            kw.push((":phases", self.cfg.phases.to_string()));
        }
        if self.cfg.idle != EvIdle::Paused {
            kw.push((":idle", format!("'{}", self.cfg.idle)));
        }
        if self.cfg.ramp_rate_w_per_s.is_finite() {
            kw.push((":ramp-rate", lf(self.cfg.ramp_rate_w_per_s)));
        }
        if self.interval != Duration::from_millis(1000) {
            kw.push((":interval", self.interval.as_millis().to_string()));
        }
        if self.cfg.stream_jitter_pct != 0.0 {
            kw.push((":stream-jitter-pct", lf(self.cfg.stream_jitter_pct)));
        }
        if self.cfg.resume_on_recovery {
            kw.push((":resume-on-recovery", "t".to_string()));
        }
        kw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::ev_presets::{EvOverrides, preset, test_car};
    use crate::sim::runtime::Health;
    use std::sync::Arc;

    /// A 22 kW three-phase charger with no delay and no ramp, sited
    /// under id 7 so tests can drive its health.
    fn sited(cfg: EvChargerConfig) -> (MicrogridSite, Arc<dyn SimulatedComponent>) {
        let w = MicrogridSite::new();
        w.register(EvCharger::new(7, Duration::from_secs(1), cfg));
        let ev = w.get(7).unwrap();
        (w, ev)
    }

    fn tick_n(w: &MicrogridSite, ev: &Arc<dyn SimulatedComponent>, n: usize) {
        for _ in 0..n {
            ev.tick(w, Utc::now(), Duration::from_secs(1));
        }
    }

    #[test]
    fn empty_charger_draws_nothing_and_still_advertises_its_rating() {
        let (w, ev) = sited(instant());
        ev.set_active_setpoint(22_000.0).unwrap();
        tick_n(&w, &ev, 3);
        assert_eq!(ev.aggregate_power_w(&w), 0.0);
        let b = ev.effective_active_bounds().unwrap();
        assert_eq!(b.0[0].upper, Some(22_000.0), "bounds are the charger's own");
        assert!(ev.ev_info().is_none());
    }

    #[test]
    fn plugged_car_draws_within_the_limit_on_its_own_phases() {
        let (w, ev) = sited(instant());
        ev.plug_ev(test_car("city", Some(30.0))).unwrap(); // 1 ph, 32 A
        ev.set_active_setpoint(22_000.0).unwrap();
        tick_n(&w, &ev, 2);
        // 22 kW over the charger's three phases is 31.9 A; the car takes
        // that on its single phase: 7.33 kW.
        let p = ev.aggregate_power_w(&w);
        assert!(
            (p - 22_000.0 / 3.0).abs() < 1.0,
            "one phase of the offer, got {p}"
        );
        assert_eq!(ev.ev_info().unwrap().state, EvDrawState::Charging);
    }

    #[test]
    fn limit_under_six_amps_pauses() {
        let (w, ev) = sited(instant());
        ev.plug_ev(test_car("sedan", Some(30.0))).unwrap();
        ev.set_active_setpoint(4_000.0).unwrap(); // 5.8 A on 3 phases
        tick_n(&w, &ev, 2);
        assert_eq!(ev.aggregate_power_w(&w), 0.0);
        assert_eq!(ev.ev_info().unwrap().state, EvDrawState::Paused);
        ev.set_active_setpoint(4_200.0).unwrap(); // 6.09 A
        tick_n(&w, &ev, 2);
        assert!(ev.aggregate_power_w(&w) > 4_100.0);
    }

    #[test]
    fn idle_paused_draws_nothing_and_idle_full_draws_the_cap() {
        let (w, ev) = sited(instant());
        ev.plug_ev(test_car("sedan", Some(30.0))).unwrap();
        tick_n(&w, &ev, 2);
        assert_eq!(ev.aggregate_power_w(&w), 0.0, "no command, paused idle");

        let (w, ev) = sited(EvChargerConfig {
            idle: EvIdle::Full,
            ..instant()
        });
        ev.plug_ev(test_car("sedan", Some(30.0))).unwrap();
        tick_n(&w, &ev, 2);
        let p = ev.aggregate_power_w(&w);
        assert!((p - 3.0 * 230.0 * 16.0).abs() < 1.0, "11 kW cap, got {p}");
    }

    #[test]
    fn target_soc_ends_the_session_and_unplug_clears_it() {
        let (w, ev) = sited(instant());
        ev.plug_ev(
            ConnectedEv::new(
                preset("phev").unwrap(),
                &EvOverrides {
                    soc_pct: Some(99.9),
                    target_soc_pct: Some(100.0),
                    ..Default::default()
                },
                Utc::now(),
            )
            .unwrap(),
        )
        .unwrap();
        ev.set_active_setpoint(22_000.0).unwrap();
        // ~4 A after the taper on a 13 kWh pack: 0.1 % takes ~50 s.
        tick_n(&w, &ev, 90);
        assert_eq!(ev.ev_info().unwrap().state, EvDrawState::Done);
        assert_eq!(ev.aggregate_power_w(&w), 0.0);
        assert!(ev.unplug_ev());
        assert!(ev.ev_info().is_none());
        assert!(!ev.unplug_ev(), "empty already");
    }

    /// A plug is a clean slate: whatever the charger last did with
    /// its previous car (here `Tripped`, left behind by a fault the
    /// charger has since recovered from) must not be reported as the
    /// new car's state in the window before the next tick.
    /// Killing mutation: drop the `draw_w`/`last` reset in `plug_ev`.
    #[test]
    fn plugging_clears_the_previous_session_state() {
        let (w, ev) = sited(instant());
        w.set_health(7, Health::Error).unwrap();
        tick_n(&w, &ev, 1);
        assert_eq!(ev.ev_info().map(|i| i.state), None, "nothing plugged yet");
        w.set_health(7, Health::Ok).unwrap();
        ev.plug_ev(test_car("sedan", Some(30.0))).unwrap();
        assert_eq!(
            ev.ev_info().unwrap().state,
            EvDrawState::Paused,
            "a freshly plugged car is paused, not tripped",
        );
        assert_eq!(ev.aggregate_power_w(&w), 0.0);
    }

    #[test]
    fn plug_while_plugged_errors() {
        let (_w, ev) = sited(instant());
        ev.plug_ev(test_car("sedan", Some(30.0))).unwrap();
        assert!(ev.plug_ev(test_car("van", Some(30.0))).is_err());
        assert_eq!(ev.ev_info().unwrap().ev.preset, "sedan");
    }

    #[test]
    fn health_trip_still_zeroes_and_clears_the_command() {
        let (w, ev) = sited(instant());
        ev.plug_ev(test_car("sedan", Some(30.0))).unwrap();
        ev.set_active_setpoint(22_000.0).unwrap();
        tick_n(&w, &ev, 2);
        assert!(ev.aggregate_power_w(&w) > 10_000.0);
        w.set_health(7, Health::Error).unwrap();
        tick_n(&w, &ev, 1);
        assert_eq!(ev.aggregate_power_w(&w), 0.0);
        assert_eq!(ev.ev_info().unwrap().state, EvDrawState::Tripped);
        w.set_health(7, Health::Ok).unwrap();
        tick_n(&w, &ev, 3);
        assert_eq!(
            ev.aggregate_power_w(&w),
            0.0,
            "no command survives the trip"
        );
    }

    #[test]
    fn ttl_expiry_slews_the_limit_and_the_draw_follows() {
        let (w, ev) = sited(EvChargerConfig {
            ramp_rate_w_per_s: 1_000.0,
            ..instant()
        });
        ev.plug_ev(test_car("van", Some(30.0))).unwrap(); // 3 ph 32 A = 22 kW cap
        ev.set_active_setpoint(22_000.0).unwrap();
        tick_n(&w, &ev, 25);
        assert!((ev.aggregate_power_w(&w) - 22_000.0).abs() < 100.0);
        ev.reset_setpoint();
        tick_n(&w, &ev, 1);
        let p = ev.aggregate_power_w(&w);
        assert!(p > 20_000.0 && p < 22_000.0, "one tick of slew, got {p}");
    }

    #[test]
    fn energy_matches_the_integrated_draw() {
        let (w, ev) = sited(instant());
        ev.plug_ev(test_car("sedan", Some(30.0))).unwrap();
        ev.set_active_setpoint(22_000.0).unwrap();
        tick_n(&w, &ev, 10);
        let info = ev.ev_info().unwrap();
        // First tick has no draw yet (limit promoted this tick), so nine
        // seconds at 11 040 W.
        let expected = 9.0 * 11_040.0 / 3600.0;
        assert!(
            (info.ev.energy_wh - expected).abs() < 5.0,
            "got {}",
            info.ev.energy_wh
        );
    }

    #[test]
    fn set_soc_moves_the_plugged_car_only() {
        let (_w, ev) = sited(instant());
        assert!(!ev.takes_soc_pct(), "no car, no SoC");
        ev.plug_ev(test_car("sedan", Some(30.0))).unwrap();
        assert!(ev.takes_soc_pct());
        assert!(ev.set_soc_pct(55.0));
        assert_eq!(ev.ev_info().unwrap().ev.soc_pct, 55.0);
    }

    #[test]
    fn knob_snapshot_restores_the_plug_state() {
        let (_w, ev) = sited(instant());
        let before = ev.snapshot_knob(KnobKind::Ev);
        assert!(matches!(before, Some(KnobSnapshot::Ev(None))));
        ev.plug_ev(test_car("sedan", Some(30.0))).unwrap();
        assert!(ev.restore_knob(before.unwrap()));
        assert!(ev.ev_info().is_none(), "restore unplugs");
        ev.plug_ev(test_car("van", Some(30.0))).unwrap();
        let plugged = ev.snapshot_knob(KnobKind::Ev).unwrap();
        assert!(ev.unplug_ev());
        assert!(ev.restore_knob(plugged));
        assert_eq!(ev.ev_info().unwrap().ev.preset, "van");
    }

    #[test]
    fn constructor_kwargs_round_trip() {
        let kw = |cfg: EvChargerConfig| {
            EvCharger::new(9, Duration::from_secs(1), cfg)
                .constructor_kwargs()
                .into_iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let d = kw(EvChargerConfig::default());
        assert!(d.contains(":rated-upper 22000"), "{d}");
        assert!(!d.contains(":phases"), "default phases omitted: {d}");
        assert!(!d.contains(":idle"), "default idle omitted: {d}");
        assert!(!d.contains(":capacity"), "the pack is gone: {d}");
        let s = kw(EvChargerConfig {
            phases: 1,
            idle: EvIdle::Full,
            ..Default::default()
        });
        assert!(s.contains(":phases 1"), "{s}");
        assert!(s.contains(":idle 'full"), "{s}");
    }
}
