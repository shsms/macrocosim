//! `GET` and `POST /api/frequency`: the grid frequency every
//! microgrid shares, and the model that drives it: read it, set it
//! now, change the base model, and set or clear an override.

use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::sim::frequency::{FrequencyModel, FrequencyState};
use crate::ui::api::{ApiError, Json};

/// A frequency model on the wire.
#[derive(Serialize)]
pub(in crate::ui) struct ModelResponse {
    nominal_hz: f32,
    mean_rev_rate_per_s: f32,
    sigma_hz_per_sqrt_s: f32,
}

impl From<FrequencyModel> for ModelResponse {
    fn from(m: FrequencyModel) -> Self {
        Self {
            nominal_hz: m.nominal_hz,
            mean_rev_rate_per_s: m.mean_rev_rate,
            sigma_hz_per_sqrt_s: m.sigma,
        }
    }
}

/// `GET` / `POST /api/frequency` response: the frequency now, the
/// base model, and the override driving it instead, if any.
#[derive(Serialize)]
pub(in crate::ui) struct FrequencyResponse {
    hz: f32,
    model: ModelResponse,
    #[serde(rename = "override")]
    override_model: Option<ModelResponse>,
}

impl From<&FrequencyState> for FrequencyResponse {
    fn from(s: &FrequencyState) -> Self {
        Self {
            hz: s.read_hz(),
            model: s.base.into(),
            override_model: s.override_model.map(Into::into),
        }
    }
}

/// The keys of a model a `POST` changes; a key left out keeps its
/// value.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct ModelPatch {
    nominal_hz: Option<f64>,
    mean_rev_rate_per_s: Option<f64>,
    sigma_hz_per_sqrt_s: Option<f64>,
}

impl ModelPatch {
    /// `None` when every key given is finite, and the rate and the
    /// noise are not negative; else the refusal, naming `field`.
    fn check(&self, field: &str) -> Option<String> {
        let finite = |v: Option<f64>| v.is_none_or(|v| (v as f32).is_finite());
        let not_negative = |v: Option<f64>| v.is_none_or(|v| v >= 0.0);
        if !finite(self.nominal_hz) {
            return Some(format!("{field}.nominal_hz must be a finite number"));
        }
        for (key, v) in [
            ("mean_rev_rate_per_s", self.mean_rev_rate_per_s),
            ("sigma_hz_per_sqrt_s", self.sigma_hz_per_sqrt_s),
        ] {
            if !(finite(v) && not_negative(v)) {
                return Some(format!(
                    "{field}.{key} must be a finite number of 0 or more"
                ));
            }
        }
        None
    }

    fn apply_to(&self, model: &mut FrequencyModel) {
        if let Some(v) = self.nominal_hz {
            model.nominal_hz = v as f32;
        }
        if let Some(v) = self.mean_rev_rate_per_s {
            model.mean_rev_rate = v as f32;
        }
        if let Some(v) = self.sigma_hz_per_sqrt_s {
            model.sigma = v as f32;
        }
    }
}

/// `POST /api/frequency` body. Every field optional.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct FrequencyPost {
    /// Set the frequency now; the model moves on from it.
    hz: Option<f64>,
    /// Change the base model.
    model: Option<ModelPatch>,
    /// Drive the frequency with this model until it is cleared. Its
    /// keys replace any earlier override, and a key left out comes
    /// from the base model.
    #[serde(rename = "override")]
    override_model: Option<ModelPatch>,
    /// Drop the override and go back to the base model.
    #[serde(default)]
    clear_override: bool,
}

pub(in crate::ui) async fn frequency_get(State(engine): State<Engine>) -> Json<FrequencyResponse> {
    Json(FrequencyResponse::from(&*engine.grid_frequency.read()))
}

/// Apply a `POST` body: every check first, then the writes, under one
/// lock, so a refused body changes nothing. Answers with the state it
/// leaves.
pub(in crate::ui) async fn frequency_post(
    State(engine): State<Engine>,
    Json(req): Json<FrequencyPost>,
) -> Result<Json<FrequencyResponse>, ApiError> {
    if let Some(hz) = req.hz
        && !(hz as f32).is_finite()
    {
        return Err(ApiError::bad_request(format!(
            "hz must be a finite number, got {hz}"
        )));
    }
    for (field, patch) in [("model", &req.model), ("override", &req.override_model)] {
        if let Some(e) = patch.as_ref().and_then(|p| p.check(field)) {
            return Err(ApiError::bad_request(e));
        }
    }
    if req.clear_override && req.override_model.is_some() {
        return Err(ApiError::bad_request(
            "override and clear_override are mutually exclusive; send one or the other",
        ));
    }
    let mut state = engine.grid_frequency.write();
    if let Some(hz) = req.hz {
        state.current_hz = hz as f32;
    }
    if let Some(patch) = &req.model {
        patch.apply_to(&mut state.base);
    }
    if let Some(patch) = &req.override_model {
        let mut model = state.base;
        patch.apply_to(&mut model);
        state.override_model = Some(model);
    }
    if req.clear_override {
        state.override_model = None;
    }
    Ok(Json(FrequencyResponse::from(&*state)))
}
