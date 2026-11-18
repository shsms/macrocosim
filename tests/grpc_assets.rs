//! gRPC integration tests for the assets API surface
//! (`frequenz.api.platformassets.v1alpha1.PlatformAssetsService`). Each
//! test spawns a fresh `TestServer` and exercises the assets RPCs
//! against the same topology the microgrid tests use.

mod common;

use common::TestServer;
use macrocosim::proto::assets::{
    GetMicrogridRequest, ListMicrogridElectricalComponentConnectionsRequest,
    ListMicrogridElectricalComponentsRequest, ListMicrogridSensorsRequest, ListMicrogridsRequest,
    list_microgrid_electrical_components_request::MicrogridElectricalComponentsFilter,
    list_microgrids_request::MicrogridsFilter,
    platform_assets_service_client::PlatformAssetsServiceClient,
};

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
