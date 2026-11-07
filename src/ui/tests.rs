use super::*;
use crate::engine::{Engine, Metadata};
use crate::sim::MicrogridSite;
use crate::sim::sim_clock::NowSource;
use crate::test_dir::TestDir;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use chrono::Utc;
use serde_json::json;
use tower::ServiceExt;

/// A live engine on a fresh state directory, holding the microgrid
/// whose spec is `mg` (JSON). The microgrid is 2200 on gRPC port 8800
/// unless `mg` names its own `id` or `grpc_port`. No runtime is
/// started.
///
/// The directory is the engine's state directory; dropping it removes
/// it, so keep it as long as the engine.
async fn engine_with(mg: &str) -> (Engine, TestDir) {
    let dir = TestDir::new("macrocosim-ui-");
    let engine = Engine::new(dir.to_path_buf(), Metadata::default(), NowSource::wall());
    add_microgrid(&engine, mg).await;
    (engine, dir)
}

/// Create the microgrid whose spec is `mg` (JSON) on `engine`, as
/// [`engine_with`] does.
async fn add_microgrid(engine: &Engine, mg: &str) {
    let mut spec: serde_json::Value = serde_json::from_str(mg).expect("spec JSON");
    let fields = spec.as_object_mut().expect("a spec object");
    fields.entry("id").or_insert(json!(2200));
    fields.entry("grpc_port").or_insert(json!(8800));
    let spec = serde_json::from_value(spec).expect("a microgrid spec");
    crate::engine::topology::create(
        &engine.structural().await,
        &crate::runtime::MicrogridRuntimes::inert(),
        spec,
        None,
    )
    .await
    .expect("create the microgrid");
}

/// The site of microgrid 2200.
fn site(engine: &Engine) -> MicrogridSite {
    site_of(engine, 2200)
}

/// The site of microgrid `mg`.
fn site_of(engine: &Engine, mg: u64) -> MicrogridSite {
    engine.microgrids().lock()[&mg].site.clone()
}

/// One-shot a request and return (status, body). axum's `oneshot`
/// avoids binding a real port. The runtimes are inert, so nothing is
/// started and every loopback slot is empty: `metrics/status`
/// answers "not connected" without a real gRPC server. Tests that
/// want a populated handle would have to spin up the gRPC server
/// too.
async fn call(engine: Engine, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    call_with(engine, crate::runtime::MicrogridRuntimes::inert(), req).await
}

/// [`call`] with a runtimes handle the test keeps across requests.
async fn call_with(
    engine: Engine,
    runtimes: crate::runtime::MicrogridRuntimes,
    req: Request<Body>,
) -> (StatusCode, Vec<u8>) {
    let (status, _, body) = call_full(engine, runtimes, req).await;
    (status, body)
}

/// [`call_with`] that also returns the response headers.
async fn call_full(
    engine: Engine,
    runtimes: crate::runtime::MicrogridRuntimes,
    req: Request<Body>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let resp = router(engine, runtimes, std::sync::Arc::default(), None)
        .oneshot(req)
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, headers, bytes.to_vec())
}

/// [`call`] for a reply that must be JSON: panics unless its
/// `content-type` is `application/json`.
async fn call_json(engine: Engine, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let (status, headers, body) =
        call_full(engine, crate::runtime::MicrogridRuntimes::inert(), req).await;
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    assert_eq!(
        content_type,
        Some("application/json"),
        "{status} {}",
        String::from_utf8_lossy(&body)
    );
    (status, body)
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

/// The `error` field of a JSON error body; panics with the raw body
/// when it is not one.
fn error_of(body: &[u8]) -> String {
    let v: serde_json::Value = serde_json::from_slice(body)
        .unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(body)));
    v["error"]
        .as_str()
        .unwrap_or_else(|| panic!("no error field: {v}"))
        .to_string()
}

/// A topology document holding the one microgrid `mg` (JSON).
fn mg_doc(mg: &str) -> String {
    format!(r#"{{"schema_version": 1, "microgrids": [{mg}]}}"#)
}

/// The first microgrid of a `POST /api/microgrids` answer.
fn first_created(body: &[u8]) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_slice(body)
        .unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(body)));
    v["microgrids"][0].clone()
}

#[tokio::test]
async fn index_serves_embedded_shell() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(engine, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    let s = String::from_utf8_lossy(&body);
    assert!(s.contains("<title>macrocosim</title>"));
    assert!(s.contains("/assets/app.js"));
}

#[tokio::test]
async fn asset_route_serves_embedded_files() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(engine, get("/assets/app.js")).await;
    assert_eq!(status, StatusCode::OK);
    // Phrase from app.js — anchors the test against actually
    // serving the right file rather than just any 200.
    assert!(String::from_utf8_lossy(&body).contains("vis-network"));
}

#[tokio::test]
async fn asset_route_serves_vendored_lib() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(engine, get("/assets/vendor/vis-network.min.js")).await;
    assert_eq!(status, StatusCode::OK);
    // The bundle names itself throughout; "vis" alone would match
    // almost any JS file ("visibility", "provision", ...).
    assert!(String::from_utf8_lossy(&body).contains("vis-network"));
}

#[tokio::test]
async fn asset_route_404s_unknown_path() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, _) = call(engine, get("/assets/does-not-exist.js")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn topology_endpoint_emits_components_and_connections() {
    let (engine, _dir) = engine_with(
        r#"{"components": [
            {"type": "grid_connection_point", "id": 1},
            {"type": "meter", "id": 2},
            {"type": "battery", "id": 3}],
            "connections": [[1, 2], [2, 3]]}"#,
    )
    .await;

    let (status, body) = call(engine, get("/api/mg/2200/topology")).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(parsed["components"].as_array().unwrap().len(), 3);
    assert_eq!(parsed["connections"].as_array().unwrap().len(), 2);
}

/// Every per-microgrid route answers the shared 404 for an
/// unregistered microgrid, and 400 for a non-numeric one.
#[tokio::test]
async fn every_microgrid_route_shares_the_unregistered_404() {
    let (engine, _dir) = engine_with("{}").await;
    let gets = [
        "topology",
        "formula?metric=grid",
        "weather",
        "metrics/status",
        "metrics/latest",
        "metrics/history",
        "metrics/formulas",
        "component/1",
        "component/1/history?metric=active_power",
        "component/1/setpoints",
        "component/1/ev",
        "undo",
        "spec",
        "spec.py",
        "recording",
        "recording/events",
        "recording/report",
        "recording/csv",
        "recording/csv/x.csv",
        "dispatches",
    ];
    for suffix in gets {
        let (status, body) = call(engine.clone(), get(&format!("/api/mg/9999/{suffix}"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "GET {suffix}");
        assert_eq!(
            error_of(&body),
            "microgrid 9999 not registered",
            "GET {suffix}"
        );
    }
    let posts = [
        ("component/1/status", "{}"),
        ("component/1/drive", "{}"),
        ("component/1/runtime", "{}"),
        ("component/1/ev", "{}"),
        ("weather", "{}"),
        ("grid", "{}"),
        ("undo", ""),
        ("redo", ""),
        ("recording/start", r#"{"name":"x"}"#),
        ("recording/stop", ""),
        ("recording/event", r#"{"kind":"x"}"#),
        ("dispatches", "{}"),
        ("dispatches/1/active", "{}"),
        ("sample-lag", "{}"),
        ("streams/cancel", ""),
    ];
    for (suffix, body) in posts {
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!("/api/mg/9999/{suffix}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let (status, resp) = call(engine.clone(), req).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "POST {suffix}");
        assert_eq!(
            error_of(&resp),
            "microgrid 9999 not registered",
            "POST {suffix}"
        );
    }
    let req = Request::builder()
        .method(Method::PUT)
        .uri("/api/mg/9999/spec")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let (status, resp) = call(engine.clone(), req).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "PUT spec");
    assert_eq!(error_of(&resp), "microgrid 9999 not registered");
    let (status, resp) = call(engine.clone(), delete_req("/api/mg/9999")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "DELETE the microgrid");
    assert_eq!(error_of(&resp), "microgrid 9999 not registered");
    for suffix in ["dispatches/1", "component/1/ev", "weather"] {
        let req = delete_req(&format!("/api/mg/9999/{suffix}"));
        let (status, resp) = call(engine.clone(), req).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "DELETE {suffix}");
        assert_eq!(
            error_of(&resp),
            "microgrid 9999 not registered",
            "DELETE {suffix}"
        );
    }
    let (status, body) = call(engine, get("/api/mg/abc/topology")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_of(&body), "invalid microgrid id: \"abc\"");
}

/// An engine that runs no script refuses every hello with 403.
#[tokio::test]
async fn a_hello_without_a_script_is_refused() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/runner/hello",
            r#"{"token": "t", "client_version": "0.0.0", "topology": {"schema_version": 1, "microgrids": []}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_of(&body), "this engine runs no script");
    assert_eq!(engine.microgrids.lock().len(), 1, "nothing changed");
}

/// Per-microgrid routes outside `/api/mg/{mg}`, and the routes the
/// HTTP API no longer has, answer 404 `no route`.
#[tokio::test]
async fn unscoped_microgrid_routes_are_gone() {
    let (engine, _dir) = engine_with("{}").await;
    for path in [
        "/api/topology",
        "/api/weather",
        "/api/history?id=1&metric=active_power",
        "/api/setpoints?id=1",
        "/api/component?id=1",
        "/api/microgrid/status",
        "/api/microgrid/latest",
        "/api/microgrid/history",
        "/api/microgrid/formulas",
        "/api/scenario",
        "/api/scenario/report",
        "/api/mg/2200/microgrid/status",
        "/api/mg/2200/component?id=1",
        "/api/mg/2200/history?id=1&metric=active_power",
        "/api/mg/2200/setpoints?id=1",
        "/api/mg/2200/ev/1",
        "/api/symbols",
        "/api/clock",
        "/api/scenarios",
        "/api/scripts",
        "/api/mg/2200/snapshots",
        "/api/mg/2200/scenario",
        "/api/mg/2200/scenario/events",
        "/api/mg/2200/scenario/report",
        "/api/mg/2200/scenario/csv",
    ] {
        let (status, body) = call(engine.clone(), get(path)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(error_of(&body).starts_with("no route for GET"), "{path}");
    }
    for path in [
        "/api/microgrids/create",
        "/api/mg/2200/snapshots/save",
        "/api/eval",
        "/api/mg/2200/eval",
        "/api/format",
        "/api/scenarios/stop",
        "/api/scenarios/x/start",
        "/api/load",
        "/api/load-as",
        "/api/mg/2200/adopt",
        "/api/mg/2200/snapshots",
        "/api/mg/2200/snapshots/load",
    ] {
        let (status, body) = call(engine.clone(), post_json(path, "{}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "POST {path}");
        assert!(error_of(&body).starts_with("no route for POST"), "{path}");
    }
}

/// A registered microgrid whose runtime never started reports
/// "not connected", not "not registered".
#[tokio::test]
async fn metrics_for_a_microgrid_without_a_loopback_are_not_connected() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(engine, get("/api/mg/2200/metrics/status")).await;
    assert_ne!(
        status,
        StatusCode::NOT_FOUND,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["connected"], false);
}

/// Success bodies carry no `ok`; actions with nothing to return are
/// 204; metrics without a client report "not connected".
#[tokio::test]
async fn success_bodies_are_plain() {
    let (engine, _dir) =
        engine_with(r#"{"components": [{"type": "grid_connection_point", "id": 1}]}"#).await;

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/mg/2200/component/1/status")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"health":"ok"}"#))
        .unwrap();
    let (status, body) = call(engine.clone(), req).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(body.is_empty());

    let (status, body) = call(engine.clone(), get("/api/mg/2200/undo")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v.get("ok").is_none(), "{v}");
    assert!(v.get("undo_depth").is_some(), "{v}");

    let (status, body) = call(engine.clone(), get("/api/mg/2200/metrics/status")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"connected": false, "component_count": null})
    );

    let (status, body) = call(engine.clone(), get("/api/mg/2200/metrics/latest")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({})
    );

    let (status, body) = call(engine, get("/api/mg/2200/metrics/formulas")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_of(&body), "metrics client not connected");
}

/// Creating microgrids is a POST of a topology document to the
/// collection, answering 201 with each one's id, port and runtime.
/// They live in memory only, with a stored spec, and are unsaved.
#[tokio::test]
async fn creating_a_microgrid_answers_201() {
    let (engine, dir) = engine_with("{}").await;
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/microgrids",
            r#"{"schema_version": 1, "microgrids": [
                {"name": "b", "id": 2301, "components": [{"type": "meter", "id": 5000}]},
                {"name": "c", "components": []}]}"#,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let created = v["microgrids"].as_array().unwrap();
    assert_eq!(created.len(), 2);
    assert_eq!(created[0]["id"], 2301);
    assert!(created[0]["grpc_port"].as_u64().is_some(), "{v}");
    assert!(created[0]["runtime"].is_null(), "inert runtimes: {v}");
    assert_eq!(created[1]["id"], 2201, "the lowest free id");
    let registry = engine.microgrids();
    let registry = registry.lock();
    let entry = &registry[&2301];
    assert!(entry.unsaved && entry.source.is_none());
    assert!(entry.site.get(5000).is_some());
    assert!(!dir.join("microgrids").exists(), "nothing is written");
}

/// A create refuses a component without an id (422), and a document
/// whose second microgrid is refused leaves nothing behind.
#[tokio::test]
async fn a_refused_create_creates_nothing() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/microgrids",
            &mg_doc(r#"{"name": "b", "components": [{"type": "meter"}]}"#),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error_of(&body).contains("has no id"), "{}", error_of(&body));
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/microgrids",
            r#"{"schema_version": 1, "microgrids": [
                {"id": 50, "components": [{"type": "meter", "id": 5000}]},
                {"id": 2200, "components": []}]}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(error_of(&body).contains("2200"), "{}", error_of(&body));
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/microgrids",
            &mg_doc(r#"{"id": 51, "components": [{"type": "meter", "id": 7}]}"#),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        error_of(&body).contains("component id 7 is already in microgrid 2200"),
        "{}",
        error_of(&body)
    );
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/microgrids",
            r#"{"schema_version": 2, "microgrids": []}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let ids: Vec<u64> = engine.microgrids().lock().keys().copied().collect();
    assert_eq!(ids, [2200]);
}

