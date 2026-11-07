//! Embedded web UI server.
//!
//! Runs alongside the gRPC server on the same tokio runtime, separate
//! port (default 8801). The SPA shell + vendored assets are bundled
//! via rust-embed.
//!
//! The fanout is:
//!
//! - `state` — types shared between the loopback supervisor, the WS
//!   pump, and the HTTP handlers.
//! - `loopback` — gRPC loopback supervisor.
//! - `events_ws` — WebSocket push channel.
//! - `handlers` — one submodule per HTTP topic (topology, spec,
//!   history, recording, microgrids, …).
//! - `api` — the error type every handler fails with, and the
//!   extractors and fallbacks that answer axum's own failures with
//!   it.
//! - `app_state` — the router's state: the `Engine`, the runtimes,
//!   the export's interpreter and the script's supervisor.

mod api;
mod app_state;
mod events_ws;
mod handlers;
mod loopback;
mod state;

pub use loopback::{spawn_microgrid_loopback, stop_microgrid_loopback};
pub use state::{
    HistorySample, MicrogridLoopbacks, MicrogridSampleSnapshot, MicrogridState, SharedMicrogrid,
    new_microgrid_loopbacks, new_microgrid_slot,
};

use std::sync::Arc;

use axum::{
    Router,
    routing::{delete, get, post},
};

use crate::engine::Engine;
use app_state::AppState;
use events_ws::events_ws;

/// Run the UI HTTP server on an already-bound listener.
///
/// `runtimes` starts and reports each microgrid's runtime, and holds
/// the per-microgrid loopback slots the `/api/mg/{mg}/metrics/*`
/// routes read.
///
/// `python` is the interpreter the `.py` export runs on, and `runner`
/// the supervisor of the engine's script, which the `/api/runner`
/// routes answer for; `None` without a script.
pub async fn serve_with_listener(
    listener: tokio::net::TcpListener,
    engine: Engine,
    runtimes: crate::runtime::MicrogridRuntimes,
    python: Arc<crate::runner::python::Interpreter>,
    runner: Option<Arc<crate::runner::ScriptSupervisor>>,
) -> std::io::Result<()> {
    axum::serve(listener, router(engine, runtimes, python, runner))
        .await
        .map_err(std::io::Error::other)
}

