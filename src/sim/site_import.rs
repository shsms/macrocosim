//! Import of microgrid API site exports: the JSON files a site's
//! `ListElectricalComponents` / `ListElectricalComponentConnections`
//! calls produce. Parsing is ported from component-graph-web's
//! import module, extended to keep the physics the simulator can
//! use — rated power bounds, battery capacity, SoC bounds, and the
//! grid's rated fuse current — instead of discarding them.
//!
//! The import does not build components directly. It emits one
//! `(progn (make-* …) … (connect …) …)` form that the import
//! handler evals against the new microgrid — the exact same path a
//! UI edit takes, so the persistence gate appends the form to the
//! microgrid's overrides file and it replays at every boot.
//!
//! Field sources, per the `frequenz-api-common` protos vendored in
//! `submodules/`:
//! - `categorySpecificInfo.inverter.type` → battery vs solar make fn
//! - `categorySpecificInfo.gridConnectionPoint.ratedFuseCurrent`
//!   → `:rated-fuse-current`
//! - `metricConfigBounds[METRIC_AC_POWER_ACTIVE]` → `:rated-lower` /
//!   `:rated-upper`
//! - `metricConfigBounds[METRIC_BATTERY_CAPACITY]` → `:capacity` (Wh)
//! - `metricConfigBounds[METRIC_BATTERY_SOC_PCT]` → `:soc-lower` /
//!   `:soc-upper`
//! - `categorySpecificInfo.battery.type` (chemistry) is dropped: the
//!   simulator's battery carries no chemistry yet.

use serde::Deserialize;

/// The `components.json` shape: `{"electricalComponents": [...]}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentsFile {
    electrical_components: Vec<ApiComponent>,
}

/// A component id: the protobuf JSON encoding writes uint64 as a
/// string, but re-processed exports often carry plain numbers —
/// accept both.
#[derive(Deserialize)]
#[serde(untagged)]
enum ApiId {
    Number(u64),
    Text(String),
}

impl ApiId {
    fn parse(&self) -> Result<u64, String> {
        match self {
            ApiId::Number(n) => Ok(*n),
            ApiId::Text(t) => t.parse().map_err(|_| format!("bad component id: {t}")),
        }
    }
}

