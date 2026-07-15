use super::*;
use crate::lisp::Config;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use std::io::Write;
use tower::ServiceExt;

/// Boots a `Config` against a freshly-written tiny config file
/// holding `body`, so the live tulisp ctx + MicrogridSite are wired up the
/// same way the binary wires them. Returns the Config; caller
/// composes a router with it.
///
/// Each call gets its own unique subdirectory under `temp_dir()`
/// so concurrent test runs don't stomp each other's config.lisp
/// (cargo runs the lib test suite multi-threaded by default).
async fn config_with(body: &str) -> Config {
    // tulisp-async's executor needs a tokio runtime in scope; we
    // already have one via #[tokio::test], so Config::new works.
    let mut p = std::env::temp_dir();
    p.push(format!(
        "switchyard-ui-{}-{}",
        std::process::id(),
        // Counter — even if SystemTime resolves the same nanos for
        // two near-simultaneous tests, the AtomicU64 disambiguates.
        UNIQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&p).unwrap();
    let path = p.join("config.lisp");
    let wrapped = wrap_test_body(body);
    write!(std::fs::File::create(&path).unwrap(), "{wrapped}").unwrap();
    Config::new(path.to_str().unwrap()).expect("config eval")
}

/// Wrap a test body in `(make-microgrid …)` if the body doesn't already
/// register one. Any inline `(set-microgrid-id N)` from the pre-
/// migration shape gets stripped and its N seeds the wrapper's :id so
/// per-mg id assertions keep their original targets.
fn wrap_test_body(body: &str) -> String {
    if body.contains("make-microgrid") {
        return body.to_string();
    }
    let (stripped, mg_id) = strip_set_microgrid_id(body);
    let inner = if stripped.trim().is_empty() {
        "nil".to_string()
    } else {
        stripped
    };
    format!("(make-microgrid :id {mg_id} :grpc-port 8800 :topology (lambda () {inner}))")
}

fn strip_set_microgrid_id(body: &str) -> (String, u64) {
    let needle = "(set-microgrid-id ";
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    let mut mg_id: u64 = 2200;
    while let Some(idx) = rest.find(needle) {
        out.push_str(&rest[..idx]);
        let tail = &rest[idx + needle.len()..];
        if let Some(close) = tail.find(')') {
            let n_str = tail[..close].trim();
            if let Ok(v) = n_str.parse::<u64>() {
                mg_id = v;
            }
            rest = &tail[close + 1..];
        } else {
            out.push_str(&rest[idx..]);
            return (out, mg_id);
        }
    }
    out.push_str(rest);
    (out, mg_id)
}

static UNIQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One-shot a request and return (status, body). axum's `oneshot`
/// avoids binding a real port. Microgrid loopback slot is empty —
/// the new `/api/microgrid/status` endpoint returns 503 without a
/// real gRPC server, which is exactly the expected unit-test
/// behaviour. Tests that want a populated handle would have to
/// spin up the gRPC server too.
async fn call(config: Config, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = router(config, new_microgrid_slot(), new_microgrid_loopbacks())
        .oneshot(req)
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, bytes.to_vec())
}

fn get(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
}

