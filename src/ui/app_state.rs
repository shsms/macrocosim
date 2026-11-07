//! The router's state: the [`Engine`] every handler reads the
//! simulation through, and the Lisp [`Config`] the handlers that
//! evaluate Lisp still need. Each handler extracts the half it uses
//! with `State<Engine>` or `State<Config>`.

use axum::extract::FromRef;

use crate::engine::Engine;
use crate::lisp::Config;

#[derive(Clone)]
pub(in crate::ui) struct AppState {
    pub engine: Engine,
    pub config: Config,
}

impl AppState {
    /// The state for `config` and the engine it drives.
    pub fn new(config: Config) -> Self {
        Self {
            engine: config.engine().clone(),
            config,
        }
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
