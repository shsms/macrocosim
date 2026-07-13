//! Typed JSON control endpoints — component stimuli without Lisp.
//!
//! `POST /api/component/{id}/status` and `POST /api/component/{id}/drive`
//! (plus the `/api/mg/{mg_id}/…` variants) give programmatic clients a
//! structured way to inject faults and drive the environment. Validation
//! errors come back as HTTP 400 with a JSON error body, and an unknown
//! component or microgrid is 404 — no `ok: false` payload the caller
//! must remember to check. `/api/eval` remains the escape hatch for
//! dynamic (lambda / symbol) drive sources and everything else Lisp.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::lisp::Config;
use crate::sim::microgrid_site::MicrogridSite;
use crate::sim::runtime::{CommandMode, Health, TelemetryMode};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct StatusRequest {
    /// New health state (`ok` / `error` / `standby`), if changing.
    health: Option<String>,
    /// New command-channel mode (`normal` / `timeout` / `error` /
    /// `over-bound`), if changing.
    command_mode: Option<String>,
    /// New telemetry mode (`normal` / `silent` / `closed` /
    /// `error-empty` / `not-found`), if changing.
    telemetry_mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct DriveRequest {
    /// Constant active-power override for a meter (watts), if driving.
    power_w: Option<f64>,
    /// Sunlight percentage for a PV inverter, if driving.
    sunlight_pct: Option<f64>,
    /// Teleport a battery's state of charge to this percentage.
    soc_pct: Option<f64>,
}

/// Empty JSON on success; the error text on any rejection.
#[derive(Serialize)]
pub(in crate::ui) struct ControlError {
    error: String,
}

type ControlResult = Result<Json<serde_json::Value>, (StatusCode, Json<ControlError>)>;

fn reject(status: StatusCode, error: String) -> (StatusCode, Json<ControlError>) {
    (status, Json(ControlError { error }))
}

fn site_for(
    config: &Config,
    mg_id: Option<u64>,
) -> Result<MicrogridSite, (StatusCode, Json<ControlError>)> {
    match mg_id {
        None => Ok(config.site()),
        Some(id) => config
            .microgrids()
            .lock()
            .get(&id)
            .map(|entry| entry.site.clone())
            .ok_or_else(|| {
                reject(
                    StatusCode::NOT_FOUND,
                    format!("microgrid {id} not registered"),
                )
            }),
    }
}

fn apply_status(site: &MicrogridSite, id: u64, req: &StatusRequest) -> ControlResult {
    if site.get(id).is_none() {
        return Err(reject(
            StatusCode::NOT_FOUND,
            format!("component {id} not found"),
        ));
    }
    // Parse everything first, apply after: a request with one bad field
    // changes nothing (no half-applied status).
    let health: Option<Health> = parse_enum(&req.health, "health")?;
    let command: Option<CommandMode> = parse_enum(&req.command_mode, "command_mode")?;
    let telemetry: Option<TelemetryMode> = parse_enum(&req.telemetry_mode, "telemetry_mode")?;
    if let Some(h) = health {
        site.set_health(id, h);
    }
    if let Some(m) = command {
        site.set_command_mode(id, m);
    }
    if let Some(m) = telemetry {
        site.set_telemetry_mode(id, m);
    }
    Ok(Json(serde_json::json!({})))
}

fn parse_enum<T: std::str::FromStr>(
    value: &Option<String>,
    field: &str,
) -> Result<Option<T>, (StatusCode, Json<ControlError>)> {
    match value {
        None => Ok(None),
        Some(s) => s
            .parse::<T>()
            .map(Some)
            .map_err(|_| reject(StatusCode::BAD_REQUEST, format!("invalid {field}: {s:?}"))),
    }
}

fn apply_drive(site: &MicrogridSite, id: u64, req: &DriveRequest) -> ControlResult {
    let Some(component) = site.get(id) else {
        return Err(reject(
            StatusCode::NOT_FOUND,
            format!("component {id} not found"),
        ));
    };
    if let Some(watts) = req.power_w {
        component.set_active_power_override(watts as f32);
    }
    if let Some(pct) = req.sunlight_pct {
        component.set_sunlight_pct(pct as f32);
    }
    if let Some(pct) = req.soc_pct {
        component.set_soc_pct(pct as f32);
    }
    Ok(Json(serde_json::json!({})))
}

pub(in crate::ui) async fn component_status(
    State(config): State<Config>,
    Path(id): Path<u64>,
    Json(req): Json<StatusRequest>,
) -> ControlResult {
    apply_status(&site_for(&config, None)?, id, &req)
}

pub(in crate::ui) async fn component_status_for_mg(
    State(config): State<Config>,
    Path((mg_id, id)): Path<(u64, u64)>,
    Json(req): Json<StatusRequest>,
) -> ControlResult {
    apply_status(&site_for(&config, Some(mg_id))?, id, &req)
}

pub(in crate::ui) async fn component_drive(
    State(config): State<Config>,
    Path(id): Path<u64>,
    Json(req): Json<DriveRequest>,
) -> ControlResult {
    apply_drive(&site_for(&config, None)?, id, &req)
}

pub(in crate::ui) async fn component_drive_for_mg(
    State(config): State<Config>,
    Path((mg_id, id)): Path<(u64, u64)>,
    Json(req): Json<DriveRequest>,
) -> ControlResult {
    apply_drive(&site_for(&config, Some(mg_id))?, id, &req)
}
