//! The router's state: the [`Engine`] every handler reads the
//! simulation through, the microgrid runtimes and their loopback
//! slots, the `.py` export's interpreter, and the script's
//! [`ScriptSupervisor`]. Each handler extracts the part it uses, such
//! as `State<Engine>` or `State<Runner>`.

use std::sync::Arc;

use axum::extract::FromRef;

use crate::engine::Engine;
use crate::runner::ScriptSupervisor;
use crate::runner::python::Interpreter;
use crate::runtime::MicrogridRuntimes;
use crate::ui::MicrogridLoopbacks;

#[derive(Clone)]
pub(in crate::ui) struct AppState {
    pub engine: Engine,
    pub runtimes: MicrogridRuntimes,
    pub runner: Runner,
    pub export: Export,
}

/// The supervisor of the engine's script; `None` without a script.
#[derive(Clone)]
pub(in crate::ui) struct Runner(pub Option<Arc<ScriptSupervisor>>);

/// What the `.py` export runs on: the interpreter, shared with the
/// script runner, and the one permit an export holds while it runs.
#[derive(Clone)]
pub(in crate::ui) struct Export {
    pub python: Arc<Interpreter>,
    pub permits: Arc<tokio::sync::Semaphore>,
}

impl AppState {
    /// The state for `engine`, its `runtimes`, the interpreter
    /// `python` and the script's supervisor.
    pub fn new(
        engine: Engine,
        runtimes: MicrogridRuntimes,
        python: Arc<Interpreter>,
        runner: Option<Arc<ScriptSupervisor>>,
    ) -> Self {
        Self {
            engine,
            runtimes,
            runner: Runner(runner),
            export: Export {
                python,
                permits: Arc::new(tokio::sync::Semaphore::new(1)),
            },
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
