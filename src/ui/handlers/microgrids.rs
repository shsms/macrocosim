//! The microgrid collection: list them, create them from a topology
//! document, and import one from a site export (both in memory only).

use std::collections::HashMap;

use axum::extract::{FromRequest, Request, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::engine::{Engine, topology};
use crate::runtime::{MicrogridRuntimes, RuntimeView};
use crate::spec::TopologyDoc;
use crate::ui::api::{ApiError, Json};

/// One `/api/microgrids` entry: the registry's view of the microgrid
/// plus its runtime, `null` when none was started.
#[derive(Serialize)]
pub(in crate::ui) struct MicrogridListEntry {
    #[serde(flatten)]
    view: crate::sim::microgrids::MicrogridView,
    runtime: Option<RuntimeView>,
}

pub(in crate::ui) async fn microgrids_list(
    State(engine): State<Engine>,
    State(runtimes): State<MicrogridRuntimes>,
) -> Json<Vec<MicrogridListEntry>> {
    Json(
        crate::sim::microgrids::snapshot(&engine.microgrids())
            .into_iter()
            .map(|view| {
                let runtime = runtimes.status(view.id);
                MicrogridListEntry { view, runtime }
            })
            .collect(),
    )
}

/// What `POST /api/microgrids` does with a microgrid id that is already loaded.
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(in crate::ui) enum Existing {
    /// Refuse the document (409), as a create does.
    #[default]
    Refuse,
    /// Rebuild it with carry, or leave it when its spec is unchanged: a
    /// declaration ([`topology::declare`]).
    Rebuild,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct CreateQuery {
    #[serde(default)]
    existing: Existing,
    /// The client's package version in PEP 440 form, compared before this query
    /// is parsed; listed so it is not an unknown field.
    #[serde(default, rename = "client_version")]
    _client_version: Option<String>,
}

/// POST /api/microgrids — create the microgrids of a topology document, in
/// memory only. A microgrid without an id or a `grpc_port` takes the lowest
/// free one; every component needs an id (422 without). Each microgrid's
/// runtime is started and reported in the 201, `null` when none was started:
/// the microgrid exists either way. 409 when a microgrid id, a component id or
/// a port is taken; nothing is created then.
///
/// With `?existing=rebuild`, the document is a declaration instead: 200 with
/// one entry per microgrid and its `outcome` (`created`, `rebuilt` or
/// `unchanged`). A `client_version` that is not this engine's is a 400, checked
/// before the rest of the query and the body are read, so a newer client is
/// told why.
pub(in crate::ui) async fn microgrids_create(
    State(engine): State<Engine>,
    State(runtimes): State<MicrogridRuntimes>,
    request: Request,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let uri = request.uri().clone();
    let loose = axum::extract::Query::<HashMap<String, String>>::try_from_uri(&uri)
        .map_err(|e| ApiError::new(e.status(), e.body_text()))?;
    if let Some(client) = loose.get("client_version")
        && client != crate::version::VERSION.as_str()
    {
        let message = crate::version::mismatch_message(client);
        return Err(ApiError::bad_request(message));
    }
    let axum::extract::Query(query) = axum::extract::Query::<CreateQuery>::try_from_uri(&uri)
        .map_err(|e| ApiError::new(e.status(), e.body_text()))?;
    let Json(body) = Json::<serde_json::Value>::from_request(request, &()).await?;
    let doc = TopologyDoc::from_value(body)
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    if doc.microgrids.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the document has no microgrids",
        ));
    }
    super::structural_change(engine, move |structural| async move {
        let (status, microgrids) = match query.existing {
            Existing::Refuse => {
                let created = topology::create_doc(&structural, &runtimes, doc, None).await?;
                (StatusCode::CREATED, serde_json::json!(created))
            }
            Existing::Rebuild => {
                let declared = topology::declare(&structural, &runtimes, doc).await?;
                (StatusCode::OK, serde_json::json!(declared))
            }
        };
        Ok((
            status,
            Json(serde_json::json!({ "microgrids": microgrids })),
        ))
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct ImportMicrogridBody {
    name: String,
    /// Microgrid id to claim, the one the import dialog asks for.
    /// Omit to take the lowest free one, as every import did before
    /// the dialog existed — the field is additive.
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    tso: Option<String>,
    /// The site export's components.json, verbatim.
    components: crate::sim::site_import::ComponentsFile,
    /// The site export's connections.json, verbatim (optional).
    #[serde(default)]
    connections: Option<crate::sim::site_import::ConnectionsFile>,
}

#[derive(Serialize)]
pub(in crate::ui) struct ImportMicrogridResp {
    id: u64,
    name: String,
    grpc_port: u16,
    tso: Option<String>,
    components: usize,
    connections: usize,
    /// The new microgrid's runtime; `null` when none was started.
    runtime: Option<RuntimeView>,
}

/// POST /api/microgrids/import — create a microgrid from a site
/// export, in memory only, as `POST /api/microgrids` does: the export
/// becomes a spec that keeps its component ids, and capacity, SoC
/// bounds, rated power bounds and the grid's rated fuse current all
/// survive into the simulation.
///
/// Component ids are unique across the engine, so an id another
/// microgrid holds refuses the whole import with a 409 and nothing is
/// created. The MICROGRID's own id is a separate matter: `id` claims
/// one (the import dialog asks for it, pre-filled with the lowest
/// free one), and omitting it takes the lowest free one; a taken one
/// is a 409 too.
pub(in crate::ui) async fn microgrids_import(
    State(engine): State<Engine>,
    State(runtimes): State<MicrogridRuntimes>,
    Json(body): Json<ImportMicrogridBody>,
) -> Result<Json<ImportMicrogridResp>, ApiError> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("name must be non-empty"));
    }
    let import = crate::sim::site_import::parse(body.components, body.connections)
        .map_err(ApiError::bad_request)?;
    let mut spec = import.spec().map_err(ApiError::bad_request)?;
    spec.id = body.id;
    spec.name = Some(name.clone());
    spec.tso = body.tso;
    let (created, tso) = super::structural_change(engine, move |structural| async move {
        let tso = spec.tso.clone();
        Ok((
            topology::create(&structural, &runtimes, spec, None).await?,
            tso,
        ))
    })
    .await?;
    Ok(Json(ImportMicrogridResp {
        id: created.id,
        name,
        grpc_port: created.grpc_port,
        tso,
        components: import.components.len(),
        connections: import.connections.len(),
        runtime: created.runtime,
    }))
}
