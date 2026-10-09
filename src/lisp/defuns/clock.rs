//! `(set-timezone IANA-NAME)` — flip the configured display zone
//! at runtime.

use tulisp::TulispContext;

/// Register the `(set-timezone IANA-NAME)` defun. Validates the
/// argument via chrono-tz's `FromStr` impl — a typo surfaces as a
/// lisp error at config-load time rather than silently falling
/// through to UTC at format time. The UI's TZ toggle picks up the
/// new zone on its next /api/clock poll.
pub(in crate::lisp) fn register(ctx: &mut TulispContext, clock: crate::sim::clock::SharedClock) {
    ctx.defun(
        (
            "set-timezone",
            ["name"],
            "Set the time zone the UI shows times in to NAME.\n\n\
             NAME is an IANA zone name such as \"Europe/Berlin\", which is \
             also the default. The UI's zone chip switches between this zone \
             and UTC. gRPC and the simulation keep their times in UTC. When \
             an eval, such as one from the REPL, calls this at the top level, \
             the zone is saved in enterprise.lisp. Signal an error for an \
             unknown zone. Return NAME.",
        ),
        move |name: String| -> Result<String, tulisp::Error> {
            let tz: chrono_tz::Tz = name
                .parse()
                .map_err(|_| tulisp::Error::os_error(format!("unknown timezone: {name:?}")))?;
            clock.write().tz = tz;
            Ok(name)
        },
    );
}
