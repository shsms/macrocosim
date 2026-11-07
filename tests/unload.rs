//! `DELETE /api/mg/{mg}` and a `PUT` that moves the gRPC port: what a
//! runtime started stops, and what the microgrid held is free again.

mod common;

use std::time::{Duration, Instant};

use common::{TestServer, WsClient, get_spec, put_spec};
use macrocosim::proto::microgrid::microgrid_client::MicrogridClient;
use macrocosim::proto::microgrid::{
    ListElectricalComponentsRequest, ReceiveElectricalComponentTelemetryStreamRequest,
};
use macrocosim::spec::{MicrogridSpec, TopologyDoc};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// Microgrid 7: a load (2) and a battery chain (3 → 4) under the grid
/// connection point.
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

fn doc(microgrids: Vec<Value>) -> TopologyDoc {
    serde_json::from_value(json!({"schema_version": 1, "microgrids": microgrids})).unwrap()
}

fn spec(value: Value) -> MicrogridSpec {
    doc(vec![value]).microgrids.remove(0)
}

async fn delete(client: &reqwest::Client, s: &TestServer, mg: u64) -> (StatusCode, String) {
    let r = client
        .delete(format!("{}/api/mg/{mg}", s.ui_url))
        .send()
        .await
        .unwrap();
    (r.status(), r.text().await.unwrap())
}

/// Create `spec` the way a structural route does, under the
/// structural lock; the new runtime's gRPC address.
async fn create(s: &TestServer, spec: MicrogridSpec) -> String {
    let structural = s.engine.structural().await;
    macrocosim::engine::topology::create(&structural, &s.runtimes, spec, None)
        .await
        .expect("create")
        .runtime
        .and_then(|r| r.grpc_addr)
        .expect("the new microgrid started")
}

fn grpc_addr(s: &TestServer, mg: u64) -> String {
    s.runtimes
        .status(mg)
        .and_then(|r| r.grpc_addr)
        .expect("a bound runtime")
}

async fn component_ids(addr: &str) -> Vec<u64> {
    let mut c = MicrogridClient::connect(format!("http://{addr}"))
        .await
        .unwrap();
    let mut ids: Vec<u64> = c
        .list_electrical_components(ListElectricalComponentsRequest::default())
        .await
        .unwrap()
        .into_inner()
        .electrical_components
        .iter()
        .map(|c| c.id)
        .collect();
    ids.sort_unstable();
    ids
}

/// After a `DELETE` the microgrid is gone, its port can be bound, and
/// its id and component ids can be taken by a new microgrid.
#[tokio::test(flavor = "multi_thread")]
async fn delete_frees_the_port_and_the_ids() {
    let s = TestServer::start_spec(doc(vec![plant()])).await;
    let client = reqwest::Client::new();
    let addr = grpc_addr(&s, 7);

    let (code, body) = delete(&client, &s, 7).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{body}");
    assert!(!s.engine.microgrids().lock().contains_key(&7));
    assert!(s.runtimes.status(7).is_none());
    assert!(s.runtimes.loopbacks().read().get(&7).is_none());
    drop(std::net::TcpListener::bind(&addr).expect("the old port is free"));
    let (code, _) = get_spec(&client, &s, 7).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let (code, _) = delete(&client, &s, 7).await;
    assert_eq!(code, StatusCode::NOT_FOUND);

    // Another microgrid takes the component ids.
    let mut again = plant();
    again["id"] = json!(9);
    let addr = create(&s, spec(again)).await;
    assert_eq!(component_ids(&addr).await, vec![1, 2, 3, 4]);
}

/// A client holding a telemetry stream open does not hold the
/// `DELETE` up: the stream ends, and the answer comes within 3 s.
#[tokio::test(flavor = "multi_thread")]
async fn delete_ends_an_open_stream_in_time() {
    let s = TestServer::start_spec(doc(vec![plant()])).await;
    let mut grpc = MicrogridClient::connect(s.grpc_url.clone()).await.unwrap();
    let mut stream = grpc
        .receive_electrical_component_telemetry_stream(
            ReceiveElectricalComponentTelemetryStreamRequest {
                electrical_component_id: 2,
                filter: None,
            },
        )
        .await
        .unwrap()
        .into_inner();
    assert!(stream.message().await.unwrap().is_some(), "the stream runs");

    let started = Instant::now();
    let (code, body) = delete(&reqwest::Client::new(), &s, 7).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{body}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the DELETE took {:?}",
        started.elapsed()
    );
    let ended = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match stream.message().await {
                Ok(Some(_)) => continue,
                other => break other,
            }
        }
    })
    .await
    .expect("the stream ended");
    assert!(!matches!(ended, Ok(Some(_))));
}

/// The microgrid's dispatches are deleted, each with a `Deleted`
/// event; another microgrid's stay.
#[tokio::test(flavor = "multi_thread")]
async fn delete_removes_the_dispatches() {
    use macrocosim::proto::dispatch::DispatchData;
    use macrocosim::sim::dispatch::DispatchChange;
    let s = TestServer::start_spec(doc(vec![plant()])).await;
    let store = s.engine.dispatches();
    let data = || DispatchData {
        r#type: "charge".into(),
        ..Default::default()
    };
    let made = store.create(7, data(), true).unwrap();
    store.create(2200, data(), true).unwrap();
    let mut changes = store.subscribe();

    let (code, body) = delete(&reqwest::Client::new(), &s, 7).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{body}");
    assert!(store.list_mg(7).is_empty());
    assert_eq!(store.list_mg(2200).len(), 1);
    let ev = changes.try_recv().expect("a Deleted event");
    assert_eq!(ev.microgrid_id, 7);
    assert_eq!(ev.change, DispatchChange::Deleted);
    assert_eq!(ev.dispatch_id(), made.metadata.unwrap().dispatch_id);
}

