//! gRPC integration tests for the assets API surface
//! (`frequenz.api.platformassets.v1alpha1.PlatformAssetsService`), and the
//! Assets API client (`assets_client`) read against it. Each test spawns a
//! fresh `TestServer` and exercises the assets RPCs against the same topology
//! the microgrid tests use.

mod common;

use common::{TestServer, venv_python};
use macrocosim::assets_client::{self, sign::Credentials};
use macrocosim::proto::assets::{
    GetMicrogridRequest, ListMicrogridElectricalComponentConnectionsRequest,
    ListMicrogridElectricalComponentsRequest, ListMicrogridSensorsRequest, ListMicrogridsRequest,
    list_microgrid_electrical_components_request::MicrogridElectricalComponentsFilter,
    list_microgrids_request::MicrogridsFilter,
    platform_assets_service_client::PlatformAssetsServiceClient,
};
use macrocosim::proto::common::microgrid::electrical_components as ec;
use macrocosim::spec::ComponentSpec;

const TINY_TOPOLOGY: &str = r#"{
  "id": 7,
  "components": [
    {"type": "battery", "id": 3, "rated_lower_w": -5000.0, "rated_upper_w": 5000.0},
    {"type": "battery_inverter", "id": 4, "rated_lower_w": -5000.0, "rated_upper_w": 5000.0},
    {"type": "meter", "id": 2},
    {"type": "grid_connection_point", "id": 1}
  ],
  "connections": [[4, 3], [2, 4], [1, 2]]
}"#;

fn component(id: u64, category: ec::ElectricalComponentCategory) -> ec::ElectricalComponent {
    ec::ElectricalComponent {
        id,
        category: category as i32,
        ..Default::default()
    }
}

fn hybrid(id: u64) -> ec::ElectricalComponent {
    ec::ElectricalComponent {
        category_specific_info: Some(ec::ElectricalComponentCategorySpecificInfo {
            kind: Some(
                ec::electrical_component_category_specific_info::Kind::Inverter(ec::Inverter {
                    r#type: ec::InverterType::Hybrid as i32,
                }),
            ),
        }),
        ..component(id, ec::ElectricalComponentCategory::Inverter)
    }
}

fn connections(pairs: &[(u64, u64)]) -> Vec<ec::ElectricalComponentConnection> {
    pairs
        .iter()
        .map(|&(a, b)| ec::ElectricalComponentConnection {
            source_electrical_component_id: a,
            destination_electrical_component_id: b,
            ..Default::default()
        })
        .collect()
}

async fn connect(s: &TestServer) -> PlatformAssetsServiceClient<tonic::transport::Channel> {
    PlatformAssetsServiceClient::connect(s.assets_url.clone())
        .await
        .expect("grpc connect")
}

#[tokio::test(flavor = "multi_thread")]
async fn get_microgrid_returns_configured_id() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let resp = c
        .get_microgrid(GetMicrogridRequest { microgrid_id: 7 })
        .await
        .expect("get ok")
        .into_inner();
    let m = resp.microgrid.expect("microgrid present");
    assert_eq!(m.id, 7);
    // The name the test server gives it, and active in the enterprise.
    assert_eq!(m.name, "default");
    assert_eq!(m.enterprise_id, s.engine.metadata().enterprise_id);
    assert_eq!(
        m.status,
        macrocosim::proto::common::microgrid::MicrogridStatus::Active as i32
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn get_microgrid_rejects_wrong_id() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let err = c
        .get_microgrid(GetMicrogridRequest { microgrid_id: 999 })
        .await
        .expect_err("wrong id should be rejected");
    assert_eq!(err.code(), tonic::Code::NotFound, "got {err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_components_returns_topology() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let resp = c
        .list_microgrid_electrical_components(ListMicrogridElectricalComponentsRequest {
            microgrid_id: 7,
            ..Default::default()
        })
        .await
        .expect("list ok")
        .into_inner();
    assert_eq!(resp.microgrid_id, 7);
    let ids: Vec<u64> = resp.components.iter().map(|c| c.id).collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
    assert!(ids.contains(&3));
    assert!(ids.contains(&4));
}

#[tokio::test(flavor = "multi_thread")]
async fn list_components_filters_by_id() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let resp = c
        .list_microgrid_electrical_components(ListMicrogridElectricalComponentsRequest {
            microgrid_id: 7,
            filter: Some(MicrogridElectricalComponentsFilter {
                component_ids: vec![2, 3],
                ..Default::default()
            }),
        })
        .await
        .expect("list ok")
        .into_inner();
    let mut ids: Vec<u64> = resp.components.iter().map(|c| c.id).collect();
    ids.sort();
    assert_eq!(ids, vec![2, 3]);
}

