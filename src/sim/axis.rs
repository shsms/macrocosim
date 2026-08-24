//! `PowerAxis`: one per-axis control path shared by active (P) and
//! reactive (Q) power. Composes the three primitives every
//! setpoint-taking axis already needs — `CommandDelay` (the SCADA
//! ack lag), `Ramp` (the slew limit), and `ComponentBounds` (rated
//! band + TTL augmentations) — plus, for a Q axis, a `ReactiveCapability`
//! band computed from the OTHER axis's live value.
//!
//! A P axis is configured with `rated: Some((lo, hi))`, `caps: None`.
//! A Q axis is configured with `rated: None`, `caps: Some(cap)` — its
//! static shape comes from the PF/kVA caps evaluated at the live P,
//! not from a rated pair of its own.

use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;

use crate::sim::{
    bounds::{ComponentBounds, VecBounds},
    component::SetpointError,
    ramp::{CommandDelay, Ramp},
    reactive::ReactiveCapability,
};

/// Static configuration for one `PowerAxis`.
pub struct AxisConfig {
    /// Static rated bounds. `None` for a Q axis (its static shape is the caps).
    pub rated: Option<(f32, f32)>,
    /// PF/kVA caps. `Some` for Q axes, `None` for P axes.
    pub caps: Option<ReactiveCapability>,
    pub command_delay: Duration,
    /// Slew rate per second; `f32::INFINITY` disables ramping.
    pub ramp_rate_per_s: f32,
    /// "W" or "VAr" — carried into `SetpointError::OutOfBounds`.
    pub unit: &'static str,
}

/// What `step` should target when no command is armed yet.
pub enum IdleTarget {
    /// Leave the ramp target alone when no command is armed.
    Hold,
    /// Track this value when no command is armed (clamped into the
    /// tracking envelope; an empty envelope parks at 0).
    Value(f32),
}

/// Per-tick context `step` needs beyond the axis's own state.
pub struct StepCtx<'a> {
    /// The OTHER axis's live value (P for a Q axis). Ignored when `caps` is None.
    pub other_axis: f32,
    /// Extra per-tick envelope from the component (EV SoC derate,
    /// solar sunlight floor). Intersected into the tracking envelope
    /// only — never into validation.
    pub dynamic: Option<&'a VecBounds>,
    pub idle: IdleTarget,
}

/// One axis's full control path: command delay → slew ramp →
/// published value, gated by a composed envelope (static rated/caps
/// band ∩ live TTL augmentations, plus a per-tick dynamic hook for
/// tracking only).
pub struct PowerAxis {
    /// Mirrors `AxisConfig::rated` — used to decide whether an empty
    /// static-augmentation product means "no constraint" (Q axis, no
    /// rated band of its own) or a real, if degenerate, constraint.
    rated: Option<(f32, f32)>,
    /// Rated band (P axis) or augmentations-only (Q axis) plus the
    /// live TTL augmentation queue. See `ComponentBounds::augmentations_only`.
    augs: Mutex<ComponentBounds>,
    caps: Mutex<Option<ReactiveCapability>>,
    delay: CommandDelay,
    ramp: Ramp,
    published: Mutex<f32>,
    unit: &'static str,
}

impl PowerAxis {
    pub fn new(cfg: AxisConfig) -> Self {
        let augs = match cfg.rated {
            Some((lo, hi)) => ComponentBounds::rated(lo, hi),
            None => ComponentBounds::augmentations_only(),
        };
        Self {
            rated: cfg.rated,
            augs: Mutex::new(augs),
            caps: Mutex::new(cfg.caps),
            delay: CommandDelay::new(cfg.command_delay),
            ramp: Ramp::new(cfg.ramp_rate_per_s, 0.0),
            published: Mutex::new(0.0),
            unit: cfg.unit,
        }
    }

