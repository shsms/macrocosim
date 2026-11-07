//! Every structural change to a microgrid with a stored spec: create
//! one, rebuild one from a new spec, carrying the state of each
//! component the change leaves as it was, and unload one. Every
//! caller holds the engine's create lock, the one structural lock, so
//! the checks here hold until the change is in place.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use super::Engine;
use crate::runtime::{MicrogridRuntimes, RuntimeView};
use crate::sim::MicrogridSite;
use crate::sim::microgrid_site::CarryPlan;
use crate::sim::microgrids::{
    DEFAULT_MICROGRID_NAME, MicrogridDef, MicrogridEntry, check_port, component_owner,
    next_free_id_in, next_free_port_in,
};
use crate::spec::{Defaults, MicrogridSpec, build_topology};

/// Why a structural change was refused. Nothing changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopologyError {
    /// The spec is invalid (HTTP 422).
    Invalid(String),
    /// An id or a port belongs to something else, or the microgrid was
    /// loaded from Lisp (HTTP 409).
    Conflict(String),
    /// No microgrid has the id (HTTP 404).
    NotRegistered(u64),
}

impl fmt::Display for TopologyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(e) | Self::Conflict(e) => f.write_str(e),
            Self::NotRegistered(id) => write!(f, "microgrid {id} not registered"),
        }
    }
}

impl std::error::Error for TopologyError {}

/// The refusal for a change that names a microgrid loaded from Lisp,
/// which has no stored spec.
pub fn loaded_from_lisp(id: u64) -> TopologyError {
    TopologyError::Conflict(format!("microgrid {id} was loaded from Lisp"))
}

/// A microgrid [`create`] made.
#[derive(Clone, Debug)]
pub struct Created {
    pub id: u64,
    pub grpc_port: u16,
    /// Its runtime; `None` when none was started.
    pub runtime: Option<RuntimeView>,
}

/// Give `spec` the lowest free microgrid id when it has none.
/// Component ids are never filled in: a component without one is
/// refused by the build.
pub fn assign_microgrid_id(engine: &Engine, spec: &mut MicrogridSpec) {
    if spec.id.is_none() {
        spec.id = Some(next_free_id_in(&engine.microgrids.lock()));
    }
}