#[tokio::test(flavor = "multi_thread")]
async fn list_components_rejects_wrong_id() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let err = c
        .list_microgrid_electrical_components(ListMicrogridElectricalComponentsRequest {
            microgrid_id: 999,
            ..Default::default()
        })
        .await
        .expect_err("wrong id should be rejected");
    assert_eq!(err.code(), tonic::Code::NotFound, "got {err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_connections_returns_edges() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let resp = c
        .list_microgrid_electrical_component_connections(
            ListMicrogridElectricalComponentConnectionsRequest {
                microgrid_id: 7,
                ..Default::default()
            },
        )
        .await
        .expect("list ok")
        .into_inner();
    assert_eq!(resp.microgrid_id, 7);
    // grid → meter, meter → inverter, inverter → battery.
    assert_eq!(resp.connections.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn list_connections_rejects_wrong_id() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let err = c
        .list_microgrid_electrical_component_connections(
            ListMicrogridElectricalComponentConnectionsRequest {
                microgrid_id: 999,
                ..Default::default()
            },
        )
        .await
        .expect_err("wrong id should be rejected");
    assert_eq!(err.code(), tonic::Code::NotFound, "got {err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_components_filters_by_category() {
    use macrocosim::proto::common::microgrid::electrical_components::ElectricalComponentCategory;
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let resp = c
        .list_microgrid_electrical_components(ListMicrogridElectricalComponentsRequest {
            microgrid_id: 7,
            filter: Some(MicrogridElectricalComponentsFilter {
                categories: vec![ElectricalComponentCategory::Battery as i32],
                ..Default::default()
            }),
        })
        .await
        .expect("list ok")
        .into_inner();
    let ids: Vec<u64> = resp.components.iter().map(|c| c.id).collect();
    assert_eq!(ids, vec![3]);
}

#[tokio::test(flavor = "multi_thread")]
async fn list_connections_filters_by_source_and_destination() {
    use macrocosim::proto::assets::list_microgrid_electrical_component_connections_request::MicrogridElectricalComponentConnectionsFilter;
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let edges = |source: Vec<u64>, destination: Vec<u64>| {
        ListMicrogridElectricalComponentConnectionsRequest {
            microgrid_id: 7,
            filter: Some(MicrogridElectricalComponentConnectionsFilter {
                source_component_ids: source,
                destination_component_ids: destination,
            }),
        }
    };
    for (request, expected) in [
        (edges(vec![2], vec![]), vec![(2, 4)]),
        (edges(vec![], vec![3]), vec![(4, 3)]),
        (edges(vec![1, 2], vec![4]), vec![(2, 4)]),
    ] {
        let resp = c
            .list_microgrid_electrical_component_connections(request)
            .await
            .expect("list ok")
            .into_inner();
        let got: Vec<(u64, u64)> = resp
            .connections
            .iter()
            .map(|e| {
                (
                    e.source_electrical_component_id,
                    e.destination_electrical_component_id,
                )
            })
            .collect();
        assert_eq!(got, expected);
    }
}

/// The simulator has no gridpools, so every gridpool answer is empty or
/// NOT_FOUND.
#[tokio::test(flavor = "multi_thread")]
async fn gridpool_answers_are_empty() {
    use macrocosim::proto::assets::{
        ListGridpoolEnergySchedulesRequest, ListGridpoolsRequest,
        ListMarketTopologyRelationsRequest,
    };
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let in_gridpool = c
        .list_microgrids(ListMicrogridsRequest {
            filter: Some(MicrogridsFilter {
                gridpool_ids: vec![1],
                ..Default::default()
            }),
        })
        .await
        .expect("list ok")
        .into_inner();
    assert!(in_gridpool.microgrids.is_empty());
    let gridpools = c
        .list_gridpools(ListGridpoolsRequest::default())
        .await
        .expect("list ok")
        .into_inner();
    assert!(gridpools.gridpools.is_empty());
    let relations = c
        .list_market_topology_relations(ListMarketTopologyRelationsRequest::default())
        .await
        .expect("list ok")
        .into_inner();
    assert!(relations.relations.is_empty());
    let err = c
        .list_gridpool_energy_schedules(ListGridpoolEnergySchedulesRequest {
            gridpool_id: 1,
            filter: None,
        })
        .await
        .expect_err("no gridpool 1");
    assert_eq!(err.code(), tonic::Code::NotFound, "got {err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_microgrids_lists_the_registry() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let all = c
        .list_microgrids(ListMicrogridsRequest::default())
        .await
        .expect("list ok")
        .into_inner();
    assert_eq!(
        all.microgrids.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![7]
    );
    let none = c
        .list_microgrids(ListMicrogridsRequest {
            filter: Some(MicrogridsFilter {
                microgrid_ids: vec![999],
                ..Default::default()
            }),
        })
        .await
        .expect("list ok")
        .into_inner();
    assert!(none.microgrids.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn list_sensors_is_empty_and_rejects_wrong_id() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let resp = c
        .list_microgrid_sensors(ListMicrogridSensorsRequest {
            microgrid_id: 7,
            filter: None,
        })
        .await
        .expect("list ok")
        .into_inner();
    assert!(resp.sensors.is_empty());
    let err = c
        .list_microgrid_sensors(ListMicrogridSensorsRequest {
            microgrid_id: 999,
            filter: None,
        })
        .await
        .expect_err("wrong id should be rejected");
    assert_eq!(err.code(), tonic::Code::NotFound, "got {err:?}");
}

/// The Assets API client reads the simulator's own Assets API back into a spec
/// with the same ids, types, rated bounds and connections.
#[tokio::test(flavor = "multi_thread")]
async fn the_fetch_reads_a_microgrid_back() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let url = s.assets_url.replace("http://", "grpc://") + "?ssl=false";
    let creds = Credentials::new("key", "secret").unwrap();
    let fetched = assets_client::fetch(&url, creds, 7)
        .await
        .expect("fetch ok");
    assert!(fetched.left_out.is_empty(), "{:?}", fetched.left_out);
    let spec = fetched.spec;
    assert_eq!(spec.id, Some(7));
    assert_eq!(spec.name.as_deref(), Some("default"));
    let mut types: Vec<(u64, &str)> = spec
        .components
        .iter()
        .map(|c| (c.id().unwrap(), c.type_name()))
        .collect();
    types.sort();
    assert_eq!(
        types,
        vec![
            (1, "grid_connection_point"),
            (2, "meter"),
            (3, "battery"),
            (4, "battery_inverter"),
        ]
    );
    let inverter = spec
        .components
        .iter()
        .find_map(|c| match c {
            ComponentSpec::BatteryInverter(i) => Some(i),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (inverter.rated_lower_w, inverter.rated_upper_w),
        (Some(-5000.0), Some(5000.0))
    );
    let mut edges = spec.connections.clone();
    edges.sort();
    assert_eq!(edges, vec![(1, 2), (2, 4), (4, 3)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_fetch_names_a_missing_microgrid() {
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let url = s.assets_url.replace("http://", "grpc://") + "?ssl=false";
    let creds = Credentials::new("key", "secret").unwrap();
    let err = assets_client::fetch(&url, creds, 999).await.unwrap_err();
    assert!(err.contains("NotFound") && err.contains("999"), "{err}");
}

/// A refused connection names the operating system's reason, not only tonic's
/// "transport error".
#[tokio::test(flavor = "multi_thread")]
async fn the_fetch_names_why_it_could_not_connect() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = format!("grpc://127.0.0.1:{port}?ssl=false");
    let creds = Credentials::new("key", "secret").unwrap();
    let err = assets_client::fetch(&url, creds, 7).await.unwrap_err();
    assert!(err.contains("Connection refused"), "{err}");
}

/// A server that takes the connection and never answers fails the fetch
/// when its time is up.
#[tokio::test(flavor = "multi_thread")]
async fn the_fetch_gives_up_on_a_silent_server() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("grpc://{}?ssl=false", listener.local_addr().unwrap());
    let held = tokio::spawn(async move {
        let mut sockets = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            sockets.push(socket);
        }
    });
    let creds = Credentials::new("key", "secret").unwrap();
    let timeout = std::time::Duration::from_millis(300);
    let err = assets_client::fetch_within(&url, creds, 7, timeout)
        .await
        .unwrap_err();
    assert!(err.contains("no complete answer within 300ms"), "{err}");
    held.abort();
}

/// The fetch's calls carry the signing metadata on the wire: a server that
/// refuses calls without it still answers.
#[tokio::test(flavor = "multi_thread")]
async fn the_fetch_signs_its_calls() {
    use macrocosim::assets_server::AssetsServer;
    use macrocosim::proto::assets::platform_assets_service_server::PlatformAssetsServiceServer;
    let s = TestServer::start_mg(TINY_TOPOLOGY).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let signed = |req: tonic::Request<()>| {
        let md = req.metadata();
        if ["key", "ts", "nonce", "sig"]
            .iter()
            .all(|k| md.get(*k).is_some())
        {
            Ok(req)
        } else {
            Err(tonic::Status::unauthenticated("unsigned"))
        }
    };
    let server =
        PlatformAssetsServiceServer::with_interceptor(AssetsServer::new(s.engine.clone()), signed);
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(server)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
    );
    let url = format!("grpc://{addr}?ssl=false");
    let creds = Credentials::new("key", "secret").unwrap();
    let fetched = assets_client::fetch(&url, creds, 7)
        .await
        .expect("fetch ok");
    assert_eq!(fetched.spec.id, Some(7));
}

/// A hybrid inverter and its battery come out commented, in the successors of
/// the meter above them.
#[tokio::test(flavor = "multi_thread")]
async fn a_hybrid_inverter_is_written_commented_out() {
    use macrocosim::proto::common::microgrid::Microgrid;
    use macrocosim::proto::common::microgrid::electrical_components::ElectricalComponentCategory as C;
    let Some(python) = venv_python() else { return };
    let components = [
        component(1, C::GridConnectionPoint),
        component(2, C::Meter),
        hybrid(3),
        component(4, C::Battery),
    ];
    let connections = connections(&[(1, 2), (2, 3), (3, 4)]);
    let microgrid = Microgrid {
        id: 7,
        ..Default::default()
    };
    let fetched = assets_client::spec_of(
        &microgrid,
        &components,
        &connections,
        std::time::SystemTime::now(),
    )
    .unwrap();
    let (input, notes) = fetched.module_input();
    assert_eq!(
        notes,
        vec![
            "hybrid inverter 3 and the components that go with it are written commented out \
             under component 2"
        ]
    );
    let text = macrocosim::python::write_module(&python, &input)
        .await
        .unwrap();
    assert!(
        text.contains(
            "            mc.make_meter(\n                id=2,\n                successors=[\n                    # Needs a hybrid inverter model:\n                    # mc.make_hybrid_inverter(\n                    #     id=3,\n"
        ),
        "{text}"
    );
    assert!(text.contains("#         mc.make_battery(id=4),"), "{text}");
}
