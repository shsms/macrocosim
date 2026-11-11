//! Every structural change to a microgrid: create one, rebuild one
//! from a new spec, carrying the state of each component the change
//! leaves as it was, and unload one. Each function takes a
//! [`Structural`] guard on the engine's one structural lock, so the
//! checks here hold until the change is in place.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use super::{Engine, Structural};
use crate::runtime::{MicrogridRuntimes, RuntimeView};
use crate::sim::MicrogridSite;
use crate::sim::microgrids::{
    DEFAULT_MICROGRID_NAME, MicrogridDef, MicrogridEntry, check_port, component_owners,
    next_free_id_in, next_free_port_in,
};
use crate::spec::{BuiltTopology, Defaults, MicrogridSpec, TopologyDoc, build_topology};
use crate::undo::{Direction, UndoDepths};

/// Why a structural change was refused. Nothing changed, except after
/// `Internal`: [`declare`] returns it when one of its steps fails, and the
/// microgrids applied before that step stay applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopologyError {
    /// The spec is invalid (HTTP 422).
    Invalid(String),
    /// An id or a port belongs to something else (HTTP 409).
    Conflict(String),
    /// No microgrid has the id (HTTP 404).
    NotRegistered(u64),
    /// A step the checks should have made safe failed (HTTP 500).
    Internal(String),
}

impl fmt::Display for TopologyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(e) | Self::Conflict(e) | Self::Internal(e) => f.write_str(e),
            Self::NotRegistered(id) => write!(f, "microgrid {id} not registered"),
        }
    }
}

impl std::error::Error for TopologyError {}

/// A microgrid [`create`] made.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Created {
    pub id: u64,
    pub grpc_port: u16,
    /// Its runtime; `None` when none was started.
    pub runtime: Option<RuntimeView>,
}

/// What [`declare`] did with one microgrid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeclareOutcome {
    /// It was not loaded, and was created.
    Created,
    /// Its spec changed, and it was rebuilt with carry.
    Rebuilt,
    /// Its spec was the stored one, so it was not rebuilt; a runtime that
    /// failed to start is started again.
    Unchanged,
}

/// One microgrid of a [`declare`].
#[derive(Clone, Debug, serde::Serialize)]
pub struct Declared {
    pub id: u64,
    pub grpc_port: u16,
    /// Its runtime once the declaration is applied; `None` when it has none.
    pub runtime: Option<RuntimeView>,
    pub outcome: DeclareOutcome,
}

/// Give `spec` the lowest free microgrid id from
/// `DEFAULT_MICROGRID_ID` up when it has none. Component ids are
/// never filled in: a component without one is refused by the build.
fn assign_microgrid_id(engine: &Engine, spec: &mut MicrogridSpec) {
    if spec.id.is_none() {
        spec.id = Some(next_free_id_in(&engine.microgrids.lock(), &BTreeSet::new()));
    }
}

/// Refuse a component id that appears in two of `specs`, or that the
/// registry gives to a microgrid not in `replacing`.
fn check_ids(
    engine: &Engine,
    specs: &[&MicrogridSpec],
    replacing: &[u64],
) -> Result<(), TopologyError> {
    let mut seen: BTreeMap<u64, Option<u64>> = BTreeMap::new();
    for spec in specs {
        // Within one spec, the build names a repeated id itself.
        let ids: BTreeSet<u64> = spec.components.iter().filter_map(|c| c.id()).collect();
        for id in ids {
            if let Some(other) = seen.insert(id, spec.id) {
                return Err(TopologyError::Invalid(format!(
                    "component id {id} is in microgrid {} and in microgrid {}",
                    show(other),
                    show(spec.id)
                )));
            }
        }
    }
    let owners = component_owners(&engine.microgrids.lock());
    for (&id, &mg) in &seen {
        if let Some(&owner) = owners.get(&id)
            && !replacing.contains(&owner)
        {
            return Err(TopologyError::Conflict(format!(
                "component id {id} is already in microgrid {owner}{}. Give the \
                 component another id, or unload microgrid {owner} (DELETE \
                 /api/mg/{owner}) if it is stale.",
                match mg {
                    Some(mg) => format!(", so microgrid {mg} cannot take it"),
                    None => String::new(),
                }
            )));
        }
    }
    Ok(())
}

fn show(id: Option<u64>) -> String {
    id.map_or_else(|| "(new)".to_string(), |id| id.to_string())
}

/// Build `spec` into a new microgrid, register it and start its
/// runtime. `source` is the file the spec came from, `None` for a
/// microgrid made in the UI, which is then unsaved.
pub async fn create(
    structural: &Structural,
    runtimes: &MicrogridRuntimes,
    mut spec: MicrogridSpec,
    source: Option<PathBuf>,
) -> Result<Created, TopologyError> {
    let engine = structural.engine();
    assign_microgrid_id(engine, &mut spec);
    let id = spec.id.expect("assigned above");
    check_ids(engine, &[&spec], &[])?;
    let built =
        build_topology(&spec, &Defaults::default()).map_err(|e| invalid(id, e.message()))?;
    create_built(engine, runtimes, spec, built, source).await
}

/// [`create`] from `built`, the build of `spec`, whose ids are
/// checked already.
async fn create_built(
    engine: &Engine,
    runtimes: &MicrogridRuntimes,
    mut spec: MicrogridSpec,
    built: BuiltTopology,
    source: Option<PathBuf>,
) -> Result<Created, TopologyError> {
    let id = spec.id.expect("the caller gives every spec an id");
    let components = built.filled.len();
    // The site is built and filled before the registry lock is taken:
    // installing components takes the gateway lock, which is never
    // taken under the registry's.
    let site = MicrogridSite::new();
    engine.attach_site(&site);
    site.swap_topology(built, &BTreeSet::new());
    let reserved = engine.reserved_ports();
    {
        let mut registry = engine.microgrids.lock();
        if registry.contains_key(&id) {
            return Err(TopologyError::Conflict(format!(
                "microgrid {id} is already registered"
            )));
        }
        let grpc_port = match spec.grpc_port {
            Some(p) => {
                check_port(&registry, &reserved, id, p)
                    .map_err(|c| TopologyError::Conflict(format!("gRPC port {p} {c}")))?;
                p
            }
            None => next_free_port_in(&registry, &reserved),
        };
        spec.grpc_port = Some(grpc_port);
        registry.insert(
            id,
            MicrogridEntry {
                def: MicrogridDef {
                    id,
                    name: display_name(&spec),
                    grpc_port,
                    tso: spec.tso.clone(),
                },
                site: site.clone(),
                unsaved: source.is_none(),
                source,
                spec,
            },
        );
    }
    log::info!("Microgrid #{id} created: {components} components");
    warn_if_invalid(id, &site);
    engine.announce_registered(id);
    if let Err(e) = runtimes.start(id).await {
        log::error!("Microgrid #{id}: its runtime did not start: {e}");
    }
    let grpc_port = engine.microgrids.lock()[&id].def.grpc_port;
    Ok(Created {
        id,
        grpc_port,
        runtime: runtimes.status(id),
    })
}

