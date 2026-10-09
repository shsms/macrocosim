//! `(reset-microgrid)` — clear the active site's components.

use tulisp::{Error, TulispContext};

use crate::sim::microgrids::SharedSiteRouter;

pub(super) fn register(ctx: &mut TulispContext, router: SharedSiteRouter) {
    // Rust-side: clear the active MicrogridSite's components. The
    // Lisp-side `reset-state` (in sim/common.lisp) wraps this and
    // also cancels any outstanding tulisp-async timers so the next
    // config load doesn't double-fire `every` callbacks.
    ctx.defun(
        (
            "reset-microgrid",
            "Remove every component from the current microgrid.\n\n\
             This also clears its connections, histories, energy totals, \
             setpoint logs, commands, augmentations, scenario journal, CSV \
             recordings and weather. Telemetry streams to clients end. Grid \
             values such as frequency stay, and the id counter does not go back. \
             Timers keep running; reset-state cancels them, then calls this. \
             Return t.",
        ),
        move || -> Result<bool, Error> {
            router.site().reset();
            Ok(true)
        },
    );
}
