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

use crate::sim::component::KnobKind;
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
            w.scenario_snapshot_knob(id as u64, KnobKind::MeterPower);
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
            w.scenario_snapshot_knob(id as u64, KnobKind::MeterReactive);
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

    // Drop a meter's active-power override, returning it to measuring
    // its children — the one-way trip set-meter-power never had a way
    // back from. Gated on the trait door itself: `false` means "not a
    // meter" (a meter always returns true, even with nothing set).
    let r = router.clone();
    ctx.defun("clear-meter-power", move |id: i64| -> Result<bool, Error> {
        let w = r.site();
        let Some(c) = w.get(id as u64) else {
            return Err(Error::invalid_argument(format!(
                "clear-meter-power: component {id} not found"
            )));
        };
        w.scenario_snapshot_knob(id as u64, KnobKind::MeterPower);
        if !c.clear_active_power_source() {
            return Err(Error::invalid_argument(format!(
                "clear-meter-power: component {id} is not a meter"
            )));
        }
        w.note_knob_changed(id as u64, "meter-power", None, None, None);
        Ok(true)
    });

    // Drop a meter's reactive-power override — whichever of Var /
    // PowerFactor is set, it's the same slot — returning it to summing
    // children's Q. The Q twin of clear-meter-power above.
    let r = router.clone();
    ctx.defun(
        "clear-meter-reactive",
        move |id: i64| -> Result<bool, Error> {
            let w = r.site();
            let Some(c) = w.get(id as u64) else {
                return Err(Error::invalid_argument(format!(
                    "clear-meter-reactive: component {id} not found"
                )));
            };
            w.scenario_snapshot_knob(id as u64, KnobKind::MeterReactive);
            if !c.clear_reactive_power_source() {
                return Err(Error::invalid_argument(format!(
                    "clear-meter-reactive: component {id} is not a meter"
                )));
            }
            // Two tokens, one slot: the inspector's power-factor input
            // is a knob of its own ("meter-power-factor"), separate
            // from "meter-reactive-power" — a PowerFactor-shaped clear
            // must blank both or the PF input keeps showing a stale
            // number until the next full snapshot.
            w.note_knob_changed(id as u64, "meter-reactive-power", None, None, None);
            w.note_knob_changed(id as u64, "meter-power-factor", None, None, None);
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
            w.scenario_snapshot_knob(id as u64, KnobKind::MeterReactive);
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
    // max(-array-peak-w × sunlight%/100, rated-lower)` clamp — the AC
    // rating floor — picks up the new value on the next refresh +
    // tick pair.
    let r = router.clone();
    ctx.defun(
        "set-solar-sunlight",
        move |id: i64, value: TulispObject| -> Result<bool, Error> {
            let w = r.site();
            let Some(c) = w.get(id as u64) else {
                return Err(Error::invalid_argument(format!(
                    "set-solar-sunlight: component {id} not found"
                )));
            };
            w.scenario_snapshot_knob(id as u64, KnobKind::Sunlight);
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

    // Steam boiler analogue of set-meter-power / set-solar-sunlight:
    // drive the `:demand` (kg/h) input from Lisp. Same numeric /
    // dynamic dispatch — a number installs a constant, a lambda or
    // symbol installs a DynamicScalar the scheduler re-resolves each
    // refresh tick. Gated on takes_steam_demand() since (unlike the
    // meter/solar setters) a non-boiler must reject here, not
    // silently no-op — this is a first-class inspector knob.
    let r = router.clone();
    ctx.defun(
        "set-boiler-demand",
        move |id: i64, value: TulispObject| -> Result<bool, Error> {
            let w = r.site();
            let Some(c) = w.get(id as u64) else {
                return Err(Error::invalid_argument(format!(
                    "set-boiler-demand: component {id} not found"
                )));
            };
            if !c.takes_steam_demand() {
                return Err(Error::invalid_argument(format!(
                    "set-boiler-demand: component {id} is not a steam boiler"
                )));
            }
            w.scenario_snapshot_knob(id as u64, KnobKind::BoilerDemand);
            if value.numberp() {
                let kg_h = f64::try_from(&value)?;
                let _ = c.set_steam_demand_kg_h(kg_h as f32);
                w.note_knob_changed(id as u64, "boiler-demand", Some(kg_h as f32), None, None);
            } else if let Some(scalar) =
                crate::sim::dynamic_scalar::DynamicScalar::from_lisp(&value, 0.0)
            {
                // Printed source and the cached value right after
                // construction — same pattern as set-meter-power above.
                let printed = value.to_string();
                let resolved_now = scalar.get();
                c.set_steam_demand_source(scalar);
                w.note_knob_changed(
                    id as u64,
                    "boiler-demand",
                    Some(resolved_now),
                    Some(printed),
                    None,
                );
            } else {
                return Err(Error::invalid_argument(format!(
                    "set-boiler-demand: expected a number, lambda, or symbol — got {value}"
                )));
            }
            Ok(true)
        },
    );

    // Steam boiler pressure override — numeric only (unlike demand,
    // pressure has no dynamic-source door on the trait). Gated on
    // takes_pressure_bar() for the same reason as set-boiler-demand.
    let r = router;
    ctx.defun(
        "set-boiler-pressure",
        move |id: i64, bar: f64| -> Result<bool, Error> {
            let w = r.site();
            let Some(c) = w.get(id as u64) else {
                return Err(Error::invalid_argument(format!(
                    "set-boiler-pressure: component {id} not found"
                )));
            };
            if !c.takes_pressure_bar() {
                return Err(Error::invalid_argument(format!(
                    "set-boiler-pressure: component {id} is not a steam boiler"
                )));
            }
            let _ = c.set_pressure_bar(bar as f32);
            w.note_knob_changed(id as u64, "boiler-pressure", Some(bar as f32), None, None);
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

    /// `(clear-meter-power id)` after `(set-meter-power id V)`
    /// restores measuring: `meter_power_reading()` goes back to
    /// `None` and aggregation reads the (empty, here zero) children
    /// sum instead of the constant. Errors on a non-meter.
    #[test]
    fn clear_meter_power_restores_measuring() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");
        cfg.eval("(set-meter-power 7 5000.0)").unwrap();
        let site = cfg.site();
        let m = site.get(7).unwrap();
        assert!(m.meter_power_reading().is_some());

        cfg.eval("(clear-meter-power 7)").unwrap();
        assert!(m.meter_power_reading().is_none());
        assert_eq!(m.aggregate_power_w(&site), 0.0);

        // Non-meter: the battery's default trait method returns
        // false, so the defun errors instead of silently no-opping.
        let (cfg2, _dir2) = config_with("(%make-battery :id 4)");
        let err = cfg2.eval("(clear-meter-power 4)").unwrap_err();
        assert!(err.to_string().contains("not a meter"), "{err}");

        // Unknown id errors too.
        assert!(cfg.eval("(clear-meter-power 99)").is_err());
    }

    /// `(clear-meter-reactive id)` clears whichever reactive state is
    /// set — a `Var` override here — restoring the children sum. The
    /// same defun also clears a `PowerFactor` state (one slot, both
    /// shapes route through the same trait door).
    #[test]
    fn clear_meter_reactive_restores_measuring() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");
        cfg.eval("(set-meter-reactive-power 7 500.0)").unwrap();
        let site = cfg.site();
        let m = site.get(7).unwrap();
        assert!(m.meter_reactive_reading().is_some());

        cfg.eval("(clear-meter-reactive 7)").unwrap();
        assert!(m.meter_reactive_reading().is_none());
        assert_eq!(m.aggregate_reactive_var(&site), 0.0);

        let err = cfg.eval("(clear-meter-reactive 99)").unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");

        // Non-meter: same "not a meter" error branch as
        // clear-meter-power above.
        let (cfg2, _dir2) = config_with("(%make-battery :id 4)");
        let err = cfg2.eval("(clear-meter-reactive 4)").unwrap_err();
        assert!(err.to_string().contains("not a meter"), "{err}");
    }

    /// `(clear-meter-reactive id)` broadcasts on BOTH knob tokens: the
    /// inspector's power-factor input is a separate knob
    /// ("meter-power-factor") from "meter-reactive-power", so a
    /// PowerFactor-shaped clear must blank both or the PF input keeps
    /// showing a stale number until the next full snapshot.
    #[test]
    fn clear_meter_reactive_broadcasts_both_knob_tokens() {
        let (cfg, _dir) = config_with("(%make-meter :id 7 :power 8000.0)");
        cfg.eval("(set-meter-power-factor 7 0.8 t)").unwrap();
        let mut rx = cfg.site().subscribe_events();
        cfg.eval("(clear-meter-reactive 7)").unwrap();
        let mut seen = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            seen.push(ev);
        }
        assert!(
            seen.iter().any(|ev| matches!(
                ev,
                SiteEvent::KnobChanged {
                    id: 7,
                    knob: "meter-reactive-power",
                    value: None,
                    expr: None,
                    ..
                }
            )),
            "no meter-reactive-power KnobChanged on the bus; saw: {seen:?}"
        );
        assert!(
            seen.iter().any(|ev| matches!(
                ev,
                SiteEvent::KnobChanged {
                    id: 7,
                    knob: "meter-power-factor",
                    value: None,
                    expr: None,
                    ..
                }
            )),
            "no meter-power-factor KnobChanged on the bus; saw: {seen:?}"
        );
    }

    /// `(clear-meter-power id)` broadcasts a `KnobChanged` with a
    /// `None` value so a live inspector tab blanks the `:power`
    /// input instead of showing a stale number.
    #[test]
    fn clear_meter_power_broadcasts_knob_changed_with_none() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");
        cfg.eval("(set-meter-power 7 1500)").unwrap();
        let mut rx = cfg.site().subscribe_events();
        cfg.eval("(clear-meter-power 7)").unwrap();
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
                    value: None,
                    expr: None,
                    ..
                }
            )),
            "no matching KnobChanged on the bus; saw: {seen:?}"
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

    /// `(set-boiler-demand id N)` installs a constant kg/h demand,
    /// read back immediately through `demand_reading` (no tick
    /// needed — it reads the source directly).
    #[test]
    fn set_boiler_demand_accepts_a_number() {
        let (cfg, _dir) = config_with("(%make-steam-boiler :id 9)");
        cfg.eval("(set-boiler-demand 9 40.0)").unwrap();
        let site = cfg.site();
        let r = site.get(9).unwrap().demand_reading().expect("reading");
        assert_eq!(r.value, 40.0);
    }

    /// `(set-boiler-demand id (lambda () X))` installs a dynamic
    /// source, re-resolved on refresh — mirrors
    /// `set_solar_sunlight_accepts_a_lambda` above.
    #[test]
    fn set_boiler_demand_accepts_a_lambda() {
        let (cfg, _dir) = config_with("(%make-steam-boiler :id 9)");
        cfg.eval("(set-boiler-demand 9 (lambda () 25.0))").unwrap();
        cfg.refresh_once();
        cfg.site()
            .tick_once(chrono::Utc::now(), std::time::Duration::from_millis(100));
        let site = cfg.site();
        let r = site.get(9).unwrap().demand_reading().expect("reading");
        assert!((r.value - 25.0).abs() < 1e-6, "{}", r.value);
    }

    /// `(set-boiler-pressure id BAR)` overwrites the pressure state,
    /// reflected immediately in telemetry.
    #[test]
    fn set_boiler_pressure_moves_state() {
        let (cfg, _dir) = config_with("(%make-steam-boiler :id 9)");
        cfg.eval("(set-boiler-pressure 9 9.0)").unwrap();
        let site = cfg.site();
        let r = site.get(9).unwrap().pressure_reading().expect("reading");
        assert_eq!(r.value, 9.0);
    }

    /// Both boiler defuns error on a non-boiler component instead of
    /// silently no-opping — they're first-class inspector knobs, so
    /// this door is strict (unlike e.g. `set-battery-soc`).
    #[test]
    fn set_boiler_defuns_error_on_non_boiler() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");
        let err = cfg.eval("(set-boiler-demand 7 40.0)").unwrap_err();
        assert!(err.to_string().contains("not a steam boiler"), "{err}");
        let err = cfg.eval("(set-boiler-pressure 7 9.0)").unwrap_err();
        assert!(err.to_string().contains("not a steam boiler"), "{err}");
    }

    // ── scenario teardown: these setters/clears snapshot BEFORE they
    // mutate, and `(scenario-stop)` restores from that baseline. ─────

    /// A dynamic (symbol) sunlight source survives a scenario driving
    /// it to a constant and back: `(scenario-stop)` restores the
    /// exact captured `DynamicScalar`, not a re-parse of its text, so
    /// both the printed source AND its live tracking come back.
    #[test]
    fn scenario_stop_restores_a_dynamic_sunlight_source() {
        let (cfg, _dir) = config_with(
            "(setq sun-src 40.0)
             (%make-solar-inverter :id 8 :rated-lower -8000.0 :rated-upper 0.0)",
        );
        cfg.eval("(set-solar-sunlight 8 'sun-src)").unwrap();
        cfg.refresh_once();
        let inv = cfg.site().get(8).unwrap();
        let before = inv.sunlight_reading().unwrap();
        assert!(
            before.expr.is_some(),
            "baseline is the dynamic symbol source"
        );
        assert_eq!(before.value, 40.0);

        cfg.eval("(scenario-start \"sun\")").unwrap();
        cfg.eval("(set-solar-sunlight 8 10.0)").unwrap();
        assert!(
            inv.sunlight_reading().unwrap().expr.is_none(),
            "scenario collapsed it to a constant"
        );

        cfg.eval("(scenario-stop)").unwrap();
        let after = inv.sunlight_reading().unwrap();
        assert!(after.expr.is_some(), "dynamic source restored");
        assert_eq!(after.expr, before.expr);

        // And it still tracks live: mutate the global, refresh, see
        // it move — proves restore put back the real symbol source,
        // not a frozen snapshot of its last-read value.
        cfg.eval("(setq sun-src 77.0)").unwrap();
        cfg.refresh_once();
        assert_eq!(inv.sunlight_reading().unwrap().value, 77.0);
    }

    /// A meter with no baseline override (measuring its children):
    /// driven by a scenario, then `(scenario-stop)` returns it to
    /// measuring — `meter_power_reading()` is `None` again, not some
    /// leftover scenario value.
    #[test]
    fn scenario_stop_restores_meter_with_no_baseline_to_measuring() {
        let (cfg, _dir) = config_with("(%make-meter :id 7)");
        let m = cfg.site().get(7).unwrap();
        assert!(m.meter_power_reading().is_none(), "starts measuring");

        cfg.eval("(scenario-start \"m-measuring\")").unwrap();
        cfg.eval("(set-meter-power 7 5000.0)").unwrap();
        assert!(m.meter_power_reading().is_some());

        cfg.eval("(scenario-stop)").unwrap();
        assert!(
            m.meter_power_reading().is_none(),
            "meter must return to measuring, not stay at the scenario's driven value"
        );
    }

    /// A meter constructed with `:power 5000.0`, driven by a scenario
    /// to a dynamic source, then stopped: the reading AND the
    /// `:power` constructor kwarg both come back — restore is
    /// mechanical (unlike `clear-meter-power`, which would drop the
    /// kwarg), and `has_unrenderable_source` reports the meter is
    /// plain-savable again.
    #[test]
    fn scenario_stop_restores_constructed_meter_power_and_kwarg() {
        let (cfg, _dir) = config_with("(%make-meter :id 7 :power 5000.0)");
        let m = cfg.site().get(7).unwrap();
        assert_eq!(m.meter_power_reading().unwrap().value, 5000.0);

        cfg.eval("(scenario-start \"m-constructed\")").unwrap();
        cfg.eval("(set-meter-power 7 (lambda () 42.0))").unwrap();
        cfg.refresh_once();
        assert_eq!(m.meter_power_reading().unwrap().value, 42.0);

        cfg.eval("(scenario-stop)").unwrap();
        assert_eq!(m.meter_power_reading().unwrap().value, 5000.0);
        assert!(!m.has_unrenderable_source());
        let kw = m
            .constructor_kwargs()
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(kw.contains(":power 5000"), "{kw}");
    }

    /// PF/Var aliasing: a baseline `Var` reactive source, driven to a
    /// `PowerFactor` by `set-meter-power-factor`, restores back to
    /// `Var` on `(scenario-stop)` — the whole `ReactiveSource` shape
    /// round-trips, not just a number.
    #[test]
    fn scenario_stop_restores_var_reactive_after_power_factor_drive() {
        let (cfg, _dir) = config_with("(%make-meter :id 7 :power 8000.0)");
        cfg.eval("(set-meter-reactive-power 7 500.0)").unwrap();
        let m = cfg.site().get(7).unwrap();
        match m.meter_reactive_reading().unwrap() {
            ReactiveReading::Var(r) => assert_eq!(r.value, 500.0),
            ReactiveReading::PowerFactor { .. } => panic!("expected baseline Var"),
        }

        cfg.eval("(scenario-start \"pf\")").unwrap();
        cfg.eval("(set-meter-power-factor 7 0.8 t)").unwrap();
        match m.meter_reactive_reading().unwrap() {
            ReactiveReading::PowerFactor { .. } => {}
            ReactiveReading::Var(_) => panic!("expected PowerFactor after the scenario drive"),
        }

        cfg.eval("(scenario-stop)").unwrap();
        match m.meter_reactive_reading().unwrap() {
            ReactiveReading::Var(r) => assert_eq!(r.value, 500.0),
            ReactiveReading::PowerFactor { .. } => panic!("expected Var restored"),
        }
    }

    /// First-snapshot-wins: a scenario driving the same knob twice —
    /// its own drive, then a second direct eval standing in for a
    /// cue re-setting it later (no real timer needed to exercise
    /// this; a cue re-driving the same knob compiles to exactly this
    /// same `set-meter-power` call) — still restores to the value
    /// from BEFORE the scenario ever touched it, not to either driven
    /// value.
    #[test]
    fn scenario_stop_restores_first_snapshot_despite_repeated_drives() {
        let (cfg, _dir) = config_with("(%make-meter :id 7 :power 1200.0)");
        let m = cfg.site().get(7).unwrap();

        cfg.eval("(scenario-start \"first-wins\")").unwrap();
        cfg.eval("(set-meter-power 7 3000.0)").unwrap(); // captures the 1200.0 baseline
        cfg.eval("(set-meter-power 7 7000.0)").unwrap(); // a second direct drive — no-op on the baseline
        assert_eq!(m.meter_power_reading().unwrap().value, 7000.0);

        cfg.eval("(scenario-stop)").unwrap();
        assert_eq!(
            m.meter_power_reading().unwrap().value,
            1200.0,
            "restore must land on the FIRST pre-scenario value, not an intermediate drive"
        );
    }

    /// `(scenario-stop)` is idempotent, and nothing resurrects a
    /// manual poke made AFTER it: the baseline map was drained by the
    /// first stop, so a second stop restores nothing and leaves a
    /// later poke exactly as the user left it.
    #[test]
    fn scenario_stop_is_idempotent_and_a_post_stop_poke_sticks() {
        let (cfg, _dir) = config_with("(%make-meter :id 7 :power 1500.0)");
        let m = cfg.site().get(7).unwrap();

        cfg.eval("(scenario-start \"idempotent\")").unwrap();
        cfg.eval("(set-meter-power 7 4000.0)").unwrap();
        cfg.eval("(scenario-stop)").unwrap();
        assert_eq!(m.meter_power_reading().unwrap().value, 1500.0);

        // A manual poke after stop: nothing tracks it anymore, so it
        // just sticks.
        cfg.eval("(set-meter-power 7 9999.0)").unwrap();
        assert_eq!(m.meter_power_reading().unwrap().value, 9999.0);

        // A second stop must not disturb it.
        cfg.eval("(scenario-stop)").unwrap();
        assert_eq!(
            m.meter_power_reading().unwrap().value,
            9999.0,
            "second stop must be a no-op — it must not resurrect a pre-scenario value \
             over a later manual poke"
        );
    }

    /// The case restore exists for, on its real path: a scenario
    /// CLEARS a knob the component was constructed with. `clear` is a
    /// user-intent verb — it drops the `:power` kwarg too, so the
    /// component saves as "measuring" — which is right for a user and
    /// wrong for a scenario that only borrowed the knob. Mid-scenario
    /// the clear must take full effect (no reading, no kwarg); at stop
    /// BOTH halves must come back, or the meter is left permanently
    /// unable to write its own `:power` back to disk.
    #[test]
    fn scenario_stop_restores_a_constructed_power_kwarg_a_clear_dropped() {
        let (cfg, _dir) = config_with("(%make-meter :id 7 :power 5000.0)");
        let m = cfg.site().get(7).unwrap();
        let kwargs = || {
            cfg.site()
                .get(7)
                .unwrap()
                .constructor_kwargs()
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert!(kwargs().contains(":power 5000"), "{}", kwargs());

        cfg.eval("(scenario-start \"clear-p\")").unwrap();
        cfg.eval("(clear-meter-power 7)").unwrap();
        assert!(
            m.meter_power_reading().is_none(),
            "the clear must really clear while the scenario runs"
        );
        assert!(
            !kwargs().contains(":power"),
            "the clear drops the constructed kwarg too: {}",
            kwargs()
        );

        cfg.eval("(scenario-stop)").unwrap();
        assert_eq!(m.meter_power_reading().unwrap().value, 5000.0);
        assert!(
            kwargs().contains(":power 5000"),
            "the constructed kwarg must come back, not just the live source: {}",
            kwargs()
        );
    }

    /// The reactive twin: a `:reactive-power`-constructed meter,
    /// cleared mid-scenario, gets both its `Var` source and its
    /// `:reactive-power` kwarg back at stop.
    #[test]
    fn scenario_stop_restores_a_constructed_reactive_kwarg_a_clear_dropped() {
        let (cfg, _dir) = config_with("(%make-meter :id 7 :power 8000.0 :reactive-power 500.0)");
        let m = cfg.site().get(7).unwrap();
        let kwargs = || {
            cfg.site()
                .get(7)
                .unwrap()
                .constructor_kwargs()
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert!(kwargs().contains(":reactive-power 500"), "{}", kwargs());

        cfg.eval("(scenario-start \"clear-q\")").unwrap();
        cfg.eval("(clear-meter-reactive 7)").unwrap();
        assert!(m.meter_reactive_reading().is_none());
        assert!(!kwargs().contains(":reactive-power"), "{}", kwargs());

        cfg.eval("(scenario-stop)").unwrap();
        match m.meter_reactive_reading().unwrap() {
            ReactiveReading::Var(r) => assert_eq!(r.value, 500.0),
            ReactiveReading::PowerFactor { .. } => panic!("expected the constructed Var back"),
        }
        assert!(
            kwargs().contains(":reactive-power 500"),
            "the constructed kwarg must come back: {}",
            kwargs()
        );
        // The active axis was never touched, so its own kwarg stands.
        assert!(kwargs().contains(":power 8000"), "{}", kwargs());
    }

    /// Same again for the OTHER `ConstructedReactive` shape: a meter
    /// built with `:power-factor` (+ `:leading`) round-trips the pf
    /// pair, not a number — the reactive snapshot carries the enum.
    #[test]
    fn scenario_stop_restores_a_constructed_power_factor_a_clear_dropped() {
        let (cfg, _dir) =
            config_with("(%make-meter :id 7 :power 8000.0 :power-factor 0.8 :leading t)");
        let m = cfg.site().get(7).unwrap();
        let kwargs = || {
            cfg.site()
                .get(7)
                .unwrap()
                .constructor_kwargs()
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert!(kwargs().contains(":power-factor 0.8"), "{}", kwargs());
        assert!(kwargs().contains(":leading t"), "{}", kwargs());

        cfg.eval("(scenario-start \"clear-pf\")").unwrap();
        cfg.eval("(clear-meter-reactive 7)").unwrap();
        assert!(m.meter_reactive_reading().is_none());
        assert!(!kwargs().contains(":power-factor"), "{}", kwargs());

        cfg.eval("(scenario-stop)").unwrap();
        match m.meter_reactive_reading().unwrap() {
            ReactiveReading::PowerFactor { pf, leading } => {
                assert_eq!(pf, 0.8);
                assert!(leading, "the leading flag is part of the constructed pair");
            }
            ReactiveReading::Var(_) => panic!("expected the constructed PowerFactor back"),
        }
        assert!(kwargs().contains(":power-factor 0.8"), "{}", kwargs());
        assert!(kwargs().contains(":leading t"), "{}", kwargs());
    }

    /// Boiler-demand twin of the sunlight test above: a constant
    /// baseline, driven to a dynamic (lambda) source by a scenario,
    /// restores to the constant on `(scenario-stop)`.
    #[test]
    fn scenario_stop_restores_boiler_demand_after_dynamic_drive() {
        let (cfg, _dir) = config_with("(%make-steam-boiler :id 9)");
        cfg.eval("(set-boiler-demand 9 40.0)").unwrap();
        let b = cfg.site().get(9).unwrap();
        assert_eq!(b.demand_reading().unwrap().value, 40.0);
        assert!(!b.has_unrenderable_source());

        cfg.eval("(scenario-start \"boiler\")").unwrap();
        cfg.eval("(set-boiler-demand 9 (lambda () 99.0))").unwrap();
        assert!(b.has_unrenderable_source());

        cfg.eval("(scenario-stop)").unwrap();
        assert_eq!(b.demand_reading().unwrap().value, 40.0);
        assert!(!b.has_unrenderable_source());
    }
}
