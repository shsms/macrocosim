//! Grid-frequency defuns: read the live value, write a one-shot
//! override, or retune the OU process driving the shared frequency
//! state.

use tulisp::{Error, TulispContext};

use crate::lisp::renames::Renamed;

tulisp::AsList! {
    /// Plist payload for `(set-frequency-model …)`. Every field
    /// optional — only the keys the caller passes are touched.
    pub struct FrequencyModelArgs {
        /// Mean the OU process pulls toward (Hz).
        nominal_hz<":nominal-hz">: Option<f64>,
        /// Mean reversion rate (1/s). Correlation time of the
        /// noisy fluctuations is roughly `1 / mean-rev-rate`.
        mean_rev_rate<":mean-rev-rate">: Option<f64>,
        /// Noise intensity (Hz/sqrt(s)). Equilibrium standard
        /// deviation is `sigma / sqrt(2 * mean-rev-rate)`.
        sigma: Option<f64>,
    }
}

/// Defuns the config + scenarios use to drive the shared grid
/// frequency:
///
/// - `(set-frequency F)` — one-shot write of the current value.
///   The OU driver overwrites on the next step (every 200 ms), so
///   this is useful for test fixtures or for setting an initial
///   condition the OU then evolves away from.
/// - `(set-frequency-model :nominal-hz :mean-rev-rate :sigma)` —
///   tune the *base* driver parameters. Each key optional;
///   unspecified keys keep their current base values. Defaults
///   pick a noise floor (~47 mHz std dev) and correlation time
///   (~20 s) that look like a healthy synchronous grid.
/// - `(override-frequency-model :nominal-hz :mean-rev-rate :sigma)`
///   — install an override on the OU dynamics. Driver keeps
///   integrating, but uses the override's params in place of the
///   base while it's set. Unspecified keys inherit from the
///   current active model (override if already set, else base) —
///   so `(override-frequency-model :nominal-hz 49.5)` pulls toward
///   49.5 with the base dynamics, and a later
///   `(override-frequency-model :sigma 0.05)` widens noise
///   without disturbing the override nominal.
/// - `(clear-frequency-override)` — drop the override; the
///   driver returns to base dynamics from the current value.
/// - `(current-frequency)` — read the live value.
pub(in crate::lisp) fn register(
    ctx: &mut TulispContext,
    state: crate::sim::frequency::SharedFrequency,
) {
    use crate::sim::frequency::FrequencyModel;
    fn apply_overrides(model: &mut FrequencyModel, a: &FrequencyModelArgs) {
        if let Some(v) = a.nominal_hz {
            model.nominal_hz = v as f32;
        }
        if let Some(v) = a.mean_rev_rate {
            model.mean_rev_rate = v.max(0.0) as f32;
        }
        if let Some(v) = a.sigma {
            model.sigma = v.max(0.0) as f32;
        }
    }

    let s = state.clone();
    ctx.defun(
        (
            "set-frequency",
            ["hz"],
            "Set the grid frequency to HZ now.\n\n\
             One grid frequency is shared by every microgrid. In a run on \
             the wall clock, the frequency model takes a step every 200 ms, \
             so the frequency moves away from HZ again. In a headless run \
             the model does not step, so the frequency stays at HZ. Use this \
             for a start value or in tests.\n\n\
             Return t. Signal an error if HZ is not finite.",
        ),
        move |hz: f64| -> Result<bool, Error> {
            // A NaN/Inf here would poison the shared OU state permanently
            // (`step()` integrates `current_hz += …`, so non-finite never
            // recovers) — and the state is shared by EVERY microgrid.
            if !hz.is_finite() {
                return Err(Error::invalid_argument(format!(
                    "set-frequency: hz must be finite, got {hz}"
                )));
            }
            s.write().current_hz = hz as f32;
            Ok(true)
        },
    );

    let s = state.clone();
    ctx.defun(
        (
            "set-frequency-model",
            ["args"],
            "Change the base model that drives the shared grid frequency.\n\n\
             The model pulls the frequency toward a nominal value and adds \
             random noise. One model is shared by every microgrid. Only \
             the keys you pass change; the others keep their values. While \
             an override from override-frequency-model is set, the \
             override drives the frequency instead.\n\n\
             Keys:\n  \
             :nominal-hz  the value the frequency is pulled toward \
             (starts at 50)\n  \
             :mean-rev-rate  how fast it is pulled back, in 1/s \
             (starts at 0.05)\n  \
             :sigma  the size of the noise, in Hz/sqrt(s) \
             (starts at 0.015)\n\n\
             A negative :mean-rev-rate or :sigma counts as 0. The \
             deprecated key :nominal is read as :nominal-hz. Return t.",
        ),
        move |args: tulisp::Plist<Renamed<FrequencyModelArgs>>| -> Result<bool, Error> {
            let a = args.into_inner().0;
            apply_overrides(&mut s.write().base, &a);
            Ok(true)
        },
    );

    let s = state.clone();
    ctx.defun(
        (
            "override-frequency-model",
            ["args"],
            "Drive the grid frequency with another model until it is cleared.\n\n\
             The frequency keeps its current value and moves on under the \
             new model. A key you leave out takes its value from the model \
             in use: the override if one is set, else the base model. So a \
             second call changes only the keys it passes. \
             clear-frequency-override goes back to the base model.\n\n\
             Keys:\n  \
             :nominal-hz  the value the frequency is pulled toward\n  \
             :mean-rev-rate  how fast it is pulled back, in 1/s\n  \
             :sigma  the size of the noise, in Hz/sqrt(s)\n\n\
             A negative :mean-rev-rate or :sigma counts as 0. The \
             deprecated key :nominal is read as :nominal-hz. Return t.",
        ),
        move |args: tulisp::Plist<Renamed<FrequencyModelArgs>>| -> Result<bool, Error> {
            let a = args.into_inner().0;
            let mut g = s.write();
            // Missing keys inherit from the currently-active model:
            // the existing override if there is one (so repeated
            // calls layer), else the base (so the first call after a
            // clear picks up sensible defaults).
            let mut next = g.active_model();
            apply_overrides(&mut next, &a);
            g.override_model = Some(next);
            Ok(true)
        },
    );

    let s = state.clone();
    ctx.defun(
        (
            "clear-frequency-override",
            "Remove the frequency override and use the base model again.\n\n\
             The frequency keeps its current value and moves on under the \
             base model. Return t.",
        ),
        move || -> Result<bool, Error> {
            s.write().override_model = None;
            Ok(true)
        },
    );

    let s = state;
    ctx.defun(
        (
            "current-frequency",
            "Return the current grid frequency in Hz.\n\n\
             One grid frequency is shared by every microgrid.",
        ),
        move || -> Result<f64, Error> { Ok(s.read().read_hz() as f64) },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::frequency::{SharedFrequency, new_shared};

    fn setup() -> (TulispContext, SharedFrequency) {
        let mut ctx = TulispContext::new();
        let state = new_shared();
        register(&mut ctx, state.clone());
        (ctx, state)
    }

    #[test]
    fn the_frequency_model_takes_nominal_hz() {
        let (mut ctx, state) = setup();
        ctx.eval_string("(set-frequency-model :nominal-hz 49.0)")
            .unwrap();
        assert_eq!(state.read().base.nominal_hz, 49.0);
        ctx.eval_string("(override-frequency-model :nominal-hz 50.5)")
            .unwrap();
        assert_eq!(state.read().override_model.unwrap().nominal_hz, 50.5);
    }

    #[test]
    fn the_old_nominal_keyword_is_equivalent() {
        let (mut ctx, state) = setup();
        ctx.eval_string("(set-frequency-model :nominal 49.5)")
            .unwrap();
        assert_eq!(state.read().base.nominal_hz, 49.5);
        ctx.eval_string("(override-frequency-model :nominal 50.5)")
            .unwrap();
        assert_eq!(state.read().override_model.unwrap().nominal_hz, 50.5);
    }
}