    /// Shared composition for `validation_envelope_at` /
    /// `tracking_envelope_at`: static effective ∩ caps band (if any)
    /// ∩ dynamic (if given). Starts from "nothing folded in yet" and
    /// intersects each present piece in.
    ///
    /// The static piece is skipped entirely when `rated` is `None`
    /// (a Q axis) AND no augmentation is live — that combination means
    /// "no static constraint", not "constrained to nothing". Note the
    /// second half: when augmentations ARE live and their product is
    /// empty, they exclude one another — the opposite answer — so
    /// `has_live_augmentations`, not emptiness alone, decides. Any
    /// OTHER empty result — a P axis's rated band
    /// emptied by a disjoint augmentation, a Q axis's caps band emptied
    /// by a disjoint augmentation, two live augmentations disjoint from
    /// each other, or the intersection of otherwise non-empty pieces
    /// coming up empty — IS folded in as a real, if degenerate,
    /// constraint: `accept` rejects every nonzero value against it,
    /// it does NOT treat it as unconstrained (see `accept`'s doc).
    ///
    /// Ending with nothing folded in at all (no static piece, no caps,
    /// no dynamic) is unreached in practice — P axes always carry
    /// `rated`, Q axes always carry `caps` — but if it happens the
    /// result is empty `VecBounds` too, and gets the same treatment:
    /// `accept` rejects every nonzero value (the safe default for an
    /// unconfigured axis) and `step` parks the ramp at 0.
    fn envelope(
        &self,
        now: DateTime<Utc>,
        other_axis: f32,
        dynamic: Option<&VecBounds>,
    ) -> VecBounds {
        let mut acc: Option<VecBounds> = None;

        // One lock for both reads so the two answers describe the same
        // augmentation queue.
        let (static_eff, any_live) = {
            let augs = self.augs.lock();
            (augs.effective_at(now), augs.has_live_augmentations(now))
        };
        if !(self.rated.is_none() && static_eff.0.is_empty() && !any_live) {
            acc = Some(static_eff);
        }

        if let Some(caps) = *self.caps.lock() {
            let (lo, hi) = caps.q_bounds_at(other_axis);
            let band = VecBounds::single(lo, hi);
            acc = Some(match acc {
                None => band,
                Some(a) => a.intersect(&band),
            });
        }

        if let Some(d) = dynamic {
            acc = Some(match acc {
                None => d.clone(),
                Some(a) => a.intersect(d),
            });
        }

        acc.unwrap_or_default()
    }

    /// rated ∩ caps@other ∩ live augmentations (NO dynamic hook).
    pub fn validation_envelope_at(&self, now: DateTime<Utc>, other_axis: f32) -> VecBounds {
        self.envelope(now, other_axis, None)
    }

    /// validation envelope ∩ the dynamic hook.
    pub fn tracking_envelope_at(
        &self,
        now: DateTime<Utc>,
        other_axis: f32,
        dynamic: Option<&VecBounds>,
    ) -> VecBounds {
        self.envelope(now, other_axis, dynamic)
    }

    /// NaN → OutOfBounds; 0 always accepted (park rule); else must be
    /// inside the validation envelope. On Ok, enqueues into the delay.
    pub fn accept(
        &self,
        value: f32,
        now: DateTime<Utc>,
        other_axis: f32,
    ) -> Result<(), SetpointError> {
        let envelope = self.validation_envelope_at(now, other_axis);
        if !value.is_finite() {
            return Err(SetpointError::OutOfBounds {
                value,
                unit: self.unit,
                envelope,
            });
        }
        // Mirrors ComponentBounds::validate_active_setpoint's park rule
        // exactly — including the absence of an "empty envelope means
        // unconstrained" carve-out. VecBounds::contains on an empty
        // VecBounds is always false, so an empty envelope rejects every
        // nonzero value here, same as any other envelope nothing fits in.
        if value != 0.0 && !envelope.contains(value) {
            return Err(SetpointError::OutOfBounds {
                value,
                unit: self.unit,
                envelope,
            });
        }
        self.delay.set_target(value);
        Ok(())
    }

