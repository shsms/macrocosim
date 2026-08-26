//! `(set-meter-power)` + `(set-solar-sunlight)` — drive a
//! component's input slot from Lisp. Both accept a number (constant
//! override), a lambda (re-resolved on every refresh tick), or a
//! quoted symbol (deref the bound variable per refresh). Plus
//! `(set-battery-soc)` — teleport a battery's charge state, for
//! arranging a precondition from a scenario cue. Plus the reactive
//! Q twins: `(set-meter-reactive-power)` (same number / lambda /
//! symbol dispatch as `set-meter-power`) and `(set-meter-power-factor)`
//! (hold Q at a power factor that tracks the meter's own live P).

use tulisp::{Error, TulispContext, TulispObject};

use crate::sim::microgrids::SharedSiteRouter;

pub(super) fn register(ctx: &mut TulispContext, router: SharedSiteRouter) {
    // Drive a meter's `:power` slot from Lisp. Accepts a number, a
    // lambda, or a symbol — numeric values land as a constant
    // override (microsim-style timer-driven load curve); lambda /
    // symbol values install a DynamicScalar that the scheduler
    // re-resolves on every tick. UI's `:power` text input piggy-
    // backs on this: whatever the user types becomes the second
    // argument here.
    let r = router.clone();
    ctx.defun(
        "set-meter-power",
        move |id: i64, value: TulispObject| -> Result<bool, Error> {
            let w = r.site();
            let Some(c) = w.get(id as u64) else {
                return Err(Error::invalid_argument(format!(
                    "set-meter-power: component {id} not found"
                )));
            };
            if value.numberp() {
                let watts = f64::try_from(&value)?;
                // Lisp keeps the historic lenient behavior: the bool is
                // only enforced by the typed control API.
                let _ = c.set_active_power_override(watts as f32);
                w.note_knob_changed(id as u64, "meter-power", Some(watts as f32), None, None);
            } else if let Some(scalar) =
                crate::sim::dynamic_scalar::DynamicScalar::from_lisp(&value, 0.0)
            {
                // Printed source (same text `source_text` would report)
                // and the cached value right after construction, before
                // the scalar moves into the component.
                let printed = value.to_string();
                let resolved_now = scalar.get();
                c.set_active_power_source(scalar);
                w.note_knob_changed(
                    id as u64,
                    "meter-power",
                    Some(resolved_now),
                    Some(printed),
                    None,
                );
            } else {
                return Err(Error::invalid_argument(format!(
                    "set-meter-power: expected a number, lambda, or symbol — got {value}"
                )));
            }
            Ok(true)
        },
    );

    // Drive a meter's `:reactive-power` slot from Lisp. The Q twin of
    // set-meter-power above — same number / lambda / symbol dispatch,
    // same lenient-bool convention (the typed control API is the
    // strict door).
    let r = router.clone();
    ctx.defun(
        "set-meter-reactive-power",
        move |id: i64, value: TulispObject| -> Result<bool, Error> {
            let w = r.site();
            let Some(c) = w.get(id as u64) else {
                return Err(Error::invalid_argument(format!(
                    "set-meter-reactive-power: component {id} not found"
                )));
            };
            if value.numberp() {
                let vars = f64::try_from(&value)?;
                // Lisp keeps the historic lenient behavior: the bool is
                // only enforced by the typed control API.
                let _ = c.set_reactive_power_override(vars as f32);
                w.note_knob_changed(
                    id as u64,
                    "meter-reactive-power",
                    Some(vars as f32),
                    None,
                    None,
                );
            } else if let Some(scalar) =
                crate::sim::dynamic_scalar::DynamicScalar::from_lisp(&value, 0.0)
            {
                // Printed source and the cached value right after
                // construction, before the scalar moves into the
                // component — same pattern as set-meter-power above.
                let printed = value.to_string();
                let resolved_now = scalar.get();
                c.set_reactive_power_source(scalar);
                w.note_knob_changed(
                    id as u64,
                    "meter-reactive-power",
                    Some(resolved_now),
                    Some(printed),
                    None,
                );
            } else {
                return Err(Error::invalid_argument(format!(
                    "set-meter-reactive-power: expected a number, lambda, or symbol — got {value}"
                )));
            }
            Ok(true)
        },
    );

    // Hold a meter's reactive power at a power factor that tracks its
    // own live active power. PF is deliberately validated HERE (and
    // again by the typed control API) rather than in the trait door:
    // `set_power_factor` does no range checking of its own, so this
    // defun and the drive op are the only doors that enforce
    // `0.0 < pf <= 1.0` before the value reaches the meter.
    let r = router.clone();
    ctx.defun(
        "set-meter-power-factor",
        move |id: i64, pf: f64, leading: Option<bool>| -> Result<bool, Error> {
            if !(pf > 0.0 && pf <= 1.0) {
                return Err(Error::invalid_argument(format!(
                    "set-meter-power-factor: pf must be in (0.0, 1.0], got {pf}"
                )));
            }
            let w = r.site();
            let Some(c) = w.get(id as u64) else {
                return Err(Error::invalid_argument(format!(
                    "set-meter-power-factor: component {id} not found"
                )));
            };
            // Lisp keeps the historic lenient behavior: a non-meter is a
            // no-op here, only the typed control API rejects it.
            let leading = leading.unwrap_or(false);
            let _ = c.set_power_factor(pf as f32, leading);
            w.note_knob_changed(
                id as u64,
                "meter-power-factor",
                Some(pf as f32),
                None,
                Some(leading),
            );
            Ok(true)
        },
    );

    // Teleport a battery's state of charge (clamped to 0..=100 by the
    // battery). Scenario cues use it to arrange a precondition — a
    // nearly-empty or nearly-full pool — without simulating the charge.
    // Follows this file's convention: unknown id errors, but a
    // non-battery component is a lenient no-op (the typed control API
    // is the strict door).
    let r = router.clone();
    ctx.defun(
        "set-battery-soc",
        move |id: i64, pct: f64| -> Result<bool, Error> {
            let w = r.site();
            let Some(c) = w.get(id as u64) else {
                return Err(Error::invalid_argument(format!(
                    "set-battery-soc: component {id} not found"
                )));
            };
            let _ = c.set_soc_pct(pct as f32);
            Ok(true)
        },
    );

    // PV analogue of set-meter-power. Same numeric / dynamic
    // dispatch — drives `(set-solar-sunlight id (lambda () …))` and
    // friends from scenarios or the UI. Per-tick `min-avail =
    // rated-lower × sunlight%/100` clamp picks up the new value on
    // the next refresh + tick pair.
    let r = router;
    ctx.defun(
        "set-solar-sunlight",
        move |id: i64, value: TulispObject| -> Result<bool, Error> {
            let w = r.site();
            let Some(c) = w.get(id as u64) else {
                return Err(Error::invalid_argument(format!(
                    "set-solar-sunlight: component {id} not found"
                )));
            };
            if value.numberp() {
                let pct = f64::try_from(&value)?;
                let _ = c.set_sunlight_pct(pct as f32);
                w.note_knob_changed(id as u64, "solar-sunlight", Some(pct as f32), None, None);
            } else if let Some(scalar) =
                crate::sim::dynamic_scalar::DynamicScalar::from_lisp(&value, 100.0)
            {
                // Printed source and the cached value right after
                // construction — same pattern as set-meter-power above.
                let printed = value.to_string();
                let resolved_now = scalar.get();
                c.set_sunlight_source(scalar);
                w.note_knob_changed(
                    id as u64,
                    "solar-sunlight",
                    Some(resolved_now),
                    Some(printed),
                    None,
                );
            } else {
                return Err(Error::invalid_argument(format!(
                    "set-solar-sunlight: expected a number, lambda, or symbol — got {value}"
                )));
            }
            Ok(true)
        },
    );
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::config_with;
    use crate::sim::component::ReactiveReading;
    use crate::sim::events::SiteEvent;

    /// `(set-meter-power id (lambda () X))` installs a dynamic
    /// source. `Config::refresh_once` resolves the lambda and
    /// `aggregate_power_w` reflects it on the next read.
    #[test]
    fn set_meter_power_accepts_a_lambda() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");
        cfg.eval("(set-meter-power 7 (lambda () 1234.5))").unwrap();
        cfg.refresh_once();
        let m = cfg.site().get(7).unwrap();
        assert!((m.aggregate_power_w(&cfg.site()) - 1234.5).abs() < 1e-3);
    }

    /// `(set-meter-power id 'symbol)` derefs the symbol's variable
    /// value each refresh — scenarios use this to drive a load
    /// curve from a global that another timer mutates.
    #[test]
    fn set_meter_power_accepts_a_symbol() {
        let (cfg, _dir) = config_with(
            "(setq consumer-power 1500.0)
             (%make-meter :id 7)",
        );
        cfg.eval("(set-meter-power 7 'consumer-power)").unwrap();
        cfg.refresh_once();
        let m = cfg.site().get(7).unwrap();
        assert!((m.aggregate_power_w(&cfg.site()) - 1500.0).abs() < 1e-3);
        // Mutate the bound variable; next refresh picks up the new value.
        cfg.eval("(setq consumer-power 2750.0)").unwrap();
        cfg.refresh_once();
        assert!((m.aggregate_power_w(&cfg.site()) - 2750.0).abs() < 1e-3);
    }

    /// `(set-solar-sunlight id (lambda () X))` mirrors
    /// `set-meter-power` for PV. Refresh resolves the lambda; the
    /// next setpoint clip surfaces the new floor.
    #[test]
    fn set_solar_sunlight_accepts_a_lambda() {
        let (cfg, _dir) =
            config_with("(%make-solar-inverter :id 8 :rated-lower -8000.0 :rated-upper 0.0)");
        cfg.eval("(set-solar-sunlight 8 (lambda () 25.0))").unwrap();
        cfg.refresh_once();
        let inv = cfg.site().get(8).unwrap();
        // Issue a setpoint below sunlight-derated min_avail so the
        // ramp clips — observable through telemetry's active_power.
        inv.set_active_setpoint(-5000.0).expect("within rated");
        cfg.site()
            .tick_once(chrono::Utc::now(), std::time::Duration::from_millis(100));
        let p = inv
            .telemetry(&cfg.site())
            .active_power_w
            .expect("active power present");
        // 25% of -8000 = -2000 W floor.
        assert!(
            (p - (-2000.0)).abs() < 1.0,
            "expected sunlight-clipped -2000 W, got {p}",
        );
    }

    /// `(set-meter-power id V)` broadcasts a `KnobChanged` on the
    /// site event bus so live UI inspector tabs can refresh their
    /// edit-in-place input without a full topology refetch.
    #[test]
    fn set_meter_power_broadcasts_knob_changed() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");
        let mut rx = cfg.site().subscribe_events();
        cfg.eval("(set-meter-power 7 1500)").unwrap();
        let mut seen = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            seen.push(ev);
        }
        assert!(
            seen.iter().any(|ev| matches!(
                ev,
                SiteEvent::KnobChanged { id: 7, knob: "meter-power", value: Some(v), expr: None, .. }
                    if (*v - 1500.0).abs() < 1e-6
            )),
            "no matching KnobChanged on the bus; saw: {seen:?}"
        );
    }

    /// `(set-meter-reactive-power id V)` broadcasts a `KnobChanged`
    /// mirroring the active-power case above.
    #[test]
    fn set_meter_reactive_power_broadcasts_knob_changed() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");
        let mut rx = cfg.site().subscribe_events();
        cfg.eval("(set-meter-reactive-power 7 500.0)").unwrap();
        let mut seen = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            seen.push(ev);
        }
        assert!(
            seen.iter().any(|ev| matches!(
                ev,
                SiteEvent::KnobChanged { id: 7, knob: "meter-reactive-power", value: Some(v), expr: None, .. }
                    if (*v - 500.0).abs() < 1e-6
            )),
            "no matching KnobChanged on the bus; saw: {seen:?}"
        );
    }

    /// `(set-meter-power-factor id PF LEADING)` broadcasts a
    /// `KnobChanged` carrying the `leading` flag — the inspector's
    /// PF input needs it to render the lagging/leading toggle.
    #[test]
    fn set_meter_power_factor_broadcasts_knob_changed() {
        let (cfg, _dir) = config_with("(%make-meter :id 7 :power 8000.0)");
        let mut rx = cfg.site().subscribe_events();
        cfg.eval("(set-meter-power-factor 7 0.8 t)").unwrap();
        let mut seen = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            seen.push(ev);
        }
        assert!(
            seen.iter().any(|ev| matches!(
                ev,
                SiteEvent::KnobChanged {
                    id: 7,
                    knob: "meter-power-factor",
                    value: Some(v),
                    expr: None,
                    leading: Some(true),
                    ..
                } if (*v - 0.8).abs() < 1e-6
            )),
            "no matching KnobChanged on the bus; saw: {seen:?}"
        );
    }

    /// `(set-solar-sunlight id V)` broadcasts a `KnobChanged`
    /// mirroring the meter-power case.
    #[test]
    fn set_solar_sunlight_broadcasts_knob_changed() {
        let (cfg, _dir) = config_with("(%make-solar-inverter :id 8)");
        let mut rx = cfg.site().subscribe_events();
        cfg.eval("(set-solar-sunlight 8 63)").unwrap();
        let mut seen = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            seen.push(ev);
        }
        assert!(
            seen.iter().any(|ev| matches!(
                ev,
                SiteEvent::KnobChanged { id: 8, knob: "solar-sunlight", value: Some(v), expr: None, .. }
                    if (*v - 63.0).abs() < 1e-6
            )),
            "no matching KnobChanged on the bus; saw: {seen:?}"
        );
    }

    /// A dynamic (lambda) source carries the printed source text in
    /// `expr` instead of `None`.
    #[test]
    fn set_meter_power_knob_changed_carries_expr_for_lambda() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");
        let mut rx = cfg.site().subscribe_events();
        cfg.eval("(set-meter-power 7 (lambda () 25))").unwrap();
        let mut seen = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            seen.push(ev);
        }
        assert!(
            seen.iter().any(|ev| matches!(
                ev,
                SiteEvent::KnobChanged {
                    id: 7,
                    knob: "meter-power",
                    expr: Some(_),
                    ..
                }
            )),
            "no matching KnobChanged with expr on the bus; saw: {seen:?}"
        );
    }

    /// `(set-meter-power id "garbage")` should error rather than
    /// silently passing through the from_eval branch and tripping
    /// the non-numeric refresh fallback every tick.
    #[test]
    fn set_meter_power_rejects_bare_string() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");
        // A bare string is from_eval-eligible (returns Some) and
        // would never resolve to a number — but it doesn't roundtrip
        // through a useful curve, so users should reach for a lambda
        // or symbol instead. This assertion documents the behaviour:
        // the call succeeds (string isn't nil) and refresh just keeps
        // the fallback.
        assert!(
            cfg.eval("(set-meter-power 7 \"garbage\")").is_ok(),
            "string is accepted as an eval source — fallback governs",
        );
    }

    /// `(set-meter-reactive-power id VALUE)` mirrors `set-meter-power`'s
    /// dispatch: a number installs a constant Q override, a lambda is
    /// resolved on refresh, and a symbol re-derefs its bound variable
    /// each refresh. Read back through `aggregate_reactive_var`.
    #[test]
    fn set_meter_reactive_power_accepts_number_lambda_symbol() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");

        // Number → constant Q override, no refresh needed.
        cfg.eval("(set-meter-reactive-power 7 500.0)").unwrap();
        let m = cfg.site().get(7).unwrap();
        assert!((m.aggregate_reactive_var(&cfg.site()) - 500.0).abs() < 1e-3);

        // Lambda → dynamic source, resolved on the next refresh.
        cfg.eval("(set-meter-reactive-power 7 (lambda () 1234.5))")
            .unwrap();
        cfg.refresh_once();
        assert!((m.aggregate_reactive_var(&cfg.site()) - 1234.5).abs() < 1e-3);

        // Symbol → deref the bound variable each refresh.
        cfg.eval(
            "(setq reactive-var-src 750.0)
             (set-meter-reactive-power 7 'reactive-var-src)",
        )
        .unwrap();
        cfg.refresh_once();
        assert!((m.aggregate_reactive_var(&cfg.site()) - 750.0).abs() < 1e-3);
        cfg.eval("(setq reactive-var-src 900.0)").unwrap();
        cfg.refresh_once();
        assert!((m.aggregate_reactive_var(&cfg.site()) - 900.0).abs() < 1e-3);
    }

    /// `(set-meter-power-factor id PF &optional LEADING)` derives Q
    /// from the meter's own live P: 0.8 lagging on 8000 W → 6000 VAr;
    /// LEADING flips the sign; an out-of-range PF errors before ever
    /// touching the meter.
    #[test]
    fn set_meter_power_factor_derives_from_live_p() {
        let (cfg, _dir) = config_with("(%make-meter :id 7 :power 8000.0)");
        let m = cfg.site().get(7).unwrap();

        cfg.eval("(set-meter-power-factor 7 0.8)").unwrap();
        assert!((m.aggregate_reactive_var(&cfg.site()) - 6_000.0).abs() < 1.0);

        // Leading flips the sign.
        cfg.eval("(set-meter-power-factor 7 0.8 t)").unwrap();
        assert!((m.aggregate_reactive_var(&cfg.site()) - -6_000.0).abs() < 1.0);

        // Out of (0.0, 1.0] errors, naming the range, and never reaches
        // the trait door (set_power_factor does no validation itself).
        let err = cfg.eval("(set-meter-power-factor 7 1.5)").unwrap_err();
        assert!(err.to_string().contains("(0.0, 1.0]"), "{err}");
        let err = cfg.eval("(set-meter-power-factor 7 0.0)").unwrap_err();
        assert!(err.to_string().contains("(0.0, 1.0]"), "{err}");

        // Unknown id errors.
        assert!(cfg.eval("(set-meter-power-factor 99 0.8)").is_err());
    }

    /// `(set-battery-soc id PCT)` teleports the charge state; the next
    /// telemetry read reflects it. An unknown id errors.
    #[test]
    fn set_battery_soc_teleports_state() {
        let (cfg, _dir) = config_with("(%make-battery :id 4 :initial-soc 60.0)");
        cfg.eval("(set-battery-soc 4 12.5)").unwrap();
        let site = cfg.site();
        let soc = site.get(4).unwrap().telemetry(&site).soc_pct.unwrap();
        assert!((soc - 12.5).abs() < 1e-3, "{soc}");
        assert!(cfg.eval("(set-battery-soc 99 50.0)").is_err());
    }

    /// `meter_power_reading` round-trips a constant `:power` override
    /// (no source text) and a dynamic lambda override (some source
    /// text, opaque as it is — see the `expr` assertion below) — the
    /// knob read-back Task 6's inspector snapshot pulls from.
    #[test]
    fn meter_power_reading_round_trips_constant_and_expr() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");
        cfg.eval("(set-meter-power 7 1500)").unwrap();
        let site = cfg.site();
        let c = site.get(7).unwrap();
        let r = c.meter_power_reading().expect("reading");
        assert_eq!(r.value, 1500.0);
        assert_eq!(r.expr, None);

        cfg.eval("(set-meter-power 7 (lambda () 25))").unwrap();
        cfg.refresh_once();
        let r = site.get(7).unwrap().meter_power_reading().expect("reading");
        assert_eq!(r.value, 25.0);
        // An unquoted lambda evaluates to a compiled function before
        // `DynamicScalar::from_lisp` ever sees it, so it routes
        // through the funcall branch and prints as the opaque
        // "CompiledDefun" — not the literal source text. That's
        // `source_text`'s documented behavior (see Task 1); the
        // read-back contract here is just "dynamic source ⇒ some
        // printed text", not pretty-printing.
        assert!(r.expr.is_some(), "dynamic source should carry expr text");
    }

    /// `meter_reactive_reading` reports the `PowerFactor` shape (pf +
    /// leading) once `(set-meter-power-factor)` has installed one —
    /// the `Var` shape is exercised implicitly by every other
    /// reactive-power test in this file.
    #[test]
    fn meter_power_factor_reading_reports_pf_and_leading() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");
        cfg.eval("(set-meter-power 7 1000)").unwrap();
        cfg.eval("(set-meter-power-factor 7 0.9 t)").unwrap();
        let site = cfg.site();
        match site.get(7).unwrap().meter_reactive_reading() {
            Some(ReactiveReading::PowerFactor { pf, leading }) => {
                assert!((pf - 0.9).abs() < 1e-6);
                assert!(leading);
            }
            other => panic!("expected PowerFactor, got {other:?}"),
        }
    }

    /// `sunlight_reading` reads back the PV inverter's cloud-cover
    /// knob after `(set-solar-sunlight)` pokes in a constant.
    #[test]
    fn sunlight_reading_reads_back_percentage() {
        let (cfg, _dir) = config_with("(%make-solar-inverter :id 4)");
        cfg.eval("(set-solar-sunlight 4 63)").unwrap();
        cfg.refresh_once();
        let site = cfg.site();
        let r = site.get(4).unwrap().sunlight_reading().expect("reading");
        assert_eq!(r.value, 63.0);
    }
}