/// A formula error is a 400; one with a kind keeps it beside
/// `error`.
#[tokio::test]
async fn a_formula_error_is_400_with_its_kind() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call_json(engine, get("/api/mg/2200/formula?metric=grid")).await;
    // The default test microgrid has no components.
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_of(&body), "The microgrid has no components yet.");

    let (engine, _dir) = engine_with(FORMULA_TOPOLOGY).await;
    let (status, body) = call_json(
        engine,
        get("/api/mg/2200/formula?metric=battery&component_ids=99"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["kind"], "component_not_found");
    assert!(v.get("ok").is_none(), "{v}");
}

/// `/api/defaults` serves the spec defaults table: one object per
/// component type, keyed by its `"type"` tag, with JSON field names.
#[tokio::test]
async fn defaults_endpoint_serves_the_spec_table() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(engine, get("/api/defaults")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        v["meter"],
        json!({"interval_s": 0.2, "stream_jitter_pct": 4.0})
    );
    assert_eq!(v["grid_connection_point"]["rated_fuse_current_a"], 100);
    assert_eq!(v["battery"]["health"], "ok");
    assert_eq!(v["breaker"], json!({"stream_jitter_pct": 0.0}));
    assert_eq!(v.as_object().unwrap().len(), 11, "{v}");
}

#[tokio::test]
async fn history_endpoint_returns_recent_samples() {
    // Build a site with a battery, then drive the sampler twice
    // synchronously so the rings have content to query. Battery
    // publishes soc_pct in its telemetry; that's what we query.
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "battery", "id": 1000}]}"#).await;
    let site = site(&engine);
    let now = chrono::Utc::now();
    site.record_history_snapshot(now - chrono::Duration::seconds(2));
    site.record_history_snapshot(now - chrono::Duration::seconds(1));

    let (status, body) = call(
        engine,
        get("/api/mg/2200/component/1000/history?metric=soc_pct&window_s=10"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["component_id"], 1000);
    assert_eq!(parsed["metric"], "soc_pct");
    let samples = parsed["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 2);
    // Each sample is [t_s, value], with t_s in epoch seconds.
    let t0 = samples[0][0].as_f64().unwrap();
    let t1 = samples[1][0].as_f64().unwrap();
    assert!((1.7e9..1e10).contains(&t0), "{t0}");
    assert!(t0 < t1);
}

#[tokio::test]
async fn history_endpoint_rejects_unknown_metric() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(engine, get("/api/mg/2200/component/1/history?metric=foo")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error_of(&body).contains("unknown metric"));
}

#[tokio::test]
async fn history_endpoint_rejects_unknown_component() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(
        engine,
        get("/api/mg/2200/component/999/history?metric=active_power_w"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_of(&body), "component 999 not found");
}

#[tokio::test]
async fn history_endpoint_returns_empty_for_a_known_component_without_samples() {
    // No sampler tick has run, so the battery's ring is still empty.
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "battery", "id": 1000}]}"#).await;
    let (status, body) = call(
        engine,
        get("/api/mg/2200/component/1000/history?metric=soc_pct"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["samples"].as_array().unwrap().is_empty());
}