fn router(
    engine: Engine,
    runtimes: crate::runtime::MicrogridRuntimes,
    python: Arc<crate::runner::python::Interpreter>,
    runner: Option<Arc<crate::runner::ScriptSupervisor>>,
) -> Router {
    use handlers::{
        assets::{asset, index, logs_backfill},
        component::component,
        control::{
            component_drive, component_runtime, component_status, sample_lag, streams_cancel,
        },
        defaults::defaults,
        dispatches::{dispatch_create, dispatch_delete, dispatch_set_active, dispatches},
        ev::{ev, ev_plug, ev_unplug},
        formula::formula,
        frequency::{frequency_get, frequency_post},
        grid::grid_post,
        history::{history, setpoints},
        microgrid_data::{metrics_formulas, metrics_history, metrics_latest, metrics_status},
        microgrids::{microgrids_create, microgrids_import, microgrids_list},
        recording::{
            recording_csv_file, recording_csv_list, recording_event, recording_events,
            recording_report, recording_start, recording_status, recording_stop,
        },
        runner::{runner_hello, runner_status},
        spec::{microgrid_delete, spec_get, spec_put, spec_py, spec_schema},
        topology::topology,
        undo::{redo, undo, undo_depths},
        weather::{weather_delete, weather_get, weather_post},
    };
    let per_mg = Router::new()
        .route("/", delete(microgrid_delete))
        .route("/topology", get(topology))
        .route("/spec", get(spec_get).put(spec_put))
        .route("/spec.py", get(spec_py))
        .route("/formula", get(formula))
        .route("/component/{id}", get(component))
        .route("/component/{id}/status", post(component_status))
        .route("/component/{id}/drive", post(component_drive))
        .route("/component/{id}/runtime", post(component_runtime))
        .route("/component/{id}/history", get(history))
        .route("/component/{id}/setpoints", get(setpoints))
        .route(
            "/component/{id}/ev",
            get(ev).post(ev_plug).delete(ev_unplug),
        )
        .route("/sample-lag", post(sample_lag))
        .route("/streams/cancel", post(streams_cancel))
        .route(
            "/weather",
            get(weather_get).post(weather_post).delete(weather_delete),
        )
        .route("/grid", post(grid_post))
        .route("/metrics/status", get(metrics_status))
        .route("/metrics/latest", get(metrics_latest))
        .route("/metrics/history", get(metrics_history))
        .route("/metrics/formulas", get(metrics_formulas))
        .route("/undo", get(undo_depths).post(undo))
        .route("/redo", post(redo))
        .route("/recording", get(recording_status))
        .route("/recording/start", post(recording_start))
        .route("/recording/stop", post(recording_stop))
        .route("/recording/event", post(recording_event))
        .route("/recording/events", get(recording_events))
        .route("/recording/report", get(recording_report))
        .route("/recording/csv", get(recording_csv_list))
        .route("/recording/csv/{file}", get(recording_csv_file))
        .route("/dispatches", get(dispatches).post(dispatch_create))
        .route("/dispatches/{dispatch_id}", delete(dispatch_delete))
        .route(
            "/dispatches/{dispatch_id}/active",
            post(dispatch_set_active),
        );
    Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route("/api/defaults", get(defaults))
        .route("/api/spec/schema", get(spec_schema))
        .route("/api/frequency", get(frequency_get).post(frequency_post))
        .route("/api/logs", get(logs_backfill))
        .route("/api/runner", get(runner_status))
        .route("/api/runner/hello", post(runner_hello))
        .route(
            "/api/microgrids",
            get(microgrids_list).post(microgrids_create),
        )
        .route(
            "/api/microgrids/import",
            // Site exports run to tens of MB; axum's 2 MB default
            // body limit would reject them.
            post(microgrids_import).layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .nest("/api/mg/{mg}", per_mg)
        .route("/ws/events", get(events_ws))
        .fallback(api::no_route)
        .method_not_allowed_fallback(api::method_not_allowed)
        .layer(axum::middleware::from_fn(origin_guard))
        .with_state(AppState::new(engine, runtimes, python, runner))
}

/// Same-origin guard for the browser-facing surface.
///
/// The routes change the simulation, and a cross-origin page can fire
/// some of them without any CORS preflight: a body-less POST
/// (`/undo`, `/redo`, `/recording/stop`, `/streams/cancel`) or a GET
/// that does work (`/spec.py` runs Python). CORS response headers
/// only gate *reading* the response, never executing the request, so
/// a `CorsLayer` alone would not defend them. The server refuses
/// requests that provably come from a foreign browser context
/// instead:
///
/// - a non-loopback `Host` rejects DNS rebinding (evil.example
///   resolving to 127.0.0.1 keeps its own name in `Host`);
/// - an `Origin` whose authority differs from `Host` rejects plain
///   cross-origin requests, cross-origin WebSocket handshakes
///   included (the same-origin policy never blocked those).
///
/// Non-browser clients (curl, macroctl/reqwest) pass because they
/// address the server by its loopback authority — their `Host` is a
/// loopback name and they send no `Origin`; requests with no `Host`
/// at all (in-process test calls) pass too. Browsers always send
/// `Host`, and always send `Origin` on cross-origin requests. GETs
/// are guarded too — a cross-origin read of this UI has no
/// legitimate use. The loopback allowlist needs a knob if
/// `--ui-bind` ever lands.
async fn origin_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    if let Some(host) = host
        && !is_loopback_host(authority_host(host))
    {
        return api::ApiError::new(StatusCode::FORBIDDEN, "non-loopback Host rejected")
            .into_response();
    }
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        // `Origin: null` (sandboxed iframe, file://) and malformed
        // values fall through to the rejection: only an authority
        // exactly matching `Host` passes.
        let matches_host = origin
            .to_str()
            .ok()
            .and_then(|o| o.split_once("://").map(|(_, auth)| auth))
            .zip(host)
            .is_some_and(|(o_auth, host)| o_auth.eq_ignore_ascii_case(host));
        if !matches_host {
            return api::ApiError::new(StatusCode::FORBIDDEN, "cross-origin request rejected")
                .into_response();
        }
    }
    next.run(req).await
}

/// Host part of an authority string: brackets stripped from an IPv6
/// literal, a trailing `:port` dropped.
fn authority_host(authority: &str) -> &str {
    let a = authority.trim();
    if let Some(rest) = a.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match a.rsplit_once(':') {
        // A ':' left in the head means a bare IPv6 literal, whose
        // colons are not a port separator.
        Some((h, p)) if !h.contains(':') && p.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => a,
    }
}

fn is_loopback_host(host: &str) -> bool {
    // Browsers hardwire `localhost`, any `*.localhost` name, and
    // their trailing-dot FQDN forms to loopback without consulting
    // DNS (RFC 6761), and every 127.0.0.0/8 literal reaches a
    // loopback-bound listener — all of those are this server's own
    // origin, not a foreign one.
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || host == "::1"
    {
        return true;
    }
    host.parse::<std::net::Ipv4Addr>()
        .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests;
