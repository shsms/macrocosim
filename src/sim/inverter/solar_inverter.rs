//! Solar (PV) inverter. Active-side: produces a negative power
//! proportional to `sunlight_pct`, slewed by the ramp + command-delay
//! pair. Reactive-side: a second [`PowerAxis`], the same one the
//! battery inverter uses — a real PV smart inverter (IEEE 1547-2018)
//! does Volt/VAR control alongside its real-power output.

use std::{fmt, time::Duration};

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use tulisp::TulispContext;

use crate::sim::{
    Category, MicrogridSite, SetpointError, SimulatedComponent, Telemetry,
    axis::{AxisConfig, IdleTarget, PowerAxis, StepCtx},
    bounds::VecBounds,
    component::{KnobKind, KnobSnapshot, ScalarReading},
    dynamic_scalar::DynamicScalar,
    reactive::ReactiveCapability,
    runtime::Health,
};

#[derive(Clone, Debug)]
pub struct SolarInverterConfig {
    pub rated_lower_w: f32,
    pub rated_upper_w: f32,
    pub sunlight_pct: f32,
    pub command_delay: Duration,
    pub ramp_rate_w_per_s: f32,
    pub stream_jitter_pct: f32,
    /// Q envelope. Default microsim-compatible PF cap of 0.35.
    pub reactive: ReactiveCapability,
    /// SCADA / inverter-internal latency before a Q setpoint starts
    /// being tracked. 100 ms default.
    pub reactive_command_delay: Duration,
    /// Reactive slew rate (VAR/s). 2000 default ≈ 5 s OLRT for a
    /// 10 kVAR window — IEEE 1547-2018 Cat B baseline.
    pub reactive_ramp_rate_var_per_s: f32,
    /// True when `:sunlight%` was constructed as a lambda or symbol
    /// rather than a plain number. Not a plist kwarg itself — it
    /// only tells the microgrid-file renderer to omit `:sunlight%`
    /// (a dynamic source can't round-trip as a static number) rather
    /// than write out `sunlight_pct`'s stale fallback value.
    pub sunlight_dynamic: bool,
}

impl Default for SolarInverterConfig {
    fn default() -> Self {
        Self {
            rated_lower_w: -30_000.0,
            rated_upper_w: 0.0,
            sunlight_pct: 100.0,
            command_delay: Duration::ZERO,
            ramp_rate_w_per_s: f32::INFINITY,
            stream_jitter_pct: 0.0,
            reactive: ReactiveCapability::microsim_default(),
            reactive_command_delay: Duration::from_millis(100),
            reactive_ramp_rate_var_per_s: 2000.0,
            sunlight_dynamic: false,
        }
    }
}

pub struct SolarInverter {
    id: u64,
    name: String,
    interval: Duration,
    cfg: SolarInverterConfig,
    /// Cloud-cover percentage. Either a constant (the cfg default
    /// or a numeric `:sunlight%`) or a Lisp expression
    /// (`:sunlight% (lambda () …)` / `:sunlight% 'symbol`) re-
    /// resolved each tick by `refresh_inputs`. Lisp timers can
    /// also push values via `(set-solar-sunlight ID PCT)`, which
    /// collapses any prior dynamic source to a constant.
    sunlight_source: RwLock<DynamicScalar>,
    /// Active (P) control path: rated band + TTL augmentations,
    /// command delay, slew ramp. Its `published` slot is unused — PV
    /// has no children to clip P, so telemetry reads `actual()`.
    active: PowerAxis,
    /// Reactive (Q) control path: the PF/kVA capability envelope
    /// evaluated at the live P, command delay, slew ramp, and the
    /// published Q telemetry reads.
    reactive: PowerAxis,
}

