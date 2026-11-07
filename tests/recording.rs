//! The recording routes under `/api/mg/{mg}/recording`, beside the
//! Lisp scenarios that share the journal.

mod common;

use common::{TestServer, drive, put_spec};
use macrocosim::sim::MicrogridSite;
use macrocosim::spec::TopologyDoc;
use reqwest::StatusCode;
use serde_json::{Value, json};

/// Microgrid 7: a load meter (2) and a battery chain (3 → 4) under
/// the grid connection point.
fn plant() -> Value {
    json!({
        "id": 7,
        "name": "plant",
        "components": [
            {"type": "grid_connection_point", "id": 1},
            {"type": "meter", "id": 2, "power_w": 1000.0},
            {"type": "battery_inverter", "id": 3},
            {"type": "battery", "id": 4, "initial_soc_pct": 50.0}
        ],
        "connections": [[1, 2], [1, 3], [3, 4]]
    })
}

async fn start() -> (TestServer, reqwest::Client) {
    let doc: TopologyDoc =
        serde_json::from_value(json!({"schema_version": 1, "microgrids": [plant()]})).unwrap();
    (TestServer::start_spec(doc).await, reqwest::Client::new())
}

async fn post(
    client: &reqwest::Client,
    s: &TestServer,
    rel: &str,
    body: Value,
) -> (StatusCode, Value) {
    let r = client.post(s.mg_url(rel)).json(&body).send().await.unwrap();
    let code = r.status();
    let text = r.text().await.unwrap();
    (code, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn stop(client: &reqwest::Client, s: &TestServer) -> (StatusCode, Value) {
    let r = client
        .post(s.mg_url("recording/stop"))
        .send()
        .await
        .unwrap();
    (r.status(), r.json().await.unwrap())
}

async fn get(client: &reqwest::Client, s: &TestServer, rel: &str) -> Value {
    let r = client.get(s.mg_url(rel)).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    r.json().await.unwrap()
}

async fn eval(client: &reqwest::Client, s: &TestServer, body: &str) -> (StatusCode, Value) {
    let r = client
        .post(format!("{}/api/eval", s.ui_url))
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    (r.status(), r.json().await.unwrap())
}

/// Whether the journal runs, read off `GET .../recording`.
async fn running(client: &reqwest::Client, s: &TestServer) -> bool {
    let summary = get(client, s, "recording").await;
    !summary["started_at"].is_null() && summary["ended_at"].is_null()
}

fn meter_power(site: &MicrogridSite) -> f32 {
    site.get(2)
        .unwrap()
        .meter_drive()
        .unwrap()
        .meter_power_reading()
        .unwrap()
        .value
}

#[tokio::test]
async fn start_event_stop_and_report() {
    let (s, client) = start().await;
    let (code, _) = post(&client, &s, "recording/start", json!({"name": "peak"})).await;
    assert_eq!(code, StatusCode::NO_CONTENT);

    let (code, body) = post(
        &client,
        &s,
        "recording/event",
        json!({"kind": "spike", "payload": {"kw": 30}}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert!(body["id"].is_u64(), "{body}");
    post(
        &client,
        &s,
        "recording/event",
        json!({"kind": "note", "payload": "warm"}),
    )
    .await;

    let summary = get(&client, &s, "recording").await;
    assert_eq!(summary["name"], "peak");
    assert_eq!(summary["event_count"], 2);
    let events = get(&client, &s, "recording/events").await;
    let events = events["events"].as_array().unwrap();
    assert_eq!(events[0]["kind"], "spike");
    assert_eq!(events[0]["payload"], r#"{"kw":30}"#);
    assert_eq!(events[1]["payload"], "warm");
    let running = get(&client, &s, "recording/report").await;
    assert_eq!(running["ended_by"], Value::Null, "{running}");

    let (code, report) = stop(&client, &s).await;
    assert_eq!(code, StatusCode::OK, "{report}");
    assert_eq!(report["name"], "peak");
    assert_eq!(report["ended_by"], "stop");
    assert!(report["elapsed_s"].as_f64().unwrap() >= 0.0);
    assert!(report["soc_stats"]["mean_pct"].is_f64(), "{report}");
    for gone in [
        "checks",
        "checks_passed",
        "checks_failed",
        "scenario_elapsed_s",
    ] {
        assert!(report.get(gone).is_none(), "{gone} in {report}");
    }
    assert_eq!(
        get(&client, &s, "recording/report").await["ended_by"],
        "stop"
    );

    let (code, body) = stop(&client, &s).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    let (code, body) = post(&client, &s, "recording/event", json!({"kind": "late"})).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body.get("ended_by"), None, "{body}");
}

#[tokio::test]
async fn a_second_start_answers_409() {
    let (s, client) = start().await;
    post(&client, &s, "recording/start", json!({"name": "a"})).await;
    let (code, body) = post(&client, &s, "recording/start", json!({"name": "b"})).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("already running"),
        "{body}"
    );
    assert_eq!(stop(&client, &s).await.1["name"], "a");
}

/// A Lisp scenario and a recording never share the journal, and a
/// scenario stop leaves a recording running.
#[tokio::test]
async fn lisp_scenarios_and_recordings_refuse_each_other() {
    let (s, client) = start().await;
    let (code, body) = eval(&client, &s, r#"(scenario-start "lisp")"#).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let (code, body) = post(&client, &s, "recording/start", json!({"name": "r"})).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("Lisp scenario"),
        "{body}"
    );
    eval(&client, &s, "(scenario-stop)").await;

    let (code, _) = post(&client, &s, "recording/start", json!({"name": "r"})).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (code, body) = eval(&client, &s, r#"(scenario-start "lisp")"#).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("recording is running in microgrid 7"),
        "{body}"
    );
    let (code, body) = eval(&client, &s, "(scenario-stop)").await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert!(running(&client, &s).await, "a scenario stop left it");
    assert_eq!(stop(&client, &s).await.1["name"], "r");
}

/// A knob write during a recording takes no snapshot: a later
/// scenario stop has nothing to put back.
#[tokio::test]
async fn a_knob_write_during_a_recording_takes_no_snapshot() {
    let (s, client) = start().await;
    post(&client, &s, "recording/start", json!({"name": "r"})).await;
    drive(&client, &s, 7, 2, json!({"power_w": 5000.0})).await;
    assert_eq!(stop(&client, &s).await.0, StatusCode::OK);
    eval(&client, &s, "(scenario-stop)").await;
    assert_eq!(meter_power(&s.site()), 5000.0);
}

/// A `PUT` ends the recording; the next stop answers its report with
/// `ended_by: "rebuild"`, and the one after that 409.
#[tokio::test]
async fn a_put_ends_the_recording_and_keeps_its_report() {
    let (s, client) = start().await;
    post(&client, &s, "recording/start", json!({"name": "r"})).await;
    let mut changed = plant();
    changed["components"][1]["power_w"] = json!(2000.0);
    let (code, body) = put_spec(&client, &s, 7, &changed).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert!(!running(&client, &s).await);
    let (code, body) = post(&client, &s, "recording/event", json!({"kind": "late"})).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["ended_by"], "rebuild", "{body}");

    let (code, report) = stop(&client, &s).await;
    assert_eq!(code, StatusCode::OK, "{report}");
    assert_eq!(report["name"], "r");
    assert_eq!(report["ended_by"], "rebuild");
    let (code, body) = stop(&client, &s).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
}

/// An unload ends the recording with the microgrid: the stop finds no
/// microgrid.
#[tokio::test]
async fn an_unload_ends_the_recording() {
    let (s, client) = start().await;
    post(
        &client,
        &s,
        "recording/start",
        json!({"name": "r", "csv": true}),
    )
    .await;
    let r = client
        .delete(format!("{}/api/mg/7", s.ui_url))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert_eq!(stop(&client, &s).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn csv_files_go_under_the_state_dir_with_a_safe_name() {
    let (s, client) = start().await;
    let (code, body) = post(
        &client,
        &s,
        "recording/start",
        json!({"name": "../x y", "csv": true}),
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{body}");
    let listing = get(&client, &s, "recording/csv").await;
    let dir = std::path::PathBuf::from(listing["dir"].as_str().unwrap());
    assert_eq!(
        dir.parent().unwrap(),
        s.engine.state_dir().join("recordings").join("7")
    );
    let name = dir.file_name().unwrap().to_str().unwrap();
    let ts = name.strip_prefix("___x_y-").expect(name);
    assert!(
        chrono::NaiveDateTime::parse_from_str(ts, "%Y%m%dT%H%M%SZ").is_ok(),
        "{name}"
    );
    assert!(
        listing["files"]
            .as_array()
            .unwrap()
            .contains(&json!("2-meter.csv")),
        "{listing}"
    );
    assert_eq!(stop(&client, &s).await.0, StatusCode::OK);
    let file = client
        .get(s.mg_url("recording/csv/2-meter.csv"))
        .send()
        .await
        .unwrap();
    assert_eq!(file.status(), StatusCode::OK);
}