/// A component a spec `PUT` adds shows in the topology route right
/// away.
#[tokio::test]
async fn a_spec_put_shows_in_the_topology_at_once() {
    let (engine, _dir) = engine_with("{}").await;
    let spec = json!({"components": [{"type": "grid_connection_point", "id": 42}]});
    let (status, body) = call(engine.clone(), put_json("/api/mg/2200/spec", &spec)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    let (_, body) = call(engine, get("/api/mg/2200/topology")).await;
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let components = parsed["components"].as_array().unwrap();
    assert!(components.iter().any(|c| c["id"] == 42));
}

/// A recording's start, events and summary round-trip over the
/// recording routes, and `since` pages the events.
#[tokio::test]
async fn recording_routes_round_trip_lifecycle_and_events() {
    let (engine, _dir) = engine_with("{}").await;

    // Before a start: no name, no events.
    let (_, body) = call(engine.clone(), get("/api/mg/2200/recording")).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["name"].is_null());
    assert_eq!(v["event_count"], 0);

    // Start, then record two events.
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/recording/start", r#"{"name": "warmup"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/recording/event",
            r#"{"kind": "outage", "payload": "bat-1003"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/recording/event",
            r#"{"kind": "note", "payload": {"text": "hi"}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["id"], 1);

    // The summary counts the events.
    let (status, body) = call(engine.clone(), get("/api/mg/2200/recording")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["name"], "warmup");
    assert_eq!(v["event_count"], 2);
    assert_eq!(v["next_event_id"], 2);

    // The events route with the default since=0 returns both; a
    // payload that is not a string is kept as its JSON text.
    let (status, body) = call(engine.clone(), get("/api/mg/2200/recording/events")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let events = v["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["kind"], "outage");
    assert_eq!(events[0]["payload"], "bat-1003");
    assert_eq!(events[1]["kind"], "note");
    assert_eq!(events[1]["payload"], r#"{"text":"hi"}"#);

    // since=1 returns id 1 onward.
    let (_, body) = call(engine.clone(), get("/api/mg/2200/recording/events?since=1")).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let events = v["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["id"], 1);
}

/// A recording start answers 204 with no body, and a second start
/// while it runs is a JSON 409. The stop answers the report, ended by
/// the stop; a second stop is a 409.
#[tokio::test]
async fn recording_start_and_stop() {
    let (engine, _dir) = engine_with("{}").await;
    let start = || post_json("/api/mg/2200/recording/start", r#"{"name": "tiny"}"#);
    let (status, body) = call(engine.clone(), start()).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(body.is_empty());
    let (status, body) = call_json(engine.clone(), start()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        error_of(&body),
        "a recording is already running in microgrid 2200"
    );

    let (status, body) = call(engine.clone(), post("/api/mg/2200/recording/stop", "")).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["name"], "tiny", "{v}");
    assert_eq!(v["ended_by"], "stop", "{v}");

    let (status, body) = call_json(engine, post("/api/mg/2200/recording/stop", "")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_of(&body), "no recording is running in microgrid 2200");
}

#[tokio::test]
async fn recording_report_returns_grid_peak() {
    let (engine, _dir) = engine_with(
        r#"{"components": [
            {"type": "grid_connection_point", "id": 1},
            {"type": "meter", "id": 2}],
            "connections": [[1, 2]]}"#,
    )
    .await;
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/recording/start", r#"{"name": "smoke"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // The peak rides the loopback's grid_power formula stream. This
    // fixture serves HTTP without a microgrid loopback, so the sample
    // is handed to the site hook directly — the same call the
    // forwarder makes.
    site(&engine).record_grid_power_sample(4500.0, Utc::now());

    let (status, body) = call(engine, get("/api/mg/2200/recording/report")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // No meter id: the report has no main meter.
    assert!(v.get("main_meter_id").is_none(), "{v}");
    let peak = v["peak_grid_w"].as_f64().unwrap();
    assert!((peak - 4500.0).abs() < 1e-3, "got peak {peak}");
}

/// Each microgrid records on its own journal: a recording started on
/// one leaves the other's summary, events, report and CSV files
/// empty.
#[tokio::test]
async fn recording_readouts_read_each_microgrids_own_journal() {
    let (engine, dir) = engine_with(r#"{"components": [{"type": "meter", "id": 1}]}"#).await;
    add_microgrid(&engine, r#"{"id": 2201, "grpc_port": 8802}"#).await;
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/recording/start",
            r#"{"name": "two", "csv": true}"#,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/recording/event",
            r#"{"kind": "note", "payload": "hi"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(engine.clone(), get("/api/mg/2200/recording")).await;
    assert_eq!(status, StatusCode::OK);
    let first: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(first["name"], "two");
    assert_eq!(first["event_count"], 1);
    let started = first["started_at"]
        .as_str()
        .expect("started_at is a string");
    assert!(
        started.len() == 24 && started.ends_with('Z') && &started[19..20] == ".",
        "RFC 3339 with milliseconds and Z: {first}"
    );
    assert!(first["elapsed_s"].is_f64(), "{first}");

    let (status, body) = call(engine.clone(), get("/api/mg/2201/recording")).await;
    assert_eq!(status, StatusCode::OK);
    let second: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(second["name"].is_null(), "{second}");
    assert_eq!(second["event_count"], 0);

    let (status, body) = call(engine.clone(), get("/api/mg/2200/recording/events")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
    let (status, body) = call(engine.clone(), get("/api/mg/2201/recording/events")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["events"].as_array().unwrap().is_empty(), "{v}");

    // The CSV files belong to the first microgrid alone, under the
    // state directory.
    let (status, body) = call(engine.clone(), get("/api/mg/2200/recording/csv")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["files"], json!(["1-meter.csv"]), "{v}");
    let csv_dir = std::path::PathBuf::from(v["dir"].as_str().unwrap());
    assert!(
        csv_dir.starts_with(dir.join("recordings").join("2200")),
        "{v}"
    );
    let (status, _) = call(
        engine.clone(),
        get("/api/mg/2200/recording/csv/1-meter.csv"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(engine.clone(), get("/api/mg/2201/recording/csv")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["dir"].is_null(), "{v}");
    assert!(v["files"].as_array().unwrap().is_empty(), "{v}");
    let (status, body) = call(
        engine.clone(),
        get("/api/mg/2201/recording/csv/1-meter.csv"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!error_of(&body).is_empty());

    let (status, body) = call(engine.clone(), get("/api/mg/2201/recording/report")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["name"].is_null(), "{v}");
    let (status, body) = call(engine, get("/api/mg/9999/recording")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_of(&body), "microgrid 9999 not registered");
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
    let (engine, _dir) = engine_with("{}").await;
    let store = engine.dispatches();
    seed_dispatch(&store, 2200, 1, "ALPHA", true);
    seed_dispatch(&store, 2200, 2, "PEAK_SHAVE", false);
    // A dispatch for another microgrid must not leak into 2200's list.
    seed_dispatch(&store, 999, 3, "OTHER", true);

    let (status, body) = call(engine, get("/api/mg/2200/dispatches")).await;
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
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(engine, get("/api/mg/2200/dispatches")).await;
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
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(
        engine.clone(),
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
    assert!(
        chrono::DateTime::parse_from_rfc3339(v["start"].as_str().unwrap()).is_ok(),
        "{v}"
    );
    assert!(
        chrono::DateTime::parse_from_rfc3339(v["created_at"].as_str().unwrap()).is_ok(),
        "{v}"
    );
    assert!(
        chrono::DateTime::parse_from_rfc3339(v["updated_at"].as_str().unwrap()).is_ok(),
        "{v}"
    );
    assert_eq!(engine.dispatches().list_mg(2200).len(), 1);
}

#[tokio::test]
async fn dispatch_start_is_an_rfc3339_instant() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/dispatches",
            r#"{"type":"ALPHA","target":"BATTERY","start":"2026-10-06T12:00:00Z"}"#,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );

    let (status, body) = call(engine, get("/api/mg/2200/dispatches")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let start = chrono::DateTime::parse_from_rfc3339(v[0]["start"].as_str().unwrap()).unwrap();
    let want = chrono::DateTime::parse_from_rfc3339("2026-10-06T12:00:00Z").unwrap();
    assert_eq!(start, want);
}

#[tokio::test]
async fn dispatch_start_that_is_not_rfc3339_is_400_naming_the_field() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/dispatches",
            r#"{"type":"ALPHA","target":"BATTERY","start":"noon"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error_of(&body).contains("start"), "{}", error_of(&body));
    assert!(engine.dispatches().list_mg(2200).is_empty());
}

/// A create body with the old `start_ms` field is refused with 422
/// naming it.
#[tokio::test]
async fn dispatch_create_refuses_start_ms() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call_json(
        engine.clone(),
        post_json(
            "/api/mg/2200/dispatches",
            r#"{"type":"ALPHA","target":"BATTERY","start_ms":1791288000000}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error_of(&body).contains("start_ms"), "{}", error_of(&body));
    assert!(engine.dispatches().list_mg(2200).is_empty());
}

#[tokio::test]
async fn dispatch_create_endpoint_accepts_recurrence() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(
        engine.clone(),
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
    assert!(v["end"].is_null());

    // freq "once" is the explicit no-recurrence spelling.
    let (status, body) = call(
        engine.clone(),
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
        engine,
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
    let (engine, _dir) = engine_with("{}").await;
    let (status, _) = call(
        engine,
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
    let (engine, _dir) = engine_with("{}").await;
    let id = engine
        .dispatches()
        .create(2200, active_dispatch("X"), true)
        .unwrap()
        .metadata
        .unwrap()
        .dispatch_id;

    let (status, body) = call(
        engine.clone(),
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
        !engine
            .dispatches()
            .get(2200, id)
            .unwrap()
            .data
            .unwrap()
            .is_active
    );

    // Resume.
    let (status, _) = call(
        engine.clone(),
        post_json(
            &format!("/api/mg/2200/dispatches/{id}/active"),
            r#"{"active":true}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        engine
            .dispatches()
            .get(2200, id)
            .unwrap()
            .data
            .unwrap()
            .is_active
    );
}

#[tokio::test]
async fn dispatch_delete_endpoint_removes_then_404s() {
    let (engine, _dir) = engine_with("{}").await;
    let id = engine
        .dispatches()
        .create(2200, active_dispatch("X"), true)
        .unwrap()
        .metadata
        .unwrap()
        .dispatch_id;

    let (status, _) = call(
        engine.clone(),
        delete_req(&format!("/api/mg/2200/dispatches/{id}")),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(engine.dispatches().get(2200, id).is_none());

    // Deleting again is a 404.
    let (status, _) = call(engine, delete_req(&format!("/api/mg/2200/dispatches/{id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The typed control endpoints mutate the site: a drive lands a
/// meter's constant power override, and every rejection is a
/// structured HTTP error (400/404 + JSON), not an `ok: false`
/// payload.
#[tokio::test]
async fn control_drive_sets_meter_power() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/7/drive", r#"{"power_w": 1234.5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let m = site(&engine).get(7).unwrap();
    assert!((m.aggregate_power_w(&site(&engine)) - 1234.5).abs() < 1e-3);

    // An unknown component is a 404 with the reason in the body.
    let (status, body) = call(
        engine,
        post_json("/api/mg/2200/component/999/drive", r#"{"power_w": 1.0}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"].as_str().unwrap().contains("999"));
}

/// The drive op's reactive twins mirror `power_w`: `reactive_power_var` lands
/// a constant Q override, `power_factor` (+ optional `leading`) holds Q
/// at a power factor tracking live P. A non-meter and an out-of-range
/// power_factor are both 400s.
#[tokio::test]
async fn drive_op_accepts_reactive_var_and_power_factor() {
    let (engine, _dir) = engine_with(
        r#"{"components": [
            {"type": "meter", "id": 7, "power_w": 8000.0},
            {"type": "solar_inverter", "id": 8}]}"#,
    )
    .await;

    // reactive_var: constant Q override.
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/drive",
            r#"{"reactive_power_var": 500.0}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let m = site(&engine).get(7).unwrap();
    assert!((m.aggregate_reactive_var(&site(&engine)) - 500.0).abs() < 1e-3);

    // power_factor + leading: Q derives from live P, sign flipped.
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/drive",
            r#"{"power_factor": 0.8, "leading": true}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!((m.aggregate_reactive_var(&site(&engine)) - -6_000.0).abs() < 1.0);

    // A non-meter rejects both new fields.
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/8/drive",
            r#"{"reactive_power_var": 100.0}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("reactive_power_var")
    );

    let (status, body) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/8/drive", r#"{"power_factor": 0.8}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"].as_str().unwrap().contains("power_factor"));

    // power_factor out of (0.0, 1.0] is a 400 naming the range.
    let (status, body) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/7/drive", r#"{"power_factor": 1.5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"].as_str().unwrap().contains("(0.0, 1.0]"));

    // leading without power_factor is an invalid request too.
    let (status, body) = call(
        engine,
        post_json("/api/mg/2200/component/7/drive", r#"{"leading": true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"].as_str().unwrap().contains("leading"));
}

/// `reactive_var` and `power_factor` drive the same Q slot, so sending
/// both is a 400 — not a 200 where the second silently overwrites the
/// first. Validate-first: the meter's existing Q override is untouched.
#[tokio::test]
async fn drive_op_rejects_reactive_var_with_power_factor() {
    let (engine, _dir) =
        engine_with(r#"{"components": [{"type": "meter", "id": 7, "power_w": 8000.0}]}"#).await;

    // Land a Q override first, so a silent overwrite would be visible.
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/drive",
            r#"{"reactive_power_var": 500.0}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/drive",
            r#"{"reactive_power_var": 100.0, "power_factor": 0.8}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let err = parsed["error"].as_str().unwrap();
    assert!(err.contains("reactive_power_var"), "{err}");
    assert!(err.contains("power_factor"), "{err}");

    // Nothing applied: the earlier override still stands.
    let m = site(&engine).get(7).unwrap();
    assert!((m.aggregate_reactive_var(&site(&engine)) - 500.0).abs() < 1e-3);
}

/// Status changes parse-then-apply: a valid health lands on the
/// component's runtime, a bad value is a 400 and changes nothing.
#[tokio::test]
async fn control_status_flips_health_and_rejects_bad_values() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/7/status", r#"{"health": "error"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        site(&engine).runtime_of(7).health,
        crate::sim::runtime::Health::Error
    );

    // A bad enum value: 400, and the health is untouched.
    let (status, body) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/7/status", r#"{"health": "broken"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"].as_str().unwrap().contains("health"));
    assert_eq!(
        site(&engine).runtime_of(7).health,
        crate::sim::runtime::Health::Error
    );

    // A request with one bad field applies nothing (parse-then-apply).
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/status",
            r#"{"health": "ok", "command_mode": "nonsense"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        site(&engine).runtime_of(7).health,
        crate::sim::runtime::Health::Error
    );

    // 0.1's hyphenated spellings are refused like any other value.
    for body in [
        r#"{"command_mode": "over-bound"}"#,
        r#"{"telemetry_mode": "error-empty"}"#,
    ] {
        let (status, _) = call(
            engine.clone(),
            post_json("/api/mg/2200/component/7/status", body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
}

/// `health=error` forces the command channel shut; an explicit
/// `command_mode=normal` in the same request must not re-open it.
#[tokio::test]
async fn control_status_health_error_forbids_command_normal() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/status",
            r#"{"health": "error", "command_mode": "normal"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"].as_str().unwrap().contains("health=error"));
    // Nothing applied: health is still the default.
    assert_eq!(
        site(&engine).runtime_of(7).health,
        crate::sim::runtime::Health::Ok
    );
}

/// The per-microgrid variants resolve the mg first: an unregistered
/// microgrid is a 404 before the component is even looked at.
#[tokio::test]
async fn control_for_mg_requires_a_registered_microgrid() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let (status, body) = call(
        engine,
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
    let (engine, _dir) =
        engine_with(r#"{"components": [{"type": "battery", "id": 4, "initial_soc_pct": 60.0}]}"#)
            .await;
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/4/drive", r#"{"soc_pct": 11.5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let site = site(&engine);
    let soc = site.get(4).unwrap().telemetry(&site).soc_pct.unwrap();
    assert!((soc - 11.5).abs() < 1e-3, "{soc}");

    // Out-of-range values clamp instead of corrupting the state.
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/4/drive", r#"{"soc_pct": 250.0}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let soc = site.get(4).unwrap().telemetry(&site).soc_pct.unwrap();
    assert!((soc - 100.0).abs() < 1e-3, "{soc}");
}

/// A drive stimulus that does not apply to the component's category is
/// a 400 with the reason — never a silent 200 no-op. The matching
/// stimulus on the right category still lands (sunlight covered here).
#[tokio::test]
async fn control_drive_rejects_wrong_category() {
    let (engine, _dir) = engine_with(
        r#"{"components": [
            {"type": "meter", "id": 7},
            {"type": "solar_inverter", "id": 8}]}"#,
    )
    .await;

    // Sunlight on a meter: rejected.
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/drive",
            r#"{"sunlight_pct": 80.0}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"].as_str().unwrap().contains("sunlight_pct"));

    // SoC on a meter: rejected.
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/7/drive", r#"{"soc_pct": 50.0}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Sunlight on the solar inverter: applies.
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/8/drive",
            r#"{"sunlight_pct": 25.0}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // An unknown field name is a client error too
    // (deny_unknown_fields -> axum's 422 naming the field), not a
    // silently ignored typo.
    let (status, body) = call(
        engine,
        post_json("/api/mg/2200/component/7/drive", r#"{"powr_w": 1.0}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error_of(&body).contains("powr_w"), "{}", error_of(&body));
}

/// The drive route's reactive limits land on an inverter, 0 removes
/// one, and a component without them is a 400.
#[tokio::test]
async fn drive_sets_and_removes_an_inverters_reactive_limits() {
    let (engine, _dir) = engine_with(
        r#"{"components": [
            {"type": "meter", "id": 7},
            {"type": "solar_inverter", "id": 8}]}"#,
    )
    .await;
    let caps = |engine: &Engine| {
        site(engine)
            .get(8)
            .unwrap()
            .reactive_limits()
            .unwrap()
            .reactive_capability()
    };
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/8/drive",
            r#"{"reactive_apparent_va": 5000.0, "reactive_pf_limit": 0.5}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(caps(&engine).apparent_va, Some(5000.0));
    assert_eq!(caps(&engine).pf_limit, Some(0.5));

    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/8/drive",
            r#"{"reactive_apparent_va": 0, "reactive_pf_limit": -1}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(caps(&engine).apparent_va, None);
    assert_eq!(caps(&engine).pf_limit, None);

    let (status, body) = call(
        engine,
        post_json(
            "/api/mg/2200/component/7/drive",
            r#"{"reactive_pf_limit": 0.5}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        error_of(&body).contains("reactive_pf_limit"),
        "{}",
        error_of(&body)
    );
}

/// `POST .../runtime` sets the operational mode and re-derives the
/// runtime modes; a mode it does not know is a 400 and an unknown
/// field a 422.
#[tokio::test]
async fn runtime_route_sets_the_operational_mode() {
    use crate::sim::component::OperationalMode;
    use crate::sim::runtime::{CommandMode, TelemetryMode};
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/runtime",
            r#"{"operational_mode": "inactive"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let site = site(&engine);
    assert_eq!(site.operational_mode(7), OperationalMode::Inactive);
    assert_eq!(site.runtime_of(7).telemetry, TelemetryMode::Silent);
    assert_eq!(site.runtime_of(7).command, CommandMode::Error);

    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/runtime",
            r#"{"operational_mode": "sleepy"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error_of(&body).contains("operational_mode"));
    assert_eq!(site.operational_mode(7), OperationalMode::Inactive);

    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/runtime",
            r#"{"operational_mode": "telemetry-only"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "0.1's spelling is refused");
    assert_eq!(site.operational_mode(7), OperationalMode::Inactive);

    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/runtime",
            r#"{"operational_mode": "inactive", "health": "ok"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error_of(&body).contains("health"));

    let (status, _) = call(
        engine,
        post_json(
            "/api/mg/2200/component/99/runtime",
            r#"{"operational_mode": "inactive"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// `POST .../ev` plugs a catalog car in with the values it gives, a
/// second plug is a 409, and `DELETE` unplugs, also on an empty
/// charger.
#[tokio::test]
async fn ev_route_plugs_and_unplugs_a_car() {
    let (engine, _dir) = engine_with(
        r#"{"components": [{"type": "ev_charger", "id": 6}, {"type": "meter", "id": 7}]}"#,
    )
    .await;
    let info = |engine: &Engine| site(engine).get(6).unwrap().ev_port().unwrap().ev_info();
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/6/ev",
            r#"{"preset": "city", "soc_pct": 25, "phases": 1, "taper_floor_pct": 50}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let car = info(&engine).expect("a car").ev;
    assert_eq!(car.preset, "city");
    assert_eq!(car.soc_pct, 25.0);
    assert_eq!(car.phases, 1);
    assert_eq!(car.taper_floor, 0.5);

    let (status, body) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/6/ev", r#"{"preset": "city"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(error_of(&body).contains("already"), "{}", error_of(&body));

    for _ in 0..2 {
        let (status, _) = call(engine.clone(), delete_req("/api/mg/2200/component/6/ev")).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(info(&engine).is_none());
    }

    for (body, want) in [
        (r#"{"preset": "tractor"}"#, StatusCode::BAD_REQUEST),
        (
            r#"{"preset": "city", "soc_pct": 150}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"preset": "city", "phases": 4}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"preset": "city", "taper_floor_pct": 120}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"preset": "city", "colour": "red"}"#,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (status, _) = call(
            engine.clone(),
            post_json("/api/mg/2200/component/6/ev", body),
        )
        .await;
        assert_eq!(status, want, "{body}");
        assert!(info(&engine).is_none(), "{body} plugged a car in");
    }

    let (status, body) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/7/ev", r#"{"preset": "city"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error_of(&body).contains("not an EV charger"));
    let (status, _) = call(engine, delete_req("/api/mg/2200/component/7/ev")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// `POST .../sample-lag` sets the lag in seconds; a negative lag is a
/// 400 and an unknown field a 422, and neither changes it.
#[tokio::test]
async fn sample_lag_route_takes_seconds_and_refuses_a_negative_lag() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/sample-lag", r#"{"sample_lag_s": 1.5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(site(&engine).sample_lag_ms(), 1500);

    let (status, body) = call(
        engine.clone(),
        post_json("/api/mg/2200/sample-lag", r#"{"sample_lag_s": -1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error_of(&body).contains("sample_lag_s"));
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/sample-lag", r#"{"sample_lag_ms": 5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(site(&engine).sample_lag_ms(), 1500);
}

/// `POST .../streams/cancel` moves the epoch every open stream
/// watches, so each one ends.
#[tokio::test]
async fn streams_cancel_route_ends_the_open_streams() {
    let (engine, _dir) = engine_with("{}").await;
    let before = site(&engine).stream_cancel_epoch();
    let (status, body) = call(engine.clone(), post("/api/mg/2200/streams/cancel", "")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty());
    assert_ne!(site(&engine).stream_cancel_epoch(), before);
}

/// `POST .../grid` sets each phase's voltage; a negative voltage is a
/// 400, a list of two a 422, and neither changes it.
#[tokio::test]
async fn grid_route_sets_the_voltage_per_phase() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/grid",
            r#"{"voltage_per_phase_v": [231, 229.5, 230]}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        site(&engine).grid_state().voltage_per_phase,
        (231.0, 229.5, 230.0)
    );

    for (body, want) in [
        (
            r#"{"voltage_per_phase_v": [231, -1, 230]}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"voltage_per_phase_v": [231, 230]}"#,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            r#"{"voltage_per_phase_v": [1, 2, 3], "frequency_hz": 50}"#,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (status, _) = call(engine.clone(), post_json("/api/mg/2200/grid", body)).await;
        assert_eq!(status, want, "{body}");
    }
    assert_eq!(
        site(&engine).grid_state().voltage_per_phase,
        (231.0, 229.5, 230.0)
    );
}

/// `/api/frequency` reads and writes the shared frequency state: a
/// model patch keeps the keys it leaves out, an override takes its
/// missing keys from the base model rather than an earlier override,
/// and `clear_override` drops it. A refused body changes nothing.
#[tokio::test]
async fn frequency_route_merges_the_model_and_the_override() {
    let (engine, _dir) = engine_with("{}").await;
    let read = |body: &[u8]| serde_json::from_slice::<serde_json::Value>(body).unwrap();
    let (status, body) = call_json(engine.clone(), get("/api/frequency")).await;
    assert_eq!(status, StatusCode::OK);
    let v = read(&body);
    assert_eq!(v["model"]["nominal_hz"], 50.0, "{v}");
    assert!(v["override"].is_null(), "{v}");
    assert!(v["hz"].is_number(), "{v}");

    let (status, body) = call_json(
        engine.clone(),
        post_json("/api/frequency", r#"{"model": {"nominal_hz": 60}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v = read(&body);
    assert_eq!(v["model"]["nominal_hz"], 60.0, "{v}");
    assert_eq!(v["model"]["mean_rev_rate_per_s"], 0.05, "{v}");

    let post_freq = |body: &'static str| call(engine.clone(), post_json("/api/frequency", body));
    let (status, _) = post_freq(r#"{"override": {"sigma_hz_per_sqrt_s": 0.5}}"#).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) =
        post_freq(r#"{"hz": 49.5, "override": {"nominal_hz": 49.5, "sigma_hz_per_sqrt_s": 0}}"#)
            .await;
    let v = read(&body);
    // The second override replaced the first: its sigma is the one
    // given, and its rate the base model's.
    assert_eq!(v["override"]["nominal_hz"], 49.5, "{v}");
    assert_eq!(v["override"]["sigma_hz_per_sqrt_s"], 0.0, "{v}");
    assert_eq!(v["override"]["mean_rev_rate_per_s"], 0.05, "{v}");
    let hz = engine.grid_frequency.read().read_hz();
    assert!((hz - 49.5).abs() < 1e-3, "{hz}");

    for (body, want) in [
        (
            r#"{"model": {"sigma_hz_per_sqrt_s": -1}}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"override": {"nominal_hz": 50}, "clear_override": true}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"model": {"sigma": 1}}"#,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (r#"{"frequency_hz": 50}"#, StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let (status, _) = post_freq(body).await;
        assert_eq!(status, want, "{body}");
    }
    assert!(engine.grid_frequency.read().override_model.is_some());

    let (status, body) = post_freq(r#"{"clear_override": true}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(read(&body)["override"].is_null());
}

/// Battery topology used by the formula tests:
/// grid → meter → battery-inverter → battery, mg id 2200.
const FORMULA_TOPOLOGY: &str = r#"{"components": [
    {"type": "grid_connection_point", "id": 1},
    {"type": "meter", "id": 2},
    {"type": "battery_inverter", "id": 3},
    {"type": "battery", "id": 4}],
    "connections": [[1, 2], [2, 3], [3, 4]]}"#;

#[tokio::test]
async fn formula_endpoint_returns_formula() {
    let (engine, _dir) = engine_with(FORMULA_TOPOLOGY).await;
    let (status, body) = call(engine, get("/api/mg/2200/formula?metric=battery")).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["metric"], "battery");
    assert!(parsed.get("ok").is_none(), "body: {parsed}");
    // Just the rendered string now — parsing/highlighting live
    // client-side in formula-ast.js, so ast/explanation/commented no
    // longer ride along.
    assert!(parsed["formula"].as_str().unwrap().contains('#'));
    assert!(parsed.get("ast").is_none());
    assert!(parsed.get("explanation").is_none());
    assert!(parsed.get("commented").is_none());
}

#[tokio::test]
async fn formula_endpoint_rejects_unknown_metric_and_bad_ids() {
    let (engine, _dir) = engine_with(FORMULA_TOPOLOGY).await;
    let (status, body) = call(engine.clone(), get("/api/mg/2200/formula?metric=bogus")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error_of(&body).contains("bogus"));

    let (status, body) = call(
        engine,
        get("/api/mg/2200/formula?metric=battery&component_ids=1,x"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("bad component id")
    );
}

#[tokio::test]
async fn formula_endpoint_reports_error_kind_for_missing_component() {
    let (engine, _dir) = engine_with(FORMULA_TOPOLOGY).await;
    let (status, body) = call(
        engine,
        get("/api/mg/2200/formula?metric=battery&component_ids=99"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["kind"], "component_not_found");
}

#[tokio::test]
async fn formula_endpoint_honors_allow_unconnected() {
    // The same battery topology, plus a meter nobody connects to.
    let (engine, _dir) = engine_with(
        r#"{"components": [
            {"type": "meter", "id": 9},
            {"type": "grid_connection_point", "id": 1},
            {"type": "meter", "id": 2},
            {"type": "battery_inverter", "id": 3},
            {"type": "battery", "id": 4}],
            "connections": [[1, 2], [2, 3], [3, 4]]}"#,
    )
    .await;
    // By default the unconnected meter makes the graph invalid.
    let (status, body) = call(engine.clone(), get("/api/mg/2200/formula?metric=battery")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body:?}");
    // With the flag the graph builds and the formula comes back.
    let (status, body) = call(
        engine,
        get("/api/mg/2200/formula?metric=battery&allow_unconnected=true"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["formula"].as_str().unwrap().contains('#'));
}

#[tokio::test]
async fn formula_endpoint_404s_unknown_microgrid() {
    let (engine, _dir) = engine_with(FORMULA_TOPOLOGY).await;
    let (status, _) = call(engine, get("/api/mg/9999/formula?metric=grid")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn microgrids_import_creates_a_spec_microgrid() {
    let (engine, dir) =
        engine_with(r#"{"components": [{"type": "grid_connection_point", "id": 1}]}"#).await;
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
    let (status, resp) = call(engine.clone(), post_json("/api/microgrids/import", body)).await;
    let parsed: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(status, StatusCode::OK, "body: {parsed}");
    assert_eq!(parsed["components"], 4);
    assert_eq!(parsed["connections"], 3);
    let id = parsed["id"].as_u64().unwrap();

    // The components exist right away…
    let (status, topo) = call(engine.clone(), get(&format!("/api/mg/{id}/topology"))).await;
    assert_eq!(status, StatusCode::OK);
    let topo: serde_json::Value = serde_json::from_slice(&topo).unwrap();
    assert_eq!(topo["components"].as_array().unwrap().len(), 4);
    assert_eq!(topo["connections"].as_array().unwrap().len(), 3);
    // …and the stored spec keeps the export's ids and physical
    // parameters. Nothing is written to disk.
    let (status, spec) = call(engine.clone(), get(&format!("/api/mg/{id}/spec"))).await;
    assert_eq!(status, StatusCode::OK);
    let spec: serde_json::Value = serde_json::from_slice(&spec).unwrap();
    let by_id = |cid: u64| {
        spec["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == cid)
            .cloned()
            .unwrap()
    };
    assert_eq!(by_id(10)["rated_fuse_current_a"], 125);
    assert_eq!(by_id(13)["type"], "battery");
    assert_eq!(by_id(13)["capacity_wh"], 40000.0);
    assert!(
        spec["connections"]
            .as_array()
            .unwrap()
            .contains(&json!([12, 13])),
        "{spec}"
    );
    assert!(!dir.join("microgrids").exists());

    // The registry lists it.
    let (_, list) = call(engine, get("/api/microgrids")).await;
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
    // Component id 1 already exists in microgrid 2200.
    let (engine, _dir) =
        engine_with(r#"{"components": [{"type": "grid_connection_point", "id": 1}]}"#).await;
    let body = r#"{
      "name": "colliding site",
      "components": {"electricalComponents": [
        {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT"}
      ]}
    }"#;
    let (status, resp) = call(engine.clone(), post_json("/api/microgrids/import", body)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        error_of(&resp).contains("component id 1 is already in microgrid 2200"),
        "{}",
        error_of(&resp)
    );
    // Nothing was created.
    let (_, list) = call(engine, get("/api/microgrids")).await;
    let list: serde_json::Value = serde_json::from_slice(&list).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn microgrids_import_serializes_racing_imports() {
    // Two imports carrying the same component id race. The import
    // lock runs them one at a time, so the loser's collision scan
    // sees the winner's components and returns 409 — component ids
    // stay enterprise-unique, with no silent duplicate.
    let (engine, _dir) =
        engine_with(r#"{"components": [{"type": "grid_connection_point", "id": 1}]}"#).await;
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
            engine.clone(),
            post_json("/api/microgrids/import", &body("site a"))
        ),
        call(
            engine.clone(),
            post_json("/api/microgrids/import", &body("site b"))
        ),
    );
    let statuses = [a.0, b.0];
    assert!(statuses.contains(&StatusCode::OK), "{statuses:?}");
    assert!(statuses.contains(&StatusCode::CONFLICT), "{statuses:?}");
    // Exactly one import landed next to microgrid 2200.
    let (_, list) = call(engine, get("/api/microgrids")).await;
    let list: serde_json::Value = serde_json::from_slice(&list).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn microgrids_import_rejects_unsupported_category() {
    let (engine, _dir) =
        engine_with(r#"{"components": [{"type": "grid_connection_point", "id": 1}]}"#).await;
    let body = r#"{
      "name": "hvac site",
      "components": {"electricalComponents": [
        {"id": "10", "category": "ELECTRICAL_COMPONENT_CATEGORY_HVAC"}
      ]}
    }"#;
    let (status, resp) = call(engine, post_json("/api/microgrids/import", body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error_of(&resp).contains("cannot simulate"));
}

/// The operational mode is a configuration change: the runtime modes
/// derive from it and the site enforces them.
#[tokio::test]
async fn operational_mode_derives_and_is_enforced() {
    let (engine, _dir) =
        engine_with(r#"{"components": [{"type": "grid_connection_point", "id": 1}]}"#).await;
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/1/runtime",
            r#"{"operational_mode": "control_only"}"#,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&body)
    );

    // Derived: the topology shows the mode and the silenced stream.
    let (_, body) = call(engine.clone(), get("/api/mg/2200/topology")).await;
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let c = &parsed["components"][0];
    assert_eq!(c["operational_mode"], "control_only");
    assert_eq!(c["telemetry_mode"], "silent");
    // The capability booleans the inspect panel keys its knob
    // gating off — derived server-side so the rule stays in Rust.
    assert_eq!(c["provides_telemetry"], false);
    assert_eq!(c["accepts_control"], true);

    // Enforced: setting the stream back to normal is refused while
    // the mode forbids telemetry.
    let (status, body) = call(
        engine,
        post_json(
            "/api/mg/2200/component/1/status",
            r#"{"telemetry_mode": "normal"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error_of(&body).contains("streams no telemetry"));
}

/// Two creates racing must end up as two microgrids, not one: the
/// structural lock keeps each one's id + port claim valid until its
/// entry is in the registry.
#[tokio::test]
async fn concurrent_creates_get_distinct_microgrids() {
    let (engine, _dir) = engine_with(r#"{"id": 9}"#).await;
    let (a, b) = tokio::join!(
        call(
            engine.clone(),
            post_json(
                "/api/microgrids",
                &mg_doc(r#"{"name":"a","components":[]}"#)
            )
        ),
        call(
            engine.clone(),
            post_json(
                "/api/microgrids",
                &mg_doc(r#"{"name":"b","components":[]}"#)
            )
        ),
    );
    assert_eq!(
        a.0,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&a.1)
    );
    assert_eq!(
        b.0,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&b.1)
    );
    let id_of = |body: &[u8]| first_created(body)["id"].as_u64().unwrap();
    assert_ne!(id_of(&a.1), id_of(&b.1), "each create gets its own id");
    assert_eq!(engine.microgrids().lock().len(), 3);
}

/// An explicit port on the assets or dispatch server's port is
/// refused, as another microgrid's would be.
#[tokio::test]
async fn create_refuses_a_reserved_port() {
    let (engine, _dir) = engine_with("{}").await;
    let ports = [engine.assets_socket_addr(), engine.dispatch_socket_addr()]
        .map(|a| a.parse::<std::net::SocketAddr>().unwrap().port());
    for port in ports {
        let (st, body) = call(
            engine.clone(),
            post_json(
                "/api/microgrids",
                &mg_doc(&format!(r#"{{"name":"r","grpc_port":{port}}}"#)),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT);
        let error = error_of(&body);
        assert!(error.contains("reserved"), "unexpected error: {error}");
    }
}

/// Create takes an explicit id and port, and refuses either when it
/// is already claimed — the create dialog turns that into an inline
/// error rather than quietly picking something else.
#[tokio::test]
async fn create_refuses_a_taken_id_or_port() {
    let (engine, _dir) = engine_with(r#"{"id": 9}"#).await;
    let (st, body) = call(
        engine.clone(),
        post_json(
            "/api/microgrids",
            &mg_doc(r#"{"name":"pinned","id":40,"grpc_port":8899}"#),
        ),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let v = first_created(&body);
    assert_eq!(v["id"], 40);
    assert_eq!(v["grpc_port"], 8899);

    let (st, _) = call(
        engine.clone(),
        post_json("/api/microgrids", &mg_doc(r#"{"name":"again","id":40}"#)),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "id 40 is taken");
    let (st, _) = call(
        engine.clone(),
        post_json(
            "/api/microgrids",
            &mg_doc(r#"{"name":"again","grpc_port":8899}"#),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "port 8899 is taken");
}

/// With inert runtimes, nothing is started and the list says so.
#[tokio::test]
async fn the_list_reports_no_runtime_under_inert_runtimes() {
    let (engine, _dir) = engine_with("{}").await;
    let (st, body) = call(engine, get("/api/microgrids")).await;
    assert_eq!(st, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v[0].as_object().unwrap().contains_key("runtime"), "{v}");
    assert!(v[0]["runtime"].is_null(), "{v}");
}

/// The list counts every component and says how many are hidden.
#[tokio::test]
async fn the_list_counts_hidden_components() {
    let (engine, _dir) = engine_with(
        r#"{"components": [
            {"type": "grid_connection_point", "id": 1},
            {"type": "meter", "id": 2},
            {"type": "meter", "id": 3, "hidden": true}],
            "connections": [[1, 2], [1, 3]]}"#,
    )
    .await;
    let (st, body) = call(engine, get("/api/microgrids")).await;
    assert_eq!(st, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v[0]["component_count"], 3, "{v}");
    assert_eq!(v[0]["hidden_component_count"], 1, "{v}");
}

/// Runtimes that really bind, on `[::1]`.
fn live_runtimes(engine: &Engine, ephemeral_ports: bool) -> crate::runtime::MicrogridRuntimes {
    crate::runtime::MicrogridRuntimes::new(
        engine.clone(),
        crate::runtime::RuntimeOptions {
            ephemeral_ports,
            bind_host: std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        },
    )
    .unwrap()
}

/// Microgrid `id`'s entry in `/api/microgrids`.
async fn listed(
    engine: Engine,
    runtimes: crate::runtime::MicrogridRuntimes,
    id: u64,
) -> serde_json::Value {
    let (_, list) = call_with(engine, runtimes, get("/api/microgrids")).await;
    let list: serde_json::Value = serde_json::from_slice(&list).unwrap();
    list.as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .cloned()
        .unwrap()
}

/// A created microgrid reports the address its server bound, in the
/// create answer and in the list.
#[tokio::test]
async fn create_reports_the_bound_address() {
    let (engine, _dir) = engine_with("{}").await;
    let runtimes = live_runtimes(&engine, true);
    let (st, body) = call_with(
        engine.clone(),
        runtimes.clone(),
        post_json("/api/microgrids", &mg_doc(r#"{"name":"r","id":61}"#)),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let v = first_created(&body);
    assert_eq!(v["runtime"]["status"], "running");
    let addr = v["runtime"]["grpc_addr"].as_str().unwrap().to_string();
    assert!(
        !addr.ends_with(&format!(":{}", v["grpc_port"])),
        "ephemeral, not configured: {addr}"
    );
    let entry = listed(engine, runtimes, 61).await;
    assert_eq!(entry["runtime"]["grpc_addr"], addr.as_str());
}

/// A create whose port is held answers 201 with the failure, and the
/// list shows it failed.
#[tokio::test]
async fn create_on_a_held_port_reports_a_failed_runtime() {
    let holder = std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)).unwrap();
    let port = holder.local_addr().unwrap().port();
    let (engine, _dir) = engine_with("{}").await;
    let runtimes = live_runtimes(&engine, false);
    let (st, body) = call_with(
        engine.clone(),
        runtimes.clone(),
        post_json(
            "/api/microgrids",
            &mg_doc(&format!(r#"{{"name":"h","id":62,"grpc_port":{port}}}"#)),
        ),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let v = first_created(&body);
    assert_eq!(v["runtime"]["status"], "failed");
    let entry = listed(engine, runtimes, 62).await;
    assert_eq!(entry["runtime"]["status"], "failed");
    drop(holder);
}

fn put_json(path: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::PUT)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// The ids of a spec's components, in its order.
fn spec_ids(spec: &serde_json::Value) -> Vec<u64> {
    spec["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_u64().unwrap())
        .collect()
}

/// Undo and redo walk the spec stack of a microgrid with a stored
/// spec, rebuilding it from each step, and mark a file's microgrid
/// edited.
#[tokio::test]
async fn undo_and_redo_rebuild_a_spec_microgrid() {
    let (engine, _dir) = engine_with("{}").await;
    let runtimes = crate::runtime::MicrogridRuntimes::inert();
    let doc = crate::spec::TopologyDoc::from_json(
        r#"{"schema_version": 1, "microgrids": [{"id": 40, "components": [
            {"type": "grid_connection_point", "id": 4001},
            {"type": "meter", "id": 4002}], "connections": [[4001, 4002]]}]}"#,
    )
    .unwrap();
    let spec = doc.microgrids.into_iter().next().unwrap();
    crate::engine::topology::create(
        &engine.structural().await,
        &runtimes,
        spec,
        Some(std::path::PathBuf::from("plant.py")),
    )
    .await
    .unwrap();
    let read = |engine: Engine| {
        let runtimes = runtimes.clone();
        async move {
            let (st, body) = call_with(engine, runtimes, get("/api/mg/40/spec")).await;
            assert_eq!(st, StatusCode::OK);
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()
        }
    };
    let depths = |engine: Engine| {
        let runtimes = runtimes.clone();
        async move {
            let (_, body) = call_with(engine, runtimes, get("/api/mg/40/undo")).await;
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            (
                v["undo_depth"].as_u64().unwrap(),
                v["redo_depth"].as_u64().unwrap(),
            )
        }
    };
    let (st, _) = call_with(
        engine.clone(),
        runtimes.clone(),
        post("/api/mg/40/undo", ""),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "nothing to undo yet");

    let mut edited = read(engine.clone()).await;
    edited["components"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type": "meter", "id": 5000}));
    edited["connections"]
        .as_array_mut()
        .unwrap()
        .push(json!([4002, 5000]));
    let (st, body) = call_with(
        engine.clone(),
        runtimes.clone(),
        put_json("/api/mg/40/spec", &edited),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(depths(engine.clone()).await, (1, 0));

    let (st, body) = call_with(
        engine.clone(),
        runtimes.clone(),
        post("/api/mg/40/undo", ""),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        (v["undo_depth"].clone(), v["redo_depth"].clone()),
        (json!(0), json!(1))
    );
    assert_eq!(spec_ids(&read(engine.clone()).await), [4001, 4002]);
    assert!(engine.microgrids().lock()[&40].site.get(5000).is_none());
    assert!(engine.microgrids().lock()[&40].unsaved);

    let (st, _) = call_with(
        engine.clone(),
        runtimes.clone(),
        post("/api/mg/40/redo", ""),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(spec_ids(&read(engine.clone()).await), [4001, 4002, 5000]);
    assert_eq!(depths(engine.clone()).await, (1, 0));
    let (st, body) = call_with(
        engine.clone(),
        runtimes.clone(),
        post("/api/mg/40/redo", ""),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(error_of(&body), "microgrid 40 has nothing to redo");
}

// ─── same-origin guard + body limits ──────────────────────────────

/// Browser-shaped request: `Host` plus, optionally, `Origin` — the
/// header pair the same-origin guard keys on. Requests built by the
/// plain `get`/`post` helpers carry neither, which is the
/// non-browser client shape the guard waves through.
fn browser_post(path: &str, host: &str, origin: Option<&str>, body: &str) -> Request<Body> {
    let mut b = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("host", host);
    if let Some(o) = origin {
        b = b.header("origin", o);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn origin_guard_passes_same_origin_browser_requests() {
    let (engine, _dir) = engine_with("{}").await;
    let (st, _) = call(
        engine.clone(),
        browser_post(
            "/api/mg/2200/streams/cancel",
            "localhost:8801",
            Some("http://localhost:8801"),
            "",
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    // Bracketed IPv6 authority — the `http://[::1]:8801` shape.
    let (st, _) = call(
        engine.clone(),
        browser_post(
            "/api/mg/2200/streams/cancel",
            "[::1]:8801",
            Some("http://[::1]:8801"),
            "",
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    // Loopback spellings browsers resolve without DNS: *.localhost,
    // trailing-dot FQDNs, and non-.1 addresses in 127.0.0.0/8.
    for host in ["app.localhost:8801", "localhost.:8801", "127.0.0.5:8801"] {
        let origin = format!("http://{host}");
        let (st, _) = call(
            engine.clone(),
            browser_post("/api/mg/2200/streams/cancel", host, Some(&origin), ""),
        )
        .await;
        assert_eq!(st, StatusCode::NO_CONTENT, "{host}");
    }
}

#[tokio::test]
async fn origin_guard_rejects_foreign_origin_and_rebound_host() {
    let (engine, _dir) = engine_with("{}").await;
    // Cross-origin POST — text/plain needs no CORS preflight, and
    // executing it would change the simulation, so the guard must
    // reject before routing.
    let (st, _) = call(
        engine.clone(),
        browser_post(
            "/api/mg/2200/streams/cancel",
            "localhost:8801",
            Some("http://evil.example:8801"),
            "",
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // `Origin: null` (sandboxed iframe, file://) is not same-origin.
    let (st, _) = call(
        engine.clone(),
        browser_post(
            "/api/mg/2200/streams/cancel",
            "localhost:8801",
            Some("null"),
            "",
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // DNS rebinding: same-origin from the browser's point of view,
    // but the foreign hostname survives in `Host`.
    let (st, _) = call(
        engine,
        browser_post("/api/mg/2200/streams/cancel", "evil.example:8801", None, ""),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn setpoints_resolve_per_microgrid() {
    use crate::sim::setpoints::{SetpointEvent, SetpointKind, SetpointOutcome};
    // Two microgrids: 9 and 31.
    let (engine, _dir) = engine_with(r#"{"id": 9}"#).await;
    add_microgrid(&engine, r#"{"id": 31, "grpc_port": 8831}"#).await;
    // Plant one event per microgrid, each under its own component id,
    // so the assertions can tell WHICH site each endpoint answered
    // from — an existence check alone would pass even if every route
    // read the same site.
    let ev = || SetpointEvent {
        ts: Utc::now(),
        kind: SetpointKind::ActivePower,
        value: 1234.0,
        ttl_s: Some(60),
        outcome: SetpointOutcome::Accepted {
            effective_value: Some(1234.0),
        },
    };
    site_of(&engine, 31).log_setpoint(600, ev());
    site_of(&engine, 9).log_setpoint(500, ev());
    let events_at = |path: &str| {
        let engine = engine.clone();
        let path = path.to_string();
        async move {
            let (st, body) = call(engine, get(&path)).await;
            assert_eq!(st, StatusCode::OK, "{path}");
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            v["events"].as_array().unwrap().len()
        }
    };
    assert_eq!(events_at("/api/mg/31/component/600/setpoints").await, 1);
    assert_eq!(events_at("/api/mg/9/component/600/setpoints").await, 0);
    assert_eq!(events_at("/api/mg/9/component/500/setpoints").await, 1);
    assert_eq!(events_at("/api/mg/31/component/500/setpoints").await, 0);
    let (st, _) = call(engine, get("/api/mg/9999/component/600/setpoints")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// A meter's `meter-power` knob reads back the live constant
/// override, and a power factor drive swaps its reactive source from
/// `meter-reactive-power` to `meter-power-factor` (with `leading`) —
/// the two are mutually exclusive readings of the same underlying
/// reactive source.
#[tokio::test]
async fn component_snapshot_reads_meter_knobs_and_envelope() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/drive",
            r#"{"power_w": 1500, "power_factor": 0.9, "leading": true}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = call(engine, get("/api/mg/2200/component/7")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["id"], 7);
    let knobs = v["knobs"].as_array().unwrap();
    let power = knobs.iter().find(|k| k["knob"] == "meter-power").unwrap();
    assert_eq!(power["value"], 1500.0);
    let pf = knobs
        .iter()
        .find(|k| k["knob"] == "meter-power-factor")
        .unwrap();
    assert_eq!(pf["value"], 0.9);
    assert_eq!(pf["leading"], true);
    // The meter never had a direct VAr source configured, so the
    // knob list shouldn't carry a stale `meter-reactive-power` entry
    // alongside the power-factor one.
    assert!(!knobs.iter().any(|k| k["knob"] == "meter-reactive-power"));
}

#[tokio::test]
async fn component_snapshot_404s_unknown_ids() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let (status, _b) = call(engine.clone(), get("/api/mg/2200/component/99")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _b) = call(engine, get("/api/mg/9999/component/7")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A solar inverter's knob set is sunlight + both reactive caps
/// (mirroring the client's `KNOBS_BY_CATEGORY` solar rule), and the
/// unset `reactive-apparent-va` cap still gets an entry with
/// `value: null` so the client renders the input. `envelope.reactive`
/// needs a downstream component reporting a Q band to populate (see
/// `Gateway::child_envelope`) — a battery inverter wired as a
/// (topologically nonsensical, but type-legal) child gives it one
/// without dragging in a whole battery rig.
#[tokio::test]
async fn component_snapshot_inverter_knobs_and_reactive_envelope() {
    let (engine, _dir) = engine_with(
        r#"{"components": [
            {"type": "solar_inverter", "id": 4},
            {"type": "battery_inverter", "id": 5}],
            "connections": [[4, 5]]}"#,
    )
    .await;
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/4/drive",
            r#"{"sunlight_pct": 63, "reactive_pf_limit": 0.95}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = call(engine, get("/api/mg/2200/component/4")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let knobs = v["knobs"].as_array().unwrap();
    let sunlight = knobs
        .iter()
        .find(|k| k["knob"] == "solar-sunlight")
        .unwrap();
    assert_eq!(sunlight["value"], 63.0);
    let pf_limit = knobs
        .iter()
        .find(|k| k["knob"] == "reactive-pf-limit")
        .unwrap();
    assert_eq!(pf_limit["value"], 0.95);
    let apparent_va = knobs
        .iter()
        .find(|k| k["knob"] == "reactive-apparent-va")
        .unwrap();
    assert!(apparent_va["value"].is_null());
    assert!(!v["envelope"]["reactive_var"].is_null());
}

/// A battery inverter's own reactive capability must populate
/// `envelope.reactive` even when its only child is a battery, which
/// (correctly) exposes no Q bounds of its own — Q terminates at the
/// inverter. The gateway's setpoint envelope falls back to the
/// inverter's own band when no child reports one; returning nothing
/// there would clobber the WS-fed graduation the inspector already
/// draws to null on every snapshot re-fetch (every accepted setpoint)
/// — the once-a-second flicker this test guards against.
#[tokio::test]
async fn component_snapshot_reactive_envelope_falls_back_to_own_bounds() {
    let (engine, _dir) = engine_with(
        r#"{"components": [
            {"type": "battery_inverter", "id": 3},
            {"type": "battery", "id": 4}],
            "connections": [[3, 4]]}"#,
    )
    .await;
    // No PF cap: at this inverter's idle P = 0 a PF cap alone pins Q
    // to (0, 0), masking the fallback this test targets. The kVA cap
    // alone gives the full ±VA band at P = 0.
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/3/drive",
            r#"{"reactive_pf_limit": 0, "reactive_apparent_va": 3000.0}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = call(engine, get("/api/mg/2200/component/3")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let reactive = &v["envelope"]["reactive_var"];
    assert!(!reactive.is_null(), "{v}");
    let bounds = reactive.as_array().unwrap();
    let lo = bounds[0].as_f64().unwrap();
    let hi = bounds[1].as_f64().unwrap();
    assert!(
        lo < 0.0 && 0.0 < hi,
        "expected lo < 0 < hi, got ({lo}, {hi})"
    );
}

/// `setpoints[axis].remaining_s` reflects the live `TimeoutTracker`
/// deadline a gateway setpoint with a lifetime arms, bounded by the
/// lifetime just requested. The value and axis come from the separate
/// setpoint-event log (`log_setpoint`), which the gateway does not
/// fill on its own — planted here the same way
/// `setpoints_resolve_per_microgrid` does.
#[tokio::test]
async fn component_snapshot_reports_remaining_s_for_a_timed_setpoint() {
    use crate::sim::setpoints::{SetpointEvent, SetpointKind, SetpointOutcome};
    use crate::timeout_tracker::SetpointAxis;
    let (engine, _dir) =
        engine_with(r#"{"components": [{"type": "solar_inverter", "id": 4}]}"#).await;
    let lifetime_s: f64 = 5.0;
    let site = site(&engine);
    site.gateway()
        .set_power(
            4,
            site.run_generation(),
            SetpointAxis::Active,
            -5000.0,
            std::time::Duration::from_secs_f64(lifetime_s),
            crate::sim::gateway::Mode::Reject,
        )
        .unwrap();
    site.log_setpoint(
        4,
        SetpointEvent {
            ts: Utc::now(),
            kind: SetpointKind::ActivePower,
            value: -5000.0,
            ttl_s: Some(5),
            outcome: SetpointOutcome::Accepted {
                effective_value: Some(-5000.0),
            },
        },
    );
    let (status, body) = call(engine, get("/api/mg/2200/component/4")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let sp = v["setpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["axis"] == "active")
        .unwrap();
    assert_eq!(sp["value"], -5000.0);
    assert_eq!(sp["unit"], "W");
    let remaining = sp["remaining_s"].as_f64().unwrap();
    assert!(
        remaining > 0.0 && remaining <= lifetime_s,
        "remaining={remaining}"
    );
}

/// A meter driven with `reactive_power_var` (as opposed to
/// `component_snapshot_reads_meter_knobs_and_envelope`'s
/// power-factor-derived reading) exercises the `ReactiveReading::Var`
/// read-back arm in `knobs_for` — the `meter-reactive-power` knob
/// shows up with the constant value just set.
#[tokio::test]
async fn component_snapshot_reads_reactive_var_knob() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/drive",
            r#"{"reactive_power_var": 1250}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = call(engine, get("/api/mg/2200/component/7")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let reactive = v["knobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["knob"] == "meter-reactive-power")
        .unwrap();
    assert_eq!(reactive["value"], 1250.0);
}

/// The drive route broadcasts `KnobChanged` on its success path, so a
/// live UI inspector tab refreshes its edit-in-place input whichever
/// client made the write.
#[tokio::test]
async fn control_drive_broadcasts_knob_changed() {
    use crate::sim::events::{KnobValue, SiteEvent};

    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let mut rx = site(&engine).subscribe_events();
    let (status, _) = call(
        engine,
        post_json("/api/mg/2200/component/7/drive", r#"{"power_w": 1234.5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let mut seen = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        seen.push(ev);
    }
    assert!(
        seen.iter().any(|ev| matches!(
            ev,
            SiteEvent::KnobChanged { id: Some(7), knob: "meter-power", value: Some(KnobValue::Number(v)), .. }
                if (*v - 1234.5).abs() < 1e-6
        )),
        "no matching KnobChanged on the bus; saw: {seen:?}"
    );
}

/// A status or runtime write tells the UI to read the topology again,
/// since its payload carries the states; a refused one does not.
#[tokio::test]
async fn state_writes_bump_the_topology_version() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let site = site(&engine);
    let before = site.version();
    for (route, body) in [
        ("status", r#"{"health": "standby"}"#),
        ("runtime", r#"{"operational_mode": "inactive"}"#),
    ] {
        let (status, _) = call(
            engine.clone(),
            post_json(&format!("/api/mg/2200/component/7/{route}"), body),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{route}");
    }
    assert_eq!(site.version(), before + 2);
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/7/status", r#"{"health": "sleepy"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(site.version(), before + 2);
}

/// The JSON of every `knob_changed` for `knob` on the bus, as the
/// WebSocket sends it.
fn knob_events_json(
    rx: &mut tokio::sync::broadcast::Receiver<crate::sim::events::SiteEvent>,
    knob: &str,
) -> Vec<serde_json::Value> {
    let mut found = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        let json = serde_json::to_value(&ev).unwrap();
        if json["kind"] == "knob_changed" && json["knob"] == knob {
            found.push(json);
        }
    }
    found
}

/// A knob of one component keeps a plain number for its `value`.
#[tokio::test]
async fn drive_knob_changed_value_is_a_number() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let mut rx = site(&engine).subscribe_events();
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/7/drive", r#"{"power_w": 1234.5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let evs = knob_events_json(&mut rx, "meter-power");
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert_eq!(evs[0]["component_id"], 7);
    assert_eq!(evs[0]["value"], 1234.5);
    assert_eq!(evs[0]["unit"], "W");
}

/// `POST .../runtime` sends the mode's snake_case name as the value,
/// with no unit.
#[tokio::test]
async fn runtime_route_sends_knob_changed_with_the_mode() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let mut rx = site(&engine).subscribe_events();
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/7/runtime",
            r#"{"operational_mode": "telemetry_only"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let evs = knob_events_json(&mut rx, "operational-mode");
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert_eq!(evs[0]["component_id"], 7);
    assert_eq!(evs[0]["value"], "telemetry_only");
    assert!(evs[0]["unit"].is_null(), "{}", evs[0]);
}

/// `POST .../grid` sends the three phase voltages as a list, with a
/// null `component_id`.
#[tokio::test]
async fn grid_route_sends_knob_changed_with_the_voltages() {
    let (engine, _dir) = engine_with("{}").await;
    let mut rx = site(&engine).subscribe_events();
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/grid",
            r#"{"voltage_per_phase_v": [231, 229.5, 230]}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let evs = knob_events_json(&mut rx, "grid-voltage");
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert!(evs[0]["component_id"].is_null(), "{}", evs[0]);
    assert_eq!(evs[0]["value"], serde_json::json!([231.0, 229.5, 230.0]));
    assert_eq!(evs[0]["unit"], "V");

    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/grid",
            r#"{"voltage_per_phase_v": [231, -1, 230]}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(knob_events_json(&mut rx, "grid-voltage").is_empty());
}

/// `POST .../sample-lag` sends the lag in force, in seconds, with a
/// null `component_id`.
#[tokio::test]
async fn sample_lag_route_sends_knob_changed_in_seconds() {
    let (engine, _dir) = engine_with("{}").await;
    let mut rx = site(&engine).subscribe_events();
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/sample-lag", r#"{"sample_lag_s": 1.5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let evs = knob_events_json(&mut rx, "sample-lag");
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert!(evs[0]["component_id"].is_null(), "{}", evs[0]);
    assert_eq!(evs[0]["value"], 1.5);
    assert_eq!(evs[0]["unit"], "s");
}

/// `clear_sunlight: true` returns the inverter to following the
/// weather and broadcasts its live sunlight percentage, so the
/// inspector shows the followed value at once.
#[tokio::test]
async fn control_drive_clear_sunlight_broadcasts_the_live_percentage() {
    use crate::sim::events::{KnobValue, SiteEvent};

    let (engine, _dir) = engine_with(
        r#"{"components": [{"type": "solar_inverter", "id": 8, "sunlight_pct": 40.0}]}"#,
    )
    .await;
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/8/drive",
            r#"{"sunlight_pct": 10.0}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let site = site(&engine);
    let mut rx = site.subscribe_events();
    let (status, _) = call(
        engine,
        post_json(
            "/api/mg/2200/component/8/drive",
            r#"{"clear_sunlight": true}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let live = site
        .get(8)
        .unwrap()
        .sunlight_drive()
        .unwrap()
        .sunlight_reading();
    let mut seen = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        seen.push(ev);
    }
    assert!(
        seen.iter().any(|ev| matches!(
            ev,
            SiteEvent::KnobChanged {
                id: Some(8),
                knob: "solar-sunlight",
                value: Some(KnobValue::Number(v)),
                ..
            } if *v == live
        )),
        "no KnobChanged with the live sunlight {live} on the bus; saw: {seen:?}"
    );
}

/// A steam boiler's inspector snapshot lists its demand and pressure
/// knobs and its thermostat target.
#[tokio::test]
async fn component_snapshot_boiler_knobs_and_pressure_target() {
    let (engine, _dir) =
        engine_with(r#"{"components": [{"type": "steam_boiler", "id": 6, "target_bar": 8.0}]}"#)
            .await;
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/6/drive",
            r#"{"steam_demand_kg_per_s": 0.0125, "pressure_bar": 9}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = call(engine, get("/api/mg/2200/component/6")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let knobs = v["knobs"].as_array().unwrap();
    let names: Vec<&str> = knobs.iter().map(|k| k["knob"].as_str().unwrap()).collect();
    assert_eq!(names, ["boiler-demand", "boiler-pressure"]);
    assert!((knobs[0]["value"].as_f64().unwrap() - 0.0125).abs() < 1e-7);
    assert_eq!(knobs[0]["unit"], "kg/s");
    assert_eq!(knobs[1]["value"], 9.0);
    assert_eq!(knobs[1]["unit"], "bar");
    assert_eq!(v["pressure_target_bar"], 8.0);
}

/// Each component in a mixed topology lists exactly the knobs of the
/// capabilities it has, in order, and one with no knob capability
/// lists none.
#[tokio::test]
async fn component_snapshot_lists_exactly_the_knobs_each_kind_has() {
    let (engine, _dir) = engine_with(
        r#"{"components": [
            {"type": "grid_connection_point", "id": 1},
            {"type": "meter", "id": 2, "power_w": 1000.0, "reactive_power_var": 200.0},
            {"type": "meter", "id": 3, "power_w": 1000.0, "power_factor": 0.9},
            {"type": "solar_inverter", "id": 4},
            {"type": "battery_inverter", "id": 5},
            {"type": "battery", "id": 6},
            {"type": "ev_charger", "id": 7}],
            "connections": [[1, 2], [1, 3], [1, 4], [1, 5], [5, 6], [1, 7]]}"#,
    )
    .await;
    let want: [(u64, &[&str]); 7] = [
        (1, &[]),
        (2, &["meter-power", "meter-reactive-power"]),
        (3, &["meter-power", "meter-power-factor"]),
        (
            4,
            &[
                "solar-sunlight",
                "reactive-pf-limit",
                "reactive-apparent-va",
            ],
        ),
        (5, &["reactive-pf-limit", "reactive-apparent-va"]),
        (6, &[]),
        (7, &[]),
    ];
    for (id, names) in want {
        let (status, body) =
            call(engine.clone(), get(&format!("/api/mg/2200/component/{id}"))).await;
        assert_eq!(status, StatusCode::OK, "component {id}");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let got: Vec<&str> = v["knobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k["knob"].as_str().unwrap())
            .collect();
        assert_eq!(got, names, "component {id}");
    }
}

/// An unknown path answers the JSON 404, naming the method and path.
#[tokio::test]
async fn an_unknown_path_is_a_json_404() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call_json(engine, get("/api/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_of(&body), "no route for GET /api/nope");
}

/// A plain GET of the event socket, with no upgrade headers, is a
/// JSON error.
#[tokio::test]
async fn a_ws_events_get_without_upgrade_is_a_json_error() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call_json(engine, get("/ws/events")).await;
    assert!(status.is_client_error(), "{status}");
    assert!(!error_of(&body).is_empty());
}

/// A wrong method on a known path answers the JSON 405, in the
/// nested per-microgrid router too.
#[tokio::test]
async fn a_wrong_method_is_a_json_405() {
    let (engine, _dir) = engine_with("{}").await;
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/microgrids/import")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call_json(engine.clone(), req).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(error_of(&body), "no route for GET /api/microgrids/import");

    let (status, body) = call_json(engine, delete_req("/api/mg/2200/topology")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(error_of(&body), "no route for DELETE /api/mg/2200/topology");
}

/// A malformed JSON body is a 400 whose error carries axum's
/// rejection text.
#[tokio::test]
async fn a_malformed_json_body_is_a_json_400() {
    let (engine, _dir) = engine_with("{}").await;
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/mg/2200/component/1/drive")
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let (status, body) = call_json(engine, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!error_of(&body).is_empty());
}

/// A missing required query parameter is a JSON 400.
#[tokio::test]
async fn a_missing_query_parameter_is_a_json_400() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call_json(engine, get("/api/mg/2200/formula")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error_of(&body).contains("metric"), "{}", error_of(&body));
}

/// The origin guard rejects with JSON and no trailing newline.
#[tokio::test]
async fn the_origin_guard_rejects_with_json() {
    let (engine, _dir) = engine_with("{}").await;
    let req = Request::builder()
        .uri("/api/microgrids")
        .header("host", "evil.example")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call_json(engine.clone(), req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_of(&body), "non-loopback Host rejected");

    // An unknown path behind a foreign Host meets the guard first.
    let req = Request::builder()
        .uri("/api/nope")
        .header("host", "evil.example")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call_json(engine.clone(), req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_of(&body), "non-loopback Host rejected");

    // A cross-origin write is refused before it applies.
    let mut req = browser_post(
        "/api/mg/2200/sample-lag",
        "localhost:8801",
        Some("http://evil.example:8801"),
        r#"{"sample_lag_s": 2}"#,
    );
    req.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    let (status, body) = call_json(engine.clone(), req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_of(&body), "cross-origin request rejected");
    assert_eq!(site(&engine).sample_lag_ms(), 0);
}

/// A handler error keeps its message and comes back as JSON.
#[tokio::test]
async fn a_handler_error_is_json_with_its_old_text() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call_json(engine, get("/api/mg/2200/recording/csv/notes.txt")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_of(&body), "invalid filename");
}

/// A renamed field's old name is refused with an error that names it,
/// not silently ignored.
#[tokio::test]
async fn an_old_field_name_is_refused() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let (status, body) = call_json(
        engine,
        post_json("/api/mg/2200/component/7/drive", r#"{"reactive_var": 10}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error_of(&body).contains("reactive_var"),
        "{}",
        error_of(&body)
    );
}

/// Every query struct refuses a name it does not know.
#[tokio::test]
async fn an_unknown_query_name_is_refused() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "battery", "id": 1000}]}"#).await;
    for (path, field) in [
        (
            "/api/mg/2200/component/1000/history?metric=soc_pct&window=10",
            "window",
        ),
        ("/api/mg/2200/component/1000/setpoints?window=10", "window"),
        ("/api/mg/2200/formula?metric=battery&ids=1000", "ids"),
        ("/api/mg/2200/recording/events?after=1", "after"),
    ] {
        let (status, body) = call_json(engine.clone(), get(path)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        let error = error_of(&body);
        assert!(error.contains(&format!("`{field}`")), "{path}: {error}");
    }
}

/// The drive door takes steam demand in kg/s and the knob reads it back
/// in kg/s.
#[tokio::test]
async fn steam_demand_is_driven_and_read_in_kg_per_s() {
    let (engine, _dir) =
        engine_with(r#"{"components": [{"type": "steam_boiler", "id": 9}]}"#).await;
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/component/9/drive",
            r#"{"steam_demand_kg_per_s": 0.5}"#,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let (_, body) = call(engine.clone(), get("/api/mg/2200/component/9")).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let demand = v["knobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["knob"] == "boiler-demand")
        .unwrap();
    assert_eq!(demand["value"], 0.5);
    assert_eq!(demand["unit"], "kg/s");

    let (status, body) = call_json(
        engine,
        post_json(
            "/api/mg/2200/component/9/drive",
            r#"{"steam_demand_kg_h": 1800}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error_of(&body).contains("steam_demand_kg_h"));
}

/// A weather `seed` replaces the weather, and `DELETE` removes it:
/// the route answers 404 until weather is set again.
#[tokio::test]
async fn weather_takes_a_seed_and_delete_removes_it() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/weather",
            r#"{"sunrise": "07:00", "seed": 3, "pass_cloud": {"depth_pct": 40, "duration_s": 60}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let seed_of = |engine: &Engine| site(engine).with_weather(|w| w.config().seed).flatten();
    assert_eq!(seed_of(&engine), Some(3));

    let (status, body) = call(engine.clone(), delete_req("/api/mg/2200/weather")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty());
    assert!(site(&engine).with_weather(|_| ()).is_none());
    let (status, _) = call(engine.clone(), get("/api/mg/2200/weather")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(engine, delete_req("/api/mg/2200/weather")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn weather_reports_unit_names() {
    let (engine, _dir) = engine_with("{}").await;
    let (status, _) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/weather",
            r#"{"peak_pct": 80.0, "cloud_mean_gap_s": 1200}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(engine.clone(), get("/api/mg/2200/weather")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    for k in [
        "cloud_depth_pct",
        "cloud_duration_s",
        "cloud_ramp_s",
        "cloud_mean_gap_s",
        "sunlight_pct",
    ] {
        assert!(v.get(k).is_some(), "missing {k} in {v}");
    }
    for k in [
        "pct",
        "cloud_depth",
        "cloud_duration",
        "cloud_ramp",
        "cloud_rate_per_h",
    ] {
        assert!(v.get(k).is_none(), "old {k} still in {v}");
    }
    assert_eq!(v["cloud_mean_gap_s"], 1200.0, "{v}");

    // The request takes the same names; an old one is refused.
    let (status, body) = call(
        engine.clone(),
        post_json(
            "/api/mg/2200/weather",
            r#"{"cloud_mean_gap_s": 600, "cloud_depth_pct": [10, 20]}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["cloud_depth_pct"], serde_json::json!([10.0, 20.0]));
    assert_eq!(v["cloud_mean_gap_s"], 600.0, "{v}");
    // The gap reads back exactly as it was given.
    let (status, body) = call(
        engine.clone(),
        post_json("/api/mg/2200/weather", r#"{"cloud_mean_gap_s": 13}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let (_, body) = call(engine.clone(), get("/api/mg/2200/weather")).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["cloud_mean_gap_s"], 13.0, "{v}");
    let (status, body) = call_json(
        engine.clone(),
        post_json("/api/mg/2200/weather", r#"{"cloud_depth": [10, 20]}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error_of(&body).contains("`cloud_depth`"),
        "{}",
        error_of(&body)
    );
    let (status, body) = call_json(
        engine.clone(),
        post_json("/api/mg/2200/weather", r#"{"cloud_mean_gap_s": 0.5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        error_of(&body).contains("cloud_mean_gap_s"),
        "{}",
        error_of(&body)
    );
    let (status, body) = call_json(
        engine,
        post_json("/api/mg/2200/weather", r#"{"cloud_mean_gap_s": 1e14}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let err = error_of(&body);
    assert!(
        err.contains("cloud_mean_gap_s") && err.contains("no more than 1000000000 s"),
        "{err}"
    );
}

/// An import claims the new microgrid's id under `id`; `mid` is refused.
#[tokio::test]
async fn import_claims_the_new_microgrids_id_as_id() {
    let (engine, _dir) =
        engine_with(r#"{"components": [{"type": "grid_connection_point", "id": 1}]}"#).await;
    let body = |key: &str| {
        format!(
            r#"{{"name": "imported", "{key}": 77,
                "components": {{"electricalComponents": [
                  {{"id": "10", "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT"}}
                ]}}}}"#
        )
    };
    let (status, resp) = call_json(
        engine.clone(),
        post_json("/api/microgrids/import", &body("mid")),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error_of(&resp).contains("`mid`"), "{}", error_of(&resp));
    let (status, resp) = call(engine, post_json("/api/microgrids/import", &body("id"))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&resp));
    let parsed: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(parsed["id"], 77);
}

/// The formula query names its components `component_ids`; `ids` is
/// refused.
#[tokio::test]
async fn formula_takes_component_ids() {
    let (engine, _dir) = engine_with(FORMULA_TOPOLOGY).await;
    let (status, body) = call_json(
        engine.clone(),
        get("/api/mg/2200/formula?metric=battery&component_ids=4"),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::BAD_REQUEST,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let (status, body) = call_json(engine, get("/api/mg/2200/formula?metric=battery&ids=4")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error_of(&body).contains("`ids`"), "{}", error_of(&body));
}

/// The envelope carries its unit in each name.
#[tokio::test]
async fn component_envelope_names_carry_units() {
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "meter", "id": 7}]}"#).await;
    let (status, _) = call(
        engine.clone(),
        post_json("/api/mg/2200/component/7/drive", r#"{"power_w": 1500}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = call(engine, get("/api/mg/2200/component/7")).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let envelope = v["envelope"].as_object().unwrap();
    assert!(envelope.contains_key("active_w"), "{v}");
    assert!(envelope.contains_key("reactive_var"), "{v}");
    assert!(!envelope.contains_key("active"), "{v}");
    assert!(!envelope.contains_key("reactive"), "{v}");
    let knobs = v["knobs"].as_array().unwrap();
    let power = knobs.iter().find(|k| k["knob"] == "meter-power").unwrap();
    assert_eq!(power["unit"], "W", "{v}");
}

/// History and setpoint responses name their component `component_id`,
/// and each setpoint carries its unit.
#[tokio::test]
async fn setpoints_response_names_the_component_and_unit() {
    use crate::sim::setpoints::{SetpointEvent, SetpointKind, SetpointOutcome};
    let (engine, _dir) = engine_with(r#"{"components": [{"type": "battery", "id": 1000}]}"#).await;
    let ts = Utc::now();
    site(&engine).log_setpoint(
        1000,
        SetpointEvent {
            ts,
            kind: SetpointKind::ReactivePower,
            value: 300.0,
            ttl_s: Some(5),
            outcome: SetpointOutcome::Accepted {
                effective_value: Some(300.0),
            },
        },
    );
    let (status, body) = call(engine, get("/api/mg/2200/component/1000/setpoints")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["component_id"], 1000);
    assert!(v.get("id").is_none(), "{v}");
    assert_eq!(v["events"][0]["unit"], "VAr");
    assert_eq!(v["events"][0]["value"], 300.0);
    assert_eq!(v["events"][0]["ts"], crate::timefmt::rfc3339(ts), "{v}");
}

/// The report's power factor at the reactive peak is named for that.
#[tokio::test]
async fn recording_report_names_the_power_factor_at_the_reactive_peak() {
    let (engine, _dir) = engine_with("{}").await;
    let site = site(&engine);
    let now = Utc::now();
    site.recording_start("pf".into(), None, now).unwrap();
    site.record_grid_power_sample(3000.0, now);
    site.record_grid_reactive_sample(4000.0, now);
    let (_, body) = call(engine, get("/api/mg/2200/recording/report")).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v.get("site_pf_at_peak_var").is_none(), "{v}");
    let pf = v["site_pf_at_reactive_peak"].as_f64().unwrap();
    assert!((pf - 0.6).abs() < 1e-6, "{pf}");
}

/// History spells the unit of reactive power `VAr`.
#[tokio::test]
async fn history_spells_reactive_power_in_var() {
    let (engine, _dir) = engine_with(
        r#"{"components": [{"type": "battery", "id": 1000, "initial_soc_pct": 50.0}]}"#,
    )
    .await;
    let (_, body) = call(
        engine,
        get("/api/mg/2200/component/1000/history?metric=reactive_power_var"),
    )
    .await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["unit"], "VAr", "{v}");
}
