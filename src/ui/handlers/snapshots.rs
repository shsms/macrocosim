//! Snapshot save / load endpoints. Calls `Config::save_snapshot`
//! and `Config::load_snapshot`; both wrap blocking IO.

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use crate::lisp::Config;

#[derive(Serialize)]
pub(in crate::ui) struct SnapshotsListResp {
    snapshots: Vec<String>,
}

pub(in crate::ui) async fn snapshots_list(State(config): State<Config>) -> Json<SnapshotsListResp> {
    Json(SnapshotsListResp {
        snapshots: config.list_snapshots(),
    })
}

#[derive(Deserialize)]
pub(in crate::ui) struct SnapshotsBody {
    name: String,
}

pub(in crate::ui) async fn snapshots_save(
    State(config): State<Config>,
    Json(body): Json<SnapshotsBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let path = super::blocking(move || config.save_snapshot(&body.name))
        .await?
        // Only the sanitiser's rejection is the caller's fault
        // (InvalidInput); disk-full / permissions are server-side
        // 500s a client shouldn't retry as a "bad request".
        .map_err(|e| {
            let status = if e.kind() == std::io::ErrorKind::InvalidInput {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, e.to_string())
        })?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "path": path.display().to_string(),
    })))
}

pub(in crate::ui) async fn snapshots_load(
    State(config): State<Config>,
    Json(body): Json<SnapshotsBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    super::blocking(move || config.load_snapshot(&body.name))
        .await?
        // load_snapshot returns String errors; classify by the known
        // client-fault prefixes, everything else (copy/rename IO,
        // reload failure) is a 500.
        .map_err(|e| {
            let status = if e.starts_with("invalid snapshot name") {
                StatusCode::BAD_REQUEST
            } else if e.starts_with("snapshot ") && e.ends_with("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, e)
        })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
