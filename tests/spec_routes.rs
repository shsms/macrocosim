//! `GET` / `PUT /api/mg/{mg}/spec` and `GET /api/spec/schema`: a
//! microgrid built from a spec and rebuilt with carry.

mod common;

use std::time::Duration;

use common::{TestServer, drive, get_spec, put_spec};
use macrocosim::proto::microgrid::microgrid_client::MicrogridClient;
use macrocosim::proto::microgrid::{PowerType, SetElectricalComponentPowerRequest};
use macrocosim::sim::MicrogridSite;
use macrocosim::sim::runtime::Health;
use macrocosim::spec::TopologyDoc;
use reqwest::StatusCode;
use serde_json::{Value, json};

/// Microgrid 7: two loads (2, 3) and two battery chains (4 → 5, 6 →
/// 7) under the grid connection point.
fn plant() -> Value {
    json!({
        "id": 7,
        "name": "plant",
        "components": [
            {"type": "grid_connection_point", "id": 1},
            {"type": "meter", "id": 2, "power_w": 3_600_000.0},
            {"type": "meter", "id": 3, "power_w": 3_600_000.0},
            {"type": "battery_inverter", "id": 4,
             "rated_lower_w": -5000.0, "rated_upper_w": 5000.0},
            {"type": "battery", "id": 5, "initial_soc_pct": 50.0,
             "rated_lower_w": -5000.0, "rated_upper_w": 5000.0},
            {"type": "battery_inverter", "id": 6,
             "rated_lower_w": -5000.0, "rated_upper_w": 5000.0},
            {"type": "battery", "id": 7, "initial_soc_pct": 50.0,
             "rated_lower_w": -5000.0, "rated_upper_w": 5000.0}
        ],
        "connections": [[1, 2], [1, 3], [1, 4], [4, 5], [1, 6], [6, 7]]
    })
}

fn doc(microgrids: Vec<Value>) -> TopologyDoc {
    serde_json::from_value(json!({"schema_version": 1, "microgrids": microgrids})).unwrap()
}

/// The component of `spec` with id `id`, to change in place.
fn component(spec: &mut Value, id: u64) -> &mut Value {
    spec["components"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["id"] == id)
        .unwrap()
}

fn soc(site: &MicrogridSite, id: u64) -> f32 {
    site.get(id).unwrap().dc_storage().unwrap().soc_pct()
}

