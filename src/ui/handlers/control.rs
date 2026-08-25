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
    /// Constant reactive-power override for a meter (VArs), if driving.
    reactive_var: Option<f64>,
    /// Hold a meter's reactive power at this power factor (cos phi,
    /// `0.0 < power_factor <= 1.0`), tracking its own live active power.
    power_factor: Option<f64>,
    /// With `power_factor`, capacitive (leading) instead of the
    /// default inductive (lagging). Meaningless without `power_factor`.
    leading: Option<bool>,
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
        // The default is the FIRST registered microgrid — deterministic,
        // and the same default the Python client uses for gRPC reads.
        // (`config.site()` would follow the ambient `current_microgrid`
        // scope, whose contract needs the interpreter lock we don't
        // hold.) Registry empty = single bootstrap site.
        None => Ok(config.legacy_site()),
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
    // Parse and validate everything first, apply after: a request
    // with one bad field changes nothing (no half-applied status).
    let health: Option<Health> = parse_enum(&req.health, "health")?;
    let command: Option<CommandMode> = parse_enum(&req.command_mode, "command_mode")?;
    let telemetry: Option<TelemetryMode> = parse_enum(&req.telemetry_mode, "telemetry_mode")?;
    // The operational mode forbids some knob values (e.g.
    // telemetry=normal on an inactive component). Check before any
    // setter runs, so a rejected request leaves the state untouched.
    let mode = site.operational_mode(id);
    if telemetry == Some(TelemetryMode::Normal) && !mode.provides_telemetry() {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            format!("component {id} has operational mode {mode}, which streams no telemetry"),
        ));
    }
    if command == Some(CommandMode::Normal) && !mode.accepts_control() {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            format!("component {id} has operational mode {mode}, which accepts no commands"),
        ));
    }
    // Health wins for an errored device: `health=error` forces the
    // command channel to Error, and an explicit `command_mode=normal`
    // in the same request must not re-open it. The Lisp constructors
    // enforce the same rule (see apply_initial_modes in lisp/make.rs).
    if health == Some(Health::Error) && command == Some(CommandMode::Normal) {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            format!("component {id}: health=error forbids command_mode=normal in the same request"),
        ));
    }
    // NOTE: the mode checks above and the setters below take no
    // common lock, so a concurrent (set-component-operational-mode
    // ...) eval can still fail a setter after set_health applied.
    // The window is a few instructions wide; closing it needs an
    // atomic multi-knob setter on the site (tracked in todo.org).
    if let Some(h) = health {
        site.set_health(id, h);
    }
    if let Some(m) = command {
        site.set_command_mode(id, m)
            .map_err(|e| reject(StatusCode::BAD_REQUEST, e))?;
    }
    if let Some(m) = telemetry {
        site.set_telemetry_mode(id, m)
            .map_err(|e| reject(StatusCode::BAD_REQUEST, e))?;
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
    // Validate every field first, apply after (same contract as
    // apply_status): a request with one inapplicable field changes
    // nothing. An inapplicable stimulus is a 400, never a silent no-op.
    if req.power_w.is_some() && !component.takes_active_power_override() {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            format!("component {id} does not take power_w (not a meter)"),
        ));
    }
    if req.sunlight_pct.is_some() && !component.takes_sunlight_pct() {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            format!("component {id} does not take sunlight_pct (not a solar inverter)"),
        ));
    }
    if req.soc_pct.is_some() && !component.takes_soc_pct() {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            format!("component {id} does not take soc_pct (not a battery)"),
        ));
    }
    // reactive_var and power_factor are both Q stimuli, gated by the
    // same predicate as set-meter-reactive-power / set-meter-power-factor
    // in Lisp: whether the component models a reactive-power override.
    if req.reactive_var.is_some() && !component.takes_reactive_power_override() {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            format!("component {id} does not take reactive_var (not a meter)"),
        ));
    }
    if req.power_factor.is_some() && !component.takes_reactive_power_override() {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            format!("component {id} does not take power_factor (not a meter)"),
        ));
    }
    // `reactive_var` and `power_factor` set the same slot, so a request
    // carrying both would apply one and then overwrite it — a silent
    // no-op for the loser. Same mutual exclusion `%make-meter` enforces.
    if req.reactive_var.is_some() && req.power_factor.is_some() {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            format!(
                "component {id}: reactive_var and power_factor are mutually \
                 exclusive; send one or the other"
            ),
        ));
    }
    // `leading` only means something alongside `power_factor` — same
    // shape as the health/command_mode cross-field check above.
    if req.leading.is_some() && req.power_factor.is_none() {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            format!("component {id}: leading requires power_factor in the same request"),
        ));
    }
    // Value sanity, same validate-first contract. The f64→f32 cast
    // turns any JSON number beyond f32 range into ±inf, and the meter
    // override installs whatever it's given — an inf/NaN would poison
    // the energy integrator and every aggregate upstream. Battery /
    // ramp guard their own doors; the meter path has no guard below.
    for (field, v) in [
        ("power_w", req.power_w),
        ("sunlight_pct", req.sunlight_pct),
        ("soc_pct", req.soc_pct),
        ("reactive_var", req.reactive_var),
    ] {
        if let Some(v) = v
            && !(v as f32).is_finite()
        {
            return Err(reject(
                StatusCode::BAD_REQUEST,
                format!("{field} must be a finite number, got {v}"),
            ));
        }
    }
    // `set_power_factor` deliberately does no range validation of its
    // own — this door and `set-meter-power-factor` in Lisp are the
    // only places that enforce it, before the value ever reaches the
    // trait door.
    if let Some(pf) = req.power_factor
        && !(pf > 0.0 && pf <= 1.0)
    {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            format!("power_factor must be in (0.0, 1.0], got {pf}"),
        ));
    }
    // The debug_asserts catch a takes_* predicate drifting from its
    // setter: predicate true + setter false would be a 200 that did
    // nothing, the exact silent no-op this endpoint must not produce.
    if let Some(watts) = req.power_w {
        let applied = component.set_active_power_override(watts as f32);
        debug_assert!(applied, "takes_active_power_override disagrees with setter");
    }
    if let Some(pct) = req.sunlight_pct {
        let applied = component.set_sunlight_pct(pct as f32);
        debug_assert!(applied, "takes_sunlight_pct disagrees with setter");
    }
    if let Some(pct) = req.soc_pct {
        let applied = component.set_soc_pct(pct as f32);
        debug_assert!(applied, "takes_soc_pct disagrees with setter");
    }
    if let Some(vars) = req.reactive_var {
        let applied = component.set_reactive_power_override(vars as f32);
        debug_assert!(
            applied,
            "takes_reactive_power_override disagrees with setter"
        );
    }
    if let Some(pf) = req.power_factor {
        let applied = component.set_power_factor(pf as f32, req.leading.unwrap_or(false));
        debug_assert!(
            applied,
            "takes_reactive_power_override disagrees with setter"
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::meter::Meter;
    use std::time::Duration;

    /// A drive request with one inapplicable field changes nothing:
    /// the rejection must come before any field is applied.
    #[test]
    fn rejected_drive_applies_nothing() {
        let site = MicrogridSite::new();
        site.register(Meter::new(
            5,
            Duration::from_secs(1),
            None,
            None,
            0.0,
            false,
        ));

        let req = DriveRequest {
            power_w: Some(5_000.0),
            sunlight_pct: None,
            soc_pct: Some(50.0), // not a battery → the whole request rejects
            reactive_var: None,
            power_factor: None,
            leading: None,
        };
        assert!(apply_drive(&site, 5, &req).is_err());

        // The meter's power override must not have been installed.
        let meter = site.get(5).unwrap();
        assert!(meter.aggregate_power_w(&site).abs() < 1e-6);
    }
}