/// Create every microgrid of `doc`, each with `source` as its file
/// (`None` for microgrids made in the UI). The component ids are
/// checked across the document and the registry first, and the
/// microgrid ids and ports the document leaves out are chosen around
/// the ones it names. A later refusal unloads the microgrids created
/// before it, so a refused document leaves nothing behind.
pub async fn create_doc(
    structural: &Structural,
    runtimes: &MicrogridRuntimes,
    doc: TopologyDoc,
    source: Option<PathBuf>,
) -> Result<Vec<Created>, TopologyError> {
    let engine = structural.engine();
    let mut specs = doc.microgrids;
    let refs: Vec<&MicrogridSpec> = specs.iter().collect();
    check_ids(engine, &refs, &[])?;
    {
        let registry = engine.microgrids.lock();
        let mut ids: BTreeSet<u64> = specs.iter().filter_map(|s| s.id).collect();
        for spec in specs.iter_mut().filter(|s| s.id.is_none()) {
            let id = next_free_id_in(&registry, &ids);
            ids.insert(id);
            spec.id = Some(id);
        }
        let mut used = engine.reserved_ports();
        used.extend(specs.iter().filter_map(|s| s.grpc_port));
        fill_ports(&registry, &mut used, specs.iter_mut());
    }
    let mut created: Vec<Created> = Vec::new();
    for spec in specs {
        match create(structural, runtimes, spec, source.clone()).await {
            Ok(c) => created.push(c),
            Err(e) => {
                for c in &created {
                    if let Err(e) = unload(structural, runtimes, c.id).await {
                        log::error!("Microgrid #{}: {e}", c.id);
                    }
                }
                return Err(e);
            }
        }
    }
    Ok(created)
}

/// Rebuild microgrid `id` from `spec`, carrying the state of every
/// component whose defaults-filled spec is unchanged under the same
/// component id. The comparison leaves out the name, health and modes
/// ([`crate::spec::ComponentSpec::carry_key`]), which come from the
/// new spec either way. Returns the spec it replaced. The site, its
/// physics task and its servers keep running, except that a changed
/// `grpc_port` moves the gRPC server to the new port; a failed bind
/// there marks the runtime failed and does not fail the rebuild.
pub async fn rebuild(
    structural: &Structural,
    runtimes: &MicrogridRuntimes,
    id: u64,
    spec: MicrogridSpec,
) -> Result<MicrogridSpec, TopologyError> {
    let engine = structural.engine();
    rebuild_with(engine, runtimes, id, spec, |spec| {
        check_ids(engine, &[spec], &[id])?;
        build_topology(spec, &Defaults::default()).map_err(|e| invalid(id, e.message()))
    })
    .await
}

/// Rebuild microgrid `id` from `spec` as the UI edits it: the spec it
/// replaced goes on its undo stack, and a microgrid that came from a
/// file is marked unsaved. See [`rebuild`].
pub async fn edit(
    structural: &Structural,
    runtimes: &MicrogridRuntimes,
    id: u64,
    spec: MicrogridSpec,
) -> Result<(), TopologyError> {
    let replaced = rebuild(structural, runtimes, id, spec).await?;
    record_edit(structural.engine(), id, replaced);
    Ok(())
}

/// Stack `replaced`, the spec an edit of microgrid `id` replaced, on its undo
/// stack, and mark a microgrid that came from a file unsaved.
fn record_edit(engine: &Engine, id: u64, replaced: MicrogridSpec) {
    engine
        .spec_undo
        .lock()
        .entry(id)
        .or_default()
        .push_edit(replaced);
    mark_unsaved(engine, id);
}

/// [`rebuild`], with `build` checking the ids of the spec and
/// building it once its microgrid id and port are checked.
async fn rebuild_with(
    engine: &Engine,
    runtimes: &MicrogridRuntimes,
    id: u64,
    mut spec: MicrogridSpec,
    build: impl FnOnce(&MicrogridSpec) -> Result<BuiltTopology, TopologyError>,
) -> Result<MicrogridSpec, TopologyError> {
    let (site, old, port) = {
        let registry = engine.microgrids.lock();
        let entry = registry.get(&id).ok_or(TopologyError::NotRegistered(id))?;
        let old = entry.spec.clone();
        (entry.site.clone(), old, entry.def.grpc_port)
    };
    match spec.id {
        None => spec.id = Some(id),
        Some(other) if other != id => {
            return Err(invalid(
                id,
                &format!("the spec is for microgrid {other}, not {id}"),
            ));
        }
        Some(_) => {}
    }
    let moved = match spec.grpc_port {
        None => {
            spec.grpc_port = Some(port);
            None
        }
        Some(p) if p != port => {
            let reserved = engine.reserved_ports();
            check_port(&engine.microgrids.lock(), &reserved, id, p)
                .map_err(|c| TopologyError::Conflict(format!("gRPC port {p} {c}")))?;
            Some(p)
        }
        Some(_) => None,
    };
    let built = build(&spec)?;
    let defaults = Defaults::default();
    let carried: BTreeSet<u64> = old
        .components
        .iter()
        .filter_map(|c| {
            let cid = c.id()?;
            let new = built.filled.get(&cid)?;
            (new.carry_key() == defaults.fill_component(c).carry_key()).then_some(cid)
        })
        .collect();
    let components = built.filled.len();
    site.swap_topology(built, &carried);
    {
        let mut registry = engine.microgrids.lock();
        let entry = registry
            .get_mut(&id)
            .ok_or(TopologyError::NotRegistered(id))?;
        entry.def.name = display_name(&spec);
        entry.def.tso.clone_from(&spec.tso);
        if let Some(p) = moved {
            entry.def.grpc_port = p;
        }
        entry.spec = spec;
    }
    log::info!(
        "Microgrid #{id} rebuilt: {components} components, {} carried",
        carried.len()
    );
    warn_if_invalid(id, &site);
    if let Some(p) = moved
        && let Err(e) = runtimes.restart_grpc(id, p).await
    {
        log::error!("Microgrid #{id}: its gRPC server did not move to port {p}: {e}");
    }
    Ok(old)
}

/// Log a warning when microgrid `id`'s components (`site`) do not make a
/// component graph the Microgrid API's graph library accepts.
fn warn_if_invalid(id: u64, site: &MicrogridSite) {
    if let Some(e) = crate::sim::graph_adapter::validation_error(site) {
        log::warn!("Microgrid #{id}: the component graph is not valid: {e}");
    }
}

/// Mark microgrid `id` as edited here: a microgrid that came from a
/// file now differs from it. A UI-created one is unsaved already.
fn mark_unsaved(engine: &Engine, id: u64) {
    if let Some(entry) = engine.microgrids.lock().get_mut(&id)
        && entry.source.is_some()
    {
        entry.unsaved = true;
    }
}

