//! HTTP-side integration tests. Each test spawns a fresh
//! `TestServer`, hits the live `/api/*` surface with reqwest, and
//! asserts on the JSON shape — covering the boot path that the
//! in-source `tests` module's axum-oneshot pattern doesn't.

mod common;

use common::TestServer;
use serde_json::Value;

const TINY_TOPOLOGY: &str = r#"
(set-microgrid-id 7)
(%make-grid-connection-point :id 1
            :successors
            (list (%make-meter :id 2
                               :successors
                               (list (%make-battery :id 3)))))
"#;

async fn json(client: &reqwest::Client, url: String) -> Value {
    client
        .get(&url)
        .send()
        .await
        .unwrap_or_else(|e| panic!("GET {url}: {e}"))
        .json::<Value>()
        .await
        .unwrap_or_else(|e| panic!("parse {url}: {e}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn topology_endpoint_serves_components_and_connections() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let topo = json(
        &reqwest::Client::new(),
        format!("{}/api/topology", s.ui_url),
    )
    .await;
    let components = topo["components"].as_array().expect("components array");
    let ids: Vec<i64> = components
        .iter()
        .map(|c| c["id"].as_i64().unwrap())
        .collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
    assert!(ids.contains(&3));
    let connections = topo["connections"].as_array().expect("connections array");
    assert_eq!(connections.len(), 2, "expected grid→meter, meter→battery");
}

#[tokio::test(flavor = "multi_thread")]
async fn eval_endpoint_round_trips_world_state() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/eval", s.ui_url))
        .body("(rename-component 2 \"main\")")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);

    // Read back via /api/topology to confirm the world updated.
    let topo = json(&client, format!("{}/api/topology", s.ui_url)).await;
    let renamed = topo["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == 2)
        .unwrap();
    assert_eq!(renamed["name"], "main");
}

#[tokio::test(flavor = "multi_thread")]
async fn scenario_endpoints_round_trip_via_eval() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let client = reqwest::Client::new();

    // Pre-start: lifecycle is empty.
    let pre = json(&client, format!("{}/api/scenario", s.ui_url)).await;
    assert!(pre["name"].is_null());

    // Start + record an event via /api/eval.
    for body in [
        "(scenario-start \"smoke\")",
        "(scenario-event 'note \"hi\")",
    ] {
        let r = client
            .post(format!("{}/api/eval", s.ui_url))
            .body(body)
            .send()
            .await
            .unwrap();
        assert!(
            r.status().is_success(),
            "eval {body} failed: {:?}",
            r.status()
        );
    }

    let summary = json(&client, format!("{}/api/scenario", s.ui_url)).await;
    assert_eq!(summary["name"], "smoke");
    assert_eq!(summary["event_count"], 1);

    let events = json(&client, format!("{}/api/scenario/events", s.ui_url)).await;
    let arr = events["events"].as_array().unwrap();
    assert_eq!(arr[0]["kind"], "note");

    let report = json(&client, format!("{}/api/scenario/report", s.ui_url)).await;
    assert_eq!(report["main_meter_id"], 2);
}

/// A microgrid created over HTTP gets a managed file, and every
/// structural eval against it rewrites that file — the whole
/// save-on-edit path through the live server.
#[tokio::test(flavor = "multi_thread")]
async fn structural_evals_rewrite_the_managed_microgrid_file() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let client = reqwest::Client::new();

    let created: Value = client
        .post(format!("{}/api/microgrids/create", s.ui_url))
        .json(&serde_json::json!({"name": "saved"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_u64().unwrap();
    let path = s.config.state_dir().join(format!("microgrids/{id}.lisp"));
    assert!(path.exists(), "create writes {}", path.display());

    for body in [
        "(%make-grid-connection-point :id 4001)",
        "(rename-component 4001 \"main\")",
    ] {
        client
            .post(format!("{}/api/mg/{id}/eval", s.ui_url))
            .body(body)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }

    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(
        saved.contains("(%make-grid-connection-point :id 4001"),
        "{saved}"
    );
    assert!(saved.contains(":name \"main\""), "{saved}");
    // A poke is not structure: it leaves the file alone.
    let before = std::fs::read_to_string(&path).unwrap();
    client
        .post(format!("{}/api/mg/{id}/eval", s.ui_url))
        .body("(set-component-health 4001 'error)")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(before, std::fs::read_to_string(&path).unwrap());
}

/// The whole imported-site flow over HTTP: import a site export,
/// then ask the new microgrid for an explained formula. The import
/// populates the site through the per-mg eval path, so the formula
/// engine sees the imported topology immediately.
#[tokio::test(flavor = "multi_thread")]
async fn site_import_creates_microgrid_with_working_formulas() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let client = reqwest::Client::new();

    let body = serde_json::json!({
        "name": "imported site",
        "components": {"electricalComponents": [
            {"id": "9101", "name": "grid",
             "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT",
             "categorySpecificInfo": {"gridConnectionPoint": {"ratedFuseCurrent": 125}}},
            {"id": "9102", "name": "main meter",
             "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"},
            {"id": "9103", "name": "bat inverter",
             "category": "ELECTRICAL_COMPONENT_CATEGORY_INVERTER",
             "categorySpecificInfo": {"inverter": {"type": "INVERTER_TYPE_BATTERY"}},
             "metricConfigBounds": [
                {"metric": "METRIC_AC_POWER_ACTIVE",
                 "configBounds": {"lower": -30000, "upper": 30000}}]},
            {"id": "9104", "name": "battery",
             "category": "ELECTRICAL_COMPONENT_CATEGORY_BATTERY",
             "metricConfigBounds": [
                {"metric": "METRIC_BATTERY_CAPACITY", "configBounds": {"upper": 40000}},
                {"metric": "METRIC_BATTERY_SOC_PCT",
                 "configBounds": {"lower": 5, "upper": 95}}]}
        ]},
        "connections": {"electricalComponentConnections": [
            {"sourceElectricalComponentId": "9101", "destinationElectricalComponentId": "9102"},
            {"sourceElectricalComponentId": "9102", "destinationElectricalComponentId": "9103"},
            {"sourceElectricalComponentId": "9103", "destinationElectricalComponentId": "9104"}
        ]}
    });
    let resp: serde_json::Value = client
        .post(format!("{}/api/microgrids/import", s.ui_url))
        .json(&body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["components"], 4);
    let id = resp["id"].as_u64().unwrap();

    let topo = json(&client, format!("{}/api/mg/{id}/topology", s.ui_url)).await;
    assert_eq!(topo["components"].as_array().unwrap().len(), 4);

    let formula = json(
        &client,
        format!("{}/api/mg/{id}/formula?metric=battery", s.ui_url),
    )
    .await;
    assert_eq!(formula["ok"], true, "body: {formula}");
    assert!(formula["formula"].as_str().unwrap().contains("#9103"));
    assert!(formula["explanation"].is_object());

    // The microgrid's file carries the export's physical
    // parameters, so they come back at every boot.
    let saved = std::fs::read_to_string(s.config.state_dir().join(format!("microgrids/{id}.lisp")))
        .unwrap();
    assert!(saved.contains(":capacity 40000.0"), "{saved}");
    assert!(saved.contains(":rated-fuse-current 125"), "{saved}");
}