impl SolarInverter {
    pub fn new(id: u64, interval: Duration, cfg: SolarInverterConfig) -> Self {
        let init_pct = cfg.sunlight_pct;
        let active = PowerAxis::new(AxisConfig {
            rated: Some((cfg.rated_lower_w, cfg.rated_upper_w)),
            caps: None,
            command_delay: cfg.command_delay,
            ramp_rate_per_s: cfg.ramp_rate_w_per_s,
            unit: "W",
        });
        // A fresh PV inverter is already generating from whatever sun
        // it has — it does not slew up from zero on its first tick.
        active.snap_output(cfg.rated_lower_w * init_pct / 100.0);
        // A Q axis has no rated band of its own — its static shape is
        // the PF/kVA capability evaluated at the live P.
        let reactive = PowerAxis::new(AxisConfig {
            rated: None,
            caps: Some(cfg.reactive),
            command_delay: cfg.reactive_command_delay,
            ramp_rate_per_s: cfg.reactive_ramp_rate_var_per_s,
            unit: "VAr",
        });
        Self {
            id,
            name: format!("inv-pv-{id}"),
            interval,
            cfg,
            sunlight_source: RwLock::new(DynamicScalar::constant(init_pct)),
            active,
            reactive,
        }
    }

    /// Replace the cloud-cover source with a Lisp expression that
    /// `refresh_inputs` re-resolves each tick. The make-path uses
    /// this when `:sunlight%` is a lambda or symbol; the default is
    /// a constant seeded from `cfg.sunlight_pct`.
    pub fn set_sunlight_source(&self, scalar: DynamicScalar) {
        *self.sunlight_source.write() = scalar;
    }

    /// Replace the cloud-cover source with a constant. Drives the
    /// per-tick `min_avail = rated_lower_w × sunlight_pct / 100`
    /// clamp the inverter applies to incoming setpoints. No clamp
    /// on the input — out-of-range values just produce out-of-range
    /// `min_avail`, mirroring microsim. Collapses any prior dynamic
    /// source so subsequent refreshes are no-ops.
    pub fn set_sunlight_pct(&self, pct: f32) {
        *self.sunlight_source.write() = DynamicScalar::constant(pct);
    }

    pub fn sunlight_pct(&self) -> f32 {
        self.sunlight_source.read().get()
    }

    fn min_avail_w(&self) -> f32 {
        self.cfg.rated_lower_w * self.sunlight_pct() / 100.0
    }
}

