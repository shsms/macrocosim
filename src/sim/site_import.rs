//! Import of microgrid API site exports: the JSON files a site's
//! `ListElectricalComponents` / `ListElectricalComponentConnections`
//! calls produce. Parsing is ported from component-graph-web's
//! import module, extended to keep the physics the simulator can
//! use — rated power bounds, battery capacity, SoC bounds, and the
//! grid's rated fuse current — instead of discarding them.
//!
//! The import does not build components directly. It produces a
//! `MicrogridSpec` that keeps the export's component ids
//! ([`SiteImport::spec`]), which the import handler creates as the UI
//! creates any microgrid.
//!
//! Field sources, per the `frequenz-api-common` protos vendored in
//! `submodules/`:
//! - `categorySpecificInfo.inverter.type` → battery vs solar inverter
//! - `categorySpecificInfo.gridConnectionPoint.ratedFuseCurrent` →
//!   `rated_fuse_current_a`
//! - `metricConfigBounds[METRIC_AC_POWER_ACTIVE]` → `rated_lower_w` /
//!   `rated_upper_w` (batteries prefer `METRIC_DC_POWER` — their
//!   rated range is a DC quantity)
//! - `metricConfigBounds[METRIC_BATTERY_CAPACITY]` → `capacity_wh`
//!   (Wh) — batteries only
//! - `metricConfigBounds[METRIC_BATTERY_SOC_PCT]` → `soc_lower_pct` /
//!   `soc_upper_pct` — batteries only; an EV charger's pack belongs
//!   to the car plugged into it, so these are dropped there
//! - `operationalMode` → `operational_mode` (config; the simulator
//!   derives the runtime knobs from it)
//! - `categorySpecificInfo.battery.type` (chemistry) is dropped: the
//!   simulator's battery carries no chemistry yet.
//!
//! Wind turbines, power transformers and breakers import as marker
//! components (see [`crate::sim::marker`]): present in the topology,
//! no physics. Steam boilers carry their electric rating —
//! `metricConfigBounds[METRIC_AC_POWER_ACTIVE]` → `rated_lower_w` /
//! `rated_upper_w`, same as an EV charger.

use serde::Deserialize;
use std::collections::HashMap;

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
    /// E.g. "ELECTRICAL_COMPONENT_OPERATIONAL_MODE_INACTIVE".
    #[serde(default)]
    operational_mode: Option<String>,
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

/// What [`lift`] makes of one export component.
enum Lifted {
    Component(ImportedComponent),
    /// An inverter the simulator has no model for that may leave with the
    /// components under it, lifted as a battery inverter (see
    /// [`InverterTree`]).
    Tree(ImportedComponent, InverterKind),
    /// A component the simulator has no model for, and why.
    NoModel(String),
}

/// One component lifted out of the export: the spec type of its
/// simulated twin plus the fields the export pins.
#[derive(Debug)]
pub struct ImportedComponent {
    pub id: u64,
    kind: &'static str,
    /// The pinned fields, by their spec field name, e.g.
    /// `("capacity_wh", Float(40000.0))`. Names come from a fixed
    /// set.
    kwargs: Vec<(&'static str, Value)>,
}

/// One pinned field's value.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Float(f64),
    Int(u64),
    Text(String),
    Mode(crate::sim::OperationalMode),
}

impl Value {
    /// The value as the topology document carries it.
    fn json(&self) -> serde_json::Value {
        match self {
            Value::Float(f) => serde_json::json!(f),
            Value::Int(i) => serde_json::json!(i),
            Value::Text(t) => serde_json::json!(t),
            Value::Mode(m) => serde_json::to_value(m).expect("a mode serializes"),
        }
    }
}

impl ImportedComponent {
    /// The component's spec: its type, its id and its pinned fields.
    fn spec(&self) -> Result<crate::spec::ComponentSpec, String> {
        let mut json = serde_json::Map::new();
        json.insert("type".into(), self.kind.into());
        json.insert("id".into(), self.id.into());
        for (k, v) in &self.kwargs {
            json.insert((*k).into(), v.json());
        }
        serde_json::from_value(json.into()).map_err(|e| format!("component {}: {e}", self.id))
    }
}

/// A parsed site export, ready to become a microgrid spec.
#[derive(Debug)]
pub struct SiteImport {
    pub components: Vec<ImportedComponent>,
    pub connections: Vec<(u64, u64)>,
    /// The components left out under [`Unsupported::Skip`], with why, in export
    /// order.
    pub skipped: Vec<(u64, String)>,
    /// The inverters left out under [`Unsupported::Skip`] with the components
    /// under them, in export order.
    pub inverter_trees: Vec<InverterTree>,
}

/// The kind of inverter an [`InverterTree`] holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InverterKind {
    /// A hybrid inverter, which the simulator cannot model yet.
    Hybrid,
    /// An inverter of no type over a battery: the battery is its child, or only
    /// components the simulator has no model for are in between.
    Unspecified,
}

impl InverterKind {
    /// How a note names an inverter of this kind.
    pub fn label(self) -> &'static str {
        match self {
            InverterKind::Hybrid => "hybrid inverter",
            InverterKind::Unspecified => "inverter of no type",
        }
    }

    /// The proto's name for the inverter type, without its prefix.
    fn token(self) -> &'static str {
        match self {
            InverterKind::Hybrid => "HYBRID",
            InverterKind::Unspecified => "UNSPECIFIED",
        }
    }
}

/// An inverter left out under [`Unsupported::Skip`] with the components under
/// it that no kept component leads to: a hybrid inverter, or an inverter of no
/// type over a battery. Splicing it out would put its batteries under
/// a meter. Both are in [`SiteImport::skipped`] too.
#[derive(Debug)]
pub struct InverterTree {
    /// The inverter's id.
    pub inverter: u64,
    pub kind: InverterKind,
    /// The components the inverter is directly under that the import keeps,
    /// in connection order.
    pub parents: Vec<u64>,
    /// The inverter, as a battery inverter, and the components under it,
    /// children before parents. A tree: each component keeps one connection,
    /// from the parent a breadth-first walk from the inverter reaches it from
    /// first.
    pub spec: crate::spec::MicrogridSpec,
}

