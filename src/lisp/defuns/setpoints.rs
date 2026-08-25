//! `(set-active-power)` and `(set-reactive-power)` — apply a
//! setpoint on one power axis and arm a request-lifetime timeout.
//! Mirror gRPC's `SetElectricalComponentPower`; the reset fires from
//! the loop in `Config::start_timeout_loop`.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tulisp::{Error, TulispContext};

use crate::sim::microgrids::SharedSiteRouter;
use crate::timeout_tracker::SetpointAxis;

use super::super::Metadata;

/// Lower bound on a non-zero request-lifetime that the setpoint
/// defuns can install. The timeout loop polls at
/// 100 ms and the default physics tick is 100 ms, so a sub-150 ms
/// lifetime can expire before the next physics tick observes the
/// setpoint at all — the ramp would clear without ever leaving
/// idle. `lifetime-ms = 0` is preserved as an explicit "expire
/// immediately" escape (used by tests) and bypasses the clamp.
const MIN_SETPOINT_LIFETIME_MS: u64 = 150;

/// The request lifetime for a `LIFETIME-MS` argument: omitted falls
/// back to `default-request-lifetime-ms`, `0` expires at once, any
/// other value is floored at [`MIN_SETPOINT_LIFETIME_MS`].
fn lifetime_from_arg(lifetime_ms: Option<i64>, metadata: &RwLock<Metadata>) -> Duration {
    lifetime_ms
        .map(|ms| {
            let raw = ms.max(0) as u64;
            let clamped = if raw == 0 {
                0
            } else {
                raw.max(MIN_SETPOINT_LIFETIME_MS)
            };
            Duration::from_millis(clamped)
        })
        .unwrap_or_else(|| metadata.read().default_request_lifetime)
}

