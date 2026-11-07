//! HTTP-side integration tests. Each test spawns a fresh
//! `TestServer`, hits the live `/api/*` surface with reqwest, and
//! asserts on the JSON shape — covering the boot path that the
//! in-source `tests` module's axum-oneshot pattern doesn't.

mod common;

use std::time::Duration;

use chrono::DateTime;
use common::{TestServer, WsClient};
use serde_json::Value;

const TINY_TOPOLOGY: &str = r#"{
  "components": [
    {"type": "battery", "id": 3},
    {"type": "meter", "id": 2},
    {"type": "grid_connection_point", "id": 1}
  ],
  "connections": [[2, 3], [1, 2]]
}"#;

/// A grid → meter → EV charger the two EV tests both drive: id 6 is
/// the charger, id 2 the meter that takes no car.
const EV_TOPOLOGY: &str = r#"{
  "components": [
    {"type": "grid_connection_point", "id": 1},
    {"type": "meter", "id": 2},
    {"type": "ev_charger", "id": 6}
  ],
  "connections": [[1, 2], [2, 6]]
}"#;

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

/// Fails unless `v` is RFC 3339 text with milliseconds and a `Z`,
/// e.g. `2026-10-06T12:00:00.000Z`.
fn assert_rfc3339_millis(v: &Value) {
    let s = v.as_str().unwrap_or_else(|| panic!("{v} is not a string"));
    assert!(
        s.len() == 24
            && s.ends_with('Z')
            && &s[19..20] == "."
            && DateTime::parse_from_rfc3339(s).is_ok(),
        "{s} is not RFC 3339 with milliseconds and Z"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn topology_endpoint_serves_components_and_connections() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let topo = json(&reqwest::Client::new(), s.mg_url("topology")).await;
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

/// A component renamed in a `PUT` of the spec shows its new name in
/// the topology.
#[tokio::test(flavor = "multi_thread")]
async fn a_spec_put_renames_a_component() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let client = reqwest::Client::new();

    let mut spec = json(&client, s.mg_url("spec")).await;
    let meter = spec["components"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["id"] == 2)
        .unwrap();
    meter["name"] = "main".into();
    client
        .put(s.mg_url("spec"))
        .json(&spec)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    // Read back via the topology route to confirm the world updated.
    let topo = json(&client, s.mg_url("topology")).await;
    let renamed = topo["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == 2)
        .unwrap();
    assert_eq!(renamed["name"], "main");
}

#[tokio::test(flavor = "multi_thread")]
async fn recording_endpoints_round_trip() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let client = reqwest::Client::new();

    // Before a start: the lifecycle is empty.
    let pre = json(&client, s.mg_url("recording")).await;
    assert!(pre["name"].is_null());

    // Start and add an event.
    for (route, body) in [
        ("recording/start", serde_json::json!({"name": "smoke"})),
        (
            "recording/event",
            serde_json::json!({"kind": "note", "payload": "hi"}),
        ),
    ] {
        let r = client
            .post(s.mg_url(route))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{route}: {:?}", r.status());
    }

    let summary = json(&client, s.mg_url("recording")).await;
    assert_eq!(summary["name"], "smoke");
    assert_eq!(summary["event_count"], 1);

    let events = json(&client, s.mg_url("recording/events")).await;
    let arr = events["events"].as_array().unwrap();
    assert_eq!(arr[0]["kind"], "note");

    // The report is keyed on the grid formula streams, so it carries
    // no meter id at all.
    let report = json(&client, s.mg_url("recording/report")).await;
    assert!(
        report.get("main_meter_id").is_none(),
        "main_meter_id should be retired from the report: {report}"
    );
    assert!(
        report.get("peak_grid_w").is_some(),
        "report should carry the grid peak: {report}"
    );
    for key in ["peak_grid_var", "site_pf_at_reactive_peak"] {
        assert!(
            report.get(key).is_some(),
            "report should carry {key}: {report}"
        );
    }
    assert!(
        report["grid_window_averages"].is_array(),
        "report should carry grid_window_averages as an array: {report}"
    );
}

/// A microgrid created over HTTP from a topology document lives in
/// memory with a stored spec: nothing is written, and a `PUT` of its
/// spec is how its structure changes.
#[tokio::test(flavor = "multi_thread")]
async fn a_created_microgrid_is_edited_through_its_spec() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/microgrids", s.ui_url))
        .json(&serde_json::json!({
            "schema_version": 1,
            "microgrids": [{"name": "saved", "components": [
                {"type": "grid_connection_point", "id": 5000}]}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.unwrap();
    let id = created["microgrids"][0]["id"].as_u64().unwrap();
    assert_eq!(
        created["microgrids"][0]["runtime"]["status"], "running",
        "{created}"
    );
    assert!(!s.engine.state_dir().join("microgrids").exists());

    let mut spec = json(&client, format!("{}/api/mg/{id}/spec", s.ui_url)).await;
    spec["components"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"type": "meter", "id": 5001, "name": "main"}));
    spec["connections"] = serde_json::json!([[5000, 5001]]);
    client
        .put(format!("{}/api/mg/{id}/spec", s.ui_url))
        .json(&spec)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let topo = json(&client, format!("{}/api/mg/{id}/topology", s.ui_url)).await;
    assert_eq!(topo["components"].as_array().unwrap().len(), 2, "{topo}");
}

/// The whole imported-site flow over HTTP: import a site export, then
/// ask the new microgrid for its battery formula. The import builds
/// the site from its spec, so the formula engine sees the imported
/// topology immediately.
#[tokio::test(flavor = "multi_thread")]
async fn site_import_creates_microgrid_with_working_formulas() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
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
    // No `mid` in the body — the pre-dialog behaviour, kept: the
    // server allocates the lowest free id, one above the bootstrap
    // microgrid this fixture registers.
    assert_eq!(id, 2201, "a no-mid import must still auto-assign: {resp}");

    let topo = json(&client, format!("{}/api/mg/{id}/topology", s.ui_url)).await;
    assert_eq!(topo["components"].as_array().unwrap().len(), 4);

    let formula = json(
        &client,
        format!("{}/api/mg/{id}/formula?metric=battery", s.ui_url),
    )
    .await;
    assert!(formula["formula"].is_string(), "body: {formula}");
    assert!(formula["formula"].as_str().unwrap().contains("#9103"));
    assert!(formula.get("explanation").is_none());

    // The stored spec carries the export's physical parameters.
    let spec = json(&client, format!("{}/api/mg/{id}/spec", s.ui_url)).await;
    let text = spec.to_string();
    assert!(text.contains(r#""capacity_wh":40000.0"#), "{text}");
    assert!(text.contains(r#""rated_fuse_current_a":125"#), "{text}");
}

/// A one-component site export — the smallest thing the importer
/// accepts, enough to watch where the new microgrid's id lands.
/// Component ids are enterprise-unique, so every call needs its own.
fn tiny_export(name: &str, mid: Option<u64>, component_id: u64) -> Value {
    let mut body = serde_json::json!({
        "name": name,
        "components": {"electricalComponents": [
            {"id": component_id.to_string(), "name": "grid",
             "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT"}
        ]}
    });
    if let Some(mid) = mid {
        body["id"] = serde_json::json!(mid);
    }
    body
}

/// The import dialog asks the user for the microgrid id, so the
/// import body carries an `id` — and the microgrid registers under
/// exactly that one, not the next free one.
#[tokio::test(flavor = "multi_thread")]
async fn site_import_claims_the_requested_microgrid_id() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let client = reqwest::Client::new();

    // Well clear of the lowest free id (2201), so landing on it can
    // only be the requested id being honoured.
    let wanted = 2317;
    let resp: Value = client
        .post(format!("{}/api/microgrids/import", s.ui_url))
        .json(&tiny_export("chosen id", Some(wanted), 9201))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["id"], wanted, "{resp}");

    let mgs = json(&client, format!("{}/api/microgrids", s.ui_url)).await;
    let entry = mgs
        .as_array()
        .expect("microgrids array")
        .iter()
        .find(|m| m["id"] == wanted)
        .unwrap_or_else(|| panic!("microgrid {wanted} not registered: {mgs}"));
    assert_eq!(entry["name"], "chosen id", "{entry}");
}

/// A requested id that is already taken is refused, and the refusal
/// names the id so the UI can put it in a toast. Nothing is created.
#[tokio::test(flavor = "multi_thread")]
async fn site_import_refuses_a_taken_microgrid_id() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let client = reqwest::Client::new();

    let wanted = 2318;
    client
        .post(format!("{}/api/microgrids/import", s.ui_url))
        .json(&tiny_export("first", Some(wanted), 9301))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let resp = client
        .post(format!("{}/api/microgrids/import", s.ui_url))
        .json(&tiny_export("second", Some(wanted), 9302))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "a taken id must be refused"
    );
    let body: Value = resp.json().await.unwrap();
    let error = body["error"].as_str().unwrap();
    assert!(
        error.contains(&wanted.to_string()),
        "the refusal must name the id: {error}"
    );

    // The refused import left nothing behind: no second microgrid,
    // and the microgrid under the id it asked for is still the first
    // import's, untouched.
    let mgs = json(&client, format!("{}/api/microgrids", s.ui_url)).await;
    assert_eq!(
        mgs.as_array()
            .expect("microgrids array")
            .iter()
            .filter(|m| m["id"] == wanted)
            .count(),
        1,
        "{mgs}"
    );
    let spec = json(&client, format!("{}/api/mg/{wanted}/spec", s.ui_url))
        .await
        .to_string();
    assert!(
        spec.contains("9301") && !spec.contains("9302"),
        "the refused import must not have touched microgrid {wanted}: {spec}"
    );
    assert_eq!(mgs.as_array().unwrap().len(), 2, "{mgs}");
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
    let topology = r#"{
      "components": [
        {"type": "battery", "id": 4},
        {"type": "battery_inverter", "id": 3},
        {"type": "meter", "id": 2},
        {"type": "ev_charger", "id": 6},
        {"type": "meter", "id": 5},
        {"type": "grid_connection_point", "id": 1}
      ],
      "connections": [[3, 4], [2, 3], [5, 6], [1, 2], [1, 5]]
    }"#;
    let s = TestServer::start_mg(topology).await;
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
        let body = json(&client, format!("{}/api/mg/{id}/metrics/latest", s.ui_url)).await;
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
    assert_eq!(entry["unit"], "VAr", "{snapshot}");

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
    assert_eq!(pv_entry["unit"], "VAr", "{snapshot}");
}