/// Refuse a component id that appears in two of `specs`, or that the
/// registry gives to a microgrid not in `replacing`.
pub fn check_ids(
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
    let registry = engine.microgrids.lock();
    for (&id, &mg) in &seen {
        if let Some(owner) = component_owner(&registry, id)
            && !replacing.contains(&owner)
        {
            return Err(TopologyError::Conflict(format!(
                "component id {id} is already in microgrid {owner}{}",
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
    engine: &Engine,
    runtimes: &MicrogridRuntimes,
    mut spec: MicrogridSpec,
    source: Option<PathBuf>,
) -> Result<Created, TopologyError> {
    assign_microgrid_id(engine, &mut spec);
    let id = spec.id.expect("assigned above");
    check_ids(engine, &[&spec], &[])?;
    let built =
        build_topology(&spec, &Defaults::default()).map_err(|e| invalid(id, e.message()))?;
    // The site is built and filled before the registry lock is
    // taken: installing components takes the gateway lock, which is
    // never taken under the registry's.
    let site = MicrogridSite::with_id_allocator(engine.id_allocator());
    engine.attach_site(&site);
    site.swap_topology(built, &CarryPlan::default());
    let reserved = engine.reserved_ports();
    {
        let mut registry = engine.microgrids.lock();
        if registry.contains_key(&id) {
            return Err(TopologyError::Conflict(format!(
                "microgrid {id} is already registered"
            )));
        }
        let grpc_port = match spec.grpc_port {
            Some(0) => return Err(invalid(id, "grpc_port must be 1..=65535")),
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
                site,
                unsaved: source.is_none(),
                source,
                managed: false,
                spec: Some(spec),
            },
        );
    }
    engine.announce_registered(id);
    if let Err(e) = runtimes.start(id).await {
        log::error!("Microgrid #{id}: {e}");
    }
    let grpc_port = engine.microgrids.lock()[&id].def.grpc_port;
    Ok(Created {
        id,
        grpc_port,
        runtime: runtimes.status(id),
    })
}

/// Rebuild microgrid `id` from `spec`, carrying the state of every
/// component whose defaults-filled spec is unchanged under the same
/// id. Returns the spec it replaced. The site, its physics task and
/// its servers keep running, except that a changed `grpc_port` moves
/// the gRPC server to the new port; a failed bind there marks the
/// runtime failed and does not fail the rebuild.
pub async fn rebuild(
    engine: &Engine,
    runtimes: &MicrogridRuntimes,
    id: u64,
    mut spec: MicrogridSpec,
) -> Result<MicrogridSpec, TopologyError> {
    let (site, old, port) = {
        let registry = engine.microgrids.lock();
        let entry = registry.get(&id).ok_or(TopologyError::NotRegistered(id))?;
        let old = entry.spec.clone().ok_or_else(|| loaded_from_lisp(id))?;
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
        Some(0) => return Err(invalid(id, "grpc_port must be 1..=65535")),
        Some(p) if p != port => {
            let reserved = engine.reserved_ports();
            check_port(&engine.microgrids.lock(), &reserved, id, p)
                .map_err(|c| TopologyError::Conflict(format!("gRPC port {p} {c}")))?;
            Some(p)
        }
        Some(_) => None,
    };
    check_ids(engine, &[&spec], &[id])?;
    let defaults = Defaults::default();
    let built = build_topology(&spec, &defaults).map_err(|e| invalid(id, e.message()))?;
    let mut carry = CarryPlan::default();
    for c in &old.components {
        let Some(cid) = c.id() else { continue };
        if built.filled.get(&cid) == Some(&defaults.fill_component(c)) {
            let state = site.get(cid).and_then(|live| live.physics_state());
            carry.matching.insert(cid, state);
        }
    }
    site.swap_topology(built, &carry);
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
        entry.spec = Some(spec);
    }
    if let Some(p) = moved
        && let Err(e) = runtimes.restart_grpc(id, p).await
    {
        log::error!("Microgrid #{id}: {e}");
    }
    Ok(old)
}

/// Unload microgrid `id`: take it out of the registry, which frees
/// its id and its component ids, then stop its runtime (streams, gRPC
/// servers, physics, sampler, loopback), end a running scenario
/// journal and close its CSV sinks, delete its dispatches (each with
/// a `Deleted` event), drop its undo stacks and announce it on
/// `microgrid_unloaded`. A microgrid loaded from Lisp is refused.
///
/// The entry goes first, so a runtime start still queued for `id`
/// finds nothing to start instead of restarting what is being
/// stopped.
pub async fn unload(
    engine: &Engine,
    runtimes: &MicrogridRuntimes,
    id: u64,
) -> Result<(), TopologyError> {
    let entry = {
        let mut registry = engine.microgrids.lock();
        let entry = registry.get(&id).ok_or(TopologyError::NotRegistered(id))?;
        if entry.spec.is_none() {
            return Err(loaded_from_lisp(id));
        }
        registry.remove(&id).expect("checked above")
    };
    runtimes.stop(id).await;
    let site = entry.site;
    if site.scenario_is_running() {
        site.scenario_stop(site.now(), None);
    }
    site.scenario_close_csv();
    engine.dispatches.remove_mg(id);
    engine.spec_undo.lock().remove(&id);
    let _ = engine.microgrid_unloaded.send(id);
    log::info!("Microgrid #{id} unloaded");
    Ok(())
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
    use std::sync::Arc;

    use super::*;
    use crate::engine::Metadata;
    use crate::sim::runtime::Health;
    use crate::sim::sim_clock::{NowSource, headless_base};
    use crate::spec::TopologyDoc;

    fn engine() -> Engine {
        let clock = Arc::new(tulisp_async::ManualClock::new());
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
        create(engine, &MicrogridRuntimes::inert(), spec(PLANT), None)
            .await
            .unwrap()
            .id
    }

    async fn rebuild_inert(
        engine: &Engine,
        id: u64,
        spec: MicrogridSpec,
    ) -> Result<MicrogridSpec, TopologyError> {
        rebuild(engine, &MicrogridRuntimes::inert(), id, spec).await
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
        let created = create(&engine, &MicrogridRuntimes::inert(), spec(PLANT), None)
            .await
            .unwrap();
        assert_eq!(created.id, crate::sim::microgrids::DEFAULT_MICROGRID_ID);
        assert_eq!(created.grpc_port, crate::sim::microgrids::DEFAULT_GRPC_PORT);
        assert!(created.runtime.is_none(), "inert runtimes start nothing");
        let registry = engine.microgrids.lock();
        let entry = &registry[&created.id];
        let stored = entry.spec.as_ref().unwrap();
        assert_eq!(stored.id, Some(created.id));
        assert_eq!(stored.grpc_port, Some(created.grpc_port));
        assert_eq!(entry.def.name, "plant");
        assert!(entry.unsaved, "a microgrid made in the UI is unsaved");
        assert!(!entry.managed);
        assert_eq!(entry.site.component_count(), 5);
        assert_eq!(entry.site.runtime_of(5).health, Health::Standby);
    }

    #[tokio::test]
    async fn a_file_backed_microgrid_starts_saved() {
        let engine = engine();
        let source = PathBuf::from("/tmp/topology.json");
        let created = create(
            &engine,
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
            &engine,
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
            &engine,
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
        let stored = registry[&id].spec.as_ref().unwrap();
        assert_eq!(stored.grpc_port, Some(registry[&id].def.grpc_port));
        assert_eq!(stored.components, spec(&changed).components);
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
            &engine,
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
        assert_eq!(registry[&id].spec.as_ref().unwrap().grpc_port, Some(9999));
    }

    #[tokio::test]
    async fn a_rebuild_refuses_another_mg_and_lisp() {
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
        engine.microgrids.lock().get_mut(&id).unwrap().spec = None;
        assert_eq!(
            rebuild_inert(&engine, id, spec(PLANT)).await,
            Err(loaded_from_lisp(id))
        );
    }

    /// An unload forgets the microgrid and everything kept for it,
    /// and frees its ids for the next microgrid.
    #[tokio::test]
    async fn an_unload_frees_the_ids_and_drops_what_was_kept() {
        let engine = engine();
        let id = create_plant(&engine).await;
        let site = site(&engine, id);
        site.scenario_start("run".into(), site.now());
        let dir = tempfile::TempDir::with_prefix("mc-unload-").unwrap();
        assert!(site.scenario_open_csv(dir.path()).unwrap() > 0);
        let dispatches = engine.dispatches();
        let data = crate::proto::dispatch::DispatchData {
            r#type: "x".into(),
            ..Default::default()
        };
        let made = dispatches.create(id, data, true).unwrap();
        engine.record_spec_undo(id, spec(PLANT));
        assert_eq!(engine.spec_undo_depths(id).undo, 1);
        let mut changes = dispatches.subscribe();
        let mut unregistered = engine.subscribe_microgrid_unloaded();

        unload(&engine, &MicrogridRuntimes::inert(), id)
            .await
            .unwrap();

        assert!(!engine.microgrids.lock().contains_key(&id));
        assert!(!site.scenario_is_running(), "the journal was stopped");
        assert_eq!(site.scenario_close_csv(), 0, "the sinks were closed");
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
    async fn an_unload_refuses_lisp_and_an_unknown_id() {
        let engine = engine();
        let id = create_plant(&engine).await;
        let inert = MicrogridRuntimes::inert();
        assert_eq!(
            unload(&engine, &inert, 77).await,
            Err(TopologyError::NotRegistered(77))
        );
        engine.microgrids.lock().get_mut(&id).unwrap().spec = None;
        assert_eq!(unload(&engine, &inert, id).await, Err(loaded_from_lisp(id)));
        assert!(engine.microgrids.lock().contains_key(&id));
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
