//! `POST /api/microgrids?existing=rebuild`, a client's declaration, against the
//! binary booted with no microgrids.

mod common;

use std::time::Duration;

use common::{KillOnDrop, endpoints_of};
use macrocosim::proto::microgrid::{
    ReceiveElectricalComponentTelemetryStreamRequest, microgrid_client::MicrogridClient,
};
use serde_json::{Value, json};

/// Microgrid 2200: a grid connection point, a battery inverter and its battery,
/// plus a meter when `meter` is set.
fn site(meter: bool) -> Value {
    let mut components = vec![
        json!({"type": "grid_connection_point", "id": 1}),
        json!({"type": "battery_inverter", "id": 3}),
        json!({"type": "battery", "id": 4, "initial_soc_pct": 50}),
    ];
    let mut connections = vec![json!([1, 3]), json!([3, 4])];
    if meter {
        components.push(json!({"type": "meter", "id": 5}));
        connections.push(json!([1, 5]));
    }
    json!({"id": 2200, "components": components, "connections": connections})
}

fn doc(microgrid: Value) -> Value {
    json!({"schema_version": 1, "microgrids": [microgrid]})
}

struct Server {
    _child: KillOnDrop,
    _dir: tempfile::TempDir,
    ui: String,
    client: reqwest::Client,
}

fn boot() -> Server {
    let dir = tempfile::TempDir::with_prefix("mc-declare-").unwrap();
    let (child, endpoints) = endpoints_of(dir.path(), &[]);
    let ui = format!("http://{}", endpoints["ui"].as_str().unwrap());
    Server {
        _child: child,
        _dir: dir,
        ui,
        // A request that hangs (a deadlock) fails the test instead.
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap(),
    }
}

impl Server {
    /// POST the document to `/api/microgrids` with `query`; the status and the
    /// JSON body.
    async fn post(&self, query: &str, body: &Value) -> (u16, Value) {
        let r = self
            .client
            .post(format!("{}/api/microgrids?{query}", self.ui))
            .json(body)
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap())
    }

    async fn declare(&self, body: &Value) -> (u16, Value) {
        let version = macrocosim::version::VERSION.as_str();
        self.post(&format!("existing=rebuild&client_version={version}"), body)
            .await
    }
}

fn outcome(answer: &Value) -> &str {
    answer["microgrids"][0]["outcome"].as_str().unwrap()
}

/// The same document again is `unchanged`, and a telemetry stream the app
/// opened stays open; a changed one is `rebuilt`, which ends it.
#[tokio::test]
async fn an_unchanged_declaration_keeps_the_apps_stream() {
    let s = boot();
    let (code, answer) = s.declare(&doc(site(false))).await;
    assert_eq!(code, 200, "{answer}");
    assert_eq!(outcome(&answer), "created");
    let runtime = &answer["microgrids"][0]["runtime"];
    assert_eq!(runtime["status"], "running", "{answer}");
    let addr = runtime["grpc_addr"].as_str().unwrap();
    assert!(!addr.ends_with(":8800"), "an OS-chosen port: {addr}");

    let mut grpc = MicrogridClient::connect(format!("http://{addr}"))
        .await
        .unwrap();
    let mut stream = grpc
        .receive_electrical_component_telemetry_stream(
            ReceiveElectricalComponentTelemetryStreamRequest {
                electrical_component_id: 3,
                filter: None,
            },
        )
        .await
        .unwrap()
        .into_inner();
    let next = async |stream: &mut tonic::Streaming<_>| {
        tokio::time::timeout(Duration::from_secs(5), stream.message())
            .await
            .expect("a sample within 5 s")
            .expect("the stream is open")
    };
    assert!(next(&mut stream).await.is_some());

    let (code, answer) = s.declare(&doc(site(false))).await;
    assert_eq!((code, outcome(&answer)), (200, "unchanged"), "{answer}");
    assert!(next(&mut stream).await.is_some(), "the stream continues");

    let (code, answer) = s.declare(&doc(site(true))).await;
    assert_eq!((code, outcome(&answer)), (200, "rebuilt"), "{answer}");
    // A rebuild ends the stream, so the check above means something.
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match stream.message().await {
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => return,
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "the rebuild ended the stream");
}

/// A client of another version is told so before anything else is read, even
/// when it also sends a parameter this engine does not know; an unknown
/// parameter or `existing` value alone is a 400.
#[tokio::test]
async fn the_version_is_checked_before_the_rest_of_the_query() {
    let s = boot();
    let body = doc(site(false));
    for query in [
        "existing=rebuild&client_version=0.0.1",
        "existing=rebuild&client_version=0.0.1&lease=30",
    ] {
        let (code, answer) = s.post(query, &body).await;
        let error = answer["error"].as_str().unwrap();
        assert_eq!(code, 400, "{query}: {answer}");
        assert!(
            error.contains("version 0.0.1")
                && error.contains(macrocosim::version::VERSION.as_str()),
            "{query}: {error}"
        );
    }
    for query in ["existing=rebuild&lease=30", "existing=replace"] {
        let (code, answer) = s.post(query, &body).await;
        assert_eq!(code, 400, "{query}: {answer}");
    }
}

/// Without `existing=rebuild`, a loaded id is refused as before.
#[tokio::test]
async fn a_create_still_refuses_a_loaded_id() {
    let s = boot();
    let body = doc(site(false));
    let (code, answer) = s.post("", &body).await;
    assert_eq!(code, 201, "{answer}");
    assert!(answer["microgrids"][0].get("outcome").is_none());
    let (code, answer) = s.post("existing=refuse", &body).await;
    assert_eq!(code, 409, "{answer}");
}

/// A declaration of the stored spec and a spec `PUT` of a changed one, sent
/// together, both finish, one after the other.
#[tokio::test]
async fn a_declaration_and_an_edit_sent_together_both_finish() {
    let s = boot();
    s.declare(&doc(site(false))).await;
    let put = s
        .client
        .put(format!("{}/api/mg/2200/spec", s.ui))
        .json(&site(true))
        .send();
    let same = doc(site(false));
    let (declared, put) = tokio::join!(s.declare(&same), put);
    let put = put.unwrap();
    assert!(put.status().is_success(), "{}", put.status());
    assert_eq!(declared.0, 200, "{}", declared.1);
    let stored: Value = s
        .client
        .get(format!("{}/api/mg/2200/spec", s.ui))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let has_meter = stored["components"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == 5);
    match outcome(&declared.1) {
        // The declaration ran first; the edit followed.
        "unchanged" => assert!(has_meter, "{stored}"),
        // The edit ran first; the declaration put the spec back.
        "rebuilt" => assert!(!has_meter, "{stored}"),
        other => panic!("{other}"),
    }
}
