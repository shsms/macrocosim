//! Enterprise-scoped microgrid registry — what the binary knows
//! about *every* microgrid it hosts. One entry per microgrid; each
//! entry pairs the configured metadata (id, display name, gRPC
//! port, optional TSO label) with a clonable handle on that
//! microgrid's `MicrogridSite`.
//!
//! The registry lives behind a `parking_lot::Mutex`: the mutators
//! (`engine::topology`, under the structural lock) and the readers
//! (the UI's Microgrids landing page, the gRPC servers, the loopback
//! supervisors) hold it for short critical sections, never across an
//! `.await`.
//!
//! Pure data + small helpers — no async, no I/O. The HTTP routing and
//! gRPC server spawning sit one layer up.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;

use crate::sim::MicrogridSite;

/// Where the microgrid id allocator (`next_free_id`) starts, for a
/// spec created without an id.
pub const DEFAULT_MICROGRID_ID: u64 = 2200;
/// The name of a microgrid whose spec gives none.
pub const DEFAULT_MICROGRID_NAME: &str = "default";
/// Starting gRPC port for the auto-port allocator
/// (`next_free_port`). Multi-microgrid configs auto-allocate from
/// this base upward.
pub const DEFAULT_GRPC_PORT: u16 = 8800;

#[derive(Clone, Debug, Serialize)]
pub struct MicrogridDef {
    pub id: u64,
    pub name: String,
    pub grpc_port: u16,
    /// Optional informational TSO zone label (e.g. `"TN"`, `"AM"`,
    /// `"HZ"`, `"BW"`). No behavioural effect for v1; a chip on the
    /// Microgrids landing page surfaces it for operators that care.
    pub tso: Option<String>,
}

#[derive(Clone)]
pub struct MicrogridEntry {
    pub def: MicrogridDef,
    pub site: MicrogridSite,
    /// The file the microgrid came from, canonicalized: the `--topology`
    /// document. `None` when it was made in the UI or declared.
    pub source: Option<PathBuf>,
    /// Its live spec differs from its source: a `PUT` of its spec, an undo or a
    /// declaration that rebuilt it set it, and one with no source always has
    /// it.
    pub unsaved: bool,
    /// The spec the microgrid was last built from, with every id
    /// filled in.
    pub spec: crate::spec::MicrogridSpec,
}

/// The registered microgrid whose spec holds each component id.
pub fn component_owners(entries: &BTreeMap<u64, MicrogridEntry>) -> HashMap<u64, u64> {
    entries
        .iter()
        .flat_map(|(&mg, e)| {
            e.spec
                .components
                .iter()
                .filter_map(move |c| Some((c.id()?, mg)))
        })
        .collect()
}

pub type SharedMicrogrids = Arc<Mutex<BTreeMap<u64, MicrogridEntry>>>;

pub fn new_registry() -> SharedMicrogrids {
    Arc::new(Mutex::new(BTreeMap::new()))
}

/// JSON-serialisable snapshot used by `GET /api/microgrids`. Drops the
/// `MicrogridSite` handle (not serialisable) and folds in the site's component
/// counts so the UI's card grid can render them without a second round-trip.
#[derive(Clone, Debug, Serialize)]
pub struct MicrogridView {
    pub id: u64,
    pub name: String,
    pub grpc_port: u16,
    pub tso: Option<String>,
    /// Every registered component, hidden ones included.
    pub component_count: usize,
    /// How many of those are hidden.
    pub hidden_component_count: usize,
    /// Mirrors [`MicrogridEntry::source`], rendered for display
    /// (`PathBuf` has no stable JSON form across platforms).
    pub source: Option<String>,
    /// Mirrors [`MicrogridEntry::unsaved`].
    pub unsaved: bool,
}

impl From<&MicrogridEntry> for MicrogridView {
    fn from(e: &MicrogridEntry) -> Self {
        MicrogridView {
            id: e.def.id,
            name: e.def.name.clone(),
            grpc_port: e.def.grpc_port,
            tso: e.def.tso.clone(),
            component_count: e.site.component_count(),
            hidden_component_count: e.site.hidden_component_count(),
            source: e.source.as_ref().map(|p| p.display().to_string()),
            unsaved: e.unsaved,
        }
    }
}

/// Snapshot the registry into a list of `MicrogridView`s, sorted
/// ascending by id. Driver for `GET /api/microgrids`.
pub fn snapshot(registry: &SharedMicrogrids) -> Vec<MicrogridView> {
    registry.lock().values().map(MicrogridView::from).collect()
}

/// Why a microgrid cannot claim a gRPC port.
#[derive(Debug, PartialEq, Eq)]
pub enum PortClash {
    /// Another microgrid holds it.
    Microgrid(u64),
    /// It is the assets or dispatch server's port.
    Reserved,
}

impl std::fmt::Display for PortClash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Microgrid(other) => write!(f, "is already bound by microgrid {other}"),
            Self::Reserved => write!(f, "is reserved for the assets or dispatch server"),
        }
    }
}

impl std::error::Error for PortClash {}

