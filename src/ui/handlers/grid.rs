//! `POST /api/mg/{mg}/grid`: the grid state of one microgrid, its
//! voltage per phase. The grid frequency is shared by every microgrid
//! and has its own route (`frequency.rs`).

use axum::http::StatusCode;
use serde::Deserialize;

use crate::sim::events::KnobValue;
use crate::ui::api::{ApiError, Json, Mg};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct GridRequest {
    /// The voltage of phases 1, 2 and 3, in V.
    voltage_per_phase_v: [f64; 3],
}

pub(in crate::ui) async fn grid_post(
    mg: Mg,
    Json(req): Json<GridRequest>,
) -> Result<StatusCode, ApiError> {
    let [l1, l2, l3] = req.voltage_per_phase_v.map(|v| v as f32);
    for v in [l1, l2, l3] {
        if !(v.is_finite() && v >= 0.0) {
            return Err(ApiError::bad_request(format!(
                "voltage_per_phase_v must hold three finite voltages of 0 or more, got {:?}",
                req.voltage_per_phase_v
            )));
        }
    }
    mg.site.set_voltage_per_phase((l1, l2, l3));
    mg.site.note_knob_changed(
        None,
        "grid-voltage",
        Some(KnobValue::List(vec![l1, l2, l3])),
        None,
    );
    Ok(StatusCode::NO_CONTENT)
}
