//! `GET /api/mg/{mg}/component/{id}/ev` — the simulator's private
//! view of the car plugged into a charger, for the inspector's EV
//! card. Not the gRPC API: that sees only the charger.
//!
//! The JSON keys are the snake_case twins of `ev-info`'s plist keys,
//! plus `plugged`.
//!
//! `presets` — the catalog — rides along whether or not a car is
//! plugged in, so the inspector builds its dropdown from the server's
//! list instead of a copy of it.
//!
//! `POST` on the same path plugs a car from that catalog in, the twin
//! of `(plug-ev)`; `DELETE` unplugs it, the twin of `(unplug-ev)`.

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::sim::component::{EvPort, SimulatedComponent};
use crate::sim::ev_presets::{self, ConnectedEv, EvOverrides, PRESETS};
use crate::ui::api::{ApiError, ComponentPath, Json, Mg, Path};

#[derive(Serialize, Default)]
pub(in crate::ui) struct EvResponse {
    plugged: bool,
    /// The catalog a `plug-ev` may name, sent plugged or not so the
    /// inspector's preset dropdown never hardcodes it.
    presets: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preset: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    soc_pct: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_soc_pct: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phases: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_current_a: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capacity_wh: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    energy_wh: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plugged_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<&'static str>,
}

/// The charger's EV port, or the 400 every EV route gives a component
/// that has none.
fn port_of(c: &dyn SimulatedComponent) -> Result<&dyn EvPort, ApiError> {
    c.ev_port()
        .ok_or_else(|| ApiError::bad_request(format!("component {} is not an EV charger", c.id())))
}

pub(in crate::ui) async fn ev(
    mg: Mg,
    Path(ComponentPath { id }): Path<ComponentPath>,
) -> Result<Json<EvResponse>, ApiError> {
    let c = mg.site.get(id).ok_or_else(|| ApiError::no_component(id))?;
    let port = port_of(&*c)?;
    let presets = PRESETS.iter().map(|p| p.name).collect();
    Ok(Json(match port.ev_info() {
        None => EvResponse {
            plugged: false,
            presets,
            ..Default::default()
        },
        Some(i) => EvResponse {
            plugged: true,
            presets,
            preset: Some(i.ev.preset),
            soc_pct: Some(i.ev.soc_pct),
            target_soc_pct: Some(i.ev.target_soc_pct),
            phases: Some(i.ev.phases),
            max_current_a: Some(i.ev.max_current_a),
            capacity_wh: Some(i.ev.capacity_wh),
            energy_wh: Some(i.ev.energy_wh),
            plugged_at: Some(crate::timefmt::rfc3339(i.ev.plugged_at)),
            state: Some(i.state.as_str()),
        },
    }))
}

/// `POST .../component/{id}/ev` body: a car from the catalog, by
/// preset name, and the values of that preset to change.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct PlugRequest {
    preset: String,
    soc_pct: Option<f64>,
    /// 100 unless given.
    target_soc_pct: Option<f64>,
    /// 1, 2 or 3.
    phases: Option<u8>,
    max_current_a: Option<f64>,
    capacity_wh: Option<f64>,
    taper_start_pct: Option<f64>,
    taper_floor_pct: Option<f64>,
}

/// Plug a car into charger `id`: 204, 400 for a component that is not
/// a charger or a value the car refuses, 409 when a car is already
/// plugged in.
pub(in crate::ui) async fn ev_plug(
    mg: Mg,
    Path(ComponentPath { id }): Path<ComponentPath>,
    Json(req): Json<PlugRequest>,
) -> Result<StatusCode, ApiError> {
    let c = mg.site.get(id).ok_or_else(|| ApiError::no_component(id))?;
    let port = port_of(&*c)?;
    let occupied = || ApiError::conflict(format!("charger {id} already has an EV plugged in"));
    if port.ev_info().is_some() {
        return Err(occupied());
    }
    let preset = ev_presets::preset(&req.preset).ok_or_else(|| {
        let names: Vec<_> = PRESETS.iter().map(|p| p.name).collect();
        ApiError::bad_request(format!(
            "unknown preset {:?}; the presets are {}",
            req.preset,
            names.join(", ")
        ))
    })?;
    // The car checks every other value; the taper floor is checked
    // here, in the percent the field names, before it becomes a
    // ratio.
    if let Some(v) = req.taper_floor_pct
        && !(v.is_finite() && (0.0..=100.0).contains(&v))
    {
        return Err(ApiError::bad_request(format!(
            "taper_floor_pct must be within 0..=100, got {v}"
        )));
    }
    let overrides = EvOverrides {
        soc_pct: req.soc_pct.map(|v| v as f32),
        target_soc_pct: req.target_soc_pct.map(|v| v as f32),
        phases: req.phases,
        max_current_a: req.max_current_a.map(|v| v as f32),
        capacity_wh: req.capacity_wh.map(|v| v as f32),
        taper_start_pct: req.taper_start_pct.map(|v| v as f32),
        taper_floor: req.taper_floor_pct.map(|v| (v / 100.0) as f32),
    };
    let car = ConnectedEv::new(preset, &overrides, chrono::Utc::now())
        .map_err(|e| ApiError::bad_request(format!("component {id}: {e}")))?;
    // Refused only by a plug that raced this one.
    mg.site.plug_ev(id, port, car).map_err(|_| occupied())?;
    Ok(StatusCode::NO_CONTENT)
}

/// Unplug the car from charger `id`: 204 whether or not one was
/// plugged in, 400 for a component that is not a charger.
pub(in crate::ui) async fn ev_unplug(
    mg: Mg,
    Path(ComponentPath { id }): Path<ComponentPath>,
) -> Result<StatusCode, ApiError> {
    let c = mg.site.get(id).ok_or_else(|| ApiError::no_component(id))?;
    mg.site.unplug_ev(id, port_of(&*c)?);
    Ok(StatusCode::NO_CONTENT)
}
