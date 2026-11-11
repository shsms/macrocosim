//! The router's state: the [`Engine`] every handler reads the simulation
//! through, the microgrid runtimes and their loopback slots, and the `.py`
//! export's interpreter. Each handler extracts the part it uses, such as
//! `State<Engine>`.

use std::sync::Arc;

use axum::extract::FromRef;

use crate::engine::Engine;
use crate::python::Interpreter;
use crate::runtime::MicrogridRuntimes;
use crate::ui::MicrogridLoopbacks;

#[derive(Clone)]
pub(in crate::ui) struct AppState {
    pub engine: Engine,
    pub runtimes: MicrogridRuntimes,
    pub export: Export,
}

/// What the `.py` export runs on: the interpreter, and the one permit an export
/// holds while it runs.
#[derive(Clone)]
pub(in crate::ui) struct Export {
    pub python: Arc<Interpreter>,
    pub permits: Arc<tokio::sync::Semaphore>,
}

impl AppState {
    /// The state for `engine`, its `runtimes` and the interpreter `python`.
    pub fn new(engine: Engine, runtimes: MicrogridRuntimes, python: Arc<Interpreter>) -> Self {
        Self {
            engine,
            runtimes,
            export: Export {
                python,
                permits: Arc::new(tokio::sync::Semaphore::new(1)),
            },
        }
    }
}

impl FromRef<AppState> for Engine {
    fn from_ref(state: &AppState) -> Self {
        state.engine.clone()
    }
}

impl FromRef<AppState> for MicrogridRuntimes {
    fn from_ref(state: &AppState) -> Self {
        state.runtimes.clone()
    }
}

impl FromRef<AppState> for MicrogridLoopbacks {
    fn from_ref(state: &AppState) -> Self {
        state.runtimes.loopbacks()
    }
}

impl FromRef<AppState> for Export {
    fn from_ref(state: &AppState) -> Self {
        state.export.clone()
    }
}