/// Take one step of microgrid `id`'s spec history in `dir`: rebuild
/// it, with carry, from the newest spec on that stack, and stack the
/// spec it replaced on the other one. A refused rebuild puts the step
/// back, and nothing has changed.
pub async fn step_history(
    structural: &Structural,
    runtimes: &MicrogridRuntimes,
    id: u64,
    dir: Direction,
) -> Result<UndoDepths, TopologyError> {
    let engine = structural.engine();
    let step = engine
        .spec_undo
        .lock()
        .get_mut(&id)
        .and_then(|h| h.pop(dir));
    let Some(step) = step else {
        return Err(TopologyError::Conflict(format!(
            "microgrid {id} has nothing to {dir}"
        )));
    };
    match rebuild(structural, runtimes, id, step.clone()).await {
        Ok(replaced) => {
            engine
                .spec_undo
                .lock()
                .entry(id)
                .or_default()
                .push(dir.opposite(), replaced);
            mark_unsaved(engine, id);
            Ok(engine.spec_undo_depths(id))
        }
        Err(e) => {
            engine
                .spec_undo
                .lock()
                .entry(id)
                .or_default()
                .push(dir, step);
            Err(e)
        }
    }
}

/// Unload microgrid `id`: take it out of the registry, which frees
/// its id and its component ids, then stop its runtime (streams, gRPC
/// servers, physics, sampler, loopback), end a running recording and
/// close its CSV sinks, delete its dispatches (each with a `Deleted`
/// event), drop its undo stacks and announce it on
/// `microgrid_unloaded`.
pub async fn unload(
    structural: &Structural,
    runtimes: &MicrogridRuntimes,
    id: u64,
) -> Result<(), TopologyError> {
    let engine = structural.engine();
    let entry = {
        let mut registry = engine.microgrids.lock();
        registry
            .remove(&id)
            .ok_or(TopologyError::NotRegistered(id))?
    };
    runtimes.stop(id).await;
    let site = entry.site;
    // A recording ends with the microgrid; its report has no reader,
    // since a later stop finds no microgrid.
    let _ = site.recording_stop(site.now());
    site.recording_close_csv();
    engine.dispatches.remove_mg(id);
    engine.spec_undo.lock().remove(&id);
    let _ = engine.microgrid_unloaded.send(id);
    log::info!("Microgrid #{id} unloaded");
    Ok(())
}

/// Apply a declaration of microgrids: create each listed microgrid that is not
/// loaded, rebuild with carry each loaded one whose spec changed, and leave
/// alone each one whose spec is the stored one once both have their defaults
/// filled in. Nothing is unloaded. Returns one entry per microgrid, in the
/// document's order.
///
/// Every check runs before anything changes: every microgrid has an id, used
/// once in the document; every spec builds; no gRPC port is an enterprise
/// server's; no gRPC port and no component id is in two listed microgrids or
/// held by a microgrid the document does not list (ids may move between listed
/// ones); and no loaded microgrid moves to a port another listed one still
/// holds. A new microgrid without a port is given a free one. A rebuild stacks
/// the replaced spec for undo and marks a microgrid that came from a file
/// unsaved, as [`edit`] does; a new one has no source. Then the runtime of
/// every listed microgrid that was loaded is started, which retries one whose
/// start failed and leaves a running one alone.
pub async fn declare(
    structural: &Structural,
    runtimes: &MicrogridRuntimes,
    doc: TopologyDoc,
) -> Result<Vec<Declared>, TopologyError> {
    let started = std::time::Instant::now();
    let engine = structural.engine();
    let mut specs = doc.microgrids;
    if specs.is_empty() {
        return Err(TopologyError::Invalid(
            "the document has no microgrids".to_string(),
        ));
    }
    let mut ids = Vec::with_capacity(specs.len());
    for (i, spec) in specs.iter().enumerate() {
        let Some(id) = spec.id else {
            return Err(TopologyError::Invalid(format!(
                "microgrid {i} of the document (counting from 0) has no id"
            )));
        };
        if ids.contains(&id) {
            return Err(TopologyError::Invalid(format!(
                "microgrid {id} is in the document twice"
            )));
        }
        ids.push(id);
    }
    let defaults = Defaults::default();
    let mut builds = Vec::with_capacity(specs.len());
    for spec in &specs {
        let id = spec.id.expect("checked above");
        builds.push(build_topology(spec, &defaults).map_err(|e| invalid(id, e.message()))?);
    }
    let reserved = engine.reserved_ports();
    let replacing: Vec<u64> = {
        let registry = engine.microgrids.lock();
        let replacing: Vec<u64> = ids
            .iter()
            .copied()
            .filter(|id| registry.contains_key(id))
            .collect();
        // The ports in use once the declaration is applied: every registered
        // microgrid's, each listed one moving to its own.
        let mut taken: BTreeMap<u16, u64> = BTreeMap::new();
        for spec in &specs {
            let id = spec.id.expect("checked above");
            let port = match spec.grpc_port {
                Some(p) => p,
                None => match registry.get(&id) {
                    Some(entry) => entry.def.grpc_port,
                    None => continue,
                },
            };
            if reserved.contains(&port) {
                return Err(TopologyError::Conflict(format!(
                    "gRPC port {port} of microgrid {id} is an enterprise server's"
                )));
            }
            if let Some(other) = taken.insert(port, id) {
                return Err(TopologyError::Conflict(format!(
                    "gRPC port {port} is in microgrid {other} and in microgrid {id}"
                )));
            }
        }
        for (&other, entry) in registry.iter() {
            if ids.contains(&other) {
                continue;
            }
            if let Some(&id) = taken.get(&entry.def.grpc_port) {
                return Err(TopologyError::Conflict(format!(
                    "gRPC port {} is already microgrid {other}'s, so microgrid {id} \
                     cannot take it. Give microgrid {id} another grpc_port, or unload \
                     microgrid {other} (DELETE /api/mg/{other}) if it is stale.",
                    entry.def.grpc_port
                )));
            }
        }
        // A microgrid that moves takes a port its old holder still has while
        // the declaration is applied, one microgrid at a time.
        for spec in &specs {
            let id = spec.id.expect("checked above");
            if let (Some(p), Some(entry)) = (spec.grpc_port, registry.get(&id))
                && p != entry.def.grpc_port
                && let Some((holder, _)) = registry
                    .iter()
                    .find(|(h, e)| **h != id && e.def.grpc_port == p)
            {
                return Err(TopologyError::Conflict(format!(
                    "gRPC port {p} is still microgrid {holder}'s; move microgrid {id} to it \
                     in a later declaration"
                )));
            }
        }
        // New microgrids without a port take free ones now, so no
        // later step can find its port taken.
        let mut used: Vec<u16> = reserved.clone();
        used.extend(taken.keys().copied());
        let new = specs
            .iter_mut()
            .filter(|s| s.id.is_some_and(|id| !registry.contains_key(&id)));
        fill_ports(&registry, &mut used, new);
        replacing
    };
    let refs: Vec<&MicrogridSpec> = specs.iter().collect();
    check_ids(engine, &refs, &replacing)?;

    let mut outcomes: BTreeMap<u64, DeclareOutcome> = BTreeMap::new();
    // Every rebuild before any create: a new microgrid may take a
    // port a listed one is leaving, and a rebuild never needs one a
    // new microgrid takes.
    let (rebuilt, created): (Vec<_>, Vec<_>) = specs
        .into_iter()
        .zip(builds)
        .partition(|(spec, _)| replacing.contains(&spec.id.expect("checked above")));
    for (spec, built) in rebuilt {
        let id = spec.id.expect("checked above");
        let (stored, port) = {
            let registry = engine.microgrids.lock();
            let entry = &registry[&id];
            (entry.spec.clone(), entry.def.grpc_port)
        };
        if unchanged(&stored, &spec, port) {
            outcomes.insert(id, DeclareOutcome::Unchanged);
            continue;
        }
        let replaced = rebuild_with(engine, runtimes, id, spec, |_| Ok(built))
            .await
            .map_err(|e| internal(id, "rebuild", e))?;
        record_edit(engine, id, replaced);
        outcomes.insert(id, DeclareOutcome::Rebuilt);
    }
    for (spec, built) in created {
        let id = spec.id.expect("checked above");
        create_built(engine, runtimes, spec, built, None)
            .await
            .map_err(|e| internal(id, "create", e))?;
        outcomes.insert(id, DeclareOutcome::Created);
    }
    // A created microgrid's runtime was started by its create.
    for &id in ids
        .iter()
        .filter(|id| outcomes[id] != DeclareOutcome::Created)
    {
        if let Err(e) = runtimes.start(id).await {
            log::error!("Microgrid #{id}: the declaration did not start its runtime: {e}");
        }
    }
    let of = |o: DeclareOutcome| -> Vec<u64> {
        ids.iter().copied().filter(|id| outcomes[id] == o).collect()
    };
    log::info!(
        "declaration: created {:?}, rebuilt {:?}, unchanged {:?} ({} ms)",
        of(DeclareOutcome::Created),
        of(DeclareOutcome::Rebuilt),
        of(DeclareOutcome::Unchanged),
        started.elapsed().as_millis()
    );
    let registry = engine.microgrids.lock();
    Ok(ids
        .iter()
        .map(|&id| Declared {
            id,
            grpc_port: registry[&id].def.grpc_port,
            runtime: runtimes.status(id),
            outcome: outcomes[&id],
        })
        .collect())
}