/// The metrics panel's Reactive card charts per-source Q: grid, PV,
/// battery — the same logical-meter formulas as the power streams,
/// metric AcPowerReactive. This proves both new streams end to end
/// over a site that has both categories, and that neither grows a
/// varh companion (reactive-energy integration stays out of scope,
/// as with grid Q).
#[tokio::test(flavor = "multi_thread")]
async fn per_source_reactive_streams_converge() {
    let topology = r#"{
      "components": [
        {"type": "battery", "id": 4},
        {"type": "battery_inverter", "id": 3},
        {"type": "meter", "id": 2},
        {"type": "solar_inverter", "id": 6},
        {"type": "meter", "id": 5},
        {"type": "grid_connection_point", "id": 1}
      ],
      "connections": [[3, 4], [2, 3], [5, 6], [1, 2], [1, 5]]
    }"#;
    let s = TestServer::start_mg(topology).await;
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
        let body = json(&client, format!("{}/api/mg/{id}/metrics/latest", s.ui_url)).await;
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
        assert_eq!(entry["unit"], "VAr", "{stream}: {snapshot}");
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
    let topology = r#"{
      "components": [
        {"type": "solar_inverter", "id": 3},
        {"type": "meter", "id": 2},
        {"type": "battery", "id": 6},
        {"type": "battery_inverter", "id": 5},
        {"type": "meter", "id": 4},
        {"type": "grid_connection_point", "id": 1}
      ],
      "connections": [[2, 3], [5, 6], [4, 5], [1, 2], [1, 4]]
    }"#;
    let s = TestServer::start_mg(topology).await;
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
        let body = json(&client, format!("{}/api/mg/{id}/metrics/latest", s.ui_url)).await;
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

