//! HTTP-side integration tests. Each test spawns a fresh
//! `TestServer`, hits the live `/api/*` surface with reqwest, and
//! asserts on the JSON shape — covering the boot path that the
//! in-source `tests` module's axum-oneshot pattern doesn't.

mod common;

use std::time::Duration;

use common::TestServer;
use serde_json::Value;

const TINY_TOPOLOGY: &str = r#"
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

    // The report is keyed on the grid formula streams now, so it
    // carries no meter id at all — the retired main-meter concept.
    let report = json(&client, format!("{}/api/scenario/report", s.ui_url)).await;
    assert!(
        report.get("main_meter_id").is_none(),
        "main_meter_id should be retired from the report: {report}"
    );
    assert!(
        report.get("peak_grid_w").is_some(),
        "report should carry the grid peak: {report}"
    );
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
/// then ask the new microgrid for its battery formula. The import
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
    assert!(formula.get("explanation").is_none());

    // The microgrid's file carries the export's physical
    // parameters, so they come back at every boot.
    let saved = std::fs::read_to_string(s.config.state_dir().join(format!("microgrids/{id}.lisp")))
        .unwrap();
    assert!(saved.contains(":capacity 40000.0"), "{saved}");
    assert!(saved.contains(":rated-fuse-current 125"), "{saved}");
}