    /// drop_expired → promote (poll) → re-clamp the retained armed
    /// value into the tracking envelope (empty envelope → park 0) or
    /// apply the idle rule → slew → publish → return the new actual.
    pub fn step(&self, now: DateTime<Utc>, dt: Duration, ctx: StepCtx<'_>) -> f32 {
        self.augs.lock().drop_expired(now);
        let env = self.tracking_envelope_at(now, ctx.other_axis, ctx.dynamic);

        if let Some(armed) = self.delay.poll(now) {
            // CommandDelay re-returns the armed value on every poll,
            // so re-clamping it here every tick is what makes
            // tighten→follow / re-widen→restore work with no extra
            // state on this side.
            if env.0.is_empty() {
                self.ramp.set_target(0.0);
            } else {
                self.ramp.set_target(env.clamp(armed));
            }
        } else if let IdleTarget::Value(v) = ctx.idle {
            // A non-finite idle target (NaN from a bad dynamic-scalar
            // read, say) must not reach the ramp — Ramp::set_target
            // already drops NaN downstream, but validating at the
            // door here matches accept's posture instead of relying
            // on that as the only net.
            if !v.is_finite() {
                log::debug!("PowerAxis::step ignored a non-finite idle target");
            } else if env.0.is_empty() {
                self.ramp.set_target(0.0);
            } else {
                self.ramp.set_target(env.clamp(v));
            }
        }
        // IdleTarget::Hold with nothing armed, or a non-finite idle
        // value: leave the ramp target untouched.

        let actual = self.ramp.advance(dt);
        *self.published.lock() = actual;
        actual
    }

    pub fn augment(&self, ts: DateTime<Utc>, bounds: VecBounds, lifetime: Duration) {
        self.augs.lock().add_augmentation(ts, bounds, lifetime);
    }

    /// delay.reset + ramp.set_target(park). Does not touch `published`.
    pub fn reset(&self, park: f32) {
        self.delay.reset();
        self.ramp.set_target(park);
    }

    /// ramp.snap_to(v) alone: the live output jumps to `v` with no
    /// slew, while the command delay AND the published value are left
    /// exactly as they were. Both callers are solar: its health gate
    /// (a tripped PV inverter's output collapses instantly, but its
    /// armed curtailment must survive so a recovery resumes there
    /// instead of at full sun) and `SolarInverter::new`, which seeds
    /// the ramp at the sunlight floor so a fresh inverter is already
    /// generating rather than slewing up from zero on its first tick.
    pub fn snap_output(&self, v: f32) {
        self.ramp.snap_to(v);
    }

    /// delay.reset + ramp.snap_to(0) + publish 0 — the health-trip snap.
    pub fn trip(&self) {
        self.delay.reset();
        self.ramp.snap_to(0.0);
        *self.published.lock() = 0.0;
    }

    pub fn override_published(&self, v: f32) {
        *self.published.lock() = v;
    }

    pub fn published(&self) -> f32 {
        *self.published.lock()
    }

    /// ramp.actual()
    pub fn actual(&self) -> f32 {
        self.ramp.actual()
    }

    /// delay.armed()
    pub fn armed(&self) -> Option<f32> {
        self.delay.armed()
    }

    /// Static side only: rated ∩ augmentations (telemetry's
    /// effective_active_bounds shape). Empty VecBounds when rated is None
    /// and no augmentations are live.
    pub fn effective_static(&self) -> VecBounds {
        self.effective_static_at(Utc::now())
    }

    pub fn effective_static_at(&self, now: DateTime<Utc>) -> VecBounds {
        self.augs.lock().effective_at(now)
    }

    /// No-op + debug log when caps is None.
    pub fn set_pf_limit(&self, pf: Option<f32>) {
        match &mut *self.caps.lock() {
            Some(c) => c.pf_limit = pf,
            None => log::debug!("PowerAxis::set_pf_limit ignored: axis has no reactive caps"),
        }
    }

    /// No-op + debug log when caps is None.
    pub fn set_apparent_va(&self, va: Option<f32>) {
        match &mut *self.caps.lock() {
            Some(c) => c.apparent_va = va,
            None => log::debug!("PowerAxis::set_apparent_va ignored: axis has no reactive caps"),
        }
    }

