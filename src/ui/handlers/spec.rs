//! A microgrid's stored spec: read it, export it as Python, replace
//! it (a rebuild with carry), unload the microgrid, and the JSON
//! Schema of the topology document.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;

use crate::engine::Engine;
use crate::engine::topology;
use crate::runtime::MicrogridRuntimes;
use crate::spec::{MicrogridSpec, SCHEMA_VERSION, TopologyDoc};
use crate::ui::api::{ApiError, Json, Mg};
use crate::ui::app_state::Export;

/// Microgrid `id`'s stored spec; 404 when it is not registered.
fn stored_spec(engine: &Engine, id: u64) -> Result<MicrogridSpec, ApiError> {
    engine
        .microgrids()
        .lock()
        .get(&id)
        .map(|entry| entry.spec.clone())
        .ok_or_else(|| ApiError::not_registered(id))
}

/// GET /api/mg/{mg}/spec — the stored spec, with every id filled in.
pub(in crate::ui) async fn spec_get(
    State(engine): State<Engine>,
    mg: Mg,
) -> Result<Json<MicrogridSpec>, ApiError> {
    stored_spec(&engine, mg.id).map(Json)
}

/// GET /api/mg/{mg}/spec.py — the stored spec as a Python module, as `python -m
/// macrocosim.export` writes it: the Python package owns its own syntax. 503
/// when no usable interpreter is installed, 500 when the export fails or takes
/// longer than [`crate::python::WRITE_MODULE_TIMEOUT`], and 429 while another
/// export runs.
pub(in crate::ui) async fn spec_py(
    State(engine): State<Engine>,
    State(export): State<Export>,
    mg: Mg,
) -> Result<impl IntoResponse, ApiError> {
    let Ok(_permit) = Arc::clone(&export.permits).try_acquire_owned() else {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "an export is running; try again when it is done",
        ));
    };
    let spec = stored_spec(&engine, mg.id)?;
    let python = export
        .python
        .resolve()
        .await
        .map_err(|m| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, m))?;
    let doc = TopologyDoc {
        schema_version: SCHEMA_VERSION,
        microgrids: vec![spec],
    };
    let text = crate::python::write_module(&python, &doc)
        .await
        .map_err(|what| ApiError::internal(format!("the export failed: {what}")))?;
    Ok((
        [(header::CONTENT_TYPE, "text/x-python; charset=utf-8")],
        text,
    ))
}

/// PUT /api/mg/{mg}/spec — rebuild the microgrid from the body,
/// carrying the state of every component whose spec did not change.
/// The spec it replaced goes on the microgrid's undo stack, and a
/// microgrid that came from a file is marked unsaved. A new
/// `grpc_port` moves the gRPC server. Answers the stored spec.
pub(in crate::ui) async fn spec_put(
    State(engine): State<Engine>,
    State(runtimes): State<MicrogridRuntimes>,
    mg: Mg,
    Json(spec): Json<MicrogridSpec>,
) -> Result<Json<MicrogridSpec>, ApiError> {
    super::structural_change(engine, move |structural| async move {
        topology::edit(&structural, &runtimes, mg.id, spec).await?;
        stored_spec(structural.engine(), mg.id).map(Json)
    })
    .await
}

/// DELETE /api/mg/{mg} — unload the microgrid: stop its runtime,
/// delete its dispatches and forget it. 204 once it is gone.
pub(in crate::ui) async fn microgrid_delete(
    State(engine): State<Engine>,
    State(runtimes): State<MicrogridRuntimes>,
    mg: Mg,
) -> Result<StatusCode, ApiError> {
    super::structural_change(engine, move |structural| async move {
        topology::unload(&structural, &runtimes, mg.id).await?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

/// GET /api/spec/schema — the JSON Schema of the topology document.
pub(in crate::ui) async fn spec_schema() -> Json<serde_json::Value> {
    Json(crate::spec::schema::topology_schema())
}