fn post(path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn index_serves_embedded_shell() {
    let cfg = config_with("").await;
    let (status, body) = call(cfg, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    let s = String::from_utf8_lossy(&body);
    assert!(s.contains("<title>switchyard</title>"));
    assert!(s.contains("/assets/app.js"));
}

#[tokio::test]
async fn asset_route_serves_embedded_files() {
    let cfg = config_with("").await;
    let (status, body) = call(cfg, get("/assets/app.js")).await;
    assert_eq!(status, StatusCode::OK);
    // Phrase from app.js — anchors the test against actually
    // serving the right file rather than just any 200.
    assert!(String::from_utf8_lossy(&body).contains("vis-network"));
}

#[tokio::test]
async fn asset_route_serves_vendored_lib() {
    let cfg = config_with("").await;
    let (status, body) = call(cfg, get("/assets/vendor/vis-network.min.js")).await;
    assert_eq!(status, StatusCode::OK);
    // vis-network's UMD bundle exports a global `vis` namespace.
    assert!(String::from_utf8_lossy(&body).contains("vis"));
}

#[tokio::test]
async fn asset_route_404s_unknown_path() {
    let cfg = config_with("").await;
    let (status, _) = call(cfg, get("/assets/does-not-exist.js")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn topology_endpoint_emits_components_and_connections() {
    let cfg = config_with(
        r#"(%make-grid-connection-point :id 1
             :successors
             (list (%make-meter :id 2
                     :successors
                     (list (%make-battery :id 3)))))"#,
    )
    .await;

    let (status, body) = call(cfg, get("/api/topology")).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(parsed["components"].as_array().unwrap().len(), 3);
    assert_eq!(parsed["connections"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn eval_endpoint_runs_lisp_and_returns_value() {
    let cfg = config_with("").await;
    let (status, body) = call(cfg, post("/api/eval", "(+ 2 3)")).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["value"], "5");
    assert!(parsed["error"].is_null());
}

#[tokio::test]
async fn eval_endpoint_reports_lisp_errors() {
    let cfg = config_with("").await;
    let (status, body) = call(cfg, post("/api/eval", "(undefined-fn 1)")).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["ok"], false);
    assert!(parsed["value"].is_null());
    assert!(!parsed["error"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn format_endpoint_pretty_prints_lisp() {
    let cfg = config_with("").await;
    let (status, body) = call(
        cfg,
        post("/api/format?width=20", "(when (< x 5)(inc x)(princ x))"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Width 20 forces (when …) to break header-then-body, with each
    // body form on its own line at +2.
    assert_eq!(
        String::from_utf8_lossy(&body),
        "(when (< x 5)\n  (inc x)\n  (princ x))\n"
    );
}

#[tokio::test]
async fn format_endpoint_returns_400_on_parse_error() {
    let cfg = config_with("").await;
    let (status, body) = call(cfg, post("/api/format", "(unbalanced")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!String::from_utf8_lossy(&body).is_empty());
}

#[tokio::test]
async fn history_endpoint_returns_recent_samples() {
    // Build a site with a battery, then drive the sampler twice
    // synchronously so the rings have content to query. Battery
    // publishes soc_pct in its telemetry; that's what we query.
    let cfg = config_with("(%make-battery :id 1000)").await;
    let site = cfg.site();
    let now = chrono::Utc::now();
    site.record_history_snapshot(now - chrono::Duration::seconds(2));
    site.record_history_snapshot(now - chrono::Duration::seconds(1));

    let (status, body) = call(cfg, get("/api/history?id=1000&metric=soc_pct&window_s=10")).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["id"], 1000);
    assert_eq!(parsed["metric"], "soc_pct");
    let samples = parsed["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 2);
    // Each sample is [ts_ms, value]
    assert!(samples[0][0].as_i64().unwrap() < samples[1][0].as_i64().unwrap());
}

#[tokio::test]
async fn history_endpoint_rejects_unknown_metric() {
    let cfg = config_with("").await;
    let (status, body) = call(cfg, get("/api/history?id=1&metric=foo")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("unknown metric"));
}

#[tokio::test]
async fn history_endpoint_returns_empty_for_unknown_component() {
    let cfg = config_with("").await;
    let (status, body) = call(cfg, get("/api/history?id=999&metric=active_power_w")).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["samples"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn overrides_endpoint_lists_appended_evals() {
    let cfg = config_with("(set-microgrid-id 7) (%make-grid-connection-point :id 1)").await;
    // One structural eval (persists), one error (doesn't), one
    // non-structural poke (runs, but the d6 persist gate keeps it out
    // of the overrides file so it can't replay as config on reload).
    call(cfg.clone(), post("/api/eval", "(rename-component 1 \"a\")")).await;
    call(cfg.clone(), post("/api/eval", "(undefined-fn 1)")).await;
    call(cfg.clone(), post("/api/eval", "(set-enterprise-id 42)")).await;
    let (status, body) = call(cfg, get("/api/overrides")).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let entries = parsed["persisted"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries
            .iter()
            .any(|e| e["source"].as_str().unwrap().contains("rename"))
    );
    assert!(
        !entries
            .iter()
            .any(|e| e["source"].as_str().unwrap().contains("set-enterprise-id")),
        "non-structural pokes must not persist",
    );
    assert_eq!(parsed["count"], 1);
}

/// Minimal local `load-overrides` defun for tests — real configs
/// get this from `sim/common.lisp`, but `config_with` writes a
/// bare-bones config that doesn't pull in the helper file.
const LOAD_OVERRIDES_HELPER: &str = "(defun load-overrides ()
       (when (file-exists-p \"microgrids/config.7.overrides.lisp\")
         (load \"microgrids/config.7.overrides.lisp\")))
     (load-overrides)";

#[tokio::test]
async fn persisted_remove_drops_form_immediately() {
    // Two evals append two forms to the override file. DELETE
    // /api/persisted/0 rewrites the file without that form and
    // reloads; the site reflects only the second rename, and
    // the file no longer contains the first.
    let body = format!(
        "(set-microgrid-id 7) (%make-grid-connection-point :id 1) {LOAD_OVERRIDES_HELPER}",
    );
    let cfg = config_with(&body).await;
    call(cfg.clone(), post("/api/eval", "(rename-component 1 \"a\")")).await;
    call(cfg.clone(), post("/api/eval", "(rename-component 1 \"b\")")).await;

    let (_, body) = call(cfg.clone(), get("/api/overrides")).await;
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["persisted"].as_array().unwrap().len(), 2);

    let req = axum::http::Request::builder()
        .method(axum::http::Method::DELETE)
        .uri("/api/persisted/0")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _) = call(cfg.clone(), req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = call(cfg.clone(), get("/api/overrides")).await;
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let persisted = parsed["persisted"].as_array().unwrap();
    assert_eq!(persisted.len(), 1);
    assert!(persisted[0]["source"].as_str().unwrap().contains("\"b\""));

    let (_, body) = call(cfg.clone(), get("/api/topology")).await;
    let topo: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let grid = topo["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == 1)
        .unwrap();
    assert_eq!(grid["name"], "b");

    // 404 on out-of-range idx.
    let req = axum::http::Request::builder()
        .method(axum::http::Method::DELETE)
        .uri("/api/persisted/99")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _) = call(cfg, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn persisted_bulk_remove_drops_indices_in_one_reload() {
    let body = format!(
        "(set-microgrid-id 7) (%make-grid-connection-point :id 1) {LOAD_OVERRIDES_HELPER}",
    );
    let cfg = config_with(&body).await;
    call(cfg.clone(), post("/api/eval", "(rename-component 1 \"a\")")).await;
    call(cfg.clone(), post("/api/eval", "(rename-component 1 \"b\")")).await;
    call(cfg.clone(), post("/api/eval", "(rename-component 1 \"c\")")).await;

    // Drop idx 0 + 2 → only "b" survives, site reflects "b".
    let req = axum::http::Request::builder()
        .method(axum::http::Method::POST)
        .uri("/api/persisted/delete")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(r#"{"indices":[0,2]}"#))
        .unwrap();
    let (status, body) = call(cfg.clone(), req).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["removed"], 2);

    let (_, body) = call(cfg.clone(), get("/api/overrides")).await;
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let persisted = parsed["persisted"].as_array().unwrap();
    assert_eq!(persisted.len(), 1);
    assert!(persisted[0]["source"].as_str().unwrap().contains("\"b\""));

    let (_, body) = call(cfg, get("/api/topology")).await;
    let topo: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let grid = topo["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == 1)
        .unwrap();
    assert_eq!(grid["name"], "b");
}

#[tokio::test]
async fn eval_endpoint_mutates_world() {
    // Confirm an /api/eval call that registers a component shows
    // up in the topology endpoint immediately afterwards. This is
    // the load-bearing claim of the "Lisp eval as the unifying
    // mutation API" design.
    let cfg = config_with("").await;
    let (status, _) = call(
        cfg.clone(),
        post("/api/eval", "(%make-grid-connection-point :id 42)"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, body) = call(cfg, get("/api/topology")).await;
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let components = parsed["components"].as_array().unwrap();
    assert!(components.iter().any(|c| c["id"] == 42));
}

#[tokio::test]
async fn scenario_endpoints_round_trip_lifecycle_and_events() {
    let cfg = config_with("").await;

    // Pre-start: name is null, count is 0.
    let (_, body) = call(cfg.clone(), get("/api/scenario")).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["name"].is_null());
    assert_eq!(v["event_count"], 0);

    // Start + record two events.
    call(
        cfg.clone(),
        post("/api/eval", "(scenario-start \"warmup\")"),
    )
    .await;
    call(
        cfg.clone(),
        post("/api/eval", "(scenario-event 'outage \"bat-1003\")"),
    )
    .await;
    call(
        cfg.clone(),
        post("/api/eval", "(scenario-event \"note\" \"hi\")"),
    )
    .await;

    // Summary reflects the events.
    let (status, body) = call(cfg.clone(), get("/api/scenario")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["name"], "warmup");
    assert_eq!(v["event_count"], 2);
    assert_eq!(v["next_event_id"], 2);

    // /api/scenario/events with default since=0 returns both.
    let (status, body) = call(cfg.clone(), get("/api/scenario/events")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let events = v["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["kind"], "outage");
    assert_eq!(events[1]["kind"], "note");

    // since=1 cursor returns only id 1 onward.
    let (_, body) = call(cfg.clone(), get("/api/scenario/events?since=1")).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let events = v["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["id"], 1);
}

#[tokio::test]
async fn scenario_report_endpoint_returns_main_meter_peak() {
    let cfg = config_with(
        "(set-microgrid-id 9)
         (%make-grid-connection-point
           :id 1
           :successors (list (%make-meter :id 2)))
         (scenario-start \"smoke\")
         (set-meter-power 2 4500.0)",
    )
    .await;
    // Drive the sampler so the reporter sees a peak.
    cfg.site().record_history_snapshot(Utc::now());

    let (status, body) = call(cfg, get("/api/scenario/report")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // The grid meter (fronting the connection point) is derived as main.
    assert_eq!(v["main_meter_id"], 2);
    let peak = v["peak_main_meter_w"].as_f64().unwrap();
    assert!((peak - 4500.0).abs() < 1e-3, "got peak {peak}");
}

fn seed_dispatch(
    store: &crate::sim::dispatch::SharedDispatchStore,
    mg: u64,
    id: u64,
    type_: &str,
    active: bool,
) {
    use crate::proto::dispatch as dpb;
    store.insert(
        mg,
        dpb::Dispatch {
            metadata: Some(dpb::DispatchMetadata {
                dispatch_id: id,
                ..Default::default()
            }),
            data: Some(dpb::DispatchData {
                r#type: type_.to_string(),
                is_active: active,
                ..Default::default()
            }),
        },
    );
}

#[tokio::test]
async fn dispatches_endpoint_lists_microgrid_dispatches_newest_first() {
    let cfg = config_with("").await;
    let store = cfg.dispatches();
    seed_dispatch(&store, 2200, 1, "ALPHA", true);
    seed_dispatch(&store, 2200, 2, "PEAK_SHAVE", false);
    // A dispatch for another microgrid must not leak into 2200's list.
    seed_dispatch(&store, 999, 3, "OTHER", true);

    let (status, body) = call(cfg, get("/api/mg/2200/dispatches")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // Newest (highest id) first.
    assert_eq!(arr[0]["id"], 2);
    assert_eq!(arr[0]["type"], "PEAK_SHAVE");
    assert_eq!(arr[0]["active"], false);
    assert_eq!(arr[1]["id"], 1);
    assert_eq!(arr[1]["type"], "ALPHA");
    assert_eq!(arr[1]["active"], true);
}

#[tokio::test]
async fn dispatches_endpoint_empty_for_microgrid_without_dispatches() {
    let cfg = config_with("").await;
    let (status, body) = call(cfg, get("/api/mg/4242/dispatches")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v.as_array().unwrap().is_empty());
}

fn post_json(path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn delete_req(path: &str) -> Request<Body> {
    Request::builder()
        .method(Method::DELETE)
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

fn active_dispatch(type_: &str) -> crate::proto::dispatch::DispatchData {
    crate::proto::dispatch::DispatchData {
        r#type: type_.to_string(),
        is_active: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn dispatch_create_endpoint_stores_and_returns_view() {
    let cfg = config_with("").await;
    let (status, body) = call(
        cfg.clone(),
        post_json(
            "/api/mg/2200/dispatches",
            r#"{"type":"ALPHA","target":"BATTERY","payload":{"target_power_w":5000}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["type"], "ALPHA");
    assert_eq!(v["active"], true);
    assert_eq!(v["target"], "BATTERY");
    assert_eq!(v["payload"]["target_power_w"], 5000.0);
    // start_immediately default => a start time was stamped.
    assert!(v["start_ms"].is_i64());
    assert_eq!(cfg.dispatches().list_mg(2200).len(), 1);
}

#[tokio::test]
async fn dispatch_create_endpoint_accepts_recurrence() {
    let cfg = config_with("").await;
    let (status, body) = call(
        cfg.clone(),
        post_json(
            "/api/mg/2200/dispatches",
            r#"{"type":"ALPHA","target":"battery","duration_s":3600,
                "recurrence":{"freq":"daily","interval":2}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["recurrence"], "daily ×2");
    // Recurring dispatches have no single predetermined end, even
    // with a per-occurrence duration set.
    assert!(v["end_ms"].is_null());

    // freq "once" is the explicit no-recurrence spelling.
    let (status, body) = call(
        cfg.clone(),
        post_json(
            "/api/mg/2200/dispatches",
            r#"{"type":"ALPHA","target":"battery","recurrence":{"freq":"once"}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["recurrence"].is_null());

    // Unknown frequency names are a client bug — reject loudly.
    let (status, _) = call(
        cfg,
        post_json(
            "/api/mg/2200/dispatches",
            r#"{"type":"ALPHA","target":"battery","recurrence":{"freq":"fortnightly"}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn dispatch_create_endpoint_rejects_bad_target() {
    let cfg = config_with("").await;
    let (status, _) = call(
        cfg,
        post_json(
            "/api/mg/2200/dispatches",
            r#"{"type":"X","target":"not-a-category"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn dispatch_active_endpoint_pauses_and_resumes() {
    let cfg = config_with("").await;
    let id = cfg
        .dispatches()
        .create(2200, active_dispatch("X"), true)
        .unwrap()
        .metadata
        .unwrap()
        .dispatch_id;

    let (status, body) = call(
        cfg.clone(),
        post_json(
            &format!("/api/mg/2200/dispatches/{id}/active"),
            r#"{"active":false}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["active"], false);
    assert!(
        !cfg.dispatches()
            .get(2200, id)
            .unwrap()
            .data
            .unwrap()
            .is_active
    );

    // Resume.
    let (status, _) = call(
        cfg.clone(),
        post_json(
            &format!("/api/mg/2200/dispatches/{id}/active"),
            r#"{"active":true}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        cfg.dispatches()
            .get(2200, id)
            .unwrap()
            .data
            .unwrap()
            .is_active
    );
}

#[tokio::test]
async fn dispatch_delete_endpoint_removes_then_404s() {
    let cfg = config_with("").await;
    let id = cfg
        .dispatches()
        .create(2200, active_dispatch("X"), true)
        .unwrap()
        .metadata
        .unwrap()
        .dispatch_id;

    let (status, _) = call(
        cfg.clone(),
        delete_req(&format!("/api/mg/2200/dispatches/{id}")),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(cfg.dispatches().get(2200, id).is_none());

    // Deleting again is a 404.
    let (status, _) = call(cfg, delete_req(&format!("/api/mg/2200/dispatches/{id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The typed control endpoints mutate the site without Lisp: a drive
/// lands a meter's constant power override, and every rejection is a
/// structured HTTP error (400/404 + JSON), not an `ok: false` payload.
#[tokio::test]
async fn control_drive_sets_meter_power() {
    let cfg = config_with("(%make-meter :id 7)").await;
    let (status, _) = call(
        cfg.clone(),
        post_json("/api/component/7/drive", r#"{"power_w": 1234.5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    cfg.refresh_once();
    let m = cfg.site().get(7).unwrap();
    assert!((m.aggregate_power_w(&cfg.site()) - 1234.5).abs() < 1e-3);

    // An unknown component is a 404 with the reason in the body.
    let (status, body) = call(
        cfg,
        post_json("/api/component/999/drive", r#"{"power_w": 1.0}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"].as_str().unwrap().contains("999"));
}

/// Status changes parse-then-apply: a valid health lands on the
/// component's runtime, a bad value is a 400 and changes nothing.
#[tokio::test]
async fn control_status_flips_health_and_rejects_bad_values() {
    let cfg = config_with("(%make-meter :id 7)").await;
    let (status, _) = call(
        cfg.clone(),
        post_json("/api/component/7/status", r#"{"health": "error"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        cfg.site().runtime_of(7).health,
        crate::sim::runtime::Health::Error
    );

    // A bad enum value: 400, and the health is untouched.
    let (status, body) = call(
        cfg.clone(),
        post_json("/api/component/7/status", r#"{"health": "broken"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"].as_str().unwrap().contains("health"));
    assert_eq!(
        cfg.site().runtime_of(7).health,
        crate::sim::runtime::Health::Error
    );

    // A request with one bad field applies nothing (parse-then-apply).
    let (status, _) = call(
        cfg.clone(),
        post_json(
            "/api/component/7/status",
            r#"{"health": "ok", "command_mode": "nonsense"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        cfg.site().runtime_of(7).health,
        crate::sim::runtime::Health::Error
    );
}

/// The per-microgrid variants resolve the mg first: an unregistered
/// microgrid is a 404 before the component is even looked at.
#[tokio::test]
async fn control_for_mg_requires_a_registered_microgrid() {
    let cfg = config_with("(%make-meter :id 7)").await;
    let (status, body) = call(
        cfg,
        post_json("/api/mg/33/component/7/drive", r#"{"power_w": 1.0}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"].as_str().unwrap().contains("microgrid 33"));
}

/// Driving a battery's SoC teleports its state: the test can arrange a
/// nearly-empty or nearly-full pool without simulating the charge.
#[tokio::test]
async fn control_drive_sets_battery_soc() {
    let cfg = config_with("(%make-battery :id 4 :initial-soc 60.0)").await;
    let (status, _) = call(
        cfg.clone(),
        post_json("/api/component/4/drive", r#"{"soc_pct": 11.5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let site = cfg.site();
    let soc = site.get(4).unwrap().telemetry(&site).soc_pct.unwrap();
    assert!((soc - 11.5).abs() < 1e-3, "{soc}");

    // Out-of-range values clamp instead of corrupting the state.
    let (status, _) = call(
        cfg.clone(),
        post_json("/api/component/4/drive", r#"{"soc_pct": 250.0}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let soc = site.get(4).unwrap().telemetry(&site).soc_pct.unwrap();
    assert!((soc - 100.0).abs() < 1e-3, "{soc}");
}

/// A drive stimulus that does not apply to the component's category is
/// a 400 with the reason — never a silent 200 no-op. The matching
/// stimulus on the right category still lands (sunlight covered here).
#[tokio::test]
async fn control_drive_rejects_wrong_category() {
    let cfg = config_with(
        "(%make-meter :id 7)
         (%make-solar-inverter :id 8)",
    )
    .await;

    // Sunlight on a meter: rejected.
    let (status, body) = call(
        cfg.clone(),
        post_json("/api/component/7/drive", r#"{"sunlight_pct": 80.0}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"].as_str().unwrap().contains("sunlight_pct"));

    // SoC on a meter: rejected.
    let (status, _) = call(
        cfg.clone(),
        post_json("/api/component/7/drive", r#"{"soc_pct": 50.0}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Sunlight on the solar inverter: applies.
    let (status, _) = call(
        cfg.clone(),
        post_json("/api/component/8/drive", r#"{"sunlight_pct": 25.0}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // An unknown field name is a client error too (deny_unknown_fields
    // -> axum's 422), not a silently ignored typo.
    let (status, _) = call(
        cfg,
        post_json("/api/component/7/drive", r#"{"powr_w": 1.0}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Battery topology used by the explained-formula tests:
/// grid → meter → battery-inverter → battery, mg id 2200.
const FORMULA_TOPOLOGY: &str = r#"(%make-grid-connection-point :id 1
     :successors
     (list (%make-meter :id 2
             :successors
             (list (%make-battery-inverter :id 3
                     :successors
                     (list (%make-battery :id 4)))))))"#;

#[tokio::test]
async fn formula_endpoint_returns_ast_and_explanation() {
    let cfg = config_with(FORMULA_TOPOLOGY).await;
    let (status, body) = call(cfg, get("/api/mg/2200/formula?metric=battery")).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["ok"], true, "body: {parsed}");
    assert_eq!(parsed["metric"], "battery");
    // The rendered string, its `//`-commented twin, the AST, and the
    // explanation tree all ride along.
    assert!(parsed["formula"].as_str().unwrap().contains('#'));
    assert!(parsed["commented"].as_str().unwrap().contains("//"));
    assert!(parsed["ast"].is_object());
    assert!(parsed["explanation"].is_object());
}

#[tokio::test]
async fn formula_endpoint_rejects_unknown_metric_and_bad_ids() {
    let cfg = config_with(FORMULA_TOPOLOGY).await;
    let (status, body) = call(cfg.clone(), get("/api/mg/2200/formula?metric=bogus")).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["ok"], false);
    assert!(parsed["error"].as_str().unwrap().contains("bogus"));

    let (_, body) = call(cfg, get("/api/mg/2200/formula?metric=battery&ids=1,x")).await;
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["ok"], false);
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("bad component id")
    );
}

#[tokio::test]
async fn formula_endpoint_reports_error_kind_for_missing_component() {
    let cfg = config_with(FORMULA_TOPOLOGY).await;
    let (_, body) = call(cfg, get("/api/mg/2200/formula?metric=battery&ids=99")).await;
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["kind"], "component_not_found");
}

#[tokio::test]
async fn formula_endpoint_404s_unknown_microgrid() {
    let cfg = config_with(FORMULA_TOPOLOGY).await;
    let (status, _) = call(cfg, get("/api/mg/9999/formula?metric=grid")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn microgrids_import_creates_entry_with_overrides() {
    let cfg = config_with("(%make-grid-connection-point :id 1)").await;
    let body = r#"{
      "name": "imported site",
      "components": {"electricalComponents": [
        {"id": "10", "name": "grid", "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT",
         "categorySpecificInfo": {"gridConnectionPoint": {"ratedFuseCurrent": 125}}},
        {"id": "11", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"},
        {"id": "12", "category": "ELECTRICAL_COMPONENT_CATEGORY_INVERTER",
         "categorySpecificInfo": {"inverter": {"type": "INVERTER_TYPE_BATTERY"}}},
        {"id": "13", "category": "ELECTRICAL_COMPONENT_CATEGORY_BATTERY",
         "metricConfigBounds": [
           {"metric": "METRIC_BATTERY_CAPACITY", "configBounds": {"upper": 40000}}
         ]}
      ]},
      "connections": {"electricalComponentConnections": [
        {"sourceElectricalComponentId": "10", "destinationElectricalComponentId": "11"},
        {"sourceElectricalComponentId": "11", "destinationElectricalComponentId": "12"},
        {"sourceElectricalComponentId": "12", "destinationElectricalComponentId": "13"}
      ]}
    }"#;
    let (status, resp) = call(cfg.clone(), post_json("/api/microgrids/import", body)).await;
    let parsed: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(status, StatusCode::OK, "body: {parsed}");
    assert_eq!(parsed["components"], 4);
    assert_eq!(parsed["connections"], 3);
    let id = parsed["id"].as_u64().unwrap();

    // The import populates the new site through the per-mg eval
    // path, so the components exist right away…
    let (status, topo) = call(cfg.clone(), get(&format!("/api/mg/{id}/topology"))).await;
    assert_eq!(status, StatusCode::OK);
    let topo: serde_json::Value = serde_json::from_slice(&topo).unwrap();
    assert_eq!(topo["components"].as_array().unwrap().len(), 4);
    assert_eq!(topo["connections"].as_array().unwrap().len(), 3);
    // …and the persistence gate appended the form to the overrides
    // file, with the export's physical parameters, for boot replay.
    let overrides = std::fs::read_to_string(
        cfg.microgrids_dir()
            .join(format!("config.{id}.overrides.lisp")),
    )
    .unwrap();
    assert!(overrides.contains(":rated-fuse-current 125"));
    assert!(overrides.contains("(make-battery :id 13 :capacity 40000.0)"));
    assert!(overrides.contains("(connect 12 13)"));

    // The registry lists it.
    let (_, list) = call(cfg, get("/api/microgrids")).await;
    let list: serde_json::Value = serde_json::from_slice(&list).unwrap();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"].as_u64() == Some(id) && m["name"] == "imported site")
    );
}

#[tokio::test]
async fn microgrids_import_rejects_id_collisions_atomically() {
    // Component id 1 already exists in the config's microgrid.
    let cfg = config_with("(%make-grid-connection-point :id 1)").await;
    let body = r#"{
      "name": "colliding site",
      "components": {"electricalComponents": [
        {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT"}
      ]}
    }"#;
    let (status, resp) = call(cfg.clone(), post_json("/api/microgrids/import", body)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(String::from_utf8_lossy(&resp).contains("enterprise-unique"));
    // Nothing was created.
    let (_, list) = call(cfg, get("/api/microgrids")).await;
    let list: serde_json::Value = serde_json::from_slice(&list).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn microgrids_import_serializes_racing_imports() {
    // Two imports carrying the same component id race. The import
    // lock runs them one at a time, so the loser's collision scan
    // sees the winner's components and returns 409 — component ids
    // stay enterprise-unique, with no silent duplicate.
    let cfg = config_with("(%make-grid-connection-point :id 1)").await;
    let body = |name: &str| {
        format!(
            r#"{{
      "name": "{name}",
      "components": {{"electricalComponents": [
        {{"id": "40", "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT"}}
      ]}}
    }}"#
        )
    };
    let (a, b) = tokio::join!(
        call(
            cfg.clone(),
            post_json("/api/microgrids/import", &body("site a"))
        ),
        call(
            cfg.clone(),
            post_json("/api/microgrids/import", &body("site b"))
        ),
    );
    let statuses = [a.0, b.0];
    assert!(statuses.contains(&StatusCode::OK), "{statuses:?}");
    assert!(statuses.contains(&StatusCode::CONFLICT), "{statuses:?}");
    // Exactly one import landed next to the config's own microgrid.
    let (_, list) = call(cfg, get("/api/microgrids")).await;
    let list: serde_json::Value = serde_json::from_slice(&list).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn microgrids_import_rejects_unsupported_category() {
    let cfg = config_with("(%make-grid-connection-point :id 1)").await;
    let body = r#"{
      "name": "hvac site",
      "components": {"electricalComponents": [
        {"id": "10", "category": "ELECTRICAL_COMPONENT_CATEGORY_HVAC"}
      ]}
    }"#;
    let (status, resp) = call(cfg, post_json("/api/microgrids/import", body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&resp).contains("cannot simulate"));
}

/// set-component-operational-mode is a CONFIG change: it persists
/// through the overrides gate (unlike the runtime pokes), and the
/// runtime knobs derive from it.
#[tokio::test]
async fn operational_mode_eval_persists_and_derives() {
    let cfg = config_with("(%make-grid-connection-point :id 1)").await;
    let (status, body) = call(
        cfg.clone(),
        post(
            "/api/eval",
            "(set-component-operational-mode 1 'control-only)",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["ok"], true, "body: {parsed}");

    // Persisted: the overrides list carries the eval.
    let (_, body) = call(cfg.clone(), get("/api/overrides")).await;
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let persisted = parsed["persisted"].as_array().unwrap();
    assert!(
        persisted
            .iter()
            .any(|e| e["source"].as_str().unwrap().contains("operational-mode")),
        "expected the mode eval in the overrides list: {parsed}"
    );

    // Derived: the topology snapshot shows the mode and the silenced
    // stream.
    let (_, body) = call(cfg.clone(), get("/api/topology")).await;
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let c = &parsed["components"][0];
    assert_eq!(c["operational_mode"], "control-only");
    assert_eq!(c["telemetry_mode"], "silent");

    // Enforced: poking the stream back to normal is rejected while
    // the mode forbids telemetry.
    let (_, body) = call(
        cfg,
        post("/api/eval", "(set-component-telemetry-mode 1 'normal)"),
    )
    .await;
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["ok"], false);
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("streams no telemetry")
    );
}
