//! gRPC server: implements the Frequenz Assets API
//! (`frequenz.api.platformassets.v1alpha1.PlatformAssetsService`) on top of
//! macrocosim's `MicrogridSite` registry + `Engine`.
//!
//! Exposes the same topology data as `MicrogridServer`, just under the assets
//! API surface. Each microgrid RPC looks up the requested `microgrid_id` in the
//! enterprise-scoped registry and dispatches against that entry's
//! `MicrogridSite` — anything missing comes back as `NotFound`. The enterprise
//! has no gridpools, market relations or sensors: those lists are empty.

use crate::engine::Engine;
use crate::proto::assets::{
    GetMicrogridRequest, GetMicrogridResponse, ListGridpoolEnergySchedulesRequest,
    ListGridpoolEnergySchedulesResponse, ListGridpoolsRequest, ListGridpoolsResponse,
    ListMarketTopologyRelationsRequest, ListMarketTopologyRelationsResponse,
    ListMicrogridElectricalComponentConnectionsRequest,
    ListMicrogridElectricalComponentConnectionsResponse, ListMicrogridElectricalComponentsRequest,
    ListMicrogridElectricalComponentsResponse, ListMicrogridSensorsRequest,
    ListMicrogridSensorsResponse, ListMicrogridsRequest, ListMicrogridsResponse,
    platform_assets_service_server,
};
use crate::proto::common::microgrid::Microgrid;
use crate::proto::common::microgrid::electrical_components::ElectricalComponentConnection;
use crate::proto_conv::{make_component_proto, make_microgrid_proto};
use crate::sim::MicrogridSite;
use crate::sim::microgrids::MicrogridDef;

pub struct AssetsServer {
    pub engine: Engine,
}

impl AssetsServer {
    pub fn new(engine: Engine) -> Self {
        Self { engine }
    }

    /// Resolve the registry entry for `requested` or return `NotFound`.
    /// Returned tuple is `(MicrogridDef, MicrogridSite)` so callers
    /// dispatch components / connections against the resolved site
    /// instead of the bootstrap one.
    fn lookup_microgrid(
        &self,
        requested: u64,
    ) -> Result<(MicrogridDef, MicrogridSite), tonic::Status> {
        let reg = self.engine.microgrids();
        let r = reg.lock();
        r.get(&requested)
            .map(|e| (e.def.clone(), e.site.clone()))
            .ok_or_else(|| tonic::Status::not_found(format!("microgrid {requested} not found")))
    }

    /// The Assets API's view of the microgrid `def` describes.
    fn microgrid_of(&self, def: &MicrogridDef) -> Microgrid {
        make_microgrid_proto(def.id, &def.name, self.engine.metadata().enterprise_id)
    }
}

#[tonic::async_trait]
impl platform_assets_service_server::PlatformAssetsService for AssetsServer {
    async fn get_microgrid(
        &self,
        request: tonic::Request<GetMicrogridRequest>,
    ) -> Result<tonic::Response<GetMicrogridResponse>, tonic::Status> {
        let req = request.into_inner();
        let (def, _site) = self.lookup_microgrid(req.microgrid_id)?;
        Ok(tonic::Response::new(GetMicrogridResponse {
            microgrid: Some(self.microgrid_of(&def)),
        }))
    }

    async fn list_microgrids(
        &self,
        request: tonic::Request<ListMicrogridsRequest>,
    ) -> Result<tonic::Response<ListMicrogridsResponse>, tonic::Status> {
        let filter = request.into_inner().filter.unwrap_or_default();
        // No microgrid is in a gridpool, so a gridpool filter matches none.
        if !filter.gridpool_ids.is_empty() {
            return Ok(tonic::Response::new(ListMicrogridsResponse::default()));
        }
        let defs: Vec<MicrogridDef> = {
            let reg = self.engine.microgrids();
            let r = reg.lock();
            r.values()
                .filter(|e| {
                    filter.microgrid_ids.is_empty() || filter.microgrid_ids.contains(&e.def.id)
                })
                .map(|e| e.def.clone())
                .collect()
        };
        Ok(tonic::Response::new(ListMicrogridsResponse {
            microgrids: defs.iter().map(|d| self.microgrid_of(d)).collect(),
        }))
    }