/// `(set-active-power ID WATTS &OPTIONAL LIFETIME-MS CLAMP)` — apply an
/// active-power setpoint and arm a request-lifetime timeout, mirroring
/// what gRPC's `SetElectricalComponentPower` does. Returns `t` on
/// success; signals an error if the component doesn't exist or
/// rejects the setpoint (e.g. out-of-bounds, unsupported kind).
///
/// `LIFETIME-MS` is the duration after which the setpoint snaps back
/// to idle. Omitting it falls back to `default-request-lifetime-ms`,
/// matching the gRPC behaviour. The reset fires from the loop in
/// `Config::start_timeout_loop`.
///
/// `CLAMP` (default nil) — when non-nil, a setpoint outside the live
/// envelope (the inverter's own bounds intersected with its children's
/// DC bounds) is clamped into range and applied instead of rejected.
/// This is the primitive an in-sim controller scripted with `(every …)`
/// uses to command "max within whatever cap the limiter currently
/// allows" each tick without tracking the augmentations itself. With
/// `CLAMP` nil the out-of-envelope command is rejected, like the gRPC
/// gateway. 0 W (the fail-safe park) is applied as-is either way.
///
/// `(set-reactive-power ID VARS &OPTIONAL LIFETIME-MS CLAMP)` — the
/// reactive-axis twin of `set-active-power`: apply a reactive-power
/// setpoint and arm a request-lifetime timeout on the reactive axis
/// only, mirroring gRPC's `SetElectricalComponentPower` with
/// `PowerType::Reactive`. Returns `t` on success; signals an error
/// if the component doesn't exist, has no reactive axis (meters,
/// batteries), or rejects the value.
///
/// `LIFETIME-MS` follows the same rule as `set-active-power`
/// (omitted → `default-request-lifetime-ms`, `0` → expire at once,
/// otherwise floored at 150 ms).
///
/// `CLAMP` works exactly as it does on the active axis, over the
/// reactive gateway envelope: the component's live Q band (the PF /
/// apparent-power caps at its current active power, intersected with
/// any live augmentation) narrowed by whatever Q bounds its children
/// report. With `CLAMP` nil an out-of-envelope request is rejected;
/// with it non-nil the request is pulled to the nearest edge and
/// applied. 0 VAr always passes, like the 0 W park.
pub(super) fn register(
    ctx: &mut TulispContext,
    router: SharedSiteRouter,
    metadata: Arc<RwLock<Metadata>>,
) {
    let metadata_q = metadata.clone();
    let r = router.clone();
    ctx.defun(
        "set-active-power",
        move |id: i64,
              watts: f64,
              lifetime_ms: Option<i64>,
              clamp: Option<bool>|
              -> Result<bool, Error> {
            let w = r.site();
            let component = w.get(id as u64).ok_or_else(|| {
                Error::invalid_argument(format!("set-active-power: component {id} not found"))
            })?;
            let mut watts = watts as f32;
            // Envelope a setpoint must respect: the inverter's own bounds
            // intersected with its children's DC bounds (None when it has
            // no bounded children — then only its own bounds apply).
            // 0 W (the fail-safe park) bypasses both arms below.
            if watts != 0.0 {
                if clamp.unwrap_or(false) {
                    // Clamp into the live envelope instead of rejecting, so
                    // an in-sim controller can command "max within the cap"
                    // each tick without tracking the limiter's
                    // augmentations itself. Falls back to the component's
                    // own bounds when it has no bounded children.
                    if let Some(envelope) = w
                        .active_setpoint_envelope(id as u64)
                        .or_else(|| component.effective_active_bounds())
                    {
                        watts = envelope.clamp(watts);
                    }
                } else {
                    // The same gate as the gRPC SetPower route: reject
                    // a command the battery can't accept rather than
                    // silently saturating it.
                    w.gate_setpoint(id as u64, SetpointAxis::Active, watts)
                        .map_err(|m| Error::invalid_argument(format!("set-active-power: {m}")))?;
                }
            }
            component
                .set_active_setpoint(watts)
                .map_err(|e| Error::invalid_argument(format!("set-active-power: {e}")))?;
            let lifetime = lifetime_from_arg(lifetime_ms, &metadata);
            w.add_timeout(
                id as u64,
                crate::timeout_tracker::SetpointAxis::Active,
                lifetime,
            );
            Ok(true)
        },
    );

    // `(set-reactive-power …)` — documented with `register` above.
    let r = router;
    ctx.defun(
        "set-reactive-power",
        move |id: i64,
              vars: f64,
              lifetime_ms: Option<i64>,
              clamp: Option<bool>|
              -> Result<bool, Error> {
            let w = r.site();
            let component = w.get(id as u64).ok_or_else(|| {
                Error::invalid_argument(format!("set-reactive-power: component {id} not found"))
            })?;
            let mut vars = vars as f32;
            // Same two arms as `set-active-power`, over the reactive
            // envelope: the component's own live Q band intersected
            // with any Q bounds its children report (None when no
            // child reports any — then only its own band applies).
            // 0 VAr (the fail-safe park) bypasses both arms.
            if vars != 0.0 {
                if clamp.unwrap_or(false) {
                    // Clamp into the live envelope instead of
                    // rejecting, so an in-sim controller can command
                    // "max Q within the current cap" each tick
                    // without tracking augmentations itself. Falls
                    // back to the component's own band when it has no
                    // Q-reporting children. Full multi-band clamp,
                    // like the active arm's.
                    if let Some(envelope) = w
                        .reactive_setpoint_envelope(id as u64)
                        .or_else(|| component.reactive_bounds())
                    {
                        vars = envelope.clamp(vars);
                    }
                } else {
                    // The same gate as the gRPC SetPower route, on the
                    // Q axis.
                    w.gate_setpoint(id as u64, SetpointAxis::Reactive, vars)
                        .map_err(|m| Error::invalid_argument(format!("set-reactive-power: {m}")))?;
                }
            }
            component
                .set_reactive_setpoint(vars)
                .map_err(|e| Error::invalid_argument(format!("set-reactive-power: {e}")))?;
            let lifetime = lifetime_from_arg(lifetime_ms, &metadata_q);
            w.add_timeout(
                id as u64,
                crate::timeout_tracker::SetpointAxis::Reactive,
                lifetime,
            );
            Ok(true)
        },
    );
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::config_with;

    /// set-active-power applies a setpoint and arms the timeout tracker.
    /// We can verify both by checking that MicrogridSite registers a deadline
    /// for the targeted component after the call.
    #[test]
    fn set_active_power_applies_setpoint_and_arms_timeout() {
        let (cfg, _dir) = config_with(
            "(setq b1 (%make-battery :id 1 :rated-lower -5000.0 :rated-upper 5000.0))
             (%make-battery-inverter :id 2 :rated-lower -5000.0 :rated-upper 5000.0
                                       :successors (list b1))",
        );
        // 30-second lifetime — applies the setpoint and arms the
        // tracker; nothing should be expired yet.
        cfg.eval("(set-active-power 2 1500.0 30000)").unwrap();
        assert_eq!(cfg.site().drain_expired_timeouts(), Vec::new());
        // Lifetime 0 → instantly elapses; the next drain returns id.
        cfg.eval("(set-active-power 2 1500.0 0)").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(
            cfg.site().drain_expired_timeouts(),
            vec![(2, crate::timeout_tracker::SetpointAxis::Active)]
        );
    }

    /// set-active-power gates against the *intersection* of the
    /// inverter's own bounds and its battery child's bounds — not just
    /// the inverter's own — so a value the inverter alone would accept
    /// but the battery can't is rejected, not silently saturated.
    #[test]
    fn set_active_power_rejects_outside_battery_inverter_intersection() {
        let (cfg, _dir) = config_with(
            // Inverter rated ±5 kW, but its battery only ±1 kW -> the
            // combined envelope is ±1 kW.
            "(setq b1 (%make-battery :id 1 :rated-lower -1000.0 :rated-upper 1000.0))
             (%make-battery-inverter :id 2 :rated-lower -5000.0 :rated-upper 5000.0
                                       :successors (list b1))",
        );
        // +3 kW is inside the inverter's own ±5 kW but outside the
        // battery's ±1 kW -> rejected against the intersection.
        let res = cfg.eval("(set-active-power 2 3000.0 30000)");
        assert!(res.is_err(), "expected rejection, got {res:?}");
        assert!(
            res.as_ref().unwrap_err().contains("envelope"),
            "expected 'envelope' in error, got {res:?}"
        );
        // Discharge side mirrors it.
        assert!(cfg.eval("(set-active-power 2 -3000.0 30000)").is_err());
        // Within the ±1 kW intersection is accepted.
        cfg.eval("(set-active-power 2 800.0 30000)").unwrap();
        // 0 W (the fail-safe park) is always accepted.
        cfg.eval("(set-active-power 2 0.0 30000)").unwrap();
    }

    /// With the CLAMP arg, an out-of-envelope setpoint is clamped into
    /// the battery∩inverter envelope and applied instead of rejected —
    /// the primitive an in-sim controller uses to track the live cap.
    #[test]
    fn set_active_power_clamp_arg_clamps_into_envelope() {
        use std::time::Duration;
        let (cfg, _dir) = config_with(
            // Inverter ±5 kW, battery ±1 kW -> combined envelope ±1 kW.
            "(setq b1 (%make-battery :id 1 :rated-lower -1000.0 :rated-upper 1000.0))
             (%make-battery-inverter :id 2 :rated-lower -5000.0 :rated-upper 5000.0
                                       :successors (list b1))",
        );
        // Without clamp, +3 kW is rejected.
        assert!(cfg.eval("(set-active-power 2 3000.0 30000)").is_err());
        // With clamp = t, +3 kW is pulled to the +1 kW edge and applied.
        cfg.eval("(set-active-power 2 3000.0 30000 t)").unwrap();
        let site = cfg.site();
        let inv = site.get(2).unwrap();
        // command-delay is zero and ramp is infinite on the primitive
        // inverter, so one tick settles the commanded power.
        inv.tick(&site, chrono::Utc::now(), Duration::from_millis(100));
        let p = inv.aggregate_power_w(&site);
        assert!((p - 1000.0).abs() < 1.0, "expected clamp to +1 kW, got {p}");
        // Discharge side clamps symmetrically.
        cfg.eval("(set-active-power 2 -3000.0 30000 t)").unwrap();
        inv.tick(&site, chrono::Utc::now(), Duration::from_millis(100));
        let p = inv.aggregate_power_w(&site);
        assert!((p + 1000.0).abs() < 1.0, "expected clamp to -1 kW, got {p}");
    }

    /// set-active-power on an unknown id surfaces an error, and a setpoint
    /// rejected by the component (e.g. unsupported kind on a meter)
    /// also propagates rather than silently no-op'ing.
    #[test]
    fn set_active_power_rejects_unknown_or_unsupported() {
        let (cfg, _dir) = config_with("(%make-meter :id 1)");
        let res = cfg.eval("(set-active-power 999 1500.0)");
        assert!(res.is_err(), "expected error, got {res:?}");
        assert!(res.unwrap_err().contains("999"));
        // Meter doesn't support active setpoints — set_active_setpoint
        // returns Unsupported, which we surface as a Lisp error.
        let res = cfg.eval("(set-active-power 1 1500.0)");
        assert!(res.is_err(), "expected error, got {res:?}");
    }

    /// A battery-inverter with a 5 kVA apparent-power cap and no PF
    /// limit (the inherited default would pin Q to 0 at idle) has a
    /// ±5 kVAr reactive band at idle. Used by the reactive tests below.
    const REACTIVE_SITE: &str =
        "(setq b1 (%make-battery :id 1 :rated-lower -5000.0 :rated-upper 5000.0))
         (%make-battery-inverter :id 2 :rated-lower -5000.0 :rated-upper 5000.0
                                   :reactive-pf-limit 0
                                   :reactive-apparent-va 5000.0
                                   :reactive-command-delay-ms 0
                                   :reactive-ramp-rate 1e9
                                   :successors (list b1))";

    /// set-reactive-power applies a setpoint and arms the *reactive*
    /// axis of the timeout tracker, leaving the active axis alone.
    #[test]
    fn set_reactive_power_applies_setpoint_and_arms_reactive_timeout() {
        let (cfg, _dir) = config_with(REACTIVE_SITE);
        cfg.eval("(set-reactive-power 2 1500.0 30000)").unwrap();
        assert_eq!(cfg.site().drain_expired_timeouts(), Vec::new());
        cfg.eval("(set-reactive-power 2 1500.0 0)").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(
            cfg.site().drain_expired_timeouts(),
            vec![(2, crate::timeout_tracker::SetpointAxis::Reactive)]
        );
    }

    /// Outside the inverter's live reactive band the request is
    /// rejected, like gRPC's SetElectricalComponentPower(Reactive);
    /// inside it, and 0 VAr, are accepted.
    #[test]
    fn set_reactive_power_rejects_outside_band() {
        let (cfg, _dir) = config_with(REACTIVE_SITE);
        let res = cfg.eval("(set-reactive-power 2 6000.0 30000)");
        assert!(res.is_err(), "expected rejection, got {res:?}");
        assert!(cfg.eval("(set-reactive-power 2 -6000.0 30000)").is_err());
        cfg.eval("(set-reactive-power 2 3000.0 30000)").unwrap();
        cfg.eval("(set-reactive-power 2 0.0 30000)").unwrap();
    }

    /// With CLAMP, an out-of-band request is pulled to the band edge
    /// and applied: the published Q settles at ±5 kVAr.
    #[test]
    fn set_reactive_power_clamp_arg_clamps_into_band() {
        use std::time::Duration;
        let (cfg, _dir) = config_with(REACTIVE_SITE);
        assert!(cfg.eval("(set-reactive-power 2 6000.0 30000)").is_err());
        cfg.eval("(set-reactive-power 2 6000.0 30000 t)").unwrap();
        let site = cfg.site();
        let inv = site.get(2).unwrap();
        inv.tick(&site, chrono::Utc::now(), Duration::from_millis(100));
        let q = inv.telemetry(&site).reactive_power_var.unwrap();
        assert!(
            (q - 5000.0).abs() < 1.0,
            "expected clamp to +5 kVAr, got {q}"
        );
        cfg.eval("(set-reactive-power 2 -6000.0 30000 t)").unwrap();
        inv.tick(&site, chrono::Utc::now(), Duration::from_millis(100));
        let q = inv.telemetry(&site).reactive_power_var.unwrap();
        assert!(
            (q + 5000.0).abs() < 1.0,
            "expected clamp to -5 kVAr, got {q}"
        );
    }

    /// A non-finite value (a lambda that divided by zero) is rejected
    /// instead of riding the ramp into telemetry as NaN.
    #[test]
    fn set_reactive_power_rejects_nan() {
        let (cfg, _dir) = config_with(REACTIVE_SITE);
        let res = cfg.eval("(set-reactive-power 2 (/ 0.0 0.0) 30000)");
        assert!(res.is_err(), "expected rejection, got {res:?}");
        assert!(
            cfg.eval("(set-reactive-power 2 (/ 0.0 0.0) 30000 t)")
                .is_err()
        );
    }

    /// A live Q augmentation narrows what `set-reactive-power`
    /// accepts, CLAMP pulls a too-big request down to the narrowed
    /// edge, and once the augmentation expires the wide band is back.
    /// The DSL-visible half of the `AugmentElectricalComponentBounds`
    /// round trip on the reactive axis.
    #[test]
    fn reactive_augmentation_narrows_accepts_and_expires() {
        use crate::sim::bounds::VecBounds;
        use std::time::Duration;
        let (cfg, _dir) = config_with(REACTIVE_SITE);
        // Wide band to start with: ±5 kVAr at P = 0.
        cfg.eval("(set-reactive-power 2 3000.0 30000)").unwrap();

        let site = cfg.site();
        let inv = site.get(2).unwrap();
        // Narrow Q to ±1 kVAr. The two-second lifetime is the window
        // the three evals + tick + telemetry read below have to land
        // in; they take microseconds each, but a parallel `cargo
        // test` can steal this thread for a long while, so leave real
        // slack. The expiry leg sleeps only what is *left* of the
        // lifetime, so a wide window costs no extra wall time.
        const LIFETIME: Duration = Duration::from_secs(2);
        let armed_at = chrono::Utc::now();
        inv.augment_reactive_bounds(armed_at, VecBounds::single(-1000.0, 1000.0), LIFETIME);

        // 3 kVAr no longer fits the live band.
        let res = cfg.eval("(set-reactive-power 2 3000.0 30000)");
        assert!(
            res.is_err(),
            "expected rejection under the augmentation, got {res:?}"
        );
        // With CLAMP it is pulled to the +1 kVAr edge and applied.
        cfg.eval("(set-reactive-power 2 3000.0 30000 t)").unwrap();
        inv.tick(&site, chrono::Utc::now(), Duration::from_millis(100));
        let q = inv.telemetry(&site).reactive_power_var.unwrap();
        assert!(
            (q - 1000.0).abs() < 1.0,
            "expected clamp to the augmented +1 kVAr edge, got {q}"
        );

        // Past the augmentation's lifetime the caps band alone
        // applies. Sleep to the expiry instant plus a small margin,
        // not a flat interval — whatever the assertions above already
        // consumed comes off this wait.
        let spent = (chrono::Utc::now() - armed_at)
            .to_std()
            .unwrap_or(Duration::ZERO);
        std::thread::sleep(LIFETIME.saturating_sub(spent) + Duration::from_millis(100));
        cfg.eval("(set-reactive-power 2 3000.0 30000)")
            .expect("3 kVAr fits the rated band again once the augmentation expires");
    }

    /// The reactive axis carries the same gateway shape as the active
    /// one. An inverter's battery child exposes no Q bounds (reactive
    /// power terminates at the inverter), so
    /// `reactive_setpoint_envelope` is `None` and the gateway falls
    /// through to the component's own band — which still rejects an
    /// out-of-band request, with the same wording the active arm uses.
    #[test]
    fn reactive_gateway_mirrors_active() {
        let (cfg, _dir) = config_with(REACTIVE_SITE);
        let site = cfg.site();
        // The active side has a combined envelope: the battery child
        // reports DC bounds and they get summed in.
        assert!(
            site.active_setpoint_envelope(2).is_some(),
            "the battery child reports active bounds, so P has a combined envelope"
        );
        // The reactive side has none: no child reports Q bounds.
        assert!(
            site.aggregate_child_reactive_bounds(2).is_none(),
            "a battery exposes no reactive bounds"
        );
        assert!(
            site.reactive_setpoint_envelope(2).is_none(),
            "with no Q-reporting child there is no combined Q envelope"
        );
        // With no gateway envelope, the component's own band decides,
        // and the error reads like the active arm's.
        let res = cfg.eval("(set-reactive-power 2 9000.0 30000)");
        assert!(res.is_err(), "expected rejection, got {res:?}");
        let msg = res.unwrap_err();
        assert!(
            msg.contains("set-reactive-power") && msg.contains("VAr"),
            "expected the set-reactive-power / VAr wording, got {msg:?}"
        );
    }

    /// The moment a child *does* report Q bounds, the reactive
    /// gateway gates on the intersection — exactly like the active
    /// one, down to the "exceeds combined envelope" wording. No
    /// production topology nests a Q-reporting child under an
    /// inverter yet, so this hangs a solar inverter (which does
    /// report a Q band) off the battery inverter to reach the branch.
    #[test]
    fn reactive_gateway_rejects_outside_the_child_intersection() {
        use std::time::Duration;
        let (cfg, _dir) = config_with(
            // Battery inverter: ±5 kVAr at P = 0. Its child solar
            // inverter carries a 1 kVA cap -> ±1 kVAr, so the
            // combined Q envelope is ±1 kVAr.
            "(setq pv (%make-solar-inverter :id 3 :sunlight% 0
                                            :rated-lower -1000.0 :rated-upper 0.0
                                            :reactive-pf-limit 0
                                            :reactive-apparent-va 1000.0))
             (%make-battery-inverter :id 2 :rated-lower -5000.0 :rated-upper 5000.0
                                       :reactive-pf-limit 0
                                       :reactive-apparent-va 5000.0
                                       :reactive-command-delay-ms 0
                                       :reactive-ramp-rate 1e9
                                       :successors (list pv))",
        );
        let site = cfg.site();
        let envelope = site
            .reactive_setpoint_envelope(2)
            .expect("the solar child reports Q bounds, so there is a combined envelope");
        assert_eq!(envelope.0.len(), 1, "expected one band, got {envelope}");
        assert_eq!(envelope.0[0].lower, Some(-1000.0));
        assert_eq!(envelope.0[0].upper, Some(1000.0));

        // 3 kVAr fits the inverter's own ±5 kVAr but not the
        // intersection — rejected at the gateway, same wording as
        // `set-active-power`'s.
        let res = cfg.eval("(set-reactive-power 2 3000.0 30000)");
        assert!(res.is_err(), "expected rejection, got {res:?}");
        let msg = res.unwrap_err();
        assert!(
            msg.contains("exceeds combined envelope"),
            "expected the active arm's envelope wording, got {msg:?}"
        );
        // 0 VAr (the fail-safe park) still passes.
        cfg.eval("(set-reactive-power 2 0.0 30000)").unwrap();
        // CLAMP pulls it into the combined envelope instead.
        cfg.eval("(set-reactive-power 2 3000.0 30000 t)").unwrap();
        let inv = site.get(2).unwrap();
        inv.tick(&site, chrono::Utc::now(), Duration::from_millis(100));
        let q = inv.telemetry(&site).reactive_power_var.unwrap();
        assert!(
            (q - 1000.0).abs() < 1.0,
            "expected clamp to the combined +1 kVAr edge, got {q}"
        );
    }

    /// A Q augmentation disjoint from the caps band leaves zero
    /// headroom: the live envelope normalizes to the single (0, 0)
    /// band. CLAMP then pulls any request to 0, which the park rule
    /// always accepts; without CLAMP the request is rejected outright.
    #[test]
    fn set_reactive_power_clamps_to_zero_at_zero_headroom() {
        use crate::sim::bounds::VecBounds;
        use std::time::Duration;
        let (cfg, _dir) = config_with(REACTIVE_SITE);
        let site = cfg.site();
        let inv = site.get(2).unwrap();
        // ±5 kVAr caps band ∩ [20 kVAr, 30 kVAr] is empty, which
        // `or_zero_band` reports as "zero headroom", not "absent".
        inv.augment_reactive_bounds(
            chrono::Utc::now(),
            VecBounds::single(20_000.0, 30_000.0),
            Duration::from_secs(30),
        );
        let band = inv
            .reactive_bounds()
            .expect("the inverter publishes Q bounds");
        assert_eq!(band.0.len(), 1, "expected one normalized band, got {band}");
        assert_eq!(band.0[0].lower, Some(0.0));
        assert_eq!(band.0[0].upper, Some(0.0));

        // Without CLAMP nothing non-zero fits.
        assert!(cfg.eval("(set-reactive-power 2 3000.0 30000)").is_err());
        // With CLAMP the request is pulled to 0 and applied.
        cfg.eval("(set-reactive-power 2 3000.0 nil t)").unwrap();
        inv.tick(&site, chrono::Utc::now(), Duration::from_millis(100));
        let q = inv.telemetry(&site).reactive_power_var.unwrap();
        assert!(q.abs() < 1.0, "expected clamp to 0 VAr, got {q}");
    }

    /// Unknown ids and components without a reactive axis (a meter)
    /// error out instead of silently no-op'ing.
    #[test]
    fn set_reactive_power_rejects_unknown_or_unsupported() {
        let (cfg, _dir) = config_with("(%make-meter :id 1)");
        let res = cfg.eval("(set-reactive-power 999 100.0)");
        assert!(res.is_err(), "expected error, got {res:?}");
        assert!(res.unwrap_err().contains("999"));
        assert!(cfg.eval("(set-reactive-power 1 100.0)").is_err());
    }
}