impl fmt::Display for SolarInverter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl SimulatedComponent for SolarInverter {
    fn id(&self) -> u64 {
        self.id
    }
    fn category(&self) -> Category {
        Category::Inverter
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn stream_interval(&self) -> Duration {
        self.interval
    }
    fn refresh_inputs(&self, ctx: &mut TulispContext) {
        // No-op for the constant case — DynamicScalar::refresh
        // returns immediately when there's no source expression.
        self.sunlight_source.read().refresh(ctx);
    }

    fn tick(&self, world: &MicrogridSite, now: DateTime<Utc>, dt: Duration) {
        // Own-health gate: a faulted or standby PV inverter is tripped
        // offline — zero output. It must NOT fall back to its default of
        // producing from sunlight. Unlike a battery inverter (which awaits
        // re-dispatch), a recovered PV inverter reconnects and resumes
        // generating from whatever sunlight is available, so the healthy
        // path below picks production back up on its own — we only snap the
        // live output to zero and leave any curtailment setpoint intact.
        //
        // The reactive axis gets the harder treatment — a full trip,
        // clearing its armed command too. Q comes from the IGBTs
        // switching, and they have stopped; without the trip an
        // Error→Ok recovery would resurrect the pre-trip Q with nobody
        // having dispatched it (todo #998).
        if world.runtime_of(self.id).health != Health::Ok {
            self.active.snap_output(0.0);
            self.reactive.trip();
            return;
        }
        // What sunlight allows right now. Production is negative, so
        // `[avail, 0]` is the band the active axis may sit in — a
        // curtailment inside it is honoured, anything asking for more
        // production than the sun gives is pulled back to `avail`, and
        // a free-running inverter idles right at `avail` (tracks the
        // sun). Passed as the per-tick dynamic band rather than folded
        // into the static bounds, since it changes every tick and must
        // not leak into telemetry's advertised envelope.
        let avail = self.min_avail_w();
        // The band's upper edge is the rated upper — `0.0` for a
        // pure-generation config, but a `:rated-upper > 0` solar
        // config (e.g. one that can also sink power) needs its real
        // upper edge here, not a hard-coded 0.
        let sun = VecBounds::single(avail, self.cfg.rated_upper_w);
        // The axis intersects that with rated ∩ live augmentations: an
        // augmented cap actually reduces generation, and generation
        // recovers toward available when the cap relaxes. (microsim
        // parity: power limited to live bounds.) If the two do not
        // overlap at all there is no legal output, so the axis parks
        // at 0 rather than generating sun it does not have.
        let p = self.active.step(
            now,
            dt,
            StepCtx {
                other_axis: 0.0,
                dynamic: Some(&sun),
                idle: IdleTarget::Value(avail),
            },
        );
        // Reactive: validated when accepted, re-clamped to the live
        // envelope at p as the command is promoted, then slewed.
        // Solar has no children to clip Q so step()'s auto-publish
        // is what telemetry reads next tick.
        self.reactive.step(
            now,
            dt,
            StepCtx {
                other_axis: p,
                dynamic: None,
                idle: IdleTarget::Hold,
            },
        );
    }

    fn telemetry(&self, site: &MicrogridSite) -> Telemetry {
        let p = self.active.actual();
        super::inverter_telemetry(
            self.id,
            site,
            p,
            self.reactive.published(),
            self.active.effective_static(),
            // The trait's telemetry-shaped Q envelope: the live
            // envelope at the measured P it samples itself (normally
            // the `p` above, though a concurrent tick between the two
            // reads can slide it by one step), with a genuinely empty
            // one (a live Q augmentation disjoint from the caps band, or two
            // live augmentations disjoint from each other) normalized
            // to a present (0, 0) band — otherwise every telemetry
            // consumer sees an absent bound instead of the real "zero
            // headroom" answer. Always `Some` for an inverter.
            self.reactive_bounds().unwrap_or_default(),
        )
    }

    fn set_active_setpoint(&self, power_w: f32) -> Result<(), SetpointError> {
        // Wall clock, not the tick clock: this runs on a gRPC/UI
        // thread with no access to the site's clock, and that is how
        // setpoint validation has always judged augmentation liveness.
        self.active.accept(power_w, Utc::now(), 0.0)
    }

    fn set_reactive_setpoint(&self, vars: f32) -> Result<(), SetpointError> {
        self.reactive.accept(vars, Utc::now(), self.active.actual())
    }

    fn reset_setpoint(&self) {
        // Ramp back toward the cloud-cover-determined floor rather
        // than snapping — a reset is a control event, not a physical
        // discontinuity, and the battery inverter's reset ramps the
        // same way. The per-tick clamp recomputes the target from
        // live sunlight anyway, so a reset racing a cloud shift still
        // converges on the right floor.
        self.active.reset(self.min_avail_w());
        self.reactive.reset(0.0);
    }

    fn reset_setpoint_axis(&self, axis: crate::timeout_tracker::SetpointAxis) {
        // Dual-axis: an expired curtailment (active) releases back to
        // the sunlight floor without disturbing a running Volt/VAR
        // command, and vice versa.
        use crate::timeout_tracker::SetpointAxis;
        match axis {
            SetpointAxis::Active => self.active.reset(self.min_avail_w()),
            SetpointAxis::Reactive => self.reactive.reset(0.0),
        }
    }

    fn augmentation_active(
        &self,
        axis: crate::timeout_tracker::SetpointAxis,
        now: DateTime<Utc>,
    ) -> bool {
        use crate::timeout_tracker::SetpointAxis;
        match axis {
            SetpointAxis::Active => self.active.augmented(now),
            SetpointAxis::Reactive => self.reactive.augmented(now),
        }
    }

    /// `None` for the dynamic slot, deliberately: the sunlight band
    /// `tick` passes to `step` is a per-tick availability window that
    /// swings with the sun and is explicitly kept OUT of the
    /// advertised envelope (`effective_active_bounds` below is rated ∩
    /// augmentations only). Gating augmentations on it would bounce a
    /// legal curtailment at night and accept it again at noon; the
    /// derate-aware gate is for the EV/boiler bands, which telemetry
    /// does advertise.
    fn try_augment_active_bounds(
        &self,
        ts: DateTime<Utc>,
        bounds: crate::sim::bounds::VecBounds,
        lifetime: Duration,
    ) -> Result<(), crate::sim::bounds::VecBounds> {
        self.active.try_augment(ts, bounds, lifetime, 0.0, None)
    }

    fn try_augment_reactive_bounds(
        &self,
        ts: DateTime<Utc>,
        bounds: crate::sim::bounds::VecBounds,
        lifetime: Duration,
    ) -> Result<(), crate::sim::bounds::VecBounds> {
        // `None`: the Q axis carries no dynamic band (`step` passes
        // none either) — its whole shape is the caps band at P.
        self.reactive
            .try_augment(ts, bounds, lifetime, self.active.actual(), None)
    }

    fn active_power_w(&self, _site: &MicrogridSite) -> Option<f32> {
        Some(self.active.actual())
    }

    fn aggregate_power_w(&self, _world: &MicrogridSite) -> f32 {
        self.active.actual()
    }

    fn aggregate_reactive_var(&self, _world: &MicrogridSite) -> f32 {
        self.reactive.published()
    }

    fn rated_active_bounds(&self) -> Option<(f32, f32)> {
        Some((self.cfg.rated_lower_w, self.cfg.rated_upper_w))
    }

    fn reactive_bounds_raw(&self) -> Option<VecBounds> {
        let p = self.active.actual();
        Some(self.reactive.tracking_envelope_at(Utc::now(), p, None))
    }

    fn reactive_capability(&self) -> Option<crate::sim::reactive::ReactiveCapability> {
        self.reactive.capability()
    }

    fn set_reactive_pf_limit(&self, pf: Option<f32>) {
        self.reactive.set_pf_limit(pf);
    }

    fn set_reactive_apparent_va(&self, va: Option<f32>) {
        self.reactive.set_apparent_va(va);
    }

    fn subtype(&self) -> Option<&'static str> {
        Some("solar")
    }