impl std::fmt::Display for ApiId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiId::Number(n) => write!(f, "{n}"),
            ApiId::Text(t) => write!(f, "{t}"),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiComponent {
    id: ApiId,
    #[serde(default)]
    name: Option<String>,
    /// E.g. "ELECTRICAL_COMPONENT_CATEGORY_METER".
    category: String,
    #[serde(default)]
    category_specific_info: Option<CategoryInfo>,
    /// Per-metric configured bounds; carries the rated power range,
    /// battery capacity and SoC range in current exports.
    #[serde(default)]
    metric_config_bounds: Vec<MetricConfigBounds>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CategoryInfo {
    #[serde(default)]
    inverter: Option<TypedInfo>,
    #[serde(default)]
    grid_connection_point: Option<GridInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TypedInfo {
    /// E.g. "INVERTER_TYPE_PV".
    #[serde(default)]
    r#type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GridInfo {
    #[serde(default)]
    rated_fuse_current: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetricConfigBounds {
    #[serde(default)]
    metric: Option<String>,
    #[serde(default)]
    config_bounds: Option<Bounds>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bounds {
    #[serde(default)]
    lower: Option<f64>,
    #[serde(default)]
    upper: Option<f64>,
}

/// The `connections.json` shape:
/// `{"electricalComponentConnections": [...]}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionsFile {
    electrical_component_connections: Vec<ApiConnection>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiConnection {
    source_electrical_component_id: ApiId,
    destination_electrical_component_id: ApiId,
}

/// One component lifted out of the export: the make function that
/// builds its simulated twin plus the kwargs the export pins.
#[derive(Debug)]
pub struct ImportedComponent {
    pub id: u64,
    make_fn: &'static str,
    /// Rendered kwarg pairs, e.g. `(":capacity", "40000")`. Values
    /// are already Lisp-syntax; names come from a fixed set.
    kwargs: Vec<(&'static str, String)>,
}

/// A parsed site export, ready to render as overrides-file forms.
#[derive(Debug)]
pub struct SiteImport {
    pub components: Vec<ImportedComponent>,
    pub connections: Vec<(u64, u64)>,
}

/// Strips the first matching prefix, so both the current
/// `ELECTRICAL_COMPONENT_*` tokens and the older `COMPONENT_*` ones
/// are accepted.
fn strip_prefixes<'a>(token: &'a str, prefixes: &[&str]) -> &'a str {
    for prefix in prefixes {
        if let Some(rest) = token.strip_prefix(prefix) {
            return rest;
        }
    }
    token
}

/// Escape " and \ inside a Lisp string literal, and strip control
/// characters — same rule the microgrid stub writer applies.
fn esc(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .flat_map(|c| match c {
            '\\' => vec!['\\', '\\'],
            '"' => vec!['\\', '"'],
            c => vec![c],
        })
        .collect()
}

/// Renders an f64 so tulisp reads it back as a float (always with a
/// decimal point).
fn lisp_float(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// The configured bounds for one metric, by its token suffix.
fn bounds_for<'a>(c: &'a ApiComponent, suffix: &str) -> Option<&'a Bounds> {
    c.metric_config_bounds.iter().find_map(|b| {
        let token = b.metric.as_deref()?;
        (strip_prefixes(token, &["METRIC_"]) == suffix)
            .then_some(b.config_bounds.as_ref())
            .flatten()
    })
}

/// The `:rated-lower` / `:rated-upper` pair from the active-power
/// bounds, when the export carries both.
fn rated_kwargs(c: &ApiComponent, out: &mut Vec<(&'static str, String)>) {
    if let Some(b) = bounds_for(c, "AC_POWER_ACTIVE") {
        if let Some(l) = b.lower {
            out.push((":rated-lower", lisp_float(l)));
        }
        if let Some(u) = b.upper {
            out.push((":rated-upper", lisp_float(u)));
        }
    }
}

/// Battery-shaped storage kwargs: capacity (Wh) and the SoC range.
/// Batteries and EV chargers both take them.
fn storage_kwargs(c: &ApiComponent, out: &mut Vec<(&'static str, String)>) {
    if let Some(cap) = bounds_for(c, "BATTERY_CAPACITY").and_then(|b| b.upper.or(b.lower)) {
        out.push((":capacity", lisp_float(cap)));
    }
    if let Some(b) = bounds_for(c, "BATTERY_SOC_PCT") {
        if let Some(l) = b.lower {
            out.push((":soc-lower", lisp_float(l)));
        }
        if let Some(u) = b.upper {
            out.push((":soc-upper", lisp_float(u)));
        }
    }
}

/// Lifts one export component into the make function + kwargs of its
/// simulated twin. Unsupported categories are a hard error: silently
/// skipping one would import a topology that looks complete but has
/// holes where the export had components.
fn lift(c: &ApiComponent) -> Result<ImportedComponent, String> {
    let id = c.id.parse()?;
    let suffix = strip_prefixes(
        &c.category,
        &["ELECTRICAL_COMPONENT_CATEGORY_", "COMPONENT_CATEGORY_"],
    );
    let mut kwargs: Vec<(&'static str, String)> = Vec::new();
    if let Some(name) = c.name.as_deref().filter(|n| !n.is_empty()) {
        kwargs.push((":name", format!("\"{}\"", esc(name))));
    }
    let make_fn = match suffix {
        "GRID_CONNECTION_POINT" | "GRID" => {
            if let Some(fuse) = c
                .category_specific_info
                .as_ref()
                .and_then(|i| i.grid_connection_point.as_ref())
                .and_then(|g| g.rated_fuse_current)
            {
                kwargs.push((":rated-fuse-current", fuse.to_string()));
            }
            rated_kwargs(c, &mut kwargs);
            "make-grid-connection-point"
        }
        "METER" => "make-meter",
        "INVERTER" => {
            let inverter_type = c
                .category_specific_info
                .as_ref()
                .and_then(|i| i.inverter.as_ref())
                .and_then(|i| i.r#type.as_deref())
                .map(|t| strip_prefixes(t, &["INVERTER_TYPE_"]))
                .unwrap_or("UNSPECIFIED");
            rated_kwargs(c, &mut kwargs);
            match inverter_type {
                "BATTERY" => "make-battery-inverter",
                "PV" | "SOLAR" => "make-solar-inverter",
                other => {
                    // The simulator has to pick a concrete inverter
                    // model; UNSPECIFIED and HYBRID have none.
                    return Err(format!(
                        "component {id}: cannot simulate an inverter of type {other} \
                         (only battery and PV inverters exist here)"
                    ));
                }
            }
        }
        "BATTERY" => {
            storage_kwargs(c, &mut kwargs);
            rated_kwargs(c, &mut kwargs);
            "make-battery"
        }
        "EV_CHARGER" => {
            storage_kwargs(c, &mut kwargs);
            rated_kwargs(c, &mut kwargs);
            "make-ev-charger"
        }
        "CHP" => "make-chp",
        other => {
            return Err(format!("component {id}: cannot simulate category {other}"));
        }
    };
    Ok(ImportedComponent {
        id,
        make_fn,
        kwargs,
    })
}

/// Parses the two uploaded files into make/connect material. Errors
/// name the component so a 4000-line export stays debuggable.
pub fn parse(
    components: ComponentsFile,
    connections: Option<ConnectionsFile>,
) -> Result<SiteImport, String> {
    if components.electrical_components.is_empty() {
        return Err("the export has no components".to_string());
    }
    let mut seen_ids = std::collections::BTreeSet::new();
    let components = components
        .electrical_components
        .iter()
        .map(|c| {
            let lifted = lift(c)?;
            if !seen_ids.insert(lifted.id) {
                return Err(format!("component id {} appears twice", lifted.id));
            }
            Ok(lifted)
        })
        .collect::<Result<Vec<_>, String>>()?;
    // Exports sometimes repeat a connection row; keep the first copy.
    let mut edges = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for c in connections
        .map(|f| f.electrical_component_connections)
        .unwrap_or_default()
    {
        let edge = (
            c.source_electrical_component_id.parse()?,
            c.destination_electrical_component_id.parse()?,
        );
        if !seen_ids.contains(&edge.0) || !seen_ids.contains(&edge.1) {
            return Err(format!(
                "connection {} → {} references a component the export does not carry",
                edge.0, edge.1
            ));
        }
        if seen.insert(edge) {
            edges.push(edge);
        }
    }
    Ok(SiteImport {
        components,
        connections: edges,
    })
}

impl SiteImport {
    /// The highest component id in the import — the enterprise id
    /// allocator must move past it so later auto-assigned ids can't
    /// collide with imported ones.
    pub fn max_id(&self) -> u64 {
        self.components.iter().map(|c| c.id).max().unwrap_or(0)
    }

    /// Renders one atomic form: `(progn (make-* …) … (connect …) …)`.
    /// Evaluated against the new microgrid through the same eval
    /// path UI edits take, so the persistence gate appends it to the
    /// overrides file and the stub's `(load-overrides)` replays it
    /// at every later boot.
    pub fn forms(&self) -> String {
        let mut out = String::from("(progn\n");
        for c in &self.components {
            out.push_str(&format!("  ({} :id {}", c.make_fn, c.id));
            for (k, v) in &c.kwargs {
                out.push_str(&format!(" {k} {v}"));
            }
            out.push_str(")\n");
        }
        for (from, to) in &self.connections {
            out.push_str(&format!("  (connect {from} {to})\n"));
        }
        out.push(')');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small made-up site in the API export format (generic data
    /// only — real exports are proprietary and must not appear here).
    fn generic_components() -> ComponentsFile {
        serde_json::from_str(
            r#"{
              "electricalComponents": [
                {"id": "1", "name": "grid", "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT",
                 "categorySpecificInfo": {"gridConnectionPoint": {"ratedFuseCurrent": 100}}},
                {"id": "2", "name": "main meter", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"},
                {"id": "3", "name": "inverter A", "category": "ELECTRICAL_COMPONENT_CATEGORY_INVERTER",
                 "categorySpecificInfo": {"inverter": {"type": "INVERTER_TYPE_BATTERY"}},
                 "metricConfigBounds": [
                   {"metric": "METRIC_AC_POWER_ACTIVE", "configBounds": {"lower": -30000, "upper": 30000}}
                 ]},
                {"id": "4", "name": "battery A", "category": "ELECTRICAL_COMPONENT_CATEGORY_BATTERY",
                 "categorySpecificInfo": {"battery": {"type": "BATTERY_TYPE_LI_ION"}},
                 "metricConfigBounds": [
                   {"metric": "METRIC_BATTERY_CAPACITY", "configBounds": {"upper": 40000}},
                   {"metric": "METRIC_BATTERY_SOC_PCT", "configBounds": {"lower": 5, "upper": 95}}
                 ]},
                {"id": "5", "category": "ELECTRICAL_COMPONENT_CATEGORY_INVERTER",
                 "categorySpecificInfo": {"inverter": {"type": "INVERTER_TYPE_PV"}}}
              ]
            }"#,
        )
        .unwrap()
    }

    fn generic_connections() -> ConnectionsFile {
        serde_json::from_str(
            r#"{
              "electricalComponentConnections": [
                {"sourceElectricalComponentId": "1", "destinationElectricalComponentId": "2"},
                {"sourceElectricalComponentId": "2", "destinationElectricalComponentId": "3"},
                {"sourceElectricalComponentId": "3", "destinationElectricalComponentId": "4"},
                {"sourceElectricalComponentId": "2", "destinationElectricalComponentId": "5"}
              ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn import_keeps_capacity_soc_and_rated_bounds() {
        let import = parse(generic_components(), Some(generic_connections())).unwrap();
        assert_eq!(import.components.len(), 5);
        assert_eq!(import.connections.len(), 4);
        assert_eq!(import.max_id(), 5);
        let forms = import.forms();
        assert!(
            forms.contains(
                "(make-grid-connection-point :id 1 :name \"grid\" :rated-fuse-current 100)"
            )
        );
        assert!(forms.contains(
            "(make-battery-inverter :id 3 :name \"inverter A\" :rated-lower -30000.0 :rated-upper 30000.0)"
        ));
        assert!(forms.contains(
            "(make-battery :id 4 :name \"battery A\" :capacity 40000.0 :soc-lower 5.0 :soc-upper 95.0)"
        ));
        assert!(forms.contains("(make-solar-inverter :id 5)"));
        assert!(forms.contains("(connect 3 4)"));
    }

    #[test]
    fn import_rejects_unsupported_categories_and_untyped_inverters() {
        let boiler: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_WIND_TURBINE"}
            ]}"#,
        )
        .unwrap();
        let err = parse(boiler, None).unwrap_err();
        assert!(err.contains("component 1"));
        assert!(err.contains("cannot simulate"));

        let untyped: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "7", "category": "ELECTRICAL_COMPONENT_CATEGORY_INVERTER"}
            ]}"#,
        )
        .unwrap();
        let err = parse(untyped, None).unwrap_err();
        assert!(err.contains("component 7"));
        assert!(err.contains("UNSPECIFIED"));
    }

    /// The older `COMPONENT_*` token prefixes and plain-number ids
    /// are accepted too.
    #[test]
    fn import_accepts_old_prefixes_and_numeric_ids() {
        let file: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": 1, "category": "COMPONENT_CATEGORY_GRID_CONNECTION_POINT"},
                {"id": 2, "category": "COMPONENT_CATEGORY_METER"}
            ]}"#,
        )
        .unwrap();
        let connections: ConnectionsFile = serde_json::from_str(
            r#"{"electricalComponentConnections": [
                {"sourceElectricalComponentId": 1, "destinationElectricalComponentId": 2},
                {"sourceElectricalComponentId": 1, "destinationElectricalComponentId": 2}
            ]}"#,
        )
        .unwrap();
        let import = parse(file, Some(connections)).unwrap();
        assert_eq!(import.components[0].make_fn, "make-grid-connection-point");
        // The repeated connection row is kept once.
        assert_eq!(import.connections, vec![(1, 2)]);
    }

    #[test]
    fn import_rejects_duplicate_ids_and_dangling_edges() {
        let dup: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"},
                {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"}
            ]}"#,
        )
        .unwrap();
        assert!(parse(dup, None).unwrap_err().contains("appears twice"));

        let dangling: ConnectionsFile = serde_json::from_str(
            r#"{"electricalComponentConnections": [
                {"sourceElectricalComponentId": "1", "destinationElectricalComponentId": "99"}
            ]}"#,
        )
        .unwrap();
        let err = parse(generic_components(), Some(dangling)).unwrap_err();
        assert!(err.contains("99"));
    }

    /// Names with quote characters cannot break out of their string
    /// literal in the generated Lisp.
    #[test]
    fn import_escapes_names() {
        let file: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "name": "we \" like ) quotes", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"}
            ]}"#,
        )
        .unwrap();
        let forms = parse(file, None).unwrap().forms();
        assert!(forms.contains(r#":name "we \" like ) quotes""#));
    }
}