    pub fn capability(&self) -> Option<ReactiveCapability> {
        *self.caps.lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Q axis's per-tick context: the live P as the other axis, no
    /// dynamic band, and hold when nothing is armed.
    fn q_ctx(p_live: f32) -> StepCtx<'static> {
        StepCtx {
            other_axis: p_live,
            dynamic: None,
            idle: IdleTarget::Hold,
        }
    }

    /// The `(lower, upper)` Q allowance at `p_live` — the first band
    /// of the validation envelope.
    fn q_bounds(ax: &PowerAxis, p_live: f32) -> (f32, f32) {
        let env = ax.validation_envelope_at(Utc::now(), p_live);
        let b = env.0.first().expect("a caps band is always one bucket");
        (b.lower.unwrap(), b.upper.unwrap())
    }

    fn q_axis(caps: ReactiveCapability, command_delay: Duration, ramp: f32) -> PowerAxis {
        PowerAxis::new(AxisConfig {
            rated: None,
            caps: Some(caps),
            command_delay,
            ramp_rate_per_s: ramp,
            unit: "VAr",
        })
    }

    // P-flavored axis: rated bounds + augmentation TTL + re-clamp.
    #[test]
    fn armed_target_follows_a_tightening_envelope_and_restores() {
        let ax = PowerAxis::new(AxisConfig {
            rated: Some((-10_000.0, 10_000.0)),
            caps: None,
            command_delay: Duration::ZERO,
            ramp_rate_per_s: f32::INFINITY,
            unit: "W",
        });
        let t0 = Utc::now();
        ax.accept(8_000.0, t0, 0.0).unwrap();
        fn ctx(d: Option<&VecBounds>) -> StepCtx<'_> {
            StepCtx {
                other_axis: 0.0,
                dynamic: d,
                idle: IdleTarget::Hold,
            }
        }
        assert_eq!(ax.step(t0, Duration::from_secs(1), ctx(None)), 8_000.0);
        // Tighten via a 2 s augmentation → follows; expire → restores.
        ax.augment(
            t0,
            VecBounds::single(-3_000.0, 3_000.0),
            Duration::from_secs(2),
        );
        assert_eq!(
            ax.step(
                t0 + chrono::Duration::seconds(1),
                Duration::from_secs(1),
                ctx(None)
            ),
            3_000.0
        );
        assert_eq!(
            ax.step(
                t0 + chrono::Duration::seconds(3),
                Duration::from_secs(1),
                ctx(None)
            ),
            8_000.0
        );
    }

    #[test]
    fn empty_tracking_envelope_parks_at_zero_but_idle_hold_is_untouched() {
        let ax = PowerAxis::new(AxisConfig {
            rated: Some((0.0, 22_000.0)),
            caps: None,
            command_delay: Duration::ZERO,
            ramp_rate_per_s: f32::INFINITY,
            unit: "W",
        });
        let t0 = Utc::now();
        // Armed target vs a dynamic envelope that doesn't intersect it → park 0.
        ax.accept(10_000.0, t0, 0.0).unwrap();
        let derate = VecBounds::single(30_000.0, 40_000.0); // disjoint from rated
        assert_eq!(
            ax.step(
                t0,
                Duration::from_secs(1),
                StepCtx {
                    other_axis: 0.0,
                    dynamic: Some(&derate),
                    idle: IdleTarget::Hold
                }
            ),
            0.0
        );
        // Unarmed + zero-excluding augmentation: Hold means 0 stays 0.
        let ax2 = PowerAxis::new(AxisConfig {
            rated: Some((0.0, 22_000.0)),
            caps: None,
            command_delay: Duration::ZERO,
            ramp_rate_per_s: f32::INFINITY,
            unit: "W",
        });
        ax2.augment(
            t0,
            VecBounds::single(5_000.0, 22_000.0),
            Duration::from_secs(60),
        );
        assert_eq!(
            ax2.step(
                t0,
                Duration::from_secs(1),
                StepCtx {
                    other_axis: 0.0,
                    dynamic: None,
                    idle: IdleTarget::Hold
                }
            ),
            0.0
        );
    }

    #[test]
    fn q_axis_validates_against_caps_and_augmentations_but_not_dynamic() {
        let ax = PowerAxis::new(AxisConfig {
            rated: None,
            caps: Some(ReactiveCapability {
                pf_limit: None,
                apparent_va: Some(5_000.0),
            }),
            command_delay: Duration::ZERO,
            ramp_rate_per_s: f32::INFINITY,
            unit: "VAr",
        });
        let t0 = Utc::now();
        // At P=3000, kVA circle allows |Q| ≤ 4000.
        assert!(ax.accept(4_500.0, t0, 3_000.0).is_err());
        ax.accept(3_500.0, t0, 3_000.0).unwrap();
        // A TTL augmentation narrows the Q envelope — the spec's failing RPC case.
        ax.augment(
            t0,
            VecBounds::single(-1_000.0, 1_000.0),
            Duration::from_secs(60),
        );
        assert!(
            ax.accept(3_500.0, t0 + chrono::Duration::seconds(1), 3_000.0)
                .is_err()
        );
        let err = ax.accept(f32::NAN, t0, 3_000.0).unwrap_err();
        assert!(matches!(
            err,
            SetpointError::OutOfBounds { unit: "VAr", .. }
        ));
        // 0 always accepted.
        ax.accept(0.0, t0, 3_000.0).unwrap();
    }

    /// A fully-unbounded augmentation band (both proto edges absent =
    /// "no bound") must read as no extra constraint — even stacked.
    /// Two of them used to intersect into the disjoint sentinel and
    /// empty the Q envelope, parking the axis at 0 and bouncing every
    /// later augmentation for their whole lifetime.
    #[test]
    fn q_axis_survives_stacked_unbounded_augmentations() {
        let ax = q_axis(
            ReactiveCapability {
                pf_limit: None,
                apparent_va: Some(5_000.0),
            },
            Duration::ZERO,
            f32::INFINITY,
        );
        let t0 = Utc::now();
        let unbounded = VecBounds::new(vec![crate::proto::common::metrics::Bounds {
            lower: None,
            upper: None,
        }]);
        ax.augment(t0, unbounded.clone(), Duration::from_secs(60));
        ax.augment(t0, unbounded, Duration::from_secs(60));
        // The envelope is still the caps band: |Q| ≤ 4000 at P=3000.
        ax.accept(3_500.0, t0 + chrono::Duration::seconds(1), 3_000.0)
            .unwrap();
        assert!(
            ax.accept(4_500.0, t0 + chrono::Duration::seconds(1), 3_000.0)
                .is_err(),
            "caps still bind — unbounded augmentations widen nothing"
        );
    }

    #[test]
    fn idle_value_tracks_and_clamps() {
        // Solar-shaped: unarmed axis tracks the provided idle value,
        // clamped by an augmentation cap.
        let ax = PowerAxis::new(AxisConfig {
            rated: Some((-30_000.0, 0.0)),
            caps: None,
            command_delay: Duration::ZERO,
            ramp_rate_per_s: f32::INFINITY,
            unit: "W",
        });
        let t0 = Utc::now();
        assert_eq!(
            ax.step(
                t0,
                Duration::from_secs(1),
                StepCtx {
                    other_axis: 0.0,
                    dynamic: None,
                    idle: IdleTarget::Value(-6_000.0)
                }
            ),
            -6_000.0
        );
        ax.augment(
            t0,
            VecBounds::single(-2_000.0, 0.0),
            Duration::from_secs(60),
        );
        assert_eq!(
            ax.step(
                t0 + chrono::Duration::seconds(1),
                Duration::from_secs(1),
                StepCtx {
                    other_axis: 0.0,
                    dynamic: None,
                    idle: IdleTarget::Value(-6_000.0)
                }
            ),
            -2_000.0
        );
    }

    #[test]
    fn trip_snaps_and_reset_parks() {
        let ax = PowerAxis::new(AxisConfig {
            rated: Some((-10_000.0, 10_000.0)),
            caps: None,
            command_delay: Duration::ZERO,
            ramp_rate_per_s: 1_000.0,
            unit: "W",
        });
        let t0 = Utc::now();
        ax.accept(5_000.0, t0, 0.0).unwrap();
        ax.step(
            t0,
            Duration::from_secs(2),
            StepCtx {
                other_axis: 0.0,
                dynamic: None,
                idle: IdleTarget::Hold,
            },
        ); // → 2000
        ax.trip();
        assert_eq!(ax.actual(), 0.0);
        assert_eq!(ax.published(), 0.0);
        assert_eq!(ax.armed(), None);
        // reset(park) re-targets without snapping.
        ax.accept(5_000.0, t0, 0.0).unwrap();
        ax.step(
            t0,
            Duration::from_secs(1),
            StepCtx {
                other_axis: 0.0,
                dynamic: None,
                idle: IdleTarget::Hold,
            },
        ); // → 1000
        ax.reset(0.0);
        let v = ax.step(
            t0,
            Duration::from_secs(1) / 2,
            StepCtx {
                other_axis: 0.0,
                dynamic: None,
                idle: IdleTarget::Hold,
            },
        );
        assert!(
            (v - 500.0).abs() < 1.0,
            "ramps toward the park value, no snap: {v}"
        );
    }

    /// A P axis whose static effective bounds are emptied by a
    /// disjoint augmentation must reject every nonzero value, not
    /// silently accept anything — an empty envelope is a real
    /// constraint (nothing fits), not "unconstrained". 0 stays
    /// accepted via the park rule regardless.
    #[test]
    fn accept_rejects_everything_when_static_bounds_are_emptied_by_augmentation() {
        let ax = PowerAxis::new(AxisConfig {
            rated: Some((0.0, 100.0)),
            caps: None,
            command_delay: Duration::ZERO,
            ramp_rate_per_s: f32::INFINITY,
            unit: "W",
        });
        let t0 = Utc::now();
        ax.augment(t0, VecBounds::single(200.0, 300.0), Duration::from_secs(60));
        assert!(ax.accept(150.0, t0, 0.0).is_err());
        assert!(ax.accept(0.0, t0, 0.0).is_ok());
    }

    /// Same bug, Q-axis flavor: a caps band emptied by a disjoint
    /// augmentation must reject every nonzero value too.
    #[test]
    fn accept_rejects_everything_when_caps_band_is_emptied_by_augmentation() {
        let ax = PowerAxis::new(AxisConfig {
            rated: None,
            caps: Some(ReactiveCapability {
                pf_limit: None,
                apparent_va: Some(4_000.0),
            }),
            command_delay: Duration::ZERO,
            ramp_rate_per_s: f32::INFINITY,
            unit: "VAr",
        });
        let t0 = Utc::now();
        // At P=0 the kVA cap allows ±4000; a disjoint [5000, 6000]
        // augmentation intersects it down to nothing.
        ax.augment(
            t0,
            VecBounds::single(5_000.0, 6_000.0),
            Duration::from_secs(60),
        );
        assert!(ax.accept(3_500.0, t0, 0.0).is_err());
    }

    /// Two live Q augmentations that are mutually disjoint leave no
    /// legal band at all. "Live, but nothing fits" is a real
    /// constraint — it is NOT the "no augmentation live" case a Q axis
    /// skips because it has no static band of its own. Skipping it
    /// would silently revert the axis to the bare caps band and let
    /// through values neither augmentation permits, so `accept` must
    /// reject every nonzero value and `step` must park at 0.
    #[test]
    fn mutually_disjoint_live_q_augmentations_leave_no_legal_band() {
        let ax = q_axis(
            ReactiveCapability {
                pf_limit: None,
                apparent_va: Some(5_000.0),
            },
            Duration::ZERO,
            f32::INFINITY,
        );
        let t0 = Utc::now();
        // Baseline: at P = 0 the caps band is ±5 kVAr and 2 kVAr rides it.
        ax.accept(2_000.0, t0, 0.0).unwrap();
        assert_eq!(ax.step(t0, Duration::from_secs(1), q_ctx(0.0)), 2_000.0);

        // Two live augmentations with no overlap between them.
        ax.augment(
            t0,
            VecBounds::single(-4_000.0, -3_000.0),
            Duration::from_secs(60),
        );
        ax.augment(
            t0,
            VecBounds::single(-500.0, 500.0),
            Duration::from_secs(60),
        );

        let env = ax.validation_envelope_at(t0, 0.0);
        assert!(
            env.0.is_empty(),
            "disjoint live augmentations leave nothing legal, got {env}"
        );
        // Neither augmentation's own band is accepted either — there is
        // no value both permit.
        assert!(ax.accept(2_000.0, t0, 0.0).is_err());
        assert!(ax.accept(-3_500.0, t0, 0.0).is_err());
        assert!(ax.accept(400.0, t0, 0.0).is_err());
        // The armed 2 kVAr is re-clamped to the park, not to the caps band.
        assert_eq!(
            ax.step(
                t0 + chrono::Duration::seconds(1),
                Duration::from_secs(1),
                q_ctx(0.0)
            ),
            0.0
        );
        // The park rule survives regardless.
        assert!(ax.accept(0.0, t0, 0.0).is_ok());
    }

    #[test]
    fn q_axis_accept_then_step_drives_to_target() {
        // No PF cap, ±10 kVA envelope. 100 ms delay, 1000 VAR/s slew.
        let p = q_axis(
            ReactiveCapability {
                pf_limit: None,
                apparent_va: Some(10_000.0),
            },
            Duration::from_millis(100),
            1000.0,
        );
        let now = Utc::now();
        // P = 0; envelope is ±10 kVA. Q=5000 is in-range.
        assert!(p.accept(5000.0, now, 0.0).is_ok());

        // Before the 100 ms delay elapses, no movement.
        let q = p.step(
            now + chrono::Duration::milliseconds(50),
            Duration::from_millis(50),
            q_ctx(0.0),
        );
        assert!(q.abs() < 1.0, "expected ~0 before delay, got {q}");

        // After 1 s past the delay, ramp at 1000 VAR/s reaches 1 kVAR.
        let q = p.step(
            now + chrono::Duration::milliseconds(1100),
            Duration::from_millis(1000),
            q_ctx(0.0),
        );
        assert!((q - 1000.0).abs() < 1.0, "expected ~1000, got {q}");

        // 6 s past the delay: ramp settled at 5000.
        let q = p.step(
            now + chrono::Duration::milliseconds(6100),
            Duration::from_millis(5000),
            q_ctx(0.0),
        );
        assert!((q - 5000.0).abs() < 1.0, "expected 5000, got {q}");
        assert_eq!(p.published(), q);
    }

    #[test]
    fn q_axis_rejects_out_of_envelope() {
        let p = q_axis(
            ReactiveCapability {
                pf_limit: Some(0.5),
                apparent_va: None,
            },
            Duration::ZERO,
            f32::INFINITY,
        );
        // PF=0.5 at P=10k → ±5000 envelope. 6000 is out.
        match p.accept(6000.0, Utc::now(), 10_000.0) {
            Err(SetpointError::OutOfBounds {
                value, envelope, ..
            }) => {
                assert_eq!(value, 6000.0);
                let b = envelope.0.first().expect("single-bucket envelope");
                let (lower, upper) = (b.lower.unwrap(), b.upper.unwrap());
                assert!((lower + 5000.0).abs() < 1.0);
                assert!((upper - 5000.0).abs() < 1.0);
            }
            other => panic!("expected OutOfBounds, got {other:?}"),
        }
    }

    #[test]
    fn q_axis_re_clamps_at_promotion() {
        // Cap = ±10 kVA. No delay, no slew. Accept at P=0 (full
        // envelope), then have P drift to 9 kW before tick — the
        // command should be re-clamped to √(100-81)*1000 ≈ ±4359.
        let p = q_axis(
            ReactiveCapability {
                pf_limit: None,
                apparent_va: Some(10_000.0),
            },
            Duration::ZERO,
            f32::INFINITY,
        );
        assert!(p.accept(8000.0, Utc::now(), 0.0).is_ok());
        let q = p.step(Utc::now(), Duration::from_millis(100), q_ctx(9000.0));
        assert!(q < 4400.0 && q > 4350.0, "expected ~4359, got {q}");
    }

    /// P drifting AFTER a Q command settled must also re-clamp: the
    /// delay queue keeps returning the armed value on every poll, so
    /// each step re-clamps it to the live envelope at the current P —
    /// a sustained Volt/VAR setpoint can't push apparent power past
    /// the kVA rating when active power later rises.
    #[test]
    fn q_axis_re_clamps_on_p_drift_after_settle() {
        let p = q_axis(
            ReactiveCapability {
                pf_limit: None,
                apparent_va: Some(10_000.0),
            },
            Duration::ZERO,
            f32::INFINITY,
        );
        assert!(p.accept(8000.0, Utc::now(), 0.0).is_ok());
        // Settles at the full 8 kVAR while P = 0.
        let q = p.step(Utc::now(), Duration::from_millis(100), q_ctx(0.0));
        assert!((q - 8000.0).abs() < 1.0, "expected 8000, got {q}");
        // P rises to 9 kW under the settled Q — the next tick clamps
        // to √(100−81)·1000 ≈ 4359.
        let q = p.step(Utc::now(), Duration::from_millis(100), q_ctx(9000.0));
        assert!(q < 4400.0 && q > 4350.0, "expected ~4359, got {q}");
        // P falls back — the original commanded Q is restored.
        let q = p.step(Utc::now(), Duration::from_millis(100), q_ctx(0.0));
        assert!((q - 8000.0).abs() < 1.0, "expected 8000 back, got {q}");
    }

    #[test]
    fn q_axis_override_published_wins() {
        // step() publishes the ramp's value; override_published
        // overwrites it. Mirrors the BatteryInverter's "measured = sum
        // of children's accepted" path.
        let p = q_axis(
            ReactiveCapability::microsim_default(),
            Duration::ZERO,
            f32::INFINITY,
        );
        let _ = p.accept(3000.0, Utc::now(), 10_000.0);
        let q = p.step(Utc::now(), Duration::from_millis(100), q_ctx(10_000.0));
        assert!((q - 3000.0).abs() < 1.0);
        p.override_published(2500.0);
        assert_eq!(p.published(), 2500.0);
    }

    #[test]
    fn q_axis_runtime_caps() {
        let p = q_axis(
            ReactiveCapability {
                pf_limit: None,
                apparent_va: Some(10_000.0),
            },
            Duration::ZERO,
            f32::INFINITY,
        );
        // Initial: at P=0, Q can be ±10k.
        assert!((q_bounds(&p, 0.0).1 - 10_000.0).abs() < 1.0);
        // Drop kVA cap, add tight PF cap: at P=2k, Q ≤ 0.2×2k = ±400.
        p.set_apparent_va(None);
        p.set_pf_limit(Some(0.2));
        let (lo, hi) = q_bounds(&p, 2000.0);
        assert!((hi - 400.0).abs() < 1.0);
        assert!((lo + 400.0).abs() < 1.0);
    }

    /// A non-finite idle target (e.g. a bad dynamic-scalar read) must
    /// not move the ramp — treated the same as `IdleTarget::Hold`.
    #[test]
    fn idle_value_non_finite_is_treated_as_hold() {
        let ax = PowerAxis::new(AxisConfig {
            rated: Some((-10_000.0, 10_000.0)),
            caps: None,
            command_delay: Duration::ZERO,
            ramp_rate_per_s: f32::INFINITY,
            unit: "W",
        });
        let t0 = Utc::now();
        // Establish a non-zero ramp target via a normal idle value first.
        let v = ax.step(
            t0,
            Duration::from_secs(1),
            StepCtx {
                other_axis: 0.0,
                dynamic: None,
                idle: IdleTarget::Value(4_000.0),
            },
        );
        assert_eq!(v, 4_000.0);
        // A NaN idle target must not move the ramp target.
        let v = ax.step(
            t0,
            Duration::from_secs(1),
            StepCtx {
                other_axis: 0.0,
                dynamic: None,
                idle: IdleTarget::Value(f32::NAN),
            },
        );
        assert_eq!(v, 4_000.0);
    }
}