/// A step of a declaration that its checks should have made safe failed; the
/// microgrids applied before it stay applied.
fn internal(id: u64, step: &str, e: TopologyError) -> TopologyError {
    log::error!("Microgrid #{id}: the declaration's {step} failed: {e}");
    TopologyError::Internal(format!("microgrid {id}: the {step} failed: {e}"))
}

/// Whether `new` is the spec `stored` with nothing changed, so a declaration of
/// it need not rebuild: equal once both have their defaults filled in, with
/// `loaded_port` as the new spec's port when it names none, compared component
/// by component (every field, the name, health and modes too) and with the
/// connections as a set.
fn unchanged(stored: &MicrogridSpec, new: &MicrogridSpec, loaded_port: u16) -> bool {
    let defaults = Defaults::default();
    let tick = crate::spec::build::DEFAULT_PHYSICS_TICK_MS as f64 / 1000.0;
    let normal = |spec: &MicrogridSpec, port: u16| {
        let mut filled = crate::spec::defaults::fill(spec, &defaults);
        filled.grpc_port.get_or_insert(port);
        filled.physics_tick_s.get_or_insert(tick);
        let components: BTreeMap<Option<u64>, crate::spec::ComponentSpec> =
            filled.components.drain(..).map(|c| (c.id(), c)).collect();
        let connections: BTreeSet<(u64, u64)> = filled.connections.drain(..).collect();
        (filled, components, connections)
    };
    normal(stored, loaded_port) == normal(new, loaded_port)
}

/// Give each of `specs` that names no gRPC port a free one, around
/// the registry's ports and `used` (the reserved ports and the ones
/// the specs name), adding each one handed out to `used`.
fn fill_ports<'a>(
    registry: &BTreeMap<u64, MicrogridEntry>,
    used: &mut Vec<u16>,
    specs: impl Iterator<Item = &'a mut MicrogridSpec>,
) {
    for spec in specs.filter(|s| s.grpc_port.is_none()) {
        let port = next_free_port_in(registry, used);
        used.push(port);
        spec.grpc_port = Some(port);
    }
}

fn display_name(spec: &MicrogridSpec) -> String {
    spec.name
        .clone()
        .unwrap_or_else(|| DEFAULT_MICROGRID_NAME.to_string())
}

