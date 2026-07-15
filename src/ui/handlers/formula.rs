//! Explained-formula endpoint for the Formulas subview.
//!
//! Unlike `/api/mg/{id}/microgrid/formulas` (which reads rendered
//! strings off the loopback client's logical meter), this endpoint
//! builds its own [`ComponentGraph`] straight from the site via
//! [`graph_adapter`], so it works even when the loopback slot is
//! empty, and it can honor per-request engine options. Sites are
//! small; a per-request build is cheap.

use std::collections::BTreeSet;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use frequenz_microgrid_component_graph::{
    ComponentGraphConfig, ErrorKind, ExplainedFormula, FormulaOverrides,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::resolve_site;
use crate::lisp::Config;
use crate::sim::graph_adapter;

#[derive(Deserialize)]
pub(in crate::ui) struct FormulaQuery {
    metric: String,
    /// Comma-separated component ids for the id-taking metrics.
    #[serde(default)]
    ids: Option<String>,
    /// Engine options, mirroring `ComponentGraphConfig`. The UI keeps
    /// these client-side and passes them on every request.
    #[serde(default)]
    prefer_meters: bool,
    #[serde(default)]
    phantom_loads: bool,
    #[serde(default)]
    no_fallback: bool,
}

/// GET /api/mg/{mg_id}/formula?metric=grid[&ids=1,2][&prefer_meters=true…]
/// — the formula, its AST, and the explanation tree.
pub(in crate::ui) async fn formula_for_mg(
    State(config): State<Config>,
    Path(mg_id): Path<u64>,
    Query(query): Query<FormulaQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let site = resolve_site(&config, mg_id)?;
    // spawn_blocking like the other CPU-bound handlers: on a large
    // imported site the graph validation + explanation tree is real
    // CPU work, and running it inline would stall a tokio worker
    // that also serves the WS pump and telemetry forwarders.
    tokio::task::spawn_blocking(move || formula_body(site, query))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

fn formula_body(
    site: crate::sim::MicrogridSite,
    query: FormulaQuery,
) -> Result<Json<Value>, (StatusCode, String)> {
    let graph_config = ComponentGraphConfig::builder()
        .prefer_meters_in_component_formulas(query.prefer_meters)
        .include_phantom_loads_in_consumer_formula(query.phantom_loads)
        .disable_fallback_components(query.no_fallback)
        .formula_overrides(FormulaOverrides::builder().build())
        .build();
    let (nodes, edges) = graph_adapter::snapshot(&site);
    if nodes.is_empty() {
        return Ok(Json(json!({
            "ok": false,
            "error": "The microgrid has no components yet.",
        })));
    }
    let graph = match graph_adapter::build_from_with_config(nodes, edges, graph_config) {
        Ok(graph) => graph,
        Err(e) => {
            return Ok(Json(json!({
                "ok": false,
                "error": format!("Invalid graph: {e}"),
                "kind": kind_of(&e),
            })));
        }
    };
    let ids: Option<BTreeSet<u64>> = match query.ids.as_deref() {
        None | Some("") => None,
        Some(text) => match parse_ids(text) {
            Ok(set) => Some(set),
            Err(e) => return Ok(Json(json!({ "ok": false, "error": e }))),
        },
    };
    // The single-component metrics need exactly one id; anything else
    // is an error rather than a silent pick.
    let single_id = match ids.as_ref() {
        Some(set) if set.len() == 1 => set.first().copied(),
        _ => None,
    };
    let explained: Result<ExplainedFormula, _> = match query.metric.as_str() {
        "grid" => graph.grid_formula_explained(),
        "consumer" => graph.consumer_formula_explained(),
        "producer" => graph.producer_formula_explained(),
        "battery" => graph.battery_formula_explained(ids),
        "pv" => graph.pv_formula_explained(ids),
        "chp" => graph.chp_formula_explained(ids),
        "wind_turbine" => graph.wind_turbine_formula_explained(ids),
        "ev_charger" => graph.ev_charger_formula_explained(ids),
        "steam_boiler" => graph.steam_boiler_formula_explained(ids),
        "grid_coalesce" => graph.grid_coalesce_formula_explained(),
        "battery_ac_coalesce" => graph.battery_ac_coalesce_formula_explained(ids),
        "pv_ac_coalesce" => graph.pv_ac_coalesce_formula_explained(ids),
        metric @ ("component" | "component_ac_coalesce") => match single_id {
            Some(id) if metric == "component" => graph.component_formula_explained(id),
            Some(id) => graph.component_ac_coalesce_formula_explained(id),
            None => {
                return Ok(Json(json!({
                    "ok": false,
                    "error": "Select exactly one component for this metric.",
                })));
            }
        },
        other => {
            return Ok(Json(
                json!({ "ok": false, "error": format!("Unknown metric: {other}") }),
            ));
        }
    };
    Ok(match explained {
        Ok(explained) => Json(json!({
            "ok": true,
            "metric": query.metric,
            "formula": explained.formula.to_string(),
            // The formula with the reasons as `//` comments, for copying.
            "commented": explained.to_commented_string(),
            "ast": explained.formula.ast(),
            "explanation": explained.explanation,
        })),
        Err(e) => Json(json!({ "ok": false, "error": e.to_string(), "kind": kind_of(&e) })),
    })
}

/// Parses a comma-separated id list. A token that is not a number is
/// an error — dropping it silently would flip the result from "these
/// components" to a different selection.
fn parse_ids(ids: &str) -> Result<BTreeSet<u64>, String> {
    let mut set = BTreeSet::new();
    for token in ids.split(',').filter(|s| !s.trim().is_empty()) {
        let id = token
            .trim()
            .parse()
            .map_err(|_| format!("bad component id: {token}"))?;
        set.insert(id);
    }
    if set.is_empty() {
        return Err("no component ids given".to_string());
    }
    Ok(set)
}

/// Machine-readable error kind, so the UI can tell "your selection
/// has no such component" apart from "the graph is broken".
fn kind_of(e: &frequenz_microgrid_component_graph::Error) -> String {
    match e.kind() {
        ErrorKind::ComponentNotFound => "component_not_found",
        ErrorKind::InvalidComponent => "invalid_component",
        ErrorKind::InvalidConnection => "invalid_connection",
        ErrorKind::InvalidGraph => "invalid_graph",
        ErrorKind::ValidationErrors(_) => "validation_errors",
        ErrorKind::Internal => "internal",
        _ => "error",
    }
    .to_string()
}
