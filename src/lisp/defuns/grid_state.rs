//! Grid-state knobs: per-phase voltage and the per-microgrid
//! physics tick cadence.

use tulisp::{Error, TulispContext};

use crate::sim::microgrids::SharedSiteRouter;

use super::super::renames::warn_renamed;

pub(super) fn register(ctx: &mut TulispContext, router: SharedSiteRouter) {
    let r = router.clone();
    ctx.defun(
        (
            "set-voltage-per-phase",
            ["l1-v", "l2-v", "l3-v"],
            "Set the grid voltage of each phase of the current microgrid, in V.\n\n\
             L1-V, L2-V and L3-V are the voltages of phases 1, 2 and 3. \
             Components report these voltages in their telemetry. Meters \
             and inverters also use them to split power and current across \
             the phases. The default is 230 V on each phase. Return t.",
        ),
        move |p1: f64, p2: f64, p3: f64| -> Result<bool, Error> {
            let w = r.site();
            let mut state = w.grid_state();
            state.voltage_per_phase = (p1 as f32, p2 as f32, p3 as f32);
            w.set_grid_state(state);
            Ok(true)
        },
    );

    let r = router.clone();
    ctx.defun(
        (
            "set-physics-tick-s",
            ["tick-s"],
            "Set the time between physics steps of the current microgrid.\n\n\
             TICK-S is in seconds; the default is 0.1. TICK-S is rounded down \
             to whole milliseconds, with a minimum of 1 ms. A running \
             physics loop uses the new value after its next step.\n\n\
             Return t. Signal an error if TICK-S is negative or not finite.",
        ),
        move |secs: f64| -> Result<bool, Error> {
            let d = crate::lisp::secs_duration("set-physics-tick-s", secs)?;
            r.site().set_physics_tick_ms((d.as_millis() as u64).max(1));
            Ok(true)
        },
    );

    let r = router;
    ctx.defun(
        (
            "set-physics-tick-ms",
            ["tick-ms"],
            "Deprecated: use set-physics-tick-s, which takes seconds.\n\n\
             Set the time between physics steps of the current microgrid to \
             TICK-MS milliseconds. A TICK-MS below 1 counts as 1. Return t.",
        ),
        move |ms: i64| -> Result<bool, Error> {
            warn_renamed("set-physics-tick-ms", "set-physics-tick-s", " (seconds)");
            r.site().set_physics_tick_ms(ms.max(1) as u64);
            Ok(true)
        },
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::super::test_support::config_with;

    /// `set-physics-tick-s` takes seconds and refuses a negative
    /// value; the `-ms` name still takes milliseconds.
    #[test]
    fn physics_tick_takes_seconds() {
        let (cfg, _dir) = config_with("");
        cfg.eval("(set-physics-tick-s 0.05)").unwrap();
        assert_eq!(cfg.site().physics_tick(), Duration::from_millis(50));
        let err = cfg.eval("(set-physics-tick-s -1)").unwrap_err();
        assert!(err.contains("set-physics-tick-s"), "{err}");
        assert_eq!(cfg.site().physics_tick(), Duration::from_millis(50));
        cfg.eval("(set-physics-tick-ms 20)").unwrap();
        assert_eq!(cfg.site().physics_tick(), Duration::from_millis(20));
    }
}