/// Whether microgrid `id` may claim `port`: no other microgrid holds
/// it and it is not in `reserved`.
pub fn check_port(
    entries: &BTreeMap<u64, MicrogridEntry>,
    reserved: &[u16],
    id: u64,
    port: u16,
) -> Result<(), PortClash> {
    if let Some((other, _)) = entries
        .iter()
        .find(|(oid, e)| **oid != id && e.def.grpc_port == port)
    {
        return Err(PortClash::Microgrid(*other));
    }
    if reserved.contains(&port) {
        return Err(PortClash::Reserved);
    }
    Ok(())
}

/// Smallest port not claimed by a registered microgrid or listed in
/// `reserved`, starting at `DEFAULT_GRPC_PORT` and stepping by 10.
pub fn next_free_port(registry: &SharedMicrogrids, reserved: &[u16]) -> u16 {
    next_free_port_in(&registry.lock(), reserved)
}

/// Like [`next_free_port`] but operates on an already-locked map.
/// Lets the create-microgrid handler pick id + port + insert under
/// one critical section so two concurrent creates can't both pick the
/// same port. `reserved` holds the enterprise's assets and dispatch
/// ports.
pub fn next_free_port_in(entries: &BTreeMap<u64, MicrogridEntry>, reserved: &[u16]) -> u16 {
    let mut ports: Vec<u16> = entries
        .values()
        .map(|e| e.def.grpc_port)
        .chain(reserved.iter().copied())
        .collect();
    ports.sort_unstable();
    let mut candidate = DEFAULT_GRPC_PORT;
    for p in ports {
        if p == candidate {
            candidate = candidate.saturating_add(10);
        } else if p > candidate {
            break;
        }
    }
    candidate
}

/// Smallest microgrid id not currently registered, starting at
/// `DEFAULT_MICROGRID_ID`. Mirrors `next_free_port`'s shape.
pub fn next_free_id(registry: &SharedMicrogrids) -> u64 {
    next_free_id_in(&registry.lock(), &BTreeSet::new())
}

/// Like [`next_free_id`] but operates on an already-locked map, and
/// skips the ids in `taken` too (ids a document claims but has not
/// registered yet). Pairs with [`next_free_port_in`] so the
/// create-microgrid path can pick both + insert in one critical
/// section.
pub fn next_free_id_in(entries: &BTreeMap<u64, MicrogridEntry>, taken: &BTreeSet<u64>) -> u64 {
    let mut candidate = DEFAULT_MICROGRID_ID;
    while entries.contains_key(&candidate) || taken.contains(&candidate) {
        candidate = candidate.saturating_add(1);
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::MicrogridSite;

    fn entry(id: u64, port: u16) -> MicrogridEntry {
        MicrogridEntry {
            def: MicrogridDef {
                id,
                name: format!("mg-{id}"),
                grpc_port: port,
                tso: None,
            },
            site: MicrogridSite::new(),
            source: None,
            unsaved: false,
            spec: Default::default(),
        }
    }

    #[test]
    fn next_free_port_increments_by_10_skipping_gaps() {
        let reg = new_registry();
        // Empty registry -> default port.
        assert_eq!(next_free_port(&reg, &[]), DEFAULT_GRPC_PORT);
        // Add 8800; next is 8810.
        reg.lock().insert(2200, entry(2200, 8800));
        assert_eq!(next_free_port(&reg, &[]), 8810);
        // Add 8810 too; next is 8820.
        reg.lock().insert(2201, entry(2201, 8810));
        assert_eq!(next_free_port(&reg, &[]), 8820);
        // Add 8830 (gap); next slot is still 8820.
        reg.lock().insert(2202, entry(2202, 8830));
        assert_eq!(next_free_port(&reg, &[]), 8820);
    }

    /// Reserved ports (the assets and dispatch sockets) are never
    /// handed out: the walk steps over them like a taken port.
    #[test]
    fn next_free_port_skips_reserved_ports() {
        let reg = new_registry();
        assert_eq!(next_free_port(&reg, &[DEFAULT_GRPC_PORT]), 8810);
        assert_eq!(next_free_port(&reg, &[8810]), DEFAULT_GRPC_PORT);
        assert_eq!(next_free_port(&reg, &[DEFAULT_GRPC_PORT, 8810]), 8820);
    }

    #[test]
    fn next_free_id_starts_at_default_and_increments() {
        let reg = new_registry();
        assert_eq!(next_free_id(&reg), DEFAULT_MICROGRID_ID);
        reg.lock()
            .insert(DEFAULT_MICROGRID_ID, entry(DEFAULT_MICROGRID_ID, 8800));
        assert_eq!(next_free_id(&reg), DEFAULT_MICROGRID_ID + 1);
    }

    #[test]
    fn snapshot_is_ascending_by_id() {
        let reg = new_registry();
        for id in [2202u64, 2200, 2201] {
            reg.lock()
                .insert(id, entry(id, 8800 + (id as u16 - 2200) * 10));
        }
        let view = snapshot(&reg);
        assert_eq!(
            view.iter().map(|v| v.id).collect::<Vec<_>>(),
            vec![2200, 2201, 2202]
        );
    }
}