fn invalid(id: u64, e: &str) -> TopologyError {
    TopologyError::Invalid(format!("microgrid {id}: {e}"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::engine::Metadata;
    use crate::sim::runtime::Health;
    use crate::sim::sim_clock::{NowSource, headless_base};
    use crate::spec::TopologyDoc;

    fn engine() -> Engine {
        let clock = Arc::new(crate::sim::sim_clock::ManualClock::new());
        Engine::new(
            PathBuf::from("."),
            Metadata::default(),
            NowSource::sim(headless_base(), clock),
        )
    }

    fn spec(json: &str) -> MicrogridSpec {
        let doc = format!(r#"{{"schema_version": 1, "microgrids": [{json}]}}"#);
        TopologyDoc::from_json(&doc).unwrap().microgrids.remove(0)
    }

    const PLANT: &str = r#"{"name": "plant", "components": [
        {"type": "grid_connection_point", "id": 1},
        {"type": "battery_inverter", "id": 2},
        {"type": "battery", "id": 3, "initial_soc_pct": 50},
        {"type": "battery_inverter", "id": 4},
        {"type": "battery", "id": 5, "initial_soc_pct": 50, "health": "standby"}
    ], "connections": [[1, 2], [2, 3], [1, 4], [4, 5]]}"#;

    async fn create_plant(engine: &Engine) -> u64 {
        create(
            &engine.structural().await,
            &MicrogridRuntimes::inert(),
            spec(PLANT),
            None,
        )
        .await
        .unwrap()
        .id
    }

    async fn rebuild_inert(
        engine: &Engine,
        id: u64,
        spec: MicrogridSpec,
    ) -> Result<MicrogridSpec, TopologyError> {
        rebuild(
            &engine.structural().await,
            &MicrogridRuntimes::inert(),
            id,
            spec,
        )
        .await
    }

    fn site(engine: &Engine, id: u64) -> MicrogridSite {
        engine.microgrids.lock()[&id].site.clone()
    }

    fn soc(site: &MicrogridSite, id: u64) -> f32 {
        site.get(id).unwrap().dc_storage().unwrap().soc_pct()
    }

    #[tokio::test]
    async fn create_fills_the_ids_and_stores_the_spec() {
        let engine = engine();
        let created = create(
            &engine.structural().await,
            &MicrogridRuntimes::inert(),
            spec(PLANT),
            None,
        )
        .await
        .unwrap();
        assert_eq!(created.id, crate::sim::microgrids::DEFAULT_MICROGRID_ID);
        assert_eq!(created.grpc_port, crate::sim::microgrids::DEFAULT_GRPC_PORT);
        assert!(created.runtime.is_none(), "inert runtimes start nothing");
        let registry = engine.microgrids.lock();
        let entry = &registry[&created.id];
        let stored = &entry.spec;
        assert_eq!(stored.id, Some(created.id));
        assert_eq!(stored.grpc_port, Some(created.grpc_port));
        assert_eq!(entry.def.name, "plant");
        assert!(entry.unsaved, "a microgrid made in the UI is unsaved");
        assert_eq!(entry.site.component_count(), 5);
        assert_eq!(entry.site.runtime_of(5).health, Health::Standby);
    }

    #[tokio::test]
    async fn a_file_backed_microgrid_starts_saved() {
        let engine = engine();
        let source = PathBuf::from("/tmp/topology.json");
        let created = create(
            &engine.structural().await,
            &MicrogridRuntimes::inert(),
            spec(PLANT),
            Some(source.clone()),
        )
        .await
        .unwrap();
        let registry = engine.microgrids.lock();
        assert!(!registry[&created.id].unsaved);
        assert_eq!(registry[&created.id].source, Some(source));
    }

    #[tokio::test]
    async fn a_taken_or_missing_id_is_refused() {
        let engine = engine();
        create_plant(&engine).await;
        let err = create(
            &engine.structural().await,
            &MicrogridRuntimes::inert(),
            spec(r#"{"components": [{"type": "meter", "id": 3}]}"#),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, TopologyError::Conflict(m) if m.contains("component id 3 is already in microgrid 2200")),
            "{err:?}"
        );
        let err = create(
            &engine.structural().await,
            &MicrogridRuntimes::inert(),
            spec(r#"{"components": [{"type": "meter"}]}"#),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, TopologyError::Invalid(m) if m.contains("has no id")),
            "{err:?}"
        );
        assert_eq!(engine.microgrids.lock().len(), 1, "nothing was created");
    }

    /// An undo step whose spec another microgrid has since taken an
    /// id from is refused and stays on its stack.
    #[tokio::test]
    async fn a_refused_undo_step_stays_on_its_stack() {
        let engine = engine();
        let id = create_plant(&engine).await;
        let runtimes = MicrogridRuntimes::inert();
        let mut smaller = spec(PLANT);
        smaller.components.retain(|c| c.id() != Some(5));
        smaller.connections.retain(|&(_, to)| to != 5);
        edit(&engine.structural().await, &runtimes, id, smaller)
            .await
            .unwrap();
        create(
            &engine.structural().await,
            &runtimes,
            spec(r#"{"id": 9, "components": [{"type": "meter", "id": 5}]}"#),
            None,
        )
        .await
        .unwrap();
        let err = step_history(&engine.structural().await, &runtimes, id, Direction::Undo)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, TopologyError::Conflict(m) if m.contains("component id 5")),
            "{err:?}"
        );
        assert_eq!(engine.spec_undo_depths(id), UndoDepths { undo: 1, redo: 0 });
        assert!(site(&engine, id).get(5).is_none(), "nothing changed");
    }

    #[test]
    fn an_id_in_two_specs_is_refused() {
        let engine = engine();
        let a = spec(r#"{"id": 1, "components": [{"type": "meter", "id": 9}]}"#);
        let b = spec(r#"{"id": 2, "components": [{"type": "meter", "id": 9}]}"#);
        let err = check_ids(&engine, &[&a, &b], &[]).unwrap_err();
        assert_eq!(
            err,
            TopologyError::Invalid("component id 9 is in microgrid 1 and in microgrid 2".into())
        );
    }

    #[tokio::test]
    async fn a_rebuild_carries_what_did_not_change() {
        let engine = engine();
        let id = create_plant(&engine).await;
        let site = site(&engine, id);
        for battery in [3, 5] {
            let c = site.get(battery).unwrap();
            site.set_soc_pct(c.as_ref(), 80.0).unwrap();
        }
        let changed = PLANT.replace(
            r#""id": 3, "initial_soc_pct": 50"#,
            r#""id": 3, "initial_soc_pct": 50, "capacity_wh": 1000"#,
        );
        let replaced = rebuild_inert(&engine, id, spec(&changed)).await.unwrap();
        assert_eq!(replaced.components.len(), 5);
        assert_eq!(soc(&site, 3), 50.0, "changed: from the spec");
        assert_eq!(soc(&site, 5), 80.0, "unchanged: carried");
        let registry = engine.microgrids.lock();
        let stored = &registry[&id].spec;
        assert_eq!(stored.grpc_port, Some(registry[&id].def.grpc_port));
        assert_eq!(stored.components, spec(&changed).components);
    }

    /// A rename and a spec change of health or a mode are applied to
    /// the carried component; none of them restarts it.
    #[tokio::test]
    async fn a_rename_or_a_mode_change_keeps_the_state() {
        let engine = engine();
        let id = create_plant(&engine).await;
        let site = site(&engine, id);
        for battery in [3, 5] {
            let c = site.get(battery).unwrap();
            site.set_soc_pct(c.as_ref(), 80.0).unwrap();
        }
        let renamed = PLANT
            .replace(
                r#""id": 3, "initial_soc_pct": 50"#,
                r##""id": 3, "initial_soc_pct": 50, "name": "pack",
                    "operational_mode": "control_and_telemetry",
                    "telemetry_mode": "error_empty", "command_mode": "timeout""##,
            )
            .replace(r#""health": "standby""#, r#""health": "error""#);
        rebuild_inert(&engine, id, spec(&renamed)).await.unwrap();
        assert_eq!(soc(&site, 3), 80.0, "renamed: carried");
        assert_eq!(soc(&site, 5), 80.0, "new health: carried");
        assert_eq!(site.display_name(3).as_deref(), Some("pack"));
        assert_eq!(site.runtime_of(5).health, Health::Error);
        let modes = site.runtime_of(3);
        assert_eq!(
            (modes.telemetry, modes.command),
            (
                crate::sim::runtime::TelemetryMode::ErrorEmpty,
                crate::sim::runtime::CommandMode::Timeout
            )
        );
    }

    /// The ids and ports a document leaves out are chosen around the
    /// ones it names, wherever in the document those come.
    #[tokio::test]
    async fn a_document_mixing_chosen_and_given_ids_is_created() {
        use crate::sim::microgrids::{DEFAULT_GRPC_PORT, DEFAULT_MICROGRID_ID};
        let engine = engine();
        let doc = doc(&format!(
            r#"{{"components": [{{"type": "meter", "id": 1}}]}},
               {{"id": {DEFAULT_MICROGRID_ID}, "grpc_port": {DEFAULT_GRPC_PORT},
                 "components": [{{"type": "meter", "id": 2}}]}}"#
        ));
        let created = create_doc(
            &engine.structural().await,
            &MicrogridRuntimes::inert(),
            doc,
            None,
        )
        .await
        .unwrap();
        assert_eq!(created[1].id, DEFAULT_MICROGRID_ID);
        assert_eq!(created[1].grpc_port, DEFAULT_GRPC_PORT);
        assert_ne!(created[0].id, DEFAULT_MICROGRID_ID);
        assert_ne!(created[0].grpc_port, DEFAULT_GRPC_PORT);
    }

    #[tokio::test]
    async fn a_rebuild_moves_to_a_free_port_only() {
        let engine = engine();
        let id = create_plant(&engine).await;
        let mut zero = spec(PLANT);
        zero.grpc_port = Some(0);
        assert!(matches!(
            rebuild_inert(&engine, id, zero).await,
            Err(TopologyError::Invalid(m)) if m.contains("grpc_port")
        ));
        let other = create(
            &engine.structural().await,
            &MicrogridRuntimes::inert(),
            spec(r#"{"components": []}"#),
            None,
        )
        .await
        .unwrap();
        let mut taken = spec(PLANT);
        taken.grpc_port = Some(other.grpc_port);
        assert!(matches!(
            rebuild_inert(&engine, id, taken).await,
            Err(TopologyError::Conflict(m)) if m.contains(&format!("microgrid {}", other.id))
        ));
        let mut moved = spec(PLANT);
        moved.grpc_port = Some(9999);
        rebuild_inert(&engine, id, moved).await.unwrap();
        let registry = engine.microgrids.lock();
        assert_eq!(registry[&id].def.grpc_port, 9999);
        assert_eq!(registry[&id].spec.grpc_port, Some(9999));
    }

    #[tokio::test]
    async fn a_rebuild_refuses_another_mg_and_an_unknown_id() {
        let engine = engine();
        let id = create_plant(&engine).await;
        let mut other = spec(PLANT);
        other.id = Some(id + 1);
        assert!(matches!(
            rebuild_inert(&engine, id, other).await,
            Err(TopologyError::Invalid(_))
        ));
        assert_eq!(
            rebuild_inert(&engine, 77, spec(PLANT)).await,
            Err(TopologyError::NotRegistered(77))
        );
    }

    /// An unload forgets the microgrid and everything kept for it,
    /// and frees its ids for the next microgrid.
    #[tokio::test]
    async fn an_unload_frees_the_ids_and_drops_what_was_kept() {
        let engine = engine();
        let id = create_plant(&engine).await;
        let site = site(&engine, id);
        let dir = tempfile::TempDir::with_prefix("mc-unload-").unwrap();
        site.recording_start("run".into(), Some(dir.path()), site.now())
            .unwrap();
        let dispatches = engine.dispatches();
        let data = crate::proto::dispatch::DispatchData {
            r#type: "x".into(),
            ..Default::default()
        };
        let made = dispatches.create(id, data, true).unwrap();
        engine
            .spec_undo
            .lock()
            .entry(id)
            .or_default()
            .push_edit(spec(PLANT));
        assert_eq!(engine.spec_undo_depths(id).undo, 1);
        let mut changes = dispatches.subscribe();
        let mut unregistered = engine.subscribe_microgrid_unloaded();

        unload(&engine.structural().await, &MicrogridRuntimes::inert(), id)
            .await
            .unwrap();

        assert!(!engine.microgrids.lock().contains_key(&id));
        assert!(!site.recording_is_running(), "the recording was stopped");
        assert_eq!(site.recording_close_csv(), 0, "the sinks were closed");
        assert!(dispatches.list_mg(id).is_empty());
        let ev = changes.try_recv().unwrap();
        assert_eq!(ev.change, crate::sim::dispatch::DispatchChange::Deleted);
        assert_eq!(ev.dispatch_id(), made.metadata.unwrap().dispatch_id);
        assert_eq!(engine.spec_undo_depths(id).undo, 0);
        assert_eq!(unregistered.try_recv().unwrap(), id);
        // The id and the component ids are free again.
        assert_eq!(create_plant(&engine).await, id);
    }

    #[tokio::test]
    async fn an_unload_refuses_an_unknown_id() {
        let engine = engine();
        let id = create_plant(&engine).await;
        let inert = MicrogridRuntimes::inert();
        assert_eq!(
            unload(&engine.structural().await, &inert, 77).await,
            Err(TopologyError::NotRegistered(77))
        );
        assert!(engine.microgrids.lock().contains_key(&id));
    }

    fn doc(json: &str) -> TopologyDoc {
        TopologyDoc::from_json(&format!(
            r#"{{"schema_version": 1, "microgrids": [{json}]}}"#
        ))
        .unwrap()
    }

    const NORTH: &str = r#"{"id": 61, "components": [
        {"type": "grid_connection_point", "id": 1000},
        {"type": "battery_inverter", "id": 11},
        {"type": "battery", "id": 12, "initial_soc_pct": 50}
    ], "connections": [[1000, 11], [11, 12]]}"#;
    const SOUTH: &str = r#"{"id": 62, "components": [
        {"type": "grid_connection_point", "id": 1001}
    ]}"#;

    fn with_port(mg: &str, id: u64, port: u16) -> String {
        mg.replace(
            &format!(r#""id": {id},"#),
            &format!(r#""id": {id}, "grpc_port": {port},"#),
        )
    }

    /// A new microgrid listed first may take the port a listed one
    /// leaves: every rebuild runs before any create.
    #[tokio::test]
    async fn a_new_microgrid_takes_the_port_a_rebuilt_one_leaves() {
        let engine = engine();
        declare_inert(&engine, doc(&with_port(NORTH, 61, 9100)))
            .await
            .unwrap();
        let moved = with_port(NORTH, 61, 9110);
        let new = with_port(SOUTH, 62, 9100);
        let declared = declare_inert(&engine, doc(&format!("{new}, {moved}")))
            .await
            .unwrap();
        assert_eq!(
            outcomes(&declared),
            vec![(62, DeclareOutcome::Created), (61, DeclareOutcome::Rebuilt)]
        );
        let registry = engine.microgrids.lock();
        assert_eq!(registry[&62].def.grpc_port, 9100);
        assert_eq!(registry[&61].def.grpc_port, 9110);
    }

    /// A declaration refuses an enterprise server's port, a port listed twice,
    /// two microgrids trading ports in one declaration, and a port a microgrid
    /// it does not list holds.
    #[tokio::test]
    async fn a_declaration_refuses_ports_it_cannot_claim() {
        let engine = engine();
        let reserved = engine.reserved_ports()[0];
        let err = declare_inert(&engine, doc(&with_port(NORTH, 61, reserved)))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, TopologyError::Conflict(m) if m.contains("enterprise server's")),
            "{err:?}"
        );
        let twice = format!(
            "{}, {}",
            with_port(NORTH, 61, 9100),
            with_port(SOUTH, 62, 9100)
        );
        let err = declare_inert(&engine, doc(&twice)).await.unwrap_err();
        assert!(
            matches!(&err, TopologyError::Conflict(m) if m.contains("in microgrid 61 and in microgrid 62")),
            "{err:?}"
        );
        assert!(engine.microgrids.lock().is_empty(), "nothing was applied");

        let placed = format!(
            "{}, {}",
            with_port(NORTH, 61, 9100),
            with_port(SOUTH, 62, 9110)
        );
        declare_inert(&engine, doc(&placed)).await.unwrap();
        let swapped = format!(
            "{}, {}",
            with_port(NORTH, 61, 9110),
            with_port(SOUTH, 62, 9100)
        );
        let err = declare_inert(&engine, doc(&swapped)).await.unwrap_err();
        assert!(
            matches!(&err, TopologyError::Conflict(m) if m.contains("in a later declaration")),
            "{err:?}"
        );
        assert_eq!(engine.microgrids.lock()[&61].def.grpc_port, 9100);

        // A port a microgrid outside the declaration holds, one made in the UI
        // here.
        create(
            &engine.structural().await,
            &MicrogridRuntimes::inert(),
            spec(r#"{"id": 70, "grpc_port": 9200, "components": [{"type": "grid_connection_point", "id": 700}]}"#),
            None,
        )
        .await
        .unwrap();
        let held = format!(
            "{}, {}",
            with_port(NORTH, 61, 9200),
            with_port(SOUTH, 62, 9110)
        );
        let err = declare_inert(&engine, doc(&held)).await.unwrap_err();
        assert!(
            matches!(&err, TopologyError::Conflict(m) if m.contains("already microgrid 70's")),
            "{err:?}"
        );
        assert_eq!(engine.microgrids.lock()[&61].def.grpc_port, 9100);
    }

    /// A new microgrid without a port skips the ports the declaration names.
    #[tokio::test]
    async fn a_declaration_hands_out_ports_around_its_own_claims() {
        use crate::sim::microgrids::DEFAULT_GRPC_PORT;
        let engine = engine();
        let claimed = with_port(NORTH, 61, DEFAULT_GRPC_PORT);
        declare_inert(&engine, doc(&format!("{SOUTH}, {claimed}")))
            .await
            .unwrap();
        let registry = engine.microgrids.lock();
        assert_eq!(registry[&61].def.grpc_port, DEFAULT_GRPC_PORT);
        assert_ne!(registry[&62].def.grpc_port, DEFAULT_GRPC_PORT);
    }

    async fn declare_inert(
        engine: &Engine,
        doc: TopologyDoc,
    ) -> Result<Vec<Declared>, TopologyError> {
        declare(&engine.structural().await, &MicrogridRuntimes::inert(), doc).await
    }

    fn outcomes(declared: &[Declared]) -> Vec<(u64, DeclareOutcome)> {
        declared.iter().map(|d| (d.id, d.outcome)).collect()
    }

    /// A declaration creates a microgrid with no source; the same spec again,
    /// even with its components in another order, leaves it as it is: the same
    /// components, no undo step.
    #[tokio::test]
    async fn a_declaration_creates_then_leaves_an_unchanged_spec_alone() {
        let engine = engine();
        let declared = declare_inert(&engine, doc(NORTH)).await.unwrap();
        assert_eq!(outcomes(&declared), vec![(61, DeclareOutcome::Created)]);
        {
            let registry = engine.microgrids.lock();
            assert_eq!(registry[&61].source, None);
            assert!(registry[&61].unsaved, "in memory only");
            assert_eq!(declared[0].grpc_port, registry[&61].def.grpc_port);
        }
        let battery = site(&engine, 61).get(12).unwrap();

        let declared = declare_inert(&engine, doc(NORTH)).await.unwrap();
        assert_eq!(outcomes(&declared), vec![(61, DeclareOutcome::Unchanged)]);
        let reordered = r#"{"id": 61, "components": [
            {"type": "battery", "id": 12, "initial_soc_pct": 50},
            {"type": "battery_inverter", "id": 11},
            {"type": "grid_connection_point", "id": 1000}
        ], "connections": [[11, 12], [1000, 11]]}"#;
        let declared = declare_inert(&engine, doc(reordered)).await.unwrap();
        assert_eq!(outcomes(&declared), vec![(61, DeclareOutcome::Unchanged)]);
        assert!(
            Arc::ptr_eq(&battery, &site(&engine, 61).get(12).unwrap()),
            "not rebuilt"
        );
        assert_eq!(engine.spec_undo_depths(61).undo, 0);
    }

    /// A field written out at its default value is no change; a component's new
    /// name or health is, and the rebuild applies it.
    #[tokio::test]
    async fn a_declaration_compares_specs_with_their_defaults_filled_in() {
        let engine = engine();
        declare_inert(&engine, doc(NORTH)).await.unwrap();
        let spelled_out = NORTH.replace(
            r#"{"type": "battery_inverter", "id": 11}"#,
            r#"{"type": "battery_inverter", "id": 11, "ramp_rate_w_per_s": 5000}"#,
        );
        let declared = declare_inert(&engine, doc(&spelled_out)).await.unwrap();
        assert_eq!(outcomes(&declared), vec![(61, DeclareOutcome::Unchanged)]);

        let renamed = NORTH.replace(r#""id": 12,"#, r#""id": 12, "name": "pack","#);
        let declared = declare_inert(&engine, doc(&renamed)).await.unwrap();
        assert_eq!(outcomes(&declared), vec![(61, DeclareOutcome::Rebuilt)]);
        assert_eq!(site(&engine, 61).display_name(12).as_deref(), Some("pack"));

        let failed = renamed.replace(r#""id": 12,"#, r#""id": 12, "health": "error","#);
        let declared = declare_inert(&engine, doc(&failed)).await.unwrap();
        assert_eq!(outcomes(&declared), vec![(61, DeclareOutcome::Rebuilt)]);
        assert_eq!(site(&engine, 61).runtime_of(12).health, Health::Error);
    }

    /// A changed spec is rebuilt with carry, and its undo stack and a
    /// file-backed microgrid's unsaved mark change as after an edit.
    #[tokio::test]
    async fn a_declaration_rebuilds_a_changed_spec_with_carry() {
        let engine = engine();
        create(
            &engine.structural().await,
            &MicrogridRuntimes::inert(),
            spec(NORTH),
            Some(PathBuf::from("/tmp/topology.json")),
        )
        .await
        .unwrap();
        let north = site(&engine, 61);
        let battery = north.get(12).unwrap();
        north.set_soc_pct(battery.as_ref(), 80.0).unwrap();
        let changed = NORTH
            .replace(
                r#"{"type": "grid_connection_point", "id": 1000}"#,
                r#"{"type": "grid_connection_point", "id": 1000},
                {"type": "meter", "id": 13}"#,
            )
            .replace("[[1000, 11]", "[[1000, 13], [1000, 11]");
        let declared = declare_inert(&engine, doc(&changed)).await.unwrap();
        assert_eq!(outcomes(&declared), vec![(61, DeclareOutcome::Rebuilt)]);
        assert_eq!(soc(&north, 12), 80.0, "the battery carried");
        assert!(north.get(13).is_some());
        assert_eq!(engine.spec_undo_depths(61).undo, 1);
        let registry = engine.microgrids.lock();
        assert!(registry[&61].unsaved, "differs from its file");
        assert_eq!(
            registry[&61].source.as_deref(),
            Some(Path::new("/tmp/topology.json"))
        );
    }

    /// The answer follows the document's order, though rebuilds run before
    /// creates.
    #[tokio::test]
    async fn a_declaration_answers_in_the_documents_order() {
        let engine = engine();
        declare_inert(&engine, doc(NORTH)).await.unwrap();
        let north2 = NORTH.replace(r#""initial_soc_pct": 50"#, r#""initial_soc_pct": 60"#);
        let declared = declare_inert(&engine, doc(&format!("{SOUTH}, {north2}")))
            .await
            .unwrap();
        assert_eq!(
            outcomes(&declared),
            vec![(62, DeclareOutcome::Created), (61, DeclareOutcome::Rebuilt)]
        );
    }

    /// A declaration may move component ids between the microgrids it lists.
    #[tokio::test]
    async fn a_declaration_moves_ids_between_its_microgrids() {
        let engine = engine();
        declare_inert(&engine, doc(&format!("{NORTH}, {SOUTH}")))
            .await
            .unwrap();
        let north2 = NORTH
            .replace(
                r#"{"type": "grid_connection_point", "id": 1000}"#,
                r#"{"type": "grid_connection_point", "id": 1000},
                {"type": "meter", "id": 1001}"#,
            )
            .replace("[[1000, 11]", "[[1000, 1001], [1000, 11]");
        let south2 = SOUTH.replace("1001", "1002");
        declare_inert(&engine, doc(&format!("{north2}, {south2}")))
            .await
            .unwrap();
        assert!(site(&engine, 61).get(1001).is_some());
        assert!(site(&engine, 62).get(1002).is_some());
    }

    /// An empty document, a microgrid without an id and an id listed twice are
    /// refused, and so is a bad spec; nothing changes.
    #[tokio::test]
    async fn a_declaration_refuses_a_bad_document_and_changes_nothing() {
        let engine = engine();
        declare_inert(&engine, doc(NORTH)).await.unwrap();
        let no_id = SOUTH.replace(r#""id": 62, "#, "");
        let bad = SOUTH.replace(
            r#""components""#,
            r#""connections": [[1001, 99]], "components""#,
        );
        for (doc, want) in [
            (doc(""), "no microgrids"),
            (doc(&no_id), "has no id"),
            (doc(&format!("{SOUTH}, {SOUTH}")), "twice"),
            (doc(&format!("{SOUTH}, {bad}")), "microgrid 62"),
        ] {
            let err = declare_inert(&engine, doc).await.unwrap_err();
            assert!(
                matches!(&err, TopologyError::Invalid(m) if m.contains(want)),
                "{want}: {err:?}"
            );
        }
        let bad_north = NORTH.replace("[[1000, 11]", "[[1000, 99], [1000, 11]");
        let err = declare_inert(&engine, doc(&format!("{SOUTH}, {bad_north}")))
            .await
            .unwrap_err();
        assert!(matches!(err, TopologyError::Invalid(_)), "{err:?}");
        let registry = engine.microgrids.lock();
        assert_eq!(registry.keys().copied().collect::<Vec<_>>(), vec![61]);
        assert_eq!(registry[&61].spec.components.len(), 3);
    }

    /// A component id or a port held by a microgrid the declaration does not
    /// list is refused with a message that names it and how to unload it: what
    /// a renamed microgrid runs into.
    #[tokio::test]
    async fn a_declaration_names_the_unlisted_holder_of_an_id_or_a_port() {
        let engine = engine();
        declare_inert(&engine, doc(NORTH)).await.unwrap();
        let renamed = NORTH.replace(r#""id": 61,"#, r#""id": 71,"#);
        let err = declare_inert(&engine, doc(&renamed)).await.unwrap_err();
        assert!(
            matches!(&err, TopologyError::Conflict(m)
                if m.contains("already in microgrid 61") && m.contains("DELETE /api/mg/61")),
            "{err:?}"
        );

        let engine2 = super::tests::engine();
        let pinned = with_port(NORTH, 61, 9100);
        declare_inert(&engine2, doc(&pinned)).await.unwrap();
        let renamed = pinned.replace(r#""id": 61,"#, r#""id": 71,"#);
        let err = declare_inert(&engine2, doc(&renamed)).await.unwrap_err();
        assert!(
            matches!(&err, TopologyError::Conflict(m)
                if m.contains("microgrid 61's") && m.contains("Give microgrid 71 another grpc_port")
                    && m.contains("DELETE /api/mg/61")),
            "{err:?}"
        );
    }

    /// A microgrid whose runtime failed to start (its port was taken) is
    /// started again by the next declaration, though its spec is unchanged.
    /// Real runtimes, on a wall clock.
    #[tokio::test]
    async fn a_declaration_restarts_a_runtime_that_failed_to_start() {
        use std::net::Ipv6Addr;

        use crate::runtime::RuntimeStatus;
        use crate::runtime::tests::{free_v6_port, opts};
        let port = free_v6_port();
        let holder = std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, port)).unwrap();
        let engine = Engine::new(std::env::temp_dir(), Metadata::default(), NowSource::wall());
        let runtimes = MicrogridRuntimes::new(engine.clone(), opts(false)).unwrap();
        let pinned = with_port(NORTH, 61, port);
        let status = |d: &[Declared]| d[0].runtime.as_ref().unwrap().status;
        let first = declare(&engine.structural().await, &runtimes, doc(&pinned))
            .await
            .unwrap();
        assert_eq!(status(&first), RuntimeStatus::Failed);
        drop(holder);
        // A child process another test starts can hold a copy of the holder's
        // socket for a moment after the drop; declare again until the port is
        // free.
        let mut again = Vec::new();
        for _ in 0..50 {
            again = declare(&engine.structural().await, &runtimes, doc(&pinned))
                .await
                .unwrap();
            if status(&again) == RuntimeStatus::Running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(outcomes(&again), vec![(61, DeclareOutcome::Unchanged)]);
        assert_eq!(status(&again), RuntimeStatus::Running);
        runtimes.stop(61).await;
    }

    #[tokio::test]
    async fn a_rebuild_may_move_ids_within_its_own_microgrid() {
        let engine = engine();
        let id = create_plant(&engine).await;
        let renumbered = PLANT
            .replace(r#""id": 5,"#, r#""id": 6,"#)
            .replace("[4, 5]", "[4, 6]");
        rebuild_inert(&engine, id, spec(&renumbered)).await.unwrap();
        let site = site(&engine, id);
        assert!(site.get(5).is_none() && site.get(6).is_some());
    }
}
