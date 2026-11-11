//! HTTP handler glue. Each topic lives in its own submodule.
//! `register` in `super::router` wires every route through one of
//! the `pub(in crate::ui)` fns exported here.

use super::api::ApiError;
use crate::engine::{Engine, Structural};

pub(in crate::ui) mod assets;
pub(in crate::ui) mod component;
pub(in crate::ui) mod control;
pub(in crate::ui) mod defaults;
pub(in crate::ui) mod dispatches;
pub(in crate::ui) mod ev;
pub(in crate::ui) mod formula;
pub(in crate::ui) mod frequency;
pub(in crate::ui) mod grid;
pub(in crate::ui) mod history;
pub(in crate::ui) mod microgrid_data;
pub(in crate::ui) mod microgrids;
pub(in crate::ui) mod recording;
pub(in crate::ui) mod spec;
pub(in crate::ui) mod topology;
pub(in crate::ui) mod undo;
pub(in crate::ui) mod weather;

/// Run `f` on the blocking pool, mapping a task panic to a 500 — the
/// spawn_blocking boilerplate every handler with heavy synchronous
/// work otherwise repeats. Callers keep only their domain error
/// mapping.
pub(in crate::ui) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    blocking_with("task", f).await
}

/// Run `fut` in its own task and wait for it: a client that hangs up
/// drops the request, and with it any future the handler awaits, which
/// would leave a change half applied.
pub(in crate::ui) async fn detached<T, F>(fut: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, ApiError>> + Send + 'static,
{
    tokio::spawn(fut)
        .await
        .map_err(|e| ApiError::internal(format!("the change panicked: {e}")))?
}

/// Run `change`, a structural change, [`detached`], with `engine`'s
/// structural lock taken inside the task: the next change waits for
/// this one to finish, even if the client has gone.
pub(in crate::ui) async fn structural_change<T, F, Fut>(
    engine: Engine,
    change: F,
) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(Structural) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<T, ApiError>> + Send + 'static,
{
    detached(async move { change(engine.structural().await).await }).await
}

/// [`blocking`] with `label` naming the task in the 500's text:
/// `{label} panicked: …`.
pub(in crate::ui) async fn blocking_with<T: Send + 'static>(
    label: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::internal(format!("{label} panicked: {e}")))
}