/// What [`parse_with`] does with a component the simulator has no model for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// Refuse the whole import.
    Refuse,
    /// Leave the component out, and connect each of its parents to each of its
    /// children, so the components on either side stay connected. A hybrid
    /// inverter, and an inverter of no type over a battery, is left
    /// out with the components that only such inverters lead to (see
    /// [`InverterTree`]): a battery belongs under an inverter, not a meter.
    Skip,
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

/// The configured bounds for one metric, by its token suffix.
fn bounds_for<'a>(c: &'a ApiComponent, suffix: &str) -> Option<&'a Bounds> {
    c.metric_config_bounds.iter().find_map(|b| {
        let token = b.metric.as_deref()?;
        (strip_prefixes(token, &["METRIC_"]) == suffix)
            .then_some(b.config_bounds.as_ref())
            .flatten()
    })
}

/// The `rated_lower_w` / `rated_upper_w` pair from the first metric
/// in `metrics` the export configures bounds for. AC components pass
/// only `AC_POWER_ACTIVE`; a battery prefers `DC_POWER` (its rated
/// range is a DC quantity) with the AC bounds as fallback.
fn rated_kwargs(c: &ApiComponent, out: &mut Vec<(&'static str, Value)>, metrics: &[&str]) {
    let Some(b) = metrics.iter().find_map(|m| bounds_for(c, m)) else {
        return;
    };
    if let Some(l) = b.lower {
        out.push(("rated_lower_w", Value::Float(l)));
    }
    if let Some(u) = b.upper {
        out.push(("rated_upper_w", Value::Float(u)));
    }
}

/// The export's operational mode, kept as what it is: a config
/// parameter (`operational_mode`). The simulator derives the
/// runtime knobs from it — the import never sets those directly.
/// UNSPECIFIED is the default and is omitted. Unknown tokens are an
/// error, not a silent default.
fn mode_kwargs(c: &ApiComponent, out: &mut Vec<(&'static str, Value)>) -> Result<(), String> {
    let Some(mode) = c.operational_mode.as_deref() else {
        return Ok(());
    };
    let suffix = strip_prefixes(
        mode,
        &[
            "ELECTRICAL_COMPONENT_OPERATIONAL_MODE_",
            "COMPONENT_OPERATIONAL_MODE_",
        ],
    );
    // The API token is the mode's name in SCREAMING_SNAKE_CASE;
    // frequenz-api-common v0.8.3 and older spell the telemetry-only mode
    // `TELEMTRY_ONLY`, as older exports do.
    let name = match suffix {
        "TELEMTRY_ONLY" => "telemetry_only".to_string(),
        other => other.to_lowercase(),
    };
    let mode: crate::sim::OperationalMode = serde_json::from_value(serde_json::Value::String(name))
        .map_err(|_| format!("unknown operational mode: {suffix}"))?;
    if mode == crate::sim::OperationalMode::Unspecified {
        return Ok(());
    }
    out.push(("operational_mode", Value::Mode(mode)));
    Ok(())
}

/// Battery-shaped storage kwargs: capacity (Wh) and the SoC range.
/// Only batteries take them; an EV charger has no pack of its own.
fn storage_kwargs(c: &ApiComponent, out: &mut Vec<(&'static str, Value)>) {
    if let Some(cap) = bounds_for(c, "BATTERY_CAPACITY").and_then(|b| b.upper.or(b.lower)) {
        out.push(("capacity_wh", Value::Float(cap)));
    }
    if let Some(b) = bounds_for(c, "BATTERY_SOC_PCT") {
        if let Some(l) = b.lower {
            out.push(("soc_lower_pct", Value::Float(l)));
        }
        if let Some(u) = b.upper {
            out.push(("soc_upper_pct", Value::Float(u)));
        }
    }
}

/// Why the simulator cannot model an inverter of type `kind`.
fn no_inverter_model(kind: &str) -> String {
    format!("cannot simulate an inverter of type {kind} (only battery and PV inverters exist here)")
}

/// Lifts export component `id` into the spec type + fields of its simulated
/// twin, or says why the simulator has none. An operational mode the import
/// does not know is an error, except on a hybrid inverter or an inverter of no
/// type, which is lifted without it: the import keeps neither.
fn lift(c: &ApiComponent, id: u64) -> Result<Lifted, String> {
    let suffix = strip_prefixes(
        &c.category,
        &["ELECTRICAL_COMPONENT_CATEGORY_", "COMPONENT_CATEGORY_"],
    );
    let mut kwargs: Vec<(&'static str, Value)> = Vec::new();
    let mut tree = None;
    if let Some(name) = c.name.as_deref().filter(|n| !n.is_empty()) {
        kwargs.push(("name", Value::Text(name.to_string())));
    }
    let kind = match suffix {
        "GRID_CONNECTION_POINT" | "GRID" => {
            if let Some(fuse) = c
                .category_specific_info
                .as_ref()
                .and_then(|i| i.grid_connection_point.as_ref())
                .and_then(|g| g.rated_fuse_current)
            {
                kwargs.push(("rated_fuse_current_a", Value::Int(fuse.into())));
            }
            rated_kwargs(c, &mut kwargs, &["AC_POWER_ACTIVE"]);
            "grid_connection_point"
        }
        "METER" => "meter",
        "INVERTER" => {
            let inverter_type = c
                .category_specific_info
                .as_ref()
                .and_then(|i| i.inverter.as_ref())
                .and_then(|i| i.r#type.as_deref())
                .map(|t| strip_prefixes(t, &["INVERTER_TYPE_"]))
                .unwrap_or("UNSPECIFIED");
            rated_kwargs(c, &mut kwargs, &["AC_POWER_ACTIVE"]);
            match inverter_type {
                "BATTERY" => "battery_inverter",
                "PV" | "SOLAR" => "solar_inverter",
                "HYBRID" => {
                    tree = Some(InverterKind::Hybrid);
                    "battery_inverter"
                }
                "UNSPECIFIED" => {
                    tree = Some(InverterKind::Unspecified);
                    "battery_inverter"
                }
                other => {
                    // The simulator has to pick a concrete inverter
                    // model; a type number the protos do not define has
                    // none.
                    return Ok(Lifted::NoModel(no_inverter_model(other)));
                }
            }
        }
        "BATTERY" => {
            storage_kwargs(c, &mut kwargs);
            rated_kwargs(c, &mut kwargs, &["DC_POWER", "AC_POWER_ACTIVE"]);
            "battery"
        }
        // No `storage_kwargs`: the charger has no pack of its own —
        // the car plugged into it does — so its spec takes no
        // `capacity_wh` / `soc_lower_pct` / `soc_upper_pct`.
        "EV_CHARGER" => {
            rated_kwargs(c, &mut kwargs, &["AC_POWER_ACTIVE"]);
            "ev_charger"
        }
        "STEAM_BOILER" => {
            rated_kwargs(c, &mut kwargs, &["AC_POWER_ACTIVE"]);
            "steam_boiler"
        }
        // CHP and the marker categories have no physics of their
        // own: they complete the topology and classify the meters
        // around them; power is set on the neighboring meter.
        "CHP" => "chp",
        "WIND_TURBINE" => "wind_turbine",
        "POWER_TRANSFORMER" => "power_transformer",
        "BREAKER" => "breaker",
        other => {
            return Ok(Lifted::NoModel(format!("cannot simulate category {other}")));
        }
    };
    let mode = mode_kwargs(c, &mut kwargs);
    let lifted = ImportedComponent { id, kind, kwargs };
    if let Some(kind) = tree {
        // Not simulated either way: one with a mode the import does not know
        // goes without it.
        return Ok(Lifted::Tree(lifted, kind));
    }
    mode.map_err(|e| format!("component {id}: {e}"))?;
    Ok(Lifted::Component(lifted))
}

/// Parses the two uploaded files into components and connections. The import
/// is refused when a component has no model in the simulator: silently skipping
/// one would import a topology that looks complete but has holes where the
/// export had components. Errors name the component so a 4000-line export
/// stays debuggable.
pub fn parse(
    components: ComponentsFile,
    connections: Option<ConnectionsFile>,
) -> Result<SiteImport, String> {
    parse_with(components, connections, Unsupported::Refuse)
}

/// [`parse`], with `unsupported` saying what to do with a component the
/// simulator has no model for.
pub fn parse_with(
    components: ComponentsFile,
    connections: Option<ConnectionsFile>,
    unsupported: Unsupported,
) -> Result<SiteImport, String> {
    if components.electrical_components.is_empty() {
        return Err("the export has no components".to_string());
    }
    // Each id's position in the export.
    let mut position_of = std::collections::BTreeMap::new();
    let mut kept = Vec::new();
    let mut trees = Vec::new();
    let mut skipped = Vec::new();
    for (i, c) in components.electrical_components.iter().enumerate() {
        let id = c.id.parse()?;
        if position_of.insert(id, i).is_some() {
            return Err(format!("component id {id} appears twice"));
        }
        let why = match lift(c, id)? {
            Lifted::Component(c) => {
                kept.push(c);
                continue;
            }
            Lifted::Tree(c, kind) if unsupported == Unsupported::Skip => {
                trees.push((c, kind));
                continue;
            }
            Lifted::Tree(_, kind) => no_inverter_model(kind.token()),
            Lifted::NoModel(why) => why,
        };
        match unsupported {
            Unsupported::Refuse => return Err(format!("component {id}: {why}")),
            Unsupported::Skip => skipped.push((id, why)),
        }
    }
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
        if !position_of.contains_key(&edge.0) || !position_of.contains_key(&edge.1) {
            return Err(format!(
                "connection {} → {} references a component the export does not carry",
                edge.0, edge.1
            ));
        }
        if seen.insert(edge) {
            edges.push(edge);
        }
    }
    // Cycles must fail the whole request here, before a spec is made
    // and anything is built.
    for &(src, dst) in &edges {
        if src == dst {
            return Err(format!("connection {src} → {dst} is a self-edge"));
        }
    }
    if let Some((src, dst)) = crate::spec::order::cycle_edge(&edges) {
        return Err(format!(
            "the connections contain a cycle (reachable through {src} → {dst})"
        ));
    }
    // An inverter of no type with no battery under it is spliced out like any
    // other component with no model. A battery counts as under it when only
    // components with no model are in between.
    let batteries: std::collections::BTreeSet<u64> = kept
        .iter()
        .filter(|c| c.kind == "battery")
        .map(|c| c.id)
        .collect();
    let no_model: std::collections::BTreeSet<u64> = skipped.iter().map(|(id, _)| *id).collect();
    let through = splice(&edges, &no_model);
    let (trees, spliced): (Vec<_>, Vec<_>) = trees.into_iter().partition(|(c, kind)| {
        *kind == InverterKind::Hybrid
            || through
                .iter()
                .any(|&(p, child)| p == c.id && batteries.contains(&child))
    });
    for (c, kind) in spliced {
        skipped.push((c.id, no_inverter_model(kind.token())));
    }
    if !skipped.is_empty() {
        let skipped_ids: std::collections::BTreeSet<u64> =
            skipped.iter().map(|(id, _)| *id).collect();
        edges = splice(&edges, &skipped_ids);
    }
    let mut import = SiteImport {
        components: kept,
        connections: edges,
        skipped,
        inverter_trees: Vec::new(),
    };
    if !trees.is_empty() {
        import.carve_out(trees)?;
    }
    import.skipped.sort_by_key(|(id, _)| position_of[id]);
    if import.components.is_empty() {
        return Err("the export has no component the simulator can model".to_string());
    }
    Ok(import)
}

/// `edges` without the components in `skipped`: each edge from a kept component
/// into `skipped` is replaced by edges to the kept components it reached
/// through `skipped` alone. The edges keep their order, and a spliced edge that
/// is already there is not added again. `edges` has no cycles.
fn splice(edges: &[(u64, u64)], skipped: &std::collections::BTreeSet<u64>) -> Vec<(u64, u64)> {
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for &(parent, child) in edges {
        children.entry(parent).or_default().push(child);
    }
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for &(parent, child) in edges {
        if skipped.contains(&parent) {
            continue;
        }
        let mut stack = vec![child];
        let mut visited = std::collections::BTreeSet::new();
        while let Some(id) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            if !skipped.contains(&id) {
                if seen.insert((parent, id)) {
                    out.push((parent, id));
                }
                continue;
            }
            // Reversed, so the children come off the stack in their own order.
            stack.extend(children.get(&id).into_iter().flatten().rev());
        }
    }
    out
}

impl SiteImport {
    /// Takes the inverters in `trees` and the kept components that only they
    /// lead to out of the topology, into [`Self::inverter_trees`] and
    /// [`Self::skipped`]. A component under two of them goes with the first
    /// one.
    fn carve_out(&mut self, trees: Vec<(ImportedComponent, InverterKind)>) -> Result<(), String> {
        use std::collections::BTreeSet;
        let mut parents: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        for &(parent, child) in &self.connections {
            parents.entry(child).or_default().push(parent);
            children.entry(parent).or_default().push(child);
        }
        let children_of = |id: &u64| children.get(id).into_iter().flatten().copied();
        let inverters: Vec<(u64, InverterKind)> = trees.iter().map(|(c, k)| (c.id, *k)).collect();
        // The components only these inverters lead to: one whose parents are
        // all out goes out too, checked again each time a parent goes out.
        let mut out: BTreeSet<u64> = inverters.iter().map(|&(id, _)| id).collect();
        let mut stack: Vec<u64> = inverters
            .iter()
            .flat_map(|(id, _)| children_of(id))
            .collect();
        while let Some(id) = stack.pop() {
            if !out.contains(&id) && parents[&id].iter().all(|p| out.contains(p)) {
                out.insert(id);
                stack.extend(children_of(&id));
            }
        }
        let (mut carved, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.components)
            .into_iter()
            .partition(|c| out.contains(&c.id));
        self.components = kept;
        carved.extend(trees.into_iter().map(|(c, _)| c));
        let mut taken: BTreeSet<u64> = inverters.iter().map(|&(id, _)| id).collect();
        for &(inverter, kind) in &inverters {
            // Breadth first, so each component hangs from the first parent
            // that reaches it.
            let mut ids = vec![inverter];
            let mut edges = Vec::new();
            let mut next = 0;
            while let Some(&parent) = ids.get(next) {
                next += 1;
                for child in children_of(&parent) {
                    if out.contains(&child) && taken.insert(child) {
                        ids.push(child);
                        edges.push((parent, child));
                    }
                }
            }
            let (mine, rest): (Vec<_>, Vec<_>) =
                carved.into_iter().partition(|c| ids.contains(&c.id));
            carved = rest;
            let tree = SiteImport {
                components: mine,
                connections: edges,
                skipped: Vec::new(),
                inverter_trees: Vec::new(),
            };
            self.skipped
                .push((inverter, no_inverter_model(kind.token())));
            for id in &ids[1..] {
                self.skipped
                    .push((*id, format!("under {} {inverter}", kind.label())));
            }
            self.inverter_trees.push(InverterTree {
                inverter,
                kind,
                parents: parents
                    .get(&inverter)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|p| !out.contains(p))
                    .collect(),
                spec: tree
                    .spec()
                    .map_err(|e| format!("{} {inverter}: {e}", kind.label()))?,
            });
        }
        self.connections
            .retain(|(p, c)| !out.contains(p) && !out.contains(c));
        Ok(())
    }

    /// The order the components must be built in, and the number of
    /// walk steps it took to work that out (for
    /// `spec_walks_shared_subtrees_once`).
    ///
    /// Children before parents, so an import does not tick an
    /// inverter before the battery it pushes into. Cycles were
    /// refused at parse time. See
    /// [`crate::spec::order::children_first`].
    fn ordered_ids_counting(&self) -> (Vec<u64>, u64) {
        crate::spec::order::children_first_counting(
            self.components.iter().map(|c| c.id),
            &self.connections,
        )
    }

    /// The import as a microgrid spec with the export's component
    /// ids, children before parents. The microgrid's own id, name and
    /// port are the caller's to fill in.
    pub fn spec(&self) -> Result<crate::spec::MicrogridSpec, String> {
        let (ordered_ids, _calls) = self.ordered_ids_counting();
        let by_id: HashMap<u64, &ImportedComponent> =
            self.components.iter().map(|c| (c.id, c)).collect();
        let components = ordered_ids
            .iter()
            .map(|id| by_id[id].spec())
            .collect::<Result<Vec<_>, String>>()?;
        Ok(crate::spec::MicrogridSpec {
            components,
            connections: self.connections.clone(),
            ..Default::default()
        })
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

    /// The spec keeps the export's ids and pinned fields, children
    /// before parents, and its connections.
    #[test]
    fn spec_keeps_the_ids_and_the_pinned_fields() {
        let import = parse(generic_components(), Some(generic_connections())).unwrap();
        let spec = serde_json::to_value(import.spec().unwrap()).unwrap();
        let ids: Vec<u64> = spec["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, [4, 3, 5, 2, 1]);
        assert_eq!(
            spec["components"][0],
            serde_json::json!({"type": "battery", "id": 4, "name": "battery A",
                "capacity_wh": 40000.0, "soc_lower_pct": 5.0, "soc_upper_pct": 95.0})
        );
        assert_eq!(
            spec["components"][4],
            serde_json::json!({"type": "grid_connection_point", "id": 1, "name": "grid",
                "rated_fuse_current_a": 100})
        );
        assert_eq!(spec["components"][2]["type"], "solar_inverter");
        assert_eq!(
            spec["connections"],
            serde_json::json!([[1, 2], [2, 3], [3, 4], [2, 5]])
        );
        assert!(spec.get("id").is_none(), "{spec}");
    }

    /// The spec's components as JSON, in build order.
    fn components_of(import: &SiteImport) -> Vec<serde_json::Value> {
        let spec = serde_json::to_value(import.spec().unwrap()).unwrap();
        spec["components"].as_array().unwrap().clone()
    }

    /// The spec's component with id `id`, as JSON.
    fn component(import: &SiteImport, id: u64) -> serde_json::Value {
        components_of(import)
            .into_iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("no component {id}"))
    }

    /// Where component `id` comes in the spec's build order.
    fn pos(order: &[serde_json::Value], id: u64) -> usize {
        order
            .iter()
            .position(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("no component {id}"))
    }

    #[test]
    fn import_keeps_capacity_soc_and_rated_bounds() {
        let import = parse(generic_components(), Some(generic_connections())).unwrap();
        assert_eq!(import.components.len(), 5);
        assert_eq!(import.connections.len(), 4);
        assert_eq!(
            component(&import, 3),
            serde_json::json!({"type": "battery_inverter", "id": 3, "name": "inverter A",
                "rated_lower_w": -30000.0, "rated_upper_w": 30000.0})
        );
        assert_eq!(
            component(&import, 5),
            serde_json::json!({"type": "solar_inverter", "id": 5})
        );
        assert!(import.connections.contains(&(3, 4)));
    }

    #[test]
    fn import_rejects_unsupported_categories_and_untyped_inverters() {
        let hvac: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_HVAC"}
            ]}"#,
        )
        .unwrap();
        let err = parse(hvac, None).unwrap_err();
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

    /// A connections file with `edges`.
    fn connections_of(edges: &[(u64, u64)]) -> ConnectionsFile {
        let edges: Vec<serde_json::Value> = edges
            .iter()
            .map(|(a, b)| {
                serde_json::json!({
                    "sourceElectricalComponentId": a.to_string(),
                    "destinationElectricalComponentId": b.to_string(),
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({"electricalComponentConnections": edges}))
            .unwrap()
    }

    /// A made-up chain: grid 1 → meter 2 → precharger 3 → converter 4 → battery
    /// 5, with an HVAC 6 under the meter.
    fn chain_with_unsupported() -> (ComponentsFile, ConnectionsFile) {
        let components = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT"},
                {"id": "2", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"},
                {"id": "3", "category": "ELECTRICAL_COMPONENT_CATEGORY_PRECHARGER"},
                {"id": "4", "category": "ELECTRICAL_COMPONENT_CATEGORY_CONVERTER"},
                {"id": "5", "category": "ELECTRICAL_COMPONENT_CATEGORY_BATTERY"},
                {"id": "6", "category": "ELECTRICAL_COMPONENT_CATEGORY_HVAC"}
            ]}"#,
        )
        .unwrap();
        let connections = connections_of(&[(1, 2), (2, 3), (3, 4), (4, 5), (2, 6)]);
        (components, connections)
    }

    /// Skipping leaves the unsupported components out, names each with why, and
    /// connects across a run of them.
    #[test]
    fn skip_splices_across_unsupported_components() {
        let (components, connections) = chain_with_unsupported();
        let import = parse_with(components, Some(connections), Unsupported::Skip).unwrap();
        let ids: Vec<u64> = import.components.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![1, 2, 5]);
        assert_eq!(import.connections, vec![(1, 2), (2, 5)]);
        let skipped: Vec<u64> = import.skipped.iter().map(|(id, _)| *id).collect();
        assert_eq!(skipped, vec![3, 4, 6]);
        assert!(
            import.skipped[0].1.contains("PRECHARGER"),
            "{:?}",
            import.skipped
        );
        import.spec().unwrap();
    }

    /// Hybrid inverter 3 leaves with battery 4 and meter 8, which only
    /// hybrid inverters lead to; battery 6, which battery inverter 5 also
    /// leads to, stays. Battery 4 hangs from 3 alone in the tree, and goes
    /// with 3, not with hybrid inverter 9, which comes later.
    #[test]
    fn skip_carves_out_a_hybrid_inverter_with_its_batteries() {
        let components: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT"},
                {"id": "2", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"},
                {"id": "3", "name": "hybrid", "category": "ELECTRICAL_COMPONENT_CATEGORY_INVERTER",
                 "categorySpecificInfo": {"inverter": {"type": "INVERTER_TYPE_HYBRID"}},
                 "metricConfigBounds": [
                   {"metric": "METRIC_AC_POWER_ACTIVE", "configBounds": {"lower": -5000, "upper": 5000}}
                 ]},
                {"id": "4", "category": "ELECTRICAL_COMPONENT_CATEGORY_BATTERY"},
                {"id": "5", "category": "ELECTRICAL_COMPONENT_CATEGORY_INVERTER",
                 "categorySpecificInfo": {"inverter": {"type": "INVERTER_TYPE_BATTERY"}}},
                {"id": "6", "category": "ELECTRICAL_COMPONENT_CATEGORY_BATTERY"},
                {"id": "7", "category": "999"},
                {"id": "8", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"},
                {"id": "9", "category": "ELECTRICAL_COMPONENT_CATEGORY_INVERTER",
                 "categorySpecificInfo": {"inverter": {"type": "INVERTER_TYPE_HYBRID"}}}
            ]}"#,
        )
        .unwrap();
        let edges = [
            (1, 2),
            (2, 3),
            (3, 4),
            (3, 8),
            (8, 4),
            (2, 5),
            (5, 6),
            (3, 6),
            (2, 7),
            (2, 9),
            (9, 4),
        ];
        let import =
            parse_with(components, Some(connections_of(&edges)), Unsupported::Skip).unwrap();
        let ids: Vec<u64> = import.components.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![1, 2, 5, 6]);
        assert_eq!(import.connections, vec![(1, 2), (2, 5), (5, 6)]);
        assert_eq!(
            import.skipped,
            vec![
                (3, no_inverter_model("HYBRID")),
                (4, "under hybrid inverter 3".to_string()),
                (7, "cannot simulate category 999".to_string()),
                (8, "under hybrid inverter 3".to_string()),
                (9, no_inverter_model("HYBRID")),
            ]
        );
        let [first, second] = &import.inverter_trees[..] else {
            panic!("{:?}", import.inverter_trees);
        };
        assert_eq!((first.inverter, &first.parents[..]), (3, &[2][..]));
        assert_eq!(first.spec.connections, vec![(3, 4), (3, 8)]);
        let inverter = first.spec.components.iter().find(|c| c.id() == Some(3));
        assert_eq!(
            serde_json::to_value(inverter).unwrap(),
            serde_json::json!({"type": "battery_inverter", "id": 3, "name": "hybrid",
                               "rated_lower_w": -5000.0, "rated_upper_w": 5000.0})
        );
        assert_eq!((second.inverter, &second.parents[..]), (9, &[2][..]));
        assert_eq!(second.spec.components.len(), 1);
    }

    /// A hybrid inverter is refused under Refuse. Under Skip, one with an
    /// operational mode the import does not know still leaves with its tree,
    /// without the mode.
    #[test]
    fn a_hybrid_inverter_with_an_unknown_mode_goes_without_it() {
        let hybrid = |mode: &str| -> ComponentsFile {
            serde_json::from_str(&format!(
                r#"{{"electricalComponents": [
                    {{"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT"}},
                    {{"id": "3", "category": "ELECTRICAL_COMPONENT_CATEGORY_INVERTER",
                      "categorySpecificInfo": {{"inverter": {{"type": "INVERTER_TYPE_HYBRID"}}}},
                      "operationalMode": "{mode}"}},
                    {{"id": "4", "category": "ELECTRICAL_COMPONENT_CATEGORY_BATTERY"}}
                ]}}"#
            ))
            .unwrap()
        };
        let err = parse(hybrid("99"), None).unwrap_err();
        assert!(
            err.contains("component 3: cannot simulate an inverter of type HYBRID"),
            "{err}"
        );
        let edges = connections_of(&[(1, 3), (3, 4)]);
        let import = parse_with(hybrid("99"), Some(edges), Unsupported::Skip).unwrap();
        let ids: Vec<u64> = import.components.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![1]);
        let [tree] = &import.inverter_trees[..] else {
            panic!("{:?}", import.inverter_trees);
        };
        assert_eq!(tree.spec.connections, vec![(3, 4)]);
        let inverter = tree.spec.components.iter().find(|c| c.id() == Some(3));
        assert_eq!(
            serde_json::to_value(inverter).unwrap(),
            serde_json::json!({"type": "battery_inverter", "id": 3})
        );
    }

    /// An inverter of no type with a battery under it leaves with the battery,
    /// like a hybrid inverter, also when a precharger is in between; one with
    /// no battery under it is spliced out.
    #[test]
    fn skip_carves_out_an_inverter_of_no_type_only_over_a_battery() {
        let components: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT"},
                {"id": "2", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"},
                {"id": "3", "category": "ELECTRICAL_COMPONENT_CATEGORY_INVERTER"},
                {"id": "4", "category": "ELECTRICAL_COMPONENT_CATEGORY_BATTERY"},
                {"id": "5", "category": "ELECTRICAL_COMPONENT_CATEGORY_INVERTER"},
                {"id": "6", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"},
                {"id": "7", "category": "ELECTRICAL_COMPONENT_CATEGORY_INVERTER"},
                {"id": "8", "category": "ELECTRICAL_COMPONENT_CATEGORY_PRECHARGER"},
                {"id": "9", "category": "ELECTRICAL_COMPONENT_CATEGORY_BATTERY"}
            ]}"#,
        )
        .unwrap();
        let edges = connections_of(&[
            (1, 2),
            (2, 3),
            (3, 4),
            (2, 5),
            (5, 6),
            (2, 7),
            (7, 8),
            (8, 9),
        ]);
        let import = parse_with(components, Some(edges), Unsupported::Skip).unwrap();
        let ids: Vec<u64> = import.components.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![1, 2, 6]);
        assert_eq!(import.connections, vec![(1, 2), (2, 6)]);
        assert_eq!(
            import.skipped,
            vec![
                (3, no_inverter_model("UNSPECIFIED")),
                (4, "under inverter of no type 3".to_string()),
                (5, no_inverter_model("UNSPECIFIED")),
                (7, no_inverter_model("UNSPECIFIED")),
                (8, "cannot simulate category PRECHARGER".to_string()),
                (9, "under inverter of no type 7".to_string()),
            ]
        );
        let [tree, behind_precharger] = &import.inverter_trees[..] else {
            panic!("{:?}", import.inverter_trees);
        };
        assert_eq!(
            (tree.inverter, tree.kind, &tree.parents[..]),
            (3, InverterKind::Unspecified, &[2][..])
        );
        assert_eq!(tree.spec.connections, vec![(3, 4)]);
        assert_eq!(behind_precharger.inverter, 7);
        assert_eq!(behind_precharger.spec.connections, vec![(7, 9)]);
    }

    #[test]
    fn refuse_is_the_default() {
        let (components, connections) = chain_with_unsupported();
        let err = parse(components, Some(connections)).unwrap_err();
        assert!(
            err.contains("component 3: cannot simulate category PRECHARGER"),
            "{err}"
        );
    }

    #[test]
    fn splice_joins_every_parent_to_every_child_once() {
        // 1 and 2 over the left-out 9, which is over 3 and 4; 1 also has 3
        // directly.
        let gone = [9].into_iter().collect();
        let edges = [(1, 9), (2, 9), (9, 3), (9, 4), (1, 3)];
        assert_eq!(splice(&edges, &gone), vec![(1, 3), (1, 4), (2, 3), (2, 4)]);
    }

    #[test]
    fn skip_still_needs_one_component_it_can_model() {
        let hvac: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_HVAC"}
            ]}"#,
        )
        .unwrap();
        let err = parse_with(hvac, None, Unsupported::Skip).unwrap_err();
        assert!(
            err.contains("no component the simulator can model"),
            "{err}"
        );
    }

    #[test]
    fn import_rejects_self_edges_and_cycles() {
        let cyclic: ConnectionsFile = serde_json::from_str(
            r#"{"electricalComponentConnections": [
                {"sourceElectricalComponentId": "1", "destinationElectricalComponentId": "2"},
                {"sourceElectricalComponentId": "2", "destinationElectricalComponentId": "3"},
                {"sourceElectricalComponentId": "3", "destinationElectricalComponentId": "1"}
            ]}"#,
        )
        .unwrap();
        let err = parse(generic_components(), Some(cyclic)).unwrap_err();
        assert!(err.contains("cycle"), "{err}");

        let self_edge: ConnectionsFile = serde_json::from_str(
            r#"{"electricalComponentConnections": [
                {"sourceElectricalComponentId": "2", "destinationElectricalComponentId": "2"}
            ]}"#,
        )
        .unwrap();
        let err = parse(generic_components(), Some(self_edge)).unwrap_err();
        assert!(err.contains("self-edge"), "{err}");
    }

    /// Wind turbines, power transformers and breakers import as
    /// marker components; steam boilers carry their electric rating.
    #[test]
    fn import_lifts_marker_categories() {
        let file: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_WIND_TURBINE"},
                {"id": "2", "category": "ELECTRICAL_COMPONENT_CATEGORY_STEAM_BOILER",
                 "metricConfigBounds": [
                   {"metric": "METRIC_AC_POWER_ACTIVE", "configBounds": {"lower": 0, "upper": 500000}}
                 ]},
                {"id": "3", "category": "ELECTRICAL_COMPONENT_CATEGORY_POWER_TRANSFORMER"},
                {"id": "4", "category": "ELECTRICAL_COMPONENT_CATEGORY_BREAKER"}
            ]}"#,
        )
        .unwrap();
        let import = parse(file, None).unwrap();
        assert_eq!(
            component(&import, 1),
            serde_json::json!({"type": "wind_turbine", "id": 1})
        );
        assert_eq!(
            component(&import, 2),
            serde_json::json!({"type": "steam_boiler", "id": 2,
                "rated_lower_w": 0.0, "rated_upper_w": 500000.0})
        );
        assert_eq!(component(&import, 3)["type"], "power_transformer");
        assert_eq!(component(&import, 4)["type"], "breaker");
    }

    /// A battery's rated range is its DC power bounds; the AC bounds
    /// are only a fallback.
    #[test]
    fn import_prefers_dc_power_bounds_for_batteries() {
        let file: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_BATTERY",
                 "metricConfigBounds": [
                   {"metric": "METRIC_AC_POWER_ACTIVE", "configBounds": {"lower": -1, "upper": 1}},
                   {"metric": "METRIC_DC_POWER", "configBounds": {"lower": -50000, "upper": 50000}}
                 ]}
            ]}"#,
        )
        .unwrap();
        let battery = component(&parse(file, None).unwrap(), 1);
        assert_eq!(battery["rated_lower_w"], -50000.0);
        assert_eq!(battery["rated_upper_w"], 50000.0);
    }

    /// A charger has no pack of its own — the car it charges does —
    /// so its spec takes no `capacity_wh` / `soc_lower_pct` /
    /// `soc_upper_pct`. An export whose charger carries the battery
    /// bounds (exports do, for chargers that report an SoC) must
    /// therefore drop them rather than make a spec that fails.
    #[test]
    fn import_drops_pack_bounds_from_ev_chargers() {
        let file: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_EV_CHARGER",
                 "metricConfigBounds": [
                   {"metric": "METRIC_AC_POWER_ACTIVE", "configBounds": {"lower": 0, "upper": 22000}},
                   {"metric": "METRIC_BATTERY_CAPACITY", "configBounds": {"upper": 40000}},
                   {"metric": "METRIC_BATTERY_SOC_PCT", "configBounds": {"lower": 5, "upper": 95}}
                 ]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            component(&parse(file, None).unwrap(), 1),
            serde_json::json!({"type": "ev_charger", "id": 1,
                "rated_lower_w": 0.0, "rated_upper_w": 22000.0})
        );
    }

    /// The operational mode is a config parameter and stays one: it
    /// becomes `operational_mode`, never runtime knobs.
    #[test]
    fn import_keeps_operational_modes_as_config() {
        let file: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER",
                 "operationalMode": "ELECTRICAL_COMPONENT_OPERATIONAL_MODE_INACTIVE"},
                {"id": "2", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER",
                 "operationalMode": "ELECTRICAL_COMPONENT_OPERATIONAL_MODE_TELEMETRY_ONLY"},
                {"id": "3", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER",
                 "operationalMode": "ELECTRICAL_COMPONENT_OPERATIONAL_MODE_CONTROL_AND_TELEMETRY"},
                {"id": "4", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER",
                 "operationalMode": "ELECTRICAL_COMPONENT_OPERATIONAL_MODE_UNSPECIFIED"},
                {"id": "5", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER",
                 "operationalMode": "ELECTRICAL_COMPONENT_OPERATIONAL_MODE_TELEMTRY_ONLY"}
            ]}"#,
        )
        .unwrap();
        let import = parse(file, None).unwrap();
        assert_eq!(
            component(&import, 1),
            serde_json::json!({"type": "meter", "id": 1, "operational_mode": "inactive"})
        );
        assert_eq!(component(&import, 2)["operational_mode"], "telemetry_only");
        assert_eq!(
            component(&import, 3)["operational_mode"],
            "control_and_telemetry"
        );
        assert_eq!(
            component(&import, 4),
            serde_json::json!({"type": "meter", "id": 4})
        );
        // The proto's own spelling of the telemetry-only mode.
        assert_eq!(component(&import, 5)["operational_mode"], "telemetry_only");
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
        assert_eq!(import.components[0].kind, "grid_connection_point");
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

    /// A diamond (1→2, 1→3, 2→4, 3→4) plus an edge-less component 5
    /// listed between 2 and 3 in export order: 4 has two parents (2
    /// and 3) and must still come exactly once, and 5 — reached by no
    /// edge at all — keeps its export-order place at the end, after
    /// every edge-connected component.
    #[test]
    fn spec_lists_each_component_once_and_keeps_unconnected_ones_in_export_order() {
        let components: ComponentsFile = serde_json::from_str(
            r#"{"electricalComponents": [
                {"id": "1", "name": "grid", "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT"},
                {"id": "2", "name": "m2", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"},
                {"id": "5", "name": "m5", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"},
                {"id": "3", "name": "m3", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"},
                {"id": "4", "name": "m4", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"}
            ]}"#,
        )
        .unwrap();
        let connections: ConnectionsFile = serde_json::from_str(
            r#"{"electricalComponentConnections": [
                {"sourceElectricalComponentId": "1", "destinationElectricalComponentId": "2"},
                {"sourceElectricalComponentId": "1", "destinationElectricalComponentId": "3"},
                {"sourceElectricalComponentId": "2", "destinationElectricalComponentId": "4"},
                {"sourceElectricalComponentId": "3", "destinationElectricalComponentId": "4"}
            ]}"#,
        )
        .unwrap();
        let order = components_of(&parse(components, Some(connections)).unwrap());
        assert_eq!(order.len(), 5, "each component once: {order:?}");
        assert!(pos(&order, 4) < pos(&order, 2), "4 before 2");
        assert!(pos(&order, 4) < pos(&order, 3), "4 before 3");
        assert!(pos(&order, 2) < pos(&order, 1), "2 before 1");
        assert!(pos(&order, 3) < pos(&order, 1), "3 before 1");
        assert!(pos(&order, 1) < pos(&order, 5), "unconnected 5 after 1");
    }

    /// 25 chained diamonds: node k fans out to `a_k` and `b_k`, both
    /// of which point at node k+1. Every node below the root is a
    /// shared subtree, so a walk that only marks `seen` on the way
    /// back up re-descends node 25 once per root-to-leaf path — 2^25
    /// of them, minutes of spinning while the structural lock is
    /// held.
    ///
    /// Counted, not timed. The regressed walk reaches the SAME answer,
    /// just after exponentially many calls — about three minutes on
    /// this fixture — so asserting on the emitted order alone would
    /// pass on it and merely take a while doing so. The assertion
    /// below is on the call count instead: a memoized post-order walk
    /// enters once per component plus once per edge, so
    /// `2 * (nodes + edges)` leaves that room and no room at all for
    /// re-descending a shared subtree.
    #[test]
    fn spec_walks_shared_subtrees_once() {
        const DIAMONDS: u64 = 25;
        let node = |k: u64| 10 + k;
        let fork = |k: u64, side: u64| 100 + 2 * k + side;
        let meter = |id: u64| {
            format!(
                r#"{{"id": "{id}", "name": "m{id}", "category": "ELECTRICAL_COMPONENT_CATEGORY_METER"}}"#
            )
        };

        // Export order puts the root first, so the very first walk is
        // the one that has to cross every diamond.
        let mut comps = vec![format!(
            r#"{{"id": "{}", "name": "grid", "category": "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT"}}"#,
            node(0)
        )];
        let mut edges = Vec::new();
        for k in 0..DIAMONDS {
            comps.push(meter(fork(k, 0)));
            comps.push(meter(fork(k, 1)));
            comps.push(meter(node(k + 1)));
            for side in 0..2 {
                for (from, to) in [(node(k), fork(k, side)), (fork(k, side), node(k + 1))] {
                    edges.push(format!(
                        r#"{{"sourceElectricalComponentId": "{from}", "destinationElectricalComponentId": "{to}"}}"#
                    ));
                }
            }
        }
        assert_eq!(comps.len(), 76, "26 chain nodes plus two forks per diamond");

        let components: ComponentsFile = serde_json::from_str(&format!(
            r#"{{"electricalComponents": [{}]}}"#,
            comps.join(",")
        ))
        .unwrap();
        let connections: ConnectionsFile = serde_json::from_str(&format!(
            r#"{{"electricalComponentConnections": [{}]}}"#,
            edges.join(",")
        ))
        .unwrap();

        let import = parse(components, Some(connections)).unwrap();
        let (_, calls) = import.ordered_ids_counting();
        let nodes = import.components.len() as u64;
        let edges = import.connections.len() as u64;
        assert!(
            calls <= 2 * (nodes + edges),
            "the walk entered {calls} times for {nodes} nodes and {edges} edges; \
             a memoized post-order walk enters about {} times",
            nodes + edges
        );

        let order = components_of(&import);
        assert_eq!(order.len(), 76, "each component once");
        for k in 0..DIAMONDS {
            for side in 0..2 {
                assert!(
                    pos(&order, node(k + 1)) < pos(&order, fork(k, side)),
                    "shared child {} before its parent {}",
                    node(k + 1),
                    fork(k, side)
                );
            }
        }
    }
}