/// SP1's final review deferred the formula-convergence E2E to SP3
/// (this task) because it needs the loopback stream Task 1 added:
/// `grid_reactive_power`, fed by the upstream grid `AcPowerReactive`
/// formula. That formula spans every AC component under the grid —
/// including the EV charger, the concrete case of a component whose
/// formula term is present but whose Q slot never streams from the
/// device's own physics. SP3's mutation-check (temporarily flipping
/// the EV's `reactive_power_var` to `None`) plus a trace of the
/// vendored `frequenz-microgrid` crate sources found that
/// convergence does *not* actually depend on that sample: the crate
/// creates a resampler for every component the formula references
/// eagerly (before any data arrives), so a component that never
/// streams the metric simply resolves to `None` in its slot, and
/// the formula's own `COALESCE(..., 0.0)` absorbs it — the EV's
/// `Some(0.0)` is honest per-component telemetry, not a convergence
/// unblock. This test still proves the `grid_reactive_power` stream
/// end to end over that topology.
#[tokio::test(flavor = "multi_thread")]
async fn grid_reactive_formula_converges_over_a_site_with_an_ev_charger() {
    let topology = r#"
(%make-grid-connection-point :id 1
    :successors
    (list (%make-meter :id 2
                        :successors
                        (list (%make-battery-inverter :id 3
                                                        :successors
                                                        (list (%make-battery :id 4)))))
          (%make-meter :id 5
                       :successors
                       (list (%make-ev-charger :id 6)))))
"#;
    let s = TestServer::start(topology).await;
    let client = reqwest::Client::new();

    let mgs = json(&client, format!("{}/api/microgrids", s.ui_url)).await;
    let id = mgs
        .as_array()
        .expect("microgrids array")
        .first()
        .expect("one microgrid")["id"]
        .as_u64()
        .expect("microgrid id");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let snapshot = loop {
        let body = json(
            &client,
            format!("{}/api/mg/{id}/microgrid/latest", s.ui_url),
        )
        .await;
        let converged = body["grid_reactive_power"]["value"]
            .as_f64()
            .is_some_and(|v| v.is_finite());
        if converged || tokio::time::Instant::now() >= deadline {
            break body;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    let entry = &snapshot["grid_reactive_power"];
    let value = entry["value"]
        .as_f64()
        .unwrap_or_else(|| panic!("grid_reactive_power never converged: {snapshot}"));
    assert!(value.is_finite(), "value not finite: {snapshot}");
    assert_eq!(entry["quantity"], "ReactivePower", "{snapshot}");
    assert_eq!(entry["unit"], "var", "{snapshot}");

    // Reactive energy (varh) integration is out of scope for this
    // stream — energy_stream_for never maps grid_reactive_power to a
    // companion, so no such key should ever appear.
    assert!(
        snapshot.get("grid_reactive_energy").is_none(),
        "unexpected grid_reactive_energy stream: {snapshot}"
    );

    // No PV in this site, and the stream appears anyway: the
    // empty-category formula renders to the literal "0.0" (see
    // category.rs:182-184 upstream) rather than failing to build, so
    // pv_reactive_power is deterministically pinned at exactly 0.
    // What the other categories do on an empty site is untested here.
    let pv_entry = &snapshot["pv_reactive_power"];
    let pv_value = pv_entry["value"]
        .as_f64()
        .unwrap_or_else(|| panic!("pv_reactive_power never converged: {snapshot}"));
    assert_eq!(pv_value, 0.0, "{snapshot}");
    assert_eq!(pv_entry["quantity"], "ReactivePower", "{snapshot}");
    assert_eq!(pv_entry["unit"], "var", "{snapshot}");
}

/// The metrics panel's Reactive card charts per-source Q: grid, PV,
/// battery — the same logical-meter formulas as the power streams,
/// metric AcPowerReactive. This proves both new streams end to end
/// over a site that has both categories, and that neither grows a
/// varh companion (reactive-energy integration stays out of scope,
/// as with grid Q).
#[tokio::test(flavor = "multi_thread")]
async fn per_source_reactive_streams_converge() {
    let topology = r#"
(%make-grid-connection-point :id 1
    :successors
    (list (%make-meter :id 2
                        :successors
                        (list (%make-battery-inverter :id 3
                                                        :successors
                                                        (list (%make-battery :id 4)))))
          (%make-meter :id 5
                       :successors
                       (list (%make-solar-inverter :id 6)))))
"#;
    let s = TestServer::start(topology).await;
    let client = reqwest::Client::new();

    let mgs = json(&client, format!("{}/api/microgrids", s.ui_url)).await;
    let id = mgs
        .as_array()
        .expect("microgrids array")
        .first()
        .expect("one microgrid")["id"]
        .as_u64()
        .expect("microgrid id");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let snapshot = loop {
        let body = json(
            &client,
            format!("{}/api/mg/{id}/microgrid/latest", s.ui_url),
        )
        .await;
        let converged = ["pv_reactive_power", "battery_reactive_power"]
            .iter()
            .all(|s| body[*s]["value"].as_f64().is_some_and(|v| v.is_finite()));
        if converged || tokio::time::Instant::now() >= deadline {
            break body;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    for stream in ["pv_reactive_power", "battery_reactive_power"] {
        let entry = &snapshot[stream];
        entry["value"]
            .as_f64()
            .filter(|v| v.is_finite())
            .unwrap_or_else(|| panic!("{stream} never converged: {snapshot}"));
        assert_eq!(entry["quantity"], "ReactivePower", "{stream}: {snapshot}");
        assert_eq!(entry["unit"], "var", "{stream}: {snapshot}");
    }
    assert!(
        snapshot.get("pv_reactive_energy").is_none()
            && snapshot.get("battery_reactive_energy").is_none(),
        "unexpected varh stream: {snapshot}"
    );
}

/// grid_frequency streams via the logical meter's Frequency formula
/// (a COALESCE over the PCC's meters) — usable since
/// frequenz-microgrid 0.6.0 wired the Frequency sender arm. The
/// topology here hangs TWO meters under the grid: the shape where
/// the old main-meter workaround returned None and the frequency
/// stream silently died. The topology payload also no longer
/// carries the retired main_meter_id flag.
#[tokio::test(flavor = "multi_thread")]
async fn grid_frequency_streams_on_a_multi_feeder_site() {
    let topology = r#"
(%make-grid-connection-point :id 1
    :successors
    (list (%make-meter :id 2
                        :successors
                        (list (%make-solar-inverter :id 3)))
          (%make-meter :id 4
                       :successors
                       (list (%make-battery-inverter :id 5
                                                       :successors
                                                       (list (%make-battery :id 6)))))))
"#;
    let s = TestServer::start(topology).await;
    let client = reqwest::Client::new();

    let mgs = json(&client, format!("{}/api/microgrids", s.ui_url)).await;
    let id = mgs
        .as_array()
        .expect("microgrids array")
        .first()
        .expect("one microgrid")["id"]
        .as_u64()
        .expect("microgrid id");

    let topo = json(&client, format!("{}/api/mg/{id}/topology", s.ui_url)).await;
    assert!(
        topo.get("main_meter_id").is_none(),
        "main_meter_id should be retired from the payload: {topo}"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let snapshot = loop {
        let body = json(
            &client,
            format!("{}/api/mg/{id}/microgrid/latest", s.ui_url),
        )
        .await;
        let converged = body["grid_frequency"]["value"]
            .as_f64()
            .is_some_and(|v| v.is_finite());
        if converged || tokio::time::Instant::now() >= deadline {
            break body;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    let entry = &snapshot["grid_frequency"];
    let hz = entry["value"]
        .as_f64()
        .unwrap_or_else(|| panic!("grid_frequency never converged: {snapshot}"));
    // The OU frequency model mean-reverts around 50 Hz; anything in a
    // generous grid band proves real data, not a fabricated zero.
    assert!(
        (45.0..=55.0).contains(&hz),
        "implausible Hz {hz}: {snapshot}"
    );
    assert_eq!(entry["quantity"], "Frequency", "{snapshot}");
    assert_eq!(entry["unit"], "Hz", "{snapshot}");
    assert!(
        snapshot.get("grid_frequency_energy").is_none(),
        "unexpected energy companion: {snapshot}"
    );
}
