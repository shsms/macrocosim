//! A microgrid's stored spec: read it, replace it (a rebuild with
//! carry), unload the microgrid, and the JSON Schema of the topology
//! document.

use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;

use crate::engine::Engine;
use crate::engine::topology::{self, TopologyError};
use crate::runtime::MicrogridRuntimes;
use crate::spec::MicrogridSpec;
use crate::ui::api::{ApiError, Json, Mg};

impl From<TopologyError> for ApiError {
    fn from(e: TopologyError) -> Self {
        match e {
            TopologyError::Invalid(msg) => ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, msg),
            TopologyError::Conflict(msg) => ApiError::conflict(msg),
            TopologyError::NotRegistered(id) => ApiError::not_registered(id),
        }
    }
}

/// GET /api/mg/{mg}/spec — the stored spec, with every id filled in.
/// 409 for a microgrid loaded from Lisp, which has none.
pub(in crate::ui) async fn spec_get(
    State(engine): State<Engine>,
    mg: Mg,
) -> Result<Json<MicrogridSpec>, ApiError> {
    let registry = engine.microgrids();
    let registry = registry.lock();
    let entry = registry
        .get(&mg.id)
        .ok_or_else(|| ApiError::not_registered(mg.id))?;
    entry
        .spec
        .clone()
        .map(Json)
        .ok_or_else(|| topology::loaded_from_lisp(mg.id).into())
}

/// PUT /api/mg/{mg}/spec — rebuild the microgrid from the body,
/// carrying the state of every component whose spec did not change.
/// The spec it replaced goes on the microgrid's undo stack, and a
/// microgrid that came from a file is marked unsaved. A new
/// `grpc_port` moves the gRPC server. Answers the stored spec.
pub(in crate::ui) async fn spec_put(
    State(engine): State<Engine>,
    Extension(runtimes): Extension<MicrogridRuntimes>,
    mg: Mg,
    Json(spec): Json<MicrogridSpec>,
) -> Result<Json<MicrogridSpec>, ApiError> {
    let create_lock = engine.create_lock();
    let _structural = create_lock.lock().await;
    let replaced = topology::rebuild(&engine, &runtimes, mg.id, spec).await?;
    engine.record_spec_undo(mg.id, replaced);
    let registry = engine.microgrids();
    let mut registry = registry.lock();
    let entry = registry
        .get_mut(&mg.id)
        .ok_or_else(|| ApiError::not_registered(mg.id))?;
    if entry.source.is_some() {
        entry.unsaved = true;
    }
    let stored = entry.spec.clone().expect("a rebuilt microgrid has a spec");
    Ok(Json(stored))
}

/// DELETE /api/mg/{mg} — unload the microgrid: stop its runtime,
/// delete its dispatches and forget it. 204 once it is gone; 409 for a
/// microgrid loaded from Lisp.
pub(in crate::ui) async fn microgrid_delete(
    State(engine): State<Engine>,
    Extension(runtimes): Extension<MicrogridRuntimes>,
    mg: Mg,
) -> Result<StatusCode, ApiError> {
    let create_lock = engine.create_lock();
    let _structural = create_lock.lock().await;
    topology::unload(&engine, &runtimes, mg.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/spec/schema — the JSON Schema of the topology document.
pub(in crate::ui) async fn spec_schema() -> Json<serde_json::Value> {
    Json(crate::spec::schema::topology_schema())
}
