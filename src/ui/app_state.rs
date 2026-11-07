//! The router's state: the [`Engine`] every handler reads the
//! simulation through, the Lisp [`Config`] the handlers that evaluate
//! Lisp still need, and the script's [`ScriptSupervisor`]. Each handler
//! extracts the part it uses with `State<Engine>`, `State<Config>` or
//! `State<Runner>`.

use std::sync::Arc;

use axum::extract::FromRef;

use crate::engine::Engine;
use crate::lisp::Config;
use crate::runner::ScriptSupervisor;

#[derive(Clone)]
pub(in crate::ui) struct AppState {
    pub engine: Engine,
    pub config: Config,
    pub runner: Runner,
}

/// The supervisor of the engine's script; `None` without a script.
#[derive(Clone)]
pub(in crate::ui) struct Runner(pub Option<Arc<ScriptSupervisor>>);

impl AppState {
    /// The state for `config`, the engine it drives, and the script's
    /// supervisor.
    pub fn new(config: Config, runner: Option<Arc<ScriptSupervisor>>) -> Self {
        Self {
            engine: config.engine().clone(),
            config,
            runner: Runner(runner),
        }
    }
}

impl FromRef<AppState> for Runner {
    fn from_ref(state: &AppState) -> Self {
        state.runner.clone()
    }
}

impl FromRef<AppState> for Engine {
    fn from_ref(state: &AppState) -> Self {
        state.engine.clone()
    }
}

impl FromRef<AppState> for Config {
    fn from_ref(state: &AppState) -> Self {
        state.config.clone()
    }
}