/// A microgrid created again under the same id gets a runtime and a
/// loopback of its own: the UI's client lists the new components.
#[tokio::test(flavor = "multi_thread")]
async fn a_recreated_microgrid_starts_and_its_loopback_serves_it() {
    let s = TestServer::start_spec(doc(vec![plant()])).await;
    let old_slot = s.runtimes.loopbacks().read()[&7].clone();
    let (code, body) = delete(&reqwest::Client::new(), &s, 7).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{body}");

    let mut changed = plant();
    changed["components"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type": "meter", "id": 50}));
    changed["connections"]
        .as_array_mut()
        .unwrap()
        .push(json!([1, 50]));
    let addr = create(&s, spec(changed)).await;
    assert_eq!(component_ids(&addr).await, vec![1, 2, 3, 4, 50]);

    let slot = s.runtimes.loopbacks().read()[&7].clone();
    assert!(!std::sync::Arc::ptr_eq(&slot, &old_slot));
    assert!(
        old_slot.microgrid.read().is_none(),
        "the old loopback let go"
    );
    let client = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(mg) = &*slot.microgrid.read() {
                break mg.client();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the new loopback builds its graph");
    let mut ids: Vec<u64> = client
        .list_electrical_components(vec![], vec![])
        .await
        .unwrap()
        .iter()
        .map(|c| c.id)
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2, 3, 4, 50]);
}

/// A `PUT` with a new `grpc_port` moves the gRPC server and frees the
/// old port. The harness binds OS-chosen ports, so the server lands
/// on a fresh one rather than the number asked for; `restart_grpc`'s
/// own test covers a fixed port.
#[tokio::test(flavor = "multi_thread")]
async fn a_put_with_a_new_grpc_port_moves_the_server() {
    let s = TestServer::start_spec(doc(vec![plant()])).await;
    let client = reqwest::Client::new();
    let old = grpc_addr(&s, 7);
    let (_, stored) = get_spec(&client, &s, 7).await;
    let new_port = stored["grpc_port"].as_u64().unwrap() + 1;
    let mut moved = plant();
    moved["grpc_port"] = json!(new_port);
    let (code, body) = put_spec(&client, &s, 7, &moved).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["grpc_port"], new_port);
    assert_eq!(
        s.engine.microgrids().lock()[&7].def.grpc_port,
        new_port as u16
    );

    let new = grpc_addr(&s, 7);
    assert_ne!(new, old);
    drop(std::net::TcpListener::bind(&old).expect("the old port is free"));
    assert_eq!(component_ids(&new).await, vec![1, 2, 3, 4]);
}

/// The events socket follows a microgrid created again under the same
/// id right after an unload: it tells the UI the microgrid was
/// unloaded, or, when the new one is already registered by the time
/// the socket handles the notice, that its topology changed; and the
/// new microgrid's events arrive.
#[tokio::test(flavor = "multi_thread")]
async fn the_events_socket_follows_an_unload_and_a_recreate() {
    let s = TestServer::start_spec(doc(vec![plant()])).await;
    let mut ws = WsClient::connect(&s.ui_url).await;
    // A sample from microgrid 7 proves the socket follows it.
    ws.wait_for(Duration::from_secs(10), "event from microgrid 7", |ev| {
        ev["microgrid_id"] == 7
    })
    .await;

    let (code, body) = delete(&reqwest::Client::new(), &s, 7).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{body}");
    let mut again = plant();
    again["components"] = json!([
        {"type": "grid_connection_point", "id": 1},
        {"type": "meter", "id": 50, "power_w": 1000.0}
    ]);
    again["connections"] = json!([[1, 50]]);
    create(&s, spec(again)).await;

    let (mut told, mut new_sample) = (false, false);
    ws.wait_for(
        Duration::from_secs(10),
        "notice and sample of 50 from the new microgrid 7",
        |ev| {
            if ev["microgrid_id"] == 7 {
                told |= ev["kind"] == "microgrid_unloaded" || ev["kind"] == "topology_changed";
                new_sample |= ev["kind"] == "sample" && ev["component_id"] == 50;
            }
            told && new_sample
        },
    )
    .await;
}

/// A plain unload reaches the events socket as `microgrid_unloaded`.
#[tokio::test(flavor = "multi_thread")]
async fn the_events_socket_names_the_unloaded_microgrid() {
    let s = TestServer::start_spec(doc(vec![plant()])).await;
    let mut ws = WsClient::connect(&s.ui_url).await;
    ws.wait_for(Duration::from_secs(10), "event from microgrid 7", |ev| {
        ev["microgrid_id"] == 7
    })
    .await;

    let (code, body) = delete(&reqwest::Client::new(), &s, 7).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{body}");
    ws.wait_for(Duration::from_secs(10), "microgrid_unloaded for 7", |ev| {
        ev["kind"] == "microgrid_unloaded" && ev["microgrid_id"] == 7
    })
    .await;
}