    fn stream_jitter_pct(&self) -> f32 {
        self.cfg.stream_jitter_pct
    }

    fn effective_active_bounds(&self) -> Option<crate::sim::bounds::VecBounds> {
        Some(self.active.effective_static())
    }

    fn set_sunlight_pct(&self, pct: f32) -> bool {
        SolarInverter::set_sunlight_pct(self, pct);
        true
    }

    fn takes_sunlight_pct(&self) -> bool {
        true
    }

    fn set_sunlight_source(&self, scalar: DynamicScalar) {
        SolarInverter::set_sunlight_source(self, scalar);
    }

    fn sunlight_reading(&self) -> Option<ScalarReading> {
        let s = self.sunlight_source.read();
        Some(ScalarReading {
            value: s.get(),
            expr: s.source_text(),
        })
    }

    fn snapshot_knob(&self, kind: KnobKind) -> Option<KnobSnapshot> {
        match kind {
            KnobKind::Sunlight => Some(KnobSnapshot::Sunlight(self.sunlight_source.read().clone())),
            _ => None,
        }
    }

    fn restore_knob(&self, snap: KnobSnapshot) -> bool {
        match snap {
            KnobSnapshot::Sunlight(scalar) => {
                *self.sunlight_source.write() = scalar;
                true
            }
            _ => false,
        }
    }

