//! A microgrid's stored spec: read it, export it as Python, replace
//! it (a rebuild with carry), unload the microgrid, and the JSON
//! Schema of the topology document.

use std::process::Stdio;
use std::time::Duration;

use axum::Extension;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use tokio::io::AsyncWriteExt;

use crate::engine::Engine;
use crate::engine::topology::{self, TopologyError};
use crate::runtime::MicrogridRuntimes;
use crate::spec::{MicrogridSpec, SCHEMA_VERSION, TopologyDoc};
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

/// How long `python -m macrocosim.export` may take.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// GET /api/mg/{mg}/spec.py — the stored spec as a Python module, as
/// `python -m macrocosim.export` writes it: the Python package owns
/// its own syntax. 409 for a microgrid loaded from Lisp, 503 when no
/// usable interpreter is installed, 500 when the export fails or
/// takes longer than [`EXPORT_TIMEOUT`], and 429 while another export
/// runs.
pub(in crate::ui) async fn spec_py(
    State(engine): State<Engine>,
    mg: Mg,
) -> Result<impl IntoResponse, ApiError> {
    let Ok(_permit) = engine.exports.clone().try_acquire_owned() else {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "an export is running; try again when it is done",
        ));
    };
    let spec = {
        let registry = engine.microgrids();
        let registry = registry.lock();
        let entry = registry
            .get(&mg.id)
            .ok_or_else(|| ApiError::not_registered(mg.id))?;
        entry
            .spec
            .clone()
            .ok_or_else(|| ApiError::from(topology::loaded_from_lisp(mg.id)))?
    };
    let python = engine
        .python()
        .await
        .map_err(|m| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, m))?;
    let doc = TopologyDoc {
        schema_version: SCHEMA_VERSION,
        microgrids: vec![spec],
    };
    let input = serde_json::to_vec(&doc).map_err(|e| ApiError::internal(e.to_string()))?;
    let failed = |what: String| ApiError::internal(format!("the export failed: {what}"));
    let mut child = tokio::process::Command::new(&python)
        .args(["-P", "-m", "macrocosim.export"])
        // Its error message in UTF-8 too, whatever the locale.
        .env("PYTHONUTF8", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| failed(format!("{} does not run: {e}", python.display())))?;
    let mut stdin = child.stdin.take().expect("piped above");
    let run = async move {
        // A write error means the child exited early; its stderr says
        // why.
        let _ = stdin.write_all(&input).await;
        drop(stdin);
        child.wait_with_output().await
    };
    let out = match tokio::time::timeout(EXPORT_TIMEOUT, run).await {
        Err(_) => return Err(failed(format!("no answer within {EXPORT_TIMEOUT:?}"))),
        Ok(Err(e)) => return Err(failed(e.to_string())),
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        return Err(failed(format!(
            "{}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8(out.stdout).map_err(|e| failed(e.to_string()))?;
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
    Extension(runtimes): Extension<MicrogridRuntimes>,
    mg: Mg,
    Json(spec): Json<MicrogridSpec>,
) -> Result<Json<MicrogridSpec>, ApiError> {
    let create_lock = engine.create_lock();
    let _structural = create_lock.lock().await;
    let replaced = topology::rebuild(&engine, &runtimes, mg.id, spec).await?;
    engine.record_spec_undo(mg.id, replaced);
    topology::mark_edited(&engine, mg.id);
    let registry = engine.microgrids();
    let registry = registry.lock();
    let entry = registry
        .get(&mg.id)
        .ok_or_else(|| ApiError::not_registered(mg.id))?;
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
