//! Per-microgrid undo / redo endpoints under `/api/mg/{mg}/`.
//!
//! The history lives on the server: a microgrid keeps the specs its
//! `PUT`s replaced, and a step rebuilds it from one with carry
//! ([`crate::engine::topology::step_history`]). The UI keeps no
//! stacks of its own — a reload of the page, or a second browser tab,
//! sees the same history.

use axum::extract::State;

use crate::engine::{Engine, topology};
use crate::runtime::MicrogridRuntimes;
use crate::ui::api::{ApiError, Json, Mg};
use crate::undo::{Direction, UndoDepths};

/// GET /api/mg/{mg}/undo — how deep each stack is, so the UI can
/// enable or grey out its buttons without trying a step first.
pub(in crate::ui) async fn undo_depths(
    State(engine): State<Engine>,
    mg: Mg,
) -> Json<serde_json::Value> {
    Json(depths_json(engine.spec_undo_depths(mg.id)))
}

/// POST /api/mg/{mg}/undo — step one structural edit back.
pub(in crate::ui) async fn undo(
    State(engine): State<Engine>,
    State(runtimes): State<MicrogridRuntimes>,
    mg: Mg,
) -> Result<Json<serde_json::Value>, ApiError> {
    step(engine, runtimes, mg.id, Direction::Undo).await
}

/// POST /api/mg/{mg}/redo — step one structural edit forward.
pub(in crate::ui) async fn redo(
    State(engine): State<Engine>,
    State(runtimes): State<MicrogridRuntimes>,
    mg: Mg,
) -> Result<Json<serde_json::Value>, ApiError> {
    step(engine, runtimes, mg.id, Direction::Redo).await
}

/// Undo or redo one step of `mg_id`'s spec history, answering the
/// stacks' depths. "Nothing to undo" is a request that cannot be
/// satisfied in the world's current state, not a server fault: 409,
/// which keeps the UI from treating it as a crash.
async fn step(
    engine: Engine,
    runtimes: MicrogridRuntimes,
    mg_id: u64,
    dir: Direction,
) -> Result<Json<serde_json::Value>, ApiError> {
    super::structural_change(engine, move |structural| async move {
        let depths = topology::step_history(&structural, &runtimes, mg_id, dir).await?;
        Ok(Json(depths_json(depths)))
    })
    .await
}

fn depths_json(depths: UndoDepths) -> serde_json::Value {
    serde_json::json!({
        "undo_depth": depths.undo,
        "redo_depth": depths.redo,
    })
}