    fn make_fn(&self) -> &'static str {
        "%make-solar-inverter"
    }

    fn has_unrenderable_source(&self) -> bool {
        // A dynamic sunlight source has no static number to write.
        // Both spellings count: constructed dynamic (which
        // `constructor_kwargs` already omits `:sunlight%` for) and a
        // runtime `(set-solar-sunlight ID (lambda …))` poke, whose
        // expression the generated block cannot carry either — the
        // same case Meter reports for `set-meter-power`.
        self.cfg.sunlight_dynamic || self.sunlight_source.read().is_dynamic()
    }

    fn constructor_kwargs(&self) -> Vec<(&'static str, String)> {
        let mut kw = super::common_inverter_kwargs(super::CommonInverterCfg {
            rated_lower_w: self.cfg.rated_lower_w,
            rated_upper_w: self.cfg.rated_upper_w,
            command_delay: self.cfg.command_delay,
            ramp_rate_w_per_s: self.cfg.ramp_rate_w_per_s,
            interval: self.interval,
            stream_jitter_pct: self.cfg.stream_jitter_pct,
            reactive: self.cfg.reactive,
            reactive_command_delay: self.cfg.reactive_command_delay,
            reactive_ramp_rate_var_per_s: self.cfg.reactive_ramp_rate_var_per_s,
        });
        // A dynamic sunlight source can't round-trip as a static
        // number — the renderer omits :sunlight% entirely rather
        // than writing the (possibly stale) fallback value.
        if !self.cfg.sunlight_dynamic {
            kw.push((
                ":sunlight%",
                crate::lisp::lisp_float32(self.cfg.sunlight_pct),
            ));
        }
        kw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_sun(pct: f32) -> SolarInverterConfig {
        SolarInverterConfig {
            rated_lower_w: -10_000.0,
            rated_upper_w: 0.0,
            sunlight_pct: pct,
            ramp_rate_w_per_s: f32::INFINITY,
            ..Default::default()
        }
    }

    #[test]
    fn sunlight_pct_drives_min_avail_floor() {
        let inv = SolarInverter::new(1, Duration::from_secs(1), cfg_with_sun(50.0));
        // 50% of -10 kW rated = -5 kW available.
        assert!((inv.min_avail_w() - (-5_000.0)).abs() < 1e-3);

        // Sun goes behind a cloud → less generation available.
        inv.set_sunlight_pct(20.0);
        assert!((inv.min_avail_w() - (-2_000.0)).abs() < 1e-3);

        // Out-of-range values pass through (microsim parity).
        inv.set_sunlight_pct(150.0);
        assert!((inv.min_avail_w() - (-15_000.0)).abs() < 1e-3);
    }

    /// A dynamic sunlight source resolves on each `refresh_inputs`,
    /// driving the min_avail floor without going through the
    /// imperative `(set-solar-sunlight)` setter.
    #[test]
    fn dynamic_sunlight_source_refreshes() {
        let mut ctx = tulisp::TulispContext::new();
        let inv = SolarInverter::new(1, Duration::from_secs(1), cfg_with_sun(100.0));
        let lambda = ctx.eval_string("(lambda () 40.0)").unwrap();
        let scalar = DynamicScalar::from_lisp(&lambda, 100.0).expect("lambda → dynamic");
        inv.set_sunlight_source(scalar);

        // Pre-refresh: the cached fallback (100.0) is still in effect.
        assert!((inv.min_avail_w() - (-10_000.0)).abs() < 1e-3);

        // Refresh resolves the lambda → 40% of -10 kW rated = -4 kW.
        inv.refresh_inputs(&mut ctx);
        assert!((inv.min_avail_w() - (-4_000.0)).abs() < 1e-3);
    }

    /// `set_sunlight_pct` collapses any prior dynamic source, so a
    /// timer- or scenario-driven imperative override wins over a
    /// configured lambda.
    #[test]
    fn set_sunlight_pct_collapses_dynamic_source() {
        let mut ctx = tulisp::TulispContext::new();
        let inv = SolarInverter::new(1, Duration::from_secs(1), cfg_with_sun(100.0));
        let lambda = ctx.eval_string("(lambda () 70.0)").unwrap();
        inv.set_sunlight_source(DynamicScalar::from_lisp(&lambda, 100.0).unwrap());
        inv.refresh_inputs(&mut ctx);
        assert!((inv.min_avail_w() - (-7_000.0)).abs() < 1e-3);

        inv.set_sunlight_pct(30.0);
        // Subsequent refresh is a no-op on the constant.
        inv.refresh_inputs(&mut ctx);
        assert!((inv.min_avail_w() - (-3_000.0)).abs() < 1e-3);
    }

    /// A faulted PV inverter trips offline: it produces nothing rather
    /// than falling back to full sunlight-tracking. On recovery it
    /// reconnects and resumes producing from the available sunlight.
    #[test]
    fn errored_inverter_stops_producing() {
        let w = MicrogridSite::new();
        let inv = SolarInverter::new(1, Duration::from_secs(1), cfg_with_sun(100.0));
        w.register(inv);
        let inv = w.get(1).unwrap();
        let dt = Duration::from_millis(100);

        // Healthy at full sun: produces its rated -10 kW.
        inv.tick(&w, Utc::now(), dt);
        assert!((inv.aggregate_power_w(&w) - (-10_000.0)).abs() < 1.0);

        // Errored: tripped offline, zero output — NOT sunlight production.
        w.set_health(1, Health::Error);
        inv.tick(&w, Utc::now(), dt);
        assert!(
            inv.aggregate_power_w(&w).abs() < 1.0,
            "errored PV inverter must produce 0 W, got {}",
            inv.aggregate_power_w(&w),
        );

        // Recovery: a PV inverter reconnects and resumes from sunlight.
        w.set_health(1, Health::Ok);
        inv.tick(&w, Utc::now(), dt);
        assert!((inv.aggregate_power_w(&w) - (-10_000.0)).abs() < 1.0);
    }

    /// A health trip kills the reactive axis outright (todo #998): Q
    /// snaps to zero AND its armed command is cleared, so an Error→Ok
    /// recovery does not resurrect the pre-trip Q. The ACTIVE axis
    /// keeps its armed curtailment, because a recovered PV inverter
    /// reconnects and picks generation back up on its own.
    #[test]
    fn health_trip_trips_q_but_keeps_the_armed_curtailment() {
        let w = MicrogridSite::new();
        let mut cfg = cfg_with_sun(100.0);
        // kVA-shaped Q envelope with no delay and no slew, so a single
        // tick settles a Q command at any P.
        cfg.reactive = ReactiveCapability {
            pf_limit: None,
            apparent_va: Some(10_000.0),
        };
        cfg.reactive_command_delay = Duration::ZERO;
        cfg.reactive_ramp_rate_var_per_s = f32::INFINITY;
        w.register(SolarInverter::new(1, Duration::from_secs(1), cfg));
        let inv = w.get(1).unwrap();
        let dt = Duration::from_millis(100);

        // Curtail production to -4 kW (full sun would be -10 kW) and
        // dispatch 2 kVAR on top.
        inv.set_active_setpoint(-4_000.0).unwrap();
        inv.tick(&w, Utc::now(), dt);
        inv.set_reactive_setpoint(2_000.0).unwrap();
        inv.tick(&w, Utc::now(), dt);
        assert!((inv.aggregate_power_w(&w) - (-4_000.0)).abs() < 1.0);
        assert!((inv.aggregate_reactive_var(&w) - 2_000.0).abs() < 1.0);

        // Trip: both axes read zero.
        w.set_health(1, Health::Error);
        inv.tick(&w, Utc::now(), dt);
        assert!(inv.aggregate_power_w(&w).abs() < 1.0, "P snaps to 0");
        assert!(
            inv.aggregate_reactive_var(&w).abs() < 1.0,
            "Q snaps to 0, got {}",
            inv.aggregate_reactive_var(&w),
        );

        // Recovery: P resumes at the ARMED curtailment, not full sun;
        // Q stays parked until something dispatches it again.
        w.set_health(1, Health::Ok);
        inv.tick(&w, Utc::now(), dt);
        assert!(
            (inv.aggregate_power_w(&w) - (-4_000.0)).abs() < 1.0,
            "P resumes at the curtailment, got {}",
            inv.aggregate_power_w(&w),
        );
        assert!(
            inv.aggregate_reactive_var(&w).abs() < 1.0,
            "Q must await re-dispatch, got {}",
            inv.aggregate_reactive_var(&w),
        );

        // A fresh Q command brings the reactive axis back.
        inv.set_reactive_setpoint(1_500.0).unwrap();
        inv.tick(&w, Utc::now(), dt);
        assert!((inv.aggregate_reactive_var(&w) - 1_500.0).abs() < 1.0);
    }

    /// A reset releases a curtailment back to the SUNLIGHT FLOOR, not
    /// to zero: a PV inverter with nothing dispatched at it generates
    /// whatever the sun gives, so `reset_setpoint` parks the active
    /// axis at `min_avail_w()` and the next tick has it producing
    /// there again. Both entry points do it — the full reset and the
    /// per-axis path the `TimeoutTracker` calls when only the ACTIVE
    /// request's lifetime lapses, which must leave a Q command running
    /// alongside untouched.
    ///
    /// What a tick can actually see is the release: the armed
    /// curtailment is gone and production is back at the floor. The
    /// `min_avail_w()` park value handed to `PowerAxis::reset` is
    /// belt-and-braces on top — `step`'s `IdleTarget::Value(avail)`
    /// re-derives the same floor before the ramp advances, so no tick
    /// ever aims at a stale target either way. Parking at 0 there
    /// would be a latent trap for any future caller that reads the
    /// axis between the reset and the next tick, which is why the park
    /// value stays the floor.
    #[test]
    fn reset_releases_a_curtailment_back_to_the_sunlight_floor() {
        use crate::timeout_tracker::SetpointAxis;

        let w = MicrogridSite::new();
        // -10 kW rated at 60% sun → a -6 kW floor, distinct from both
        // zero and the curtailments dispatched below.
        let mut cfg = cfg_with_sun(60.0);
        cfg.reactive = ReactiveCapability {
            pf_limit: None,
            apparent_va: Some(10_000.0),
        };
        cfg.reactive_command_delay = Duration::ZERO;
        cfg.reactive_ramp_rate_var_per_s = f32::INFINITY;
        w.register(SolarInverter::new(1, Duration::from_secs(1), cfg));
        let inv = w.get(1).unwrap();
        let dt = Duration::from_millis(100);
        let floor = -6_000.0;

        // Curtail to -2 kW, well inside what the sun allows.
        inv.set_active_setpoint(-2_000.0).unwrap();
        inv.tick(&w, Utc::now(), dt);
        assert!(
            (inv.aggregate_power_w(&w) - (-2_000.0)).abs() < 1.0,
            "curtailed to -2 kW, got {}",
            inv.aggregate_power_w(&w),
        );

        // Full reset: production returns to the floor, NOT to 0 W.
        inv.reset_setpoint();
        inv.tick(&w, Utc::now(), dt);
        assert!(
            (inv.aggregate_power_w(&w) - floor).abs() < 1.0,
            "reset must return to the sunlight floor {floor}, got {}",
            inv.aggregate_power_w(&w),
        );

        // The active-axis TTL path, with a Q command running alongside.
        inv.set_active_setpoint(-1_000.0).unwrap();
        inv.set_reactive_setpoint(3_000.0).unwrap();
        inv.tick(&w, Utc::now(), dt);
        assert!((inv.aggregate_power_w(&w) - (-1_000.0)).abs() < 1.0);
        assert!((inv.aggregate_reactive_var(&w) - 3_000.0).abs() < 1.0);

        inv.reset_setpoint_axis(SetpointAxis::Active);
        inv.tick(&w, Utc::now(), dt);
        assert!(
            (inv.aggregate_power_w(&w) - floor).abs() < 1.0,
            "an expired curtailment releases to the floor {floor}, got {}",
            inv.aggregate_power_w(&w),
        );
        assert!(
            (inv.aggregate_reactive_var(&w) - 3_000.0).abs() < 1.0,
            "the Q command survives an active-axis expiry, got {}",
            inv.aggregate_reactive_var(&w),
        );
    }

    /// An augmentation demanding MORE production than the sun allows
    /// does not overlap the sunlight band at all, so the active axis
    /// has nowhere legal to sit and parks at 0. It must NOT produce at
    /// the augmentation's edge on sunlight it does not have.
    #[test]
    fn augmentation_beyond_available_sun_parks_at_zero() {
        let w = MicrogridSite::new();
        let cfg = SolarInverterConfig {
            rated_lower_w: -30_000.0,
            rated_upper_w: 0.0,
            sunlight_pct: 10.0, // → only -3 kW available
            ramp_rate_w_per_s: f32::INFINITY,
            ..Default::default()
        };
        w.register(SolarInverter::new(1, Duration::from_secs(1), cfg));
        let inv = w.get(1).unwrap();
        let t0 = Utc::now();
        // [-8 kW, -6 kW] is entirely below the available -3 kW.
        inv.try_augment_active_bounds(
            t0,
            VecBounds::single(-8_000.0, -6_000.0),
            Duration::from_secs(60),
        )
        .unwrap();
        inv.tick(&w, t0, Duration::from_millis(100));
        assert!(
            inv.aggregate_power_w(&w).abs() < 1.0,
            "no legal band left → park at 0, got {}",
            inv.aggregate_power_w(&w),
        );
    }

    /// A static `:sunlight%` renders as its own kwarg, sharing the
    /// same rated / command-delay / reactive kwargs as the battery
    /// inverter.
    #[test]
    fn constructor_kwargs_round_trip_solar() {
        let mut cfg = cfg_with_sun(42.0);
        cfg.rated_lower_w = -12_000.0;
        let inv = SolarInverter::new(5, Duration::from_secs(1), cfg);
        assert_eq!(inv.make_fn(), "%make-solar-inverter");
        let s = inv
            .constructor_kwargs()
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(s.contains(":sunlight% 42.0"));
        assert!(s.contains(":rated-lower -12000.0"));
    }

    /// A lambda- or symbol-driven sunlight source can't round-trip
    /// as a static number, so `:sunlight%` is omitted entirely.
    #[test]
    fn constructor_kwargs_omits_sunlight_pct_when_dynamic() {
        let mut cfg = cfg_with_sun(100.0);
        cfg.sunlight_dynamic = true;
        let inv = SolarInverter::new(6, Duration::from_secs(1), cfg);
        let s = inv
            .constructor_kwargs()
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!s.contains(":sunlight%"));
        assert!(inv.has_unrenderable_source());
    }

    /// A sunlight source installed at RUNTIME — a scenario or an
    /// `every` block calling `(set-solar-sunlight ID (lambda …))` —
    /// is just as unwritable as a constructed one, so the component
    /// has to report it. The construction-time flag alone missed it,
    /// which is the same gap Meter closes by consulting its live
    /// power source.
    #[test]
    fn a_runtime_lambda_sunlight_poke_reports_unrenderable() {
        let inv = SolarInverter::new(6, Duration::from_secs(1), cfg_with_sun(80.0));
        assert!(!inv.has_unrenderable_source(), "a static one is fine");
        let mut ctx = tulisp::TulispContext::new();
        let lambda = ctx.eval_string("(lambda () 40.0)").unwrap();
        inv.set_sunlight_source(DynamicScalar::from_lisp(&lambda, 80.0).unwrap());
        assert!(inv.has_unrenderable_source());
    }

    /// Snapshot/restore round-trip for the sunlight knob: a dynamic
    /// (lambda) source survives being swapped out for a constant and
    /// back — `is_dynamic()` and the printed source text both come
    /// back exactly as they were, because `restore_knob` writes the
    /// captured `DynamicScalar` object itself, not a re-parse of its
    /// text.
    #[test]
    fn snapshot_restore_round_trip_sunlight_dynamic_source() {
        let mut ctx = tulisp::TulispContext::new();
        let inv = SolarInverter::new(1, Duration::from_secs(1), cfg_with_sun(50.0));
        let lambda = ctx.eval_string("(lambda () 33.0)").unwrap();
        let scalar = DynamicScalar::from_lisp(&lambda, 50.0).unwrap();
        inv.set_sunlight_source(scalar);
        let text_before = inv.sunlight_reading().unwrap().expr;
        assert!(text_before.is_some());

        let snap = inv.snapshot_knob(KnobKind::Sunlight).unwrap();

        // A scenario collapses it to a constant.
        inv.set_sunlight_pct(10.0);
        assert!(inv.sunlight_reading().unwrap().expr.is_none());
        assert!(!inv.has_unrenderable_source());

        assert!(inv.restore_knob(snap));
        assert_eq!(inv.sunlight_reading().unwrap().expr, text_before);
        assert!(inv.has_unrenderable_source(), "dynamic source restored");
    }
}
