//! Recording a microgrid: start, stop, add an event, and read the
//! report. The summary, events and CSV reads share their handlers
//! with the `/scenario/...` routes, since both read the one journal.

use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::sim::microgrid_site::{RecordingError, RecordingReport, recording_dir};
use crate::sim::scenario::JournalKind;
use crate::ui::api::{ApiError, Json, Mg};

fn refused(mg: u64, e: RecordingError) -> ApiError {
    match e {
        RecordingError::Busy(JournalKind::Recording) => {
            ApiError::conflict(format!("a recording is already running in microgrid {mg}"))
        }
        RecordingError::Busy(JournalKind::Scenario) => {
            ApiError::conflict(format!("a Lisp scenario is running in microgrid {mg}"))
        }
        RecordingError::NotRunning => {
            ApiError::conflict(format!("no recording is running in microgrid {mg}"))
        }
        RecordingError::Csv(e) => {
            ApiError::internal(format!("recording CSV files for microgrid {mg}: {e}"))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct StartRequest {
    name: String,
    /// Write CSV files under `<state_dir>/recordings/<mg>/`.
    #[serde(default)]
    csv: bool,
}

/// POST /api/mg/{mg}/recording/start — start a recording. 409 while a
/// recording or a Lisp scenario runs.
pub(in crate::ui) async fn recording_start(
    State(engine): State<Engine>,
    mg: Mg,
    Json(req): Json<StartRequest>,
) -> Result<StatusCode, ApiError> {
    let now = mg.site.now();
    let dir = req
        .csv
        .then(|| recording_dir(&engine.state_dir, mg.id, &req.name, now));
    mg.site
        .recording_start(req.name, dir.as_deref(), now)
        .map_err(|e| refused(mg.id, e))?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/mg/{mg}/recording/stop — stop the recording and answer
/// its report (`ended_by: "stop"`). When a rebuild ended it, answer
/// the report the rebuild kept (`ended_by: "rebuild"`), once. 409
/// when neither is there.
pub(in crate::ui) async fn recording_stop(mg: Mg) -> Result<Json<RecordingReport>, ApiError> {
    mg.site
        .recording_stop(mg.site.now())
        .map(Json)
        .map_err(|e| refused(mg.id, e))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct EventRequest {
    kind: String,
    /// Any JSON; a string is kept as it is, anything else as its JSON
    /// text.
    #[serde(default)]
    payload: serde_json::Value,
}

#[derive(Serialize)]
pub(in crate::ui) struct RecordedEvent {
    id: u64,
}

/// POST /api/mg/{mg}/recording/event — add an event to the running
/// recording; answers its id. 409 when no recording runs.
pub(in crate::ui) async fn recording_event(
    mg: Mg,
    Json(req): Json<EventRequest>,
) -> Result<Json<RecordedEvent>, ApiError> {
    let payload = match req.payload {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    mg.site
        .recording_event(req.kind, payload, mg.site.now())
        .map(|id| Json(RecordedEvent { id }))
        .map_err(|e| refused(mg.id, e))
}

/// GET /api/mg/{mg}/recording/report — the report as it stands.
pub(in crate::ui) async fn recording_report(mg: Mg) -> Json<RecordingReport> {
    Json(mg.site.recording_report(mg.site.now()))
}