/// `GET`/`POST /api/mg/{mg}/weather`: 404 with no weather
/// configured, a partial POST creates it (config fields apply like
/// `(set-weather)`, same defaults as `WeatherConfig::default`), a
/// follow-up `GET` sees it, `pass_cloud` arms an event on top of a
/// config change, and a malformed field is a 400 that changes
/// nothing.
#[tokio::test(flavor = "multi_thread")]
async fn weather_http_round_trip() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let client = reqwest::Client::new();

    let resp = client.get(s.mg_url("weather")).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.unwrap();
    let error = body["error"].as_str().unwrap();
    assert!(error.contains("no weather configured"), "{error}");

    // A cloud-only POST on a weatherless site is a 400, the same
    // answer `(pass-cloud …)` gives — NOT a silent "install a whole
    // default sky so the one cloud has somewhere to land".
    let orphan = client
        .post(s.mg_url("weather"))
        .json(&serde_json::json!({
            "pass_cloud": {"depth_pct": 50.0, "duration_s": 600.0}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(orphan.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = orphan.json().await.unwrap();
    let error = body["error"].as_str().unwrap();
    assert!(error.contains("no weather on this site"), "{error}");

    // …and it really installed nothing: the site is still weatherless.
    let resp = client.get(s.mg_url("weather")).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // The other path: a POST carrying a CONFIG field still creates
    // weather on a weatherless site, exactly as `(set-weather …)` does.
    let created: Value = client
        .post(s.mg_url("weather"))
        .json(&serde_json::json!({"sunrise": "05:00"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created["sunrise"], "05:00", "{created}");
    // Untouched fields keep WeatherConfig::default's values.
    assert_eq!(created["sunset"], "20:00", "{created}");
    assert_eq!(created["peak_pct"], 100.0, "{created}");

    let got = json(&client, s.mg_url("weather")).await;
    assert_eq!(got["sunrise"], "05:00", "{got}");

    let cloud: Value = client
        .post(s.mg_url("weather"))
        .json(&serde_json::json!({
            "pass_cloud": {"depth_pct": 50.0, "duration_s": 600.0}
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cloud["events"].as_array().unwrap().len(), 1, "{cloud}");
    // `now` is the snapshot's own instant, stamped by the same clock as
    // the event ends — it is what the panel filters its cloud list
    // against, so a just-fired cloud has to still be ahead of it.
    assert_rfc3339_millis(&cloud["now"]);
    assert_rfc3339_millis(&cloud["events"][0]["start"]);
    assert_rfc3339_millis(&cloud["events"][0]["end"]);
    let now = DateTime::parse_from_rfc3339(cloud["now"].as_str().unwrap()).unwrap();
    let end = DateTime::parse_from_rfc3339(cloud["events"][0]["end"].as_str().unwrap()).unwrap();
    assert!(end > now, "{cloud}");
    // The config change from the prior request survived — a partial
    // update, not a reset.
    assert_eq!(cloud["sunrise"], "05:00", "{cloud}");

    let bad = client
        .post(s.mg_url("weather"))
        .json(&serde_json::json!({"sunrise": "nope"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), reqwest::StatusCode::BAD_REQUEST);

    // The rejected request changed nothing — still one armed event,
    // still the same sunrise.
    let after = json(&client, s.mg_url("weather")).await;
    assert_eq!(after["events"].as_array().unwrap().len(), 1, "{after}");
    assert_eq!(after["sunrise"], "05:00", "{after}");

    // A seed rebuilds the weather, as `:seed` does: the settings stay
    // and the cloud in the sky goes.
    let seeded: Value = client
        .post(s.mg_url("weather"))
        .json(&serde_json::json!({"seed": 7}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(seeded["sunrise"], "05:00", "{seeded}");
    assert!(seeded["events"].as_array().unwrap().is_empty(), "{seeded}");

    // DELETE removes the weather, and again on a site without one.
    for _ in 0..2 {
        let gone = client.delete(s.mg_url("weather")).send().await.unwrap();
        assert_eq!(gone.status(), reqwest::StatusCode::NO_CONTENT);
        let resp = client.get(s.mg_url("weather")).send().await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }
}

const SOLAR_TOPOLOGY: &str = r#"{
  "components": [
    {"type": "solar_inverter", "id": 2, "rated_lower_w": -10000.0, "rated_upper_w": 0.0},
    {"type": "meter", "id": 3},
    {"type": "grid_connection_point", "id": 1}
  ],
  "connections": [[1, 2], [1, 3]]
}"#;

/// `POST /api/mg/{mg}/component/{id}/drive` with `clear_sunlight:
/// true` drops a prior `sunlight_pct` poke and returns the inverter
/// to following the site's weather — with no weather on the site, the
/// read-back knob goes back to full sun. On a non-solar component (a
/// meter) the same field is a 400, not a silent no-op.
#[tokio::test(flavor = "multi_thread")]
async fn drive_clear_sunlight_returns_to_weather_and_rejects_non_solar() {
    let s = TestServer::start_mg(SOLAR_TOPOLOGY).await;
    let client = reqwest::Client::new();

    client
        .post(s.mg_url("component/2/drive"))
        .json(&serde_json::json!({"sunlight_pct": 42.0}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let comp = json(&client, s.mg_url("component/2")).await;
    let knobs = comp["knobs"].as_array().unwrap();
    let poked = knobs
        .iter()
        .find(|k| k["knob"] == "solar-sunlight")
        .unwrap_or_else(|| panic!("no solar-sunlight knob: {comp}"));
    assert_eq!(poked["value"], 42.0, "{comp}");
    assert!(poked.get("expr").is_none(), "{comp}");

    client
        .post(s.mg_url("component/2/drive"))
        .json(&serde_json::json!({"clear_sunlight": true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    // The next physics tick reads the sky: no weather is full sun.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let comp = json(&client, s.mg_url("component/2")).await;
        let cleared = comp["knobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|k| k["knob"] == "solar-sunlight")
            .unwrap_or_else(|| panic!("no solar-sunlight knob: {comp}"))
            .clone();
        if cleared["value"] == 100.0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never back to the weather: {comp}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // clear_sunlight on a meter (not a solar inverter) is a 4xx
    // rejection, not a silent no-op.
    let resp = client
        .post(s.mg_url("component/3/drive"))
        .json(&serde_json::json!({"clear_sunlight": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["error"], "component 3 does not take clear_sunlight (not a solar inverter)",
        "{body}"
    );
}

/// Each drive field sent to a component of the wrong kind is a 400
/// whose body names the component, the field and the kind it needs.
/// TINY_TOPOLOGY's meter 2 takes the non-meter fields and battery 3
/// the meter ones. `soc_pct` is pinned by
/// `drive_soc_on_an_empty_charger_names_the_missing_ev`.
#[tokio::test(flavor = "multi_thread")]
async fn drive_rejects_each_field_on_the_wrong_component_kind() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let client = reqwest::Client::new();
    let cases = [
        (
            3,
            serde_json::json!({"power_w": 1000.0}),
            "component 3 does not take power_w (not a meter)",
        ),
        (
            3,
            serde_json::json!({"reactive_power_var": 500.0}),
            "component 3 does not take reactive_power_var (not a meter)",
        ),
        (
            3,
            serde_json::json!({"power_factor": 0.9}),
            "component 3 does not take power_factor (not a meter)",
        ),
        (
            3,
            serde_json::json!({"clear_power": true}),
            "component 3 does not take clear_power (not a meter)",
        ),
        (
            3,
            serde_json::json!({"clear_reactive": true}),
            "component 3 does not take clear_reactive (not a meter)",
        ),
        (
            2,
            serde_json::json!({"sunlight_pct": 40.0}),
            "component 2 does not take sunlight_pct (not a solar inverter)",
        ),
        (
            2,
            serde_json::json!({"clear_sunlight": true}),
            "component 2 does not take clear_sunlight (not a solar inverter)",
        ),
        (
            2,
            serde_json::json!({"steam_demand_kg_per_s": 40.0}),
            "component 2 does not take steam_demand_kg_per_s (not a steam boiler)",
        ),
        (
            2,
            serde_json::json!({"pressure_bar": 9.0}),
            "component 2 does not take pressure_bar (not a steam boiler)",
        ),
    ];
    for (id, req, error) in cases {
        let resp = client
            .post(s.mg_url(&format!("component/{id}/drive")))
            .json(&req)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST, "{req}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"], error, "{req}: {body}");
    }
}

/// The steam boiler pool rides the loopback like the battery pool:
/// an aggregate power stream, a bounds envelope flattened into two
/// point streams, and a cumulative energy companion. Meter 5 fronts
/// the boiler so the pool formula reads the metered line; the
/// boiler's own reading is only the fallback.
#[tokio::test(flavor = "multi_thread")]
async fn steam_boiler_pool_streams_reach_the_loopback_snapshot() {
    let topology = r#"{
      "components": [
        {"type": "steam_boiler", "id": 6},
        {"type": "meter", "id": 5},
        {"type": "meter", "id": 2},
        {"type": "grid_connection_point", "id": 1}
      ],
      "connections": [[5, 6], [2, 5], [1, 2]]
    }"#;
    let s = TestServer::start_mg(topology).await;
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
        let body = json(&client, format!("{}/api/mg/{id}/metrics/latest", s.ui_url)).await;
        let converged = ["steam_boiler_pool_power", "steam_boiler_pool_bounds_upper"]
            .iter()
            .all(|k| body[k]["value"].as_f64().is_some_and(|v| v.is_finite()));
        if converged || tokio::time::Instant::now() >= deadline {
            break body;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    let power = &snapshot["steam_boiler_pool_power"];
    let value = power["value"]
        .as_f64()
        .unwrap_or_else(|| panic!("steam_boiler_pool_power never converged: {snapshot}"));
    assert!(value.is_finite(), "value not finite: {snapshot}");
    assert_eq!(power["quantity"], "Power", "{snapshot}");
    assert_eq!(power["unit"], "W", "{snapshot}");

    let lower = snapshot["steam_boiler_pool_bounds_lower"]["value"]
        .as_f64()
        .unwrap_or_else(|| panic!("no lower bound: {snapshot}"));
    let upper = snapshot["steam_boiler_pool_bounds_upper"]["value"]
        .as_f64()
        .unwrap_or_else(|| panic!("no upper bound: {snapshot}"));
    assert!(lower <= upper, "inverted envelope: {snapshot}");

    assert_eq!(
        snapshot["steam_boiler_pool_energy"]["unit"], "Wh",
        "energy companion missing: {snapshot}"
    );
}

/// A runtime poke such as a meter's `power_w` drive changes no
/// structure; the aggregate history behind the metrics charts must
/// survive it end to end. (Before the loopback checked the site
/// shape, the rebuild this triggered wiped the ring.) Whether the
/// poke rebuilds at all is pinned by the loopback's own unit test
/// `runtime_pokes_do_not_change_the_site_shape`.
#[tokio::test(flavor = "multi_thread")]
async fn a_meter_power_override_keeps_the_aggregate_history() {
    // The component graph wants the battery behind an inverter, or the
    // loopback's graph build never succeeds.
    let topology = r#"{
      "components": [
        {"type": "battery", "id": 4},
        {"type": "battery_inverter", "id": 3},
        {"type": "meter", "id": 2},
        {"type": "grid_connection_point", "id": 1}
      ],
      "connections": [[3, 4], [2, 3], [1, 2]]
    }"#;
    let s = TestServer::start_mg(topology).await;
    let client = reqwest::Client::new();
    let mgs = json(&client, format!("{}/api/microgrids", s.ui_url)).await;
    let id = mgs.as_array().expect("microgrids array")[0]["id"]
        .as_u64()
        .expect("microgrid id");
    let history_url = format!("{}/api/mg/{id}/metrics/history", s.ui_url);
    let grid_power_len = |body: &Value| body["grid_power"].as_array().map_or(0, |a| a.len());

    // Let the loopback connect and the grid_power ring fill a little.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let before = loop {
        let n = grid_power_len(&json(&client, history_url.clone()).await);
        if n >= 4 || tokio::time::Instant::now() >= deadline {
            break n;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(
        before >= 4,
        "grid_power history never filled: {before} samples"
    );

    client
        .post(format!("{}/api/mg/{id}/component/2/drive", s.ui_url))
        .json(&serde_json::json!({"power_w": 1000.0}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    // Past the supervisor's 300 ms debounce and long enough for a
    // rebuild to have wiped the ring and refilled only a sample or two.
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let after = grid_power_len(&json(&client, history_url.clone()).await);
    assert!(
        after >= before,
        "grid_power history restarted across the override: {before} -> {after}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ev_route_reports_plug_state() {
    let s = TestServer::start_mg(EV_TOPOLOGY).await;
    let client = reqwest::Client::new();
    let base = format!("{}/api/mg/2200", s.ui_url);

    let empty = json(&client, format!("{base}/component/6/ev")).await;
    assert_eq!(empty["plugged"], false, "{empty}");
    // The catalog rides along even on an empty charger, so the
    // inspector's dropdown never hardcodes it.
    assert!(
        empty["presets"]
            .as_array()
            .unwrap()
            .contains(&Value::from("city")),
        "{empty}"
    );

    client
        .post(s.mg_url("component/6/ev"))
        .json(&serde_json::json!({"preset": "city", "soc_pct": 25}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let plugged = json(&client, format!("{base}/component/6/ev")).await;
    assert_eq!(plugged["plugged"], true, "{plugged}");
    assert_eq!(plugged["preset"], "city");
    assert_eq!(plugged["phases"], 1);
    assert!((plugged["soc_pct"].as_f64().unwrap() - 25.0).abs() < 0.01);
    assert!(plugged["state"].is_string());
    assert_rfc3339_millis(&plugged["plugged_at"]);

    let not_charger = client
        .get(format!("{base}/component/2/ev"))
        .send()
        .await
        .unwrap();
    assert_eq!(not_charger.status(), 400);
    assert_eq!(
        not_charger.json::<Value>().await.unwrap()["error"],
        "component 2 is not an EV charger"
    );
    let missing = client
        .get(format!("{base}/component/99/ev"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
}

/// `soc_pct` on a charger drives the plugged CAR's SoC, so an empty
/// charger is a rejection — and one that says what is actually
/// missing rather than "not a battery", which is true of every
/// charger, plugged or not.
#[tokio::test(flavor = "multi_thread")]
async fn drive_soc_on_an_empty_charger_names_the_missing_ev() {
    let s = TestServer::start_mg(EV_TOPOLOGY).await;
    let client = reqwest::Client::new();
    let base = format!("{}/api/mg/2200", s.ui_url);

    let resp = client
        .post(s.mg_url("component/6/drive"))
        .json(&serde_json::json!({"soc_pct": 50.0}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "charger 6 has no EV plugged in", "{body}");

    // A component with no pack at all still gets the old message.
    let resp = client
        .post(s.mg_url("component/2/drive"))
        .json(&serde_json::json!({"soc_pct": 50.0}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["error"], "component 2 does not take soc_pct (not a battery)",
        "{body}"
    );

    // With a car plugged in the same request lands on the car.
    client
        .post(s.mg_url("component/6/ev"))
        .json(&serde_json::json!({"preset": "city", "soc_pct": 25}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    client
        .post(s.mg_url("component/6/drive"))
        .json(&serde_json::json!({"soc_pct": 50.0}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let plugged = json(&client, format!("{base}/component/6/ev")).await;
    assert!(
        (plugged["soc_pct"].as_f64().unwrap() - 50.0).abs() < 0.01,
        "{plugged}"
    );
}

/// A microgrid created after start gets a runtime, and the list shows
/// it running.
#[tokio::test(flavor = "multi_thread")]
async fn a_microgrid_created_later_is_running() {
    let server = TestServer::start_mg("{}").await;
    let client = reqwest::Client::new();
    client
        .post(format!("{}/api/microgrids", server.ui_url))
        .json(&serde_json::json!({
            "schema_version": 1,
            "microgrids": [{"id": 71, "grpc_port": 8971, "components": []}]
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let list = json(&client, format!("{}/api/microgrids", server.ui_url)).await;
        let status = list
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == 71)
            .and_then(|e| e["runtime"]["status"].as_str().map(str::to_string));
        if status.as_deref() == Some("running") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "71 never ran: {list}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_events_name_ids_and_times_like_the_http_bodies() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut ws = WsClient::connect(&s.ui_url).await;
    // The pump subscribes after the upgrade completes; a first frame
    // proves it is listening before the one-shot knob event fires.
    let first = tokio::time::timeout(Duration::from_secs(20), ws.next_json())
        .await
        .expect("a first WebSocket frame");
    let client = reqwest::Client::new();
    let resp = client
        .post(s.mg_url("component/2/drive"))
        .json(&serde_json::json!({"power_w": 1500}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // A history pass sends many metrics back to back; keep the first
    // active-power sample.
    let is_power_sample = |ev: &Value| ev["kind"] == "sample" && ev["metric"] == "active_power_w";
    let mut sample = is_power_sample(&first).then_some(first);
    let mut knob = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while sample.is_none() || knob.is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "no sample and knob_changed"
        );
        let ev = tokio::time::timeout(Duration::from_secs(20), ws.next_json())
            .await
            .expect("a WebSocket frame");
        if sample.is_none() && is_power_sample(&ev) {
            sample = Some(ev);
        } else if ev["kind"] == "knob_changed" {
            knob = Some(ev);
        }
    }
    for ev in [sample.as_ref().unwrap(), knob.as_ref().unwrap()] {
        assert_eq!(ev["microgrid_id"], s.mg_id, "{ev}");
        assert!(ev["component_id"].is_u64(), "{ev}");
        assert!(
            DateTime::parse_from_rfc3339(ev["ts"].as_str().expect("ts is a string")).is_ok(),
            "{ev}"
        );
        for gone in ["mg_id", "id", "ts_ms"] {
            assert!(ev.get(gone).is_none(), "{gone} in {ev}");
        }
    }
    let sample = sample.unwrap();
    assert_eq!(sample["unit"], "W", "{sample}");
    let knob = knob.unwrap();
    assert_eq!(knob["knob"], "meter-power");
    assert_eq!(knob["value"], 1500.0);
    assert_eq!(knob["unit"], "W");
}

/// The typed routes for the component knobs, over a real socket:
/// reactive limits on `/drive`, the operational mode on `/runtime`, a
/// car plugged and unplugged on `/ev`, the sample lag and a stream
/// cancel. Each refuses an unknown field.
#[tokio::test(flavor = "multi_thread")]
async fn typed_knob_routes_reach_the_site() {
    let s = TestServer::start_mg(
        r#"{
          "components": [
            {"type": "grid_connection_point", "id": 1},
            {"type": "meter", "id": 2},
            {"type": "ev_charger", "id": 6},
            {"type": "solar_inverter", "id": 8}
          ],
          "connections": [[1, 2], [2, 6], [2, 8]]
        }"#,
    )
    .await;
    let client = reqwest::Client::new();
    let send = |method: reqwest::Method, path: &str, body: Option<Value>| {
        let mut req = client.request(method, s.mg_url(path));
        if let Some(body) = body {
            req = req.json(&body);
        }
        async move { req.send().await.unwrap().status().as_u16() }
    };
    use reqwest::Method;
    let post = Method::POST;

    let drive = serde_json::json!({"reactive_apparent_va": 4000.0, "reactive_pf_limit": 0.4});
    assert_eq!(
        send(post.clone(), "component/8/drive", Some(drive)).await,
        204
    );
    let caps = s
        .site()
        .get(8)
        .unwrap()
        .reactive_limits()
        .unwrap()
        .reactive_capability();
    assert_eq!((caps.apparent_va, caps.pf_limit), (Some(4000.0), Some(0.4)));

    let mode = serde_json::json!({"operational_mode": "telemetry_only"});
    assert_eq!(
        send(post.clone(), "component/2/runtime", Some(mode)).await,
        204
    );
    assert_eq!(
        s.site().operational_mode(2),
        macrocosim::sim::component::OperationalMode::TelemetryOnly
    );

    let plug = serde_json::json!({"preset": "city", "soc_pct": 30});
    assert_eq!(
        send(post.clone(), "component/6/ev", Some(plug.clone())).await,
        204
    );
    let ev = json(&client, s.mg_url("component/6/ev")).await;
    assert_eq!(ev["plugged"], true, "{ev}");
    assert_eq!(send(post.clone(), "component/6/ev", Some(plug)).await, 409);
    assert_eq!(send(Method::DELETE, "component/6/ev", None).await, 204);
    let ev = json(&client, s.mg_url("component/6/ev")).await;
    assert_eq!(ev["plugged"], false, "{ev}");

    let lag = serde_json::json!({"sample_lag_s": 0.25});
    assert_eq!(send(post.clone(), "sample-lag", Some(lag)).await, 204);
    assert_eq!(s.site().sample_lag_ms(), 250);
    let negative = serde_json::json!({"sample_lag_s": -0.25});
    assert_eq!(send(post.clone(), "sample-lag", Some(negative)).await, 400);

    let epoch = s.site().stream_cancel_epoch();
    assert_eq!(send(post.clone(), "streams/cancel", None).await, 204);
    assert_ne!(s.site().stream_cancel_epoch(), epoch);

    for (path, body) in [
        ("component/8/drive", serde_json::json!({"reactive_va": 1})),
        (
            "component/2/runtime",
            serde_json::json!({"mode": "inactive"}),
        ),
        (
            "component/6/ev",
            serde_json::json!({"preset": "city", "x": 1}),
        ),
        ("sample-lag", serde_json::json!({"sample_lag_s": 1, "x": 1})),
    ] {
        assert_eq!(send(post.clone(), path, Some(body)).await, 422, "{path}");
    }
}

/// The grid voltage and the shared frequency over a real socket: an
/// override with no noise holds the frequency where it was set, every
/// microgrid reads it, and clearing it hands the frequency back to
/// the base model.
#[tokio::test(flavor = "multi_thread")]
async fn grid_and_frequency_routes_reach_the_site() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let client = reqwest::Client::new();
    let post = |url: String, body: Value| {
        let req = client.post(url).json(&body);
        async move { req.send().await.unwrap() }
    };

    let r = post(
        s.mg_url("grid"),
        serde_json::json!({"voltage_per_phase_v": [240, 235, 230]}),
    )
    .await;
    assert_eq!(r.status(), 204);
    assert_eq!(
        s.site().grid_state().voltage_per_phase,
        (240.0, 235.0, 230.0)
    );

    let url = format!("{}/api/frequency", s.ui_url);
    let r = post(
        url.clone(),
        serde_json::json!({"hz": 49.8, "override": {"nominal_hz": 49.8, "sigma_hz_per_sqrt_s": 0}}),
    )
    .await;
    assert_eq!(r.status(), 200);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let v = json(&client, url.clone()).await;
    assert!((v["hz"].as_f64().unwrap() - 49.8).abs() < 1e-3, "{v}");
    let hz = s.site().grid_state().frequency_hz;
    assert!((hz - 49.8).abs() < 1e-3, "{hz}");

    let r = post(url.clone(), serde_json::json!({"clear_override": true})).await;
    assert_eq!(r.status(), 200);
    let v = json(&client, url.clone()).await;
    assert!(v["override"].is_null(), "{v}");

    let r = post(url, serde_json::json!({"hz": 50, "nominal_hz": 50})).await;
    assert_eq!(r.status(), 422);
}