    async fn list_gridpools(
        &self,
        _request: tonic::Request<ListGridpoolsRequest>,
    ) -> Result<tonic::Response<ListGridpoolsResponse>, tonic::Status> {
        Ok(tonic::Response::new(ListGridpoolsResponse::default()))
    }

    async fn list_gridpool_energy_schedules(
        &self,
        request: tonic::Request<ListGridpoolEnergySchedulesRequest>,
    ) -> Result<tonic::Response<ListGridpoolEnergySchedulesResponse>, tonic::Status> {
        let id = request.into_inner().gridpool_id;
        Err(tonic::Status::not_found(format!("gridpool {id} not found")))
    }

    async fn list_market_topology_relations(
        &self,
        _request: tonic::Request<ListMarketTopologyRelationsRequest>,
    ) -> Result<tonic::Response<ListMarketTopologyRelationsResponse>, tonic::Status> {
        Ok(tonic::Response::new(
            ListMarketTopologyRelationsResponse::default(),
        ))
    }

    async fn list_microgrid_sensors(
        &self,
        request: tonic::Request<ListMicrogridSensorsRequest>,
    ) -> Result<tonic::Response<ListMicrogridSensorsResponse>, tonic::Status> {
        let req = request.into_inner();
        self.lookup_microgrid(req.microgrid_id)?;
        Ok(tonic::Response::new(ListMicrogridSensorsResponse {
            microgrid_id: req.microgrid_id,
            sensors: Vec::new(),
        }))
    }

    async fn list_microgrid_electrical_components(
        &self,
        request: tonic::Request<ListMicrogridElectricalComponentsRequest>,
    ) -> Result<tonic::Response<ListMicrogridElectricalComponentsResponse>, tonic::Status> {
        let req = request.into_inner();
        let (_def, site) = self.lookup_microgrid(req.microgrid_id)?;
        let filter = req.filter.unwrap_or_default();

        let components: Vec<_> = site
            .components()
            .iter()
            .filter(|c| !c.is_hidden())
            .map(|c| make_component_proto(c.as_ref(), &site, req.microgrid_id))
            .filter(|c| {
                (filter.component_ids.is_empty() || filter.component_ids.contains(&c.id))
                    && (filter.categories.is_empty() || filter.categories.contains(&c.category))
            })
            .collect();
        Ok(tonic::Response::new(
            ListMicrogridElectricalComponentsResponse {
                microgrid_id: req.microgrid_id,
                components,
            },
        ))
    }

    async fn list_microgrid_electrical_component_connections(
        &self,
        request: tonic::Request<ListMicrogridElectricalComponentConnectionsRequest>,
    ) -> Result<tonic::Response<ListMicrogridElectricalComponentConnectionsResponse>, tonic::Status>
    {
        let req = request.into_inner();
        let (_def, site) = self.lookup_microgrid(req.microgrid_id)?;
        let filter = req.filter.unwrap_or_default();

        let connections: Vec<_> = site
            .connections()
            .into_iter()
            .map(|(from, to)| ElectricalComponentConnection {
                source_electrical_component_id: from,
                destination_electrical_component_id: to,
                ..Default::default()
            })
            .filter(|c| {
                (filter.source_component_ids.is_empty()
                    || filter
                        .source_component_ids
                        .contains(&c.source_electrical_component_id))
                    && (filter.destination_component_ids.is_empty()
                        || filter
                            .destination_component_ids
                            .contains(&c.destination_electrical_component_id))
            })
            .collect();
        Ok(tonic::Response::new(
            ListMicrogridElectricalComponentConnectionsResponse {
                microgrid_id: req.microgrid_id,
                connections,
            },
        ))
    }
}
