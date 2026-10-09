//! `(set-component-health)`, `(set-component-telemetry-mode)`,
//! `(set-component-command-mode)` — flip a component's runtime
//! enum at REPL / scenario time so fault simulation is scriptable.
//! Plus the site-wide stream knobs `(cancel-all-streams)` and
//! `(set-sample-lag-s)`, and its `-ms` name that still works.

use tulisp::TulispContext;

use crate::sim::microgrids::SharedSiteRouter;

pub(super) fn register(ctx: &mut TulispContext, router: SharedSiteRouter) {
    use crate::sim::runtime::{CommandMode, Health, TelemetryMode};

    // All four setters below reject an unregistered id, matching
    // their siblings (set-meter-power, set-active-power, ...). The
    // site-level setters do that rejection; each defun only prefixes
    // its own name onto their message.
    let r = router.clone();
    ctx.defun(
        (
            "set-component-health",
            ["id", "health"],
            "Set the health of component ID to HEALTH.\n\n\
             HEALTH is one of these symbols:\n  \
             ok  the component works normally\n  \
             error  the component reports an error state\n  \
             standby  the component reports a standby state\n\n\
             While HEALTH is not ok, gRPC refuses setpoint and bounds \
             requests for the component. If the component takes setpoints, \
             the gateway also holds its power at 0 and drops its setpoint. A \
             solar inverter keeps its active-power setpoint, and so does an \
             EV charger with :resume-on-recovery.\n\n\
             Setting error also sets the command mode to error. Setting ok \
             sets the command mode back to normal, or to error when the \
             operational mode accepts no commands; this replaces a timeout \
             or over-bound command mode. Setting standby leaves the command \
             mode as it is.\n\n\
             Return t. Signal an error if ID is not a component of the \
             current microgrid.",
        ),
        move |id: i64, h: Health| -> Result<bool, tulisp::Error> {
            let w = r.site();
            w.set_health(id as u64, h).map_err(|e| {
                tulisp::Error::invalid_argument(format!("set-component-health: {e}"))
            })?;
            Ok(true)
        },
    );

    let r = router.clone();
    ctx.defun(
        (
            "set-component-telemetry-mode",
            ["id", "mode"],
            "Set how component ID sends its gRPC telemetry stream.\n\n\
             MODE is one of these symbols:\n  \
             normal  send samples at the stream interval\n  \
             silent  keep the stream open, but send no samples\n  \
             closed  end open streams; new streams end at once\n  \
             error-empty  send samples with no metrics and an error state\n  \
             not-found  end open streams and refuse new ones with NOT_FOUND\n\n\
             Return t. Signal an error if ID is not a component of the \
             current microgrid, or if MODE is normal and the operational \
             mode of the component streams no telemetry.",
        ),
        move |id: i64, m: TelemetryMode| -> Result<bool, tulisp::Error> {
            let w = r.site();
            w.set_telemetry_mode(id as u64, m).map_err(|e| {
                tulisp::Error::invalid_argument(format!("set-component-telemetry-mode: {e}"))
            })?;
            Ok(true)
        },
    );

    let r = router.clone();
    ctx.defun(
        (
            "set-component-command-mode",
            ["id", "mode"],
            "Set how component ID answers gRPC setpoint and bounds requests.\n\n\
             MODE is one of these symbols:\n  \
             normal  check each request and apply it\n  \
             timeout  never answer, so the client times out\n  \
             error  answer at once with UNAVAILABLE\n  \
             over-bound  at times refuse a non-zero setpoint inside the \
             bounds, with INVALID_ARGUMENT\n\n\
             In over-bound mode, each component refuses for about one second \
             in every minute, at a second picked from its id. A setpoint \
             of 0 is still accepted. The mode \
             does not change set-active-power and the other Lisp \
             commands.\n\n\
             Return t. Signal an error if ID is not a component of the \
             current microgrid. Also signal an error if MODE is normal and \
             the operational mode accepts no commands, or the health of the \
             component is error.",
        ),
        move |id: i64, m: CommandMode| -> Result<bool, tulisp::Error> {
            let w = r.site();
            w.set_command_mode(id as u64, m).map_err(|e| {
                tulisp::Error::invalid_argument(format!("set-component-command-mode: {e}"))
            })?;
            Ok(true)
        },
    );

    // Unlike the runtime knobs above, the operational mode is a
    // CONFIG parameter — the declared capability of the component.
    // Setting it re-derives the runtime knobs (no telemetry means a
    // silent stream, no control means an erroring command channel)
    // and, being structural, is written back into the microgrid's
    // managed file (`:operational-mode` in the generated block).
    let r = router.clone();
    ctx.defun(
        (
            "set-component-operational-mode",
            ["id", "mode"],
            "Declare whether component ID streams telemetry and accepts commands.\n\n\
             MODE is one of these symbols:\n  \
             unspecified  not declared; works like control-and-telemetry\n  \
             inactive  no telemetry and no commands\n  \
             telemetry-only  telemetry, but no commands\n  \
             control-only  commands, but no telemetry\n  \
             control-and-telemetry  telemetry and commands\n\n\
             The operational mode is part of the configuration, so a \
             managed microgrid file saves it. It also sets the runtime \
             modes. The telemetry mode becomes normal, or silent when MODE \
             has no telemetry. The command mode becomes normal, or error \
             when MODE has no commands or the health is error. This \
             replaces a fault mode you set before.\n\n\
             Return t. Signal an error if ID is not a component of the \
             current microgrid.",
        ),
        move |id: i64, m: crate::sim::component::OperationalMode| -> Result<bool, tulisp::Error> {
            let w = r.site();
            w.set_operational_mode(id as u64, m)
                .map_err(tulisp::Error::invalid_argument)?;
            Ok(true)
        },
    );

    let r = router.clone();
    ctx.defun(
        (
            "cancel-all-streams",
            "End every open gRPC telemetry stream of the current microgrid.\n\n\
             Each stream ends at its next sample time, about one stream \
             interval later, with the gRPC status CANCELLED. Clients can \
             connect again and get new streams. Return t.",
        ),
        move || -> bool {
            // Server-side graceful cancel of every active stream. Each
            // streaming task sees the epoch bump on its next iteration and
            // exits, sending the client an EOF/CANCELLED. Clients reconnect
            // and resume on fresh streams.
            r.site().cancel_all_streams();
            true
        },
    );

    let r2 = router.clone();
    ctx.defun(
        (
            "set-sample-lag-s",
            ["lag-s"],
            "Shift telemetry sample timestamps LAG-S seconds into the past.\n\n\
             This shifts the samples of every gRPC telemetry stream of the \
             current microgrid. Use it to test how a client copes with late \
             data. 0 turns the lag off. LAG-S is rounded down to whole \
             milliseconds.\n\n\
             Return t. Signal an error if LAG-S is negative or not finite.",
        ),
        move |secs: f64| -> Result<bool, tulisp::Error> {
            // Shift every outgoing telemetry sample's timestamp into
            // the past by SECS seconds. Models a server that delivers
            // samples with a fixed timestamp lag, e.g. to test how a
            // downstream resampler copes with stale data.
            let d = crate::lisp::secs_duration("set-sample-lag-s", secs)?;
            r2.site().set_sample_lag_ms(d.as_millis() as u64);
            Ok(true)
        },
    );

    let r = router;
    ctx.defun(
        (
            "set-sample-lag-ms",
            ["lag-ms"],
            "Deprecated: use set-sample-lag-s, which takes seconds.\n\n\
             Shift telemetry sample timestamps LAG-MS milliseconds into the \
             past. A negative LAG-MS counts as 0. Return t.",
        ),
        move |ms: i64| -> bool {
            super::super::renames::warn_renamed(
                "set-sample-lag-ms",
                "set-sample-lag-s",
                " (seconds)",
            );
            r.site().set_sample_lag_ms(ms.max(0) as u64);
            true
        },
    );
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::config_with;

    /// `set-sample-lag-s` takes seconds and refuses a negative value;
    /// the `-ms` name still takes milliseconds.
    #[test]
    fn sample_lag_takes_seconds() {
        let (cfg, _dir) = config_with("");
        cfg.eval("(set-sample-lag-s 0.05)").unwrap();
        assert_eq!(cfg.site().sample_lag_ms(), 50);
        let err = cfg.eval("(set-sample-lag-s -1)").unwrap_err();
        assert!(err.contains("set-sample-lag-s"), "{err}");
        assert_eq!(cfg.site().sample_lag_ms(), 50);
        cfg.eval("(set-sample-lag-ms 20)").unwrap();
        assert_eq!(cfg.site().sample_lag_ms(), 20);
    }
}
