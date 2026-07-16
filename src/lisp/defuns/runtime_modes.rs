//! `(set-component-health)`, `(set-component-telemetry-mode)`,
//! `(set-component-command-mode)` — flip a component's runtime
//! enum at REPL / scenario time so fault simulation is scriptable.
//! Plus the site-wide stream knobs `(cancel-all-streams)` and
//! `(set-sample-lag-ms)`.

use tulisp::TulispContext;

use crate::sim::microgrids::SharedSiteRouter;

pub(super) fn register(ctx: &mut TulispContext, router: SharedSiteRouter) {
    use crate::sim::runtime::{CommandMode, Health, TelemetryMode};

    // All four setters below reject an unregistered id, matching
    // their siblings (set-meter-power, set-active-power, ...). The
    // site-level setters are permissive (entry().or_default()), so
    // without this check a typo'd id in a scenario silently
    // "succeeds" while the journal reports the fault as applied.
    let r = router.clone();
    ctx.defun(
        "set-component-health",
        move |id: i64, h: Health| -> Result<bool, tulisp::Error> {
            let w = r.site();
            if w.get(id as u64).is_none() {
                return Err(tulisp::Error::invalid_argument(format!(
                    "set-component-health: no component with id {id}"
                )));
            }
            w.set_health(id as u64, h);
            Ok(true)
        },
    );

    let r = router.clone();
    ctx.defun(
        "set-component-telemetry-mode",
        move |id: i64, m: TelemetryMode| -> Result<bool, tulisp::Error> {
            let w = r.site();
            if w.get(id as u64).is_none() {
                return Err(tulisp::Error::invalid_argument(format!(
                    "set-component-telemetry-mode: no component with id {id}"
                )));
            }
            w.set_telemetry_mode(id as u64, m)
                .map_err(tulisp::Error::invalid_argument)?;
            Ok(true)
        },
    );

    let r = router.clone();
    ctx.defun(
        "set-component-command-mode",
        move |id: i64, m: CommandMode| -> Result<bool, tulisp::Error> {
            let w = r.site();
            if w.get(id as u64).is_none() {
                return Err(tulisp::Error::invalid_argument(format!(
                    "set-component-command-mode: no component with id {id}"
                )));
            }
            w.set_command_mode(id as u64, m)
                .map_err(tulisp::Error::invalid_argument)?;
            Ok(true)
        },
    );

    // Unlike the runtime knobs above, the operational mode is a
    // CONFIG parameter — the declared capability of the component.
    // Setting it re-derives the runtime knobs (no telemetry means a
    // silent stream, no control means an erroring command channel)
    // and persists through the overrides gate.
    let r = router.clone();
    ctx.defun(
        "set-component-operational-mode",
        move |id: i64, m: crate::sim::component::OperationalMode| -> Result<bool, tulisp::Error> {
            let w = r.site();
            w.set_operational_mode(id as u64, m)
                .map_err(tulisp::Error::invalid_argument)?;
            Ok(true)
        },
    );

    let r = router.clone();
    ctx.defun("cancel-all-streams", move || -> bool {
        // Server-side graceful cancel of every active stream. Each
        // streaming task sees the epoch bump on its next iteration and
        // exits, sending the client an EOF/CANCELLED. Clients reconnect
        // and resume on fresh streams.
        r.site().cancel_all_streams();
        true
    });

    let r = router;
    ctx.defun("set-sample-lag-ms", move |ms: i64| -> bool {
        // Shift every outgoing telemetry sample's timestamp into the
        // past by MS milliseconds. Models a server that delivers
        // samples with a fixed timestamp lag, e.g. to test how a
        // downstream resampler copes with stale data.
        r.site().set_sample_lag_ms(ms.max(0) as u64);
        true
    });
}
