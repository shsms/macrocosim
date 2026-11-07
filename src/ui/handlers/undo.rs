//! Per-microgrid undo / redo endpoints under `/api/mg/{mg}/`.
//!
//! The history lives on the server. A microgrid with a stored spec
//! keeps the specs its `PUT`s replaced, and a step rebuilds it from one
//! with carry ([`crate::engine::topology::step_undo`]). A microgrid
//! loaded from Lisp keeps the generated blocks its file carried (see
//! [`crate::lisp`]'s undo). The UI keeps no stacks of its own — a
//! reload of the page, or a second browser tab, sees the same history.

use axum::Extension;
use axum::extract::State;

use crate::engine::{Engine, topology};
use crate::lisp::{Config, UndoDepths};
use crate::runtime::MicrogridRuntimes;
use crate::ui::api::{ApiError, Json, Mg};
use crate::undo::Direction;

/// Whether microgrid `id` has a stored spec, so its history is the
/// spec stacks.
fn has_spec(engine: &Engine, id: u64) -> bool {
    engine
        .microgrids()
        .lock()
        .get(&id)
        .is_some_and(|e| e.spec.is_some())
}

/// GET /api/mg/{mg}/undo — how deep each stack is, so the UI can
/// enable or grey out its buttons without trying a step first.
pub(in crate::ui) async fn undo_depths(
    State(engine): State<Engine>,
    State(config): State<Config>,
    mg: Mg,
) -> Json<serde_json::Value> {
    Json(depths_json(if has_spec(&engine, mg.id) {
        engine.spec_undo_depths(mg.id)
    } else {
        config.undo_depths(mg.id)
    }))
}

/// POST /api/mg/{mg}/undo — step one structural edit back.
pub(in crate::ui) async fn undo(
    State(engine): State<Engine>,
    State(config): State<Config>,
    Extension(runtimes): Extension<MicrogridRuntimes>,
    mg: Mg,
) -> Result<Json<serde_json::Value>, ApiError> {
    step(engine, config, runtimes, mg.id, Direction::Undo).await
}

/// POST /api/mg/{mg}/redo — step one structural edit forward.
pub(in crate::ui) async fn redo(
    State(engine): State<Engine>,
    State(config): State<Config>,
    Extension(runtimes): Extension<MicrogridRuntimes>,
    mg: Mg,
) -> Result<Json<serde_json::Value>, ApiError> {
    step(engine, config, runtimes, mg.id, Direction::Redo).await
}

/// Shared body. "Nothing to undo" is a request that cannot be
/// satisfied in the world's current state, not a server fault, and so
/// is an unmanaged Lisp microgrid: both are 409, which keeps the UI
/// from treating them as a crash.
async fn step(
    engine: Engine,
    config: Config,
    runtimes: MicrogridRuntimes,
    mg_id: u64,
    dir: Direction,
) -> Result<Json<serde_json::Value>, ApiError> {
    if has_spec(&engine, mg_id) {
        let create_lock = engine.create_lock();
        let _structural = create_lock.lock().await;
        let depths = topology::step_undo(&engine, &runtimes, mg_id, dir).await?;
        return Ok(Json(depths_json(depths)));
    }
    // A Lisp step rewrites a file and reloads it, which is blocking
    // work behind the interpreter lock.
    let depths = super::blocking(move || match dir {
        Direction::Undo => config.undo(mg_id),
        Direction::Redo => config.redo(mg_id),
    })
    .await?
    .map_err(ApiError::conflict)?;
    Ok(Json(depths_json(depths)))
}

fn depths_json(depths: UndoDepths) -> serde_json::Value {
    serde_json::json!({
        "undo_depth": depths.undo,
        "redo_depth": depths.redo,
    })
}
