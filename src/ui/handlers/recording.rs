//! Recording a microgrid: start, stop, add an event, and read the
//! summary, the events, the report and the CSV files.

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::sim::microgrid_site::{RecordingError, RecordingReport, RecordingStatus, recording_dir};
use crate::ui::api::{ApiError, Json, Mg, Path, Query};

fn refused(mg: u64, e: RecordingError) -> ApiError {
    match e {
        RecordingError::Busy => {
            ApiError::conflict(format!("a recording is already running in microgrid {mg}"))
        }
        RecordingError::NotRunning => {
            ApiError::conflict(format!("no recording is running in microgrid {mg}"))
        }
        RecordingError::EndedByRebuild => ApiError::conflict(format!(
            "the recording in microgrid {mg} ended when the microgrid was rebuilt"
        ))
        .with("ended_by", "rebuild"),
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
/// recording runs.
pub(in crate::ui) async fn recording_start(
    State(engine): State<Engine>,
    mg: Mg,
    Json(req): Json<StartRequest>,
) -> Result<StatusCode, ApiError> {
    let now = mg.site.now();
    let dir = req
        .csv
        .then(|| recording_dir(&engine.state_dir, mg.id, &req.name, now));
    let site = mg.site.clone();
    super::blocking(move || site.recording_start(req.name, dir.as_deref(), now))
        .await?
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
/// recording; answers its id. 409 when no recording runs, with
/// `"ended_by": "rebuild"` beside the error when a rebuild ended it.
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

/// GET /api/mg/{mg}/recording — the recording's lifecycle. Empty
/// (`name: null`, zero counts) before any start; `elapsed_s` freezes
/// once it stops.
pub(in crate::ui) async fn recording_status(mg: Mg) -> Json<RecordingStatus> {
    Json(mg.site.recording_status(mg.site.now()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct EventsQuery {
    /// Return events with this id and later: pass back the last
    /// answer's `next_event_id`. Default 0 means "everything in the
    /// ring".
    since: Option<u64>,
    /// Cap on returned entries. Default 200.
    limit: Option<usize>,
}

#[derive(Serialize)]
pub(in crate::ui) struct RecordingEventsPage {
    events: Vec<crate::sim::recording::RecordingEvent>,
    /// `next_event_id` lets a polling client advance its `since=`
    /// cursor even when this batch was empty (because no events have
    /// arrived since last poll, but new ones might before the next).
    next_event_id: u64,
    /// Lowest event id still in the ring. Clients comparing
    /// `earliest_event_id > since` know their cursor was inside the
    /// evicted window and they missed `earliest_event_id - since`
    /// entries.
    earliest_event_id: u64,
}

/// GET /api/mg/{mg}/recording/events — the recording's events from
/// the `since` cursor on (ids of `since` and later).
pub(in crate::ui) async fn recording_events(
    mg: Mg,
    Query(q): Query<EventsQuery>,
) -> Json<RecordingEventsPage> {
    let since = q.since.unwrap_or(0);
    let limit = q.limit.unwrap_or(200).min(1000);
    let events = mg.site.recording_events_since(since, limit);
    let summary = mg.site.recording_status(mg.site.now());
    Json(RecordingEventsPage {
        events,
        next_event_id: summary.next_event_id,
        earliest_event_id: summary.earliest_event_id,
    })
}

#[derive(Serialize)]
pub(in crate::ui) struct CsvList {
    /// Recording directory, or null.
    dir: Option<String>,
    files: Vec<String>,
}

/// GET /api/mg/{mg}/recording/csv — the CSV files this microgrid's
/// running or most recent recording wrote, for the Report panel's
/// download links. Empty when nothing has been recorded.
pub(in crate::ui) async fn recording_csv_list(mg: Mg) -> Json<CsvList> {
    match mg.site.recording_csv_listing() {
        Some((dir, files)) => Json(CsvList {
            dir: Some(dir.to_string_lossy().into_owned()),
            files,
        }),
        None => Json(CsvList {
            dir: None,
            files: Vec::new(),
        }),
    }
}

/// The `{file}` segment of a `/recording/csv/{file}` route.
#[derive(Deserialize)]
pub(in crate::ui) struct CsvPath {
    file: String,
}

/// Download one recorded CSV by file name. The name must be a bare
/// `*.csv` file with no path separators (rejecting traversal) and
/// must be one the listing actually reports — so this only ever
/// serves files inside the recording directory.
pub(in crate::ui) async fn recording_csv_file(
    mg: Mg,
    Path(CsvPath { file }): Path<CsvPath>,
) -> Result<Response, ApiError> {
    if file.contains('/') || file.contains('\\') || file.contains("..") || !file.ends_with(".csv") {
        return Err(ApiError::bad_request("invalid filename"));
    }
    let (dir, files) = mg
        .site
        .recording_csv_listing()
        .ok_or_else(|| ApiError::not_found("no recording"))?;
    if !files.contains(&file) {
        return Err(ApiError::not_found("no such file"));
    }
    // Defense in depth: resolve symlinks and confirm the entry is a
    // regular file that stays inside the recording directory, so a
    // symlink planted in that dir can't turn this into an arbitrary
    // file read. The name checks above guard the request string; this
    // guards the entry it resolves to.
    let canon_dir = dir
        .canonicalize()
        .map_err(|e| ApiError::not_found(e.to_string()))?;
    let canon = dir
        .join(&file)
        .canonicalize()
        .map_err(|e| ApiError::not_found(e.to_string()))?;
    if !canon.starts_with(&canon_dir) || !canon.is_file() {
        return Err(ApiError::bad_request("invalid file"));
    }
    let body = std::fs::read(&canon).map_err(|e| ApiError::not_found(e.to_string()))?;
    Response::builder()
        .header("content-type", "text/csv")
        .header(
            "content-disposition",
            format!("attachment; filename=\"{file}\""),
        )
        .body(Body::from(body))
        .map_err(|e| ApiError::internal(e.to_string()))
}
