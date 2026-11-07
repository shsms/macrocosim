//! The script runner's routes: the hello a script child sends once
//! its script has loaded, and the runner's status.

use axum::extract::State;
use axum::http::StatusCode;

use crate::runner::ScriptStatus;
use crate::runner::hello::HelloAnswer;
use crate::ui::api::{ApiError, Json};
use crate::ui::app_state::Runner;

/// POST /api/runner/hello — put the script child's microgrids in
/// place, stopping the old child first, and answer their gRPC
/// addresses. 403 for a token that is not the pending child's (or an
/// engine without a script), 400 for another package version, 422 for
/// an invalid spec, 409 for an id or a port that is not the script's
/// to take.
pub(in crate::ui) async fn runner_hello(
    State(Runner(runner)): State<Runner>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<HelloAnswer>, ApiError> {
    let Some(runner) = runner else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "this engine runs no script",
        ));
    };
    // The supervisor takes its own lock before the structural one, so
    // the hello is detached as it is.
    super::detached(async move { runner.hello(body).await.map_err(ApiError::from) })
        .await
        .map(Json)
}

/// GET /api/runner — the script, its child's state, the last reload's
/// result and the child's recent output. Without a script, every
/// field is null or empty.
pub(in crate::ui) async fn runner_status(
    State(Runner(runner)): State<Runner>,
) -> Json<ScriptStatus> {
    Json(runner.map(|r| r.status()).unwrap_or_default())
}