/// Wait until `ok` holds, polling every 50 ms for up to 10 s.
async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn get_answers_the_stored_spec_with_its_ids() {
    let s = TestServer::start_spec(doc(vec![plant()])).await;
    let client = reqwest::Client::new();
    let (code, body) = get_spec(&client, &s, 7).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["id"], 7);
    assert!(body["grpc_port"].as_u64().is_some(), "{body}");
    assert_eq!(body["components"].as_array().unwrap().len(), 7);

    let list: Value = client
        .get(format!("{}/api/microgrids", s.ui_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<u64> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, vec![7]);
    for gone in ["has_spec", "managed"] {
        assert!(list[0].get(gone).is_none(), "{gone} in {list}");
    }

    let schema: Value = client
        .get(format!("{}/api/spec/schema", s.ui_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(schema["properties"]["microgrids"].is_object(), "{schema}");
}

/// A `PUT` that changes one battery resets that battery from its spec
/// and keeps an unchanged battery's SoC and history; an unchanged
/// meter keeps its energy, a changed one starts again.
#[tokio::test]
async fn a_put_carries_only_what_did_not_change() {
    let s = TestServer::start_spec(doc(vec![plant()])).await;
    let client = reqwest::Client::new();
    for battery in [5, 7] {
        drive(&client, &s, 7, battery, json!({"soc_pct": 80.0})).await;
    }
    let site = s.site();
    until("history and energy", || {
        !site.history_metrics(7).is_empty()
            && site.component_energy_wh(3).is_some_and(|wh| wh > 1000.0)
    })
    .await;
    // Two sampler passes, so the history kept is more than one.
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let energy_2 = site.component_energy_wh(2).unwrap();
    let energy_3 = site.component_energy_wh(3).unwrap();

    let mut changed = plant();
    component(&mut changed, 5)["capacity_wh"] = json!(1000.0);
    component(&mut changed, 3)["power_w"] = json!(1_800_000.0);
    let (code, body) = put_spec(&client, &s, 7, &changed).await;
    assert_eq!(code, StatusCode::OK, "{body}");

    assert_eq!(soc(&site, 5), 50.0, "changed: from its spec");
    assert_eq!(soc(&site, 7), 80.0, "unchanged: carried");
    let samples = |id: u64| {
        let since = site.now() - chrono::Duration::seconds(600);
        site.history_window(id, macrocosim::sim::history::Metric::SocPct, since)
            .map_or(0, |h| h.len())
    };
    assert!(samples(7) >= 2, "kept: {}", samples(7));
    assert!(samples(5) <= 1, "cleared: {}", samples(5));
    assert!(site.component_energy_wh(2).unwrap() >= energy_2, "kept");
    assert!(
        site.component_energy_wh(3).unwrap_or(0.0) < energy_3,
        "started again"
    );
    assert_eq!(s.engine.spec_undo_depths(7).undo, 1);
    assert_eq!(body["components"], changed["components"]);
}

/// A `PUT` keeps the weather, the sample lag and the UI's aggregate
/// energy.
#[tokio::test]
async fn a_put_keeps_the_microgrid_level_state() {
    let s = TestServer::start_spec(doc(vec![plant()])).await;
    let client = reqwest::Client::new();
    let site = s.site();
    site.set_weather(Some(macrocosim::sim::weather::Weather::new(
        Default::default(),
    )));
    site.set_sample_lag_ms(250);
    let grid_energy = || async {
        let latest: Value = client
            .get(s.mg_url("metrics/latest"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        latest["grid_energy"]["value"].as_f64().unwrap_or(0.0)
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let before = loop {
        let wh = grid_energy().await;
        if wh > 3000.0 {
            break wh;
        }
        assert!(std::time::Instant::now() < deadline, "no grid energy");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let (code, body) = put_spec(&client, &s, 7, &plant()).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    // Time for the loopback to rebuild on the new components.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        grid_energy().await >= before,
        "the aggregate energy was kept"
    );
    assert!(site.with_weather(|_| ()).is_some());
    assert_eq!(site.sample_lag_ms(), 250);
}

#[tokio::test]
async fn a_put_refuses_a_taken_id_a_missing_id_and_a_taken_port() {
    let mut other = plant();
    other["id"] = json!(8);
    for c in other["components"].as_array_mut().unwrap() {
        c["id"] = json!(c["id"].as_u64().unwrap() + 100);
    }
    other["connections"] = json!([[101, 102]]);
    let s = TestServer::start_spec(doc(vec![plant(), other])).await;
    let client = reqwest::Client::new();

    let mut taken = plant();
    component(&mut taken, 3)["id"] = json!(103);
    taken["connections"] = json!([[1, 2]]);
    let (code, body) = put_spec(&client, &s, 7, &taken).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("microgrid 8"),
        "{body}"
    );

    let mut missing = plant();
    component(&mut missing, 3)
        .as_object_mut()
        .unwrap()
        .remove("id");
    missing["connections"] = json!([[1, 2]]);
    let (code, body) = put_spec(&client, &s, 7, &missing).await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let (_, stored) = get_spec(&client, &s, 8).await;
    let mut moved = plant();
    moved["grpc_port"] = stored["grpc_port"].clone();
    let (code, body) = put_spec(&client, &s, 7, &moved).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("microgrid 8"),
        "{body}"
    );

    let (code, body) = put_spec(&client, &s, 7, &json!({"id": 7, "extra": 1})).await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    assert_eq!(s.site().component_count(), 7, "nothing changed");
    assert_eq!(s.engine.spec_undo_depths(7).undo, 0);
}

/// After a `PUT`, a carried inverter takes gRPC setpoints, and a
/// health changed in the spec applies.
#[tokio::test]
async fn a_put_keeps_setpoints_working_and_applies_health() {
    let s = TestServer::start_spec(doc(vec![plant()])).await;
    let client = reqwest::Client::new();
    let mut changed = plant();
    component(&mut changed, 5)["health"] = json!("error");
    let (code, body) = put_spec(&client, &s, 7, &changed).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(s.site().runtime_of(5).health, Health::Error);

    let mut grpc = MicrogridClient::connect(s.grpc_url.clone()).await.unwrap();
    let mut stream = grpc
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 6,
            power: 1000.0,
            power_type: PowerType::Active as i32,
            request_lifetime: Some(30),
        })
        .await
        .expect("set-power on the carried inverter")
        .into_inner();
    assert!(stream.message().await.unwrap().is_some(), "acknowledged");
}

/// A `PUT` on a microgrid booted from a file marks it unsaved and
/// writes nothing to the file.
#[tokio::test]
async fn a_put_leaves_the_source_file_alone() {
    let dir = tempfile::TempDir::with_prefix("mc-spec-").unwrap();
    let source = dir.path().join("topology.json");
    let text = json!({"schema_version": 1, "microgrids": [plant()]}).to_string();
    std::fs::write(&source, &text).unwrap();
    let s = TestServer::start_spec_from(doc(vec![plant()]), Some(source.clone())).await;
    let client = reqwest::Client::new();
    assert!(!s.engine.microgrids().lock()[&7].unsaved);

    let (code, _) = put_spec(&client, &s, 7, &plant()).await;
    assert_eq!(code, StatusCode::OK);
    let registry = s.engine.microgrids();
    assert!(registry.lock()[&7].unsaved, "the file no longer says it");
    assert_eq!(std::fs::read_to_string(&source).unwrap(), text);
}
