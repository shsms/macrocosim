//! The simulator's shared state without an interpreter: the
//! microgrid registry, the dispatch store, the enterprise settings,
//! the grid frequency and the clock. Every server and HTTP handler
//! reads the simulation through an [`Engine`]; the Lisp `Config`
//! holds one and builds its interpreter around it.

mod metadata;
pub mod topology;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use parking_lot::{Mutex, RwLock};
use tokio::sync::broadcast;

pub use metadata::Metadata;
pub(crate) use metadata::reserved_ports_of;

use crate::sim::MicrogridSite;
use crate::sim::clock::SharedClock;
use crate::sim::dispatch::SharedDispatchStore;
use crate::sim::frequency::SharedFrequency;
use crate::sim::microgrids::{PortPins, SharedMicrogrids};
use crate::sim::sim_clock::NowSource;
use crate::spec::MicrogridSpec;
use crate::undo::{UndoDepths, UndoHistory};

/// The simulator state every server and handler shares. Cheap to
/// clone: each field is a handle on state behind an `Arc`.
#[derive(Clone)]
pub struct Engine {
    /// Anchor for everything persistent — `enterprise.lisp`,
    /// `snapshots/`, managed microgrid files — and for the
    /// relative-path resolution of `(load …)` / `(file-exists-p …)`.
    pub(crate) state_dir: PathBuf,
    /// Enterprise-scoped microgrid registry — what
    /// `(make-microgrid …)` writes to and what the Microgrids UI
    /// mode + /api/microgrids read from. See
    /// `crate::sim::microgrids` for the data model.
    pub(crate) microgrids: SharedMicrogrids,
    /// Enterprise-wide dispatch store — the single
    /// `MicrogridDispatchService` gRPC server writes here (Create /
    /// Update / Delete from the dispatch CLI), and the per-microgrid
    /// Dispatches UI view + `/api/mg/{id}/dispatches` read from it.
    /// Keyed by `microgrid_id` internally; survives a config reload
    /// (it isn't owned by any `MicrogridSite`). See
    /// `crate::sim::dispatch` for the data model.
    pub(crate) dispatches: SharedDispatchStore,
    pub(crate) metadata: Arc<RwLock<Metadata>>,
    /// Configured display timezone. UI's TZ toggle reads the IANA
    /// name from /api/clock and formats timestamps client-side via
    /// `Intl.DateTimeFormat(..., { timeZone })`. Mutated by
    /// `(set-timezone "…")`; default Europe/Berlin.
    pub(crate) clock: SharedClock,
    /// Enterprise-wide grid frequency: one OU process drives every
    /// site, so they all read the same frequency (one AC grid → one
    /// frequency).
    pub(crate) grid_frequency: SharedFrequency,
    /// Source of "now" for scenario time + headless physics stepping.
    /// Wall-clock in a live engine; a hand-advanced clock in a
    /// headless one. See [`crate::sim::sim_clock`].
    pub(crate) now: NowSource,
    /// Process-wide component-id allocator shared by every
    /// `MicrogridSite` Lisp builds, so auto-allocated component ids
    /// stay unique across microgrids.
    pub(crate) id_allocator: Arc<AtomicU64>,
    /// The check `make-microgrid` asks before adopting a changed
    /// `:grpc-port` on a reload. See
    /// [`crate::sim::microgrids::PortPins`].
    pub(crate) port_pins: PortPins,
    /// The one structural lock. Serializes the routes that claim a
    /// microgrid id and a port, then write and load a file:
    /// `POST /api/microgrids`, the import (held from its collision
    /// scan to the eval that registers its components),
    /// `/api/load-as`, and a snapshot loaded as a new microgrid. The load cannot run under the registry
    /// lock (it evaluates lisp), so one claim at a time is what keeps
    /// each route's validation authoritative. A plain `(load …)` or
    /// an eval of `(make-microgrid …)` takes its port as it registers
    /// and does not wait here. A tokio mutex, so a route waits for it
    /// without blocking the runtime and its owned guard can move into
    /// the blocking task.
    pub(crate) create_lock: Arc<tokio::sync::Mutex<()>>,
    /// Enterprise-wide notification fired when a microgrid lands in
    /// `microgrids`. The WS event pump subscribes so it can spawn a
    /// forwarder for the new site's event bus on the fly, and the
    /// runtimes so they start its gRPC server.
    pub(crate) microgrid_registered: Arc<broadcast::Sender<u64>>,
    /// Per-microgrid undo stacks of the specs a `PUT` replaced, for
    /// the microgrids with a stored spec. In memory only.
    pub(crate) spec_undo: Arc<Mutex<HashMap<u64, UndoHistory<MicrogridSpec>>>>,
}

impl Engine {
    /// An engine with an empty registry. A live engine (`now` reads
    /// the wall clock) spawns the grid frequency driver, so it must
    /// be called inside a tokio runtime; a headless one leaves the
    /// frequency to whoever steps it.
    pub fn new(state_dir: PathBuf, metadata: Metadata, now: NowSource) -> Self {
        let grid_frequency = crate::sim::frequency::new_shared();
        if now.is_wall() {
            crate::sim::frequency::spawn_driver(grid_frequency.clone());
        }
        Self {
            state_dir,
            microgrids: crate::sim::microgrids::new_registry(),
            dispatches: crate::sim::dispatch::new_store(),
            metadata: Arc::new(RwLock::new(metadata)),
            clock: crate::sim::clock::new_clock(),
            grid_frequency,
            now,
            id_allocator: Arc::new(AtomicU64::new(crate::sim::component::FIRST_AUTO_ID)),
            port_pins: crate::sim::microgrids::new_port_pins(),
            create_lock: Arc::new(tokio::sync::Mutex::new(())),
            // Capacity = 1024 to absorb a mass-create burst (e.g. a
            // script POST'ing /api/microgrids a few hundred times
            // back-to-back) without lagging the WS event pump's
            // receiver. Even on Lagged the pump re-snapshots the
            // registry and back-fills forwarders, so capacity tuning
            // is belt-and-suspenders — but a fresh subscriber
            // spinning up mid-burst still benefits from the extra
            // slack.
            microgrid_registered: Arc::new(broadcast::channel(1024).0),
            spec_undo: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Put `site` on the engine's grid frequency and clock: every
    /// site reads the same frequency, and wall time live or the sim
    /// clock headless.
    pub(crate) fn attach_site(&self, site: &MicrogridSite) {
        site.set_grid_frequency(self.grid_frequency.clone());
        site.set_now_source(self.now.clone());
    }

    /// Tell subscribers that microgrid `id` registered. A reused
    /// registration is announced too, so a failed runtime gets
    /// another start; subscribers ignore an id they already serve.
    pub(crate) fn announce_registered(&self, id: u64) {
        let _ = self.microgrid_registered.send(id);
    }

    /// The persistence anchor — see the field doc on `state_dir`.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Shared enterprise microgrid registry.
    pub fn microgrids(&self) -> SharedMicrogrids {
        self.microgrids.clone()
    }

    /// Shared enterprise dispatch store — the
    /// `MicrogridDispatchService` gRPC server mutates it and the UI's
    /// per-microgrid Dispatches view reads it. See
    /// [`crate::sim::dispatch`].
    pub fn dispatches(&self) -> SharedDispatchStore {
        self.dispatches.clone()
    }

    pub fn metadata(&self) -> Metadata {
        self.metadata.read().clone()
    }

    /// Change the enterprise settings in place: what the binary's
    /// enterprise flags do after `enterprise.lisp` loaded.
    pub fn update_metadata(&self, f: impl FnOnce(&mut Metadata)) {
        f(&mut self.metadata.write());
    }

    pub fn assets_socket_addr(&self) -> String {
        self.metadata.read().assets_socket_addr.clone()
    }

    pub fn dispatch_socket_addr(&self) -> String {
        self.metadata.read().dispatch_socket_addr.clone()
    }

    /// The ports of the enterprise's assets and dispatch sockets; an
    /// address that does not parse contributes nothing.
    pub fn reserved_ports(&self) -> Vec<u16> {
        reserved_ports_of(&self.metadata.read())
    }

    /// Configured display-zone clock handle.
    pub fn clock_handle(&self) -> SharedClock {
        self.clock.clone()
    }

    /// IANA name of the configured display zone (default
    /// "Europe/Berlin"; redirected by `(set-timezone "…")`).
    pub fn tz_name(&self) -> &'static str {
        self.clock.read().tz_name()
    }

    /// The engine's source of "now".
    pub fn now_source(&self) -> NowSource {
        self.now.clone()
    }

    /// A headless engine runs on a hand-advanced clock and is stepped
    /// by its caller; nothing may start wall-clock physics on it.
    pub fn is_headless(&self) -> bool {
        !self.now.is_wall()
    }

    /// Shared process-wide id allocator backing every microgrid Lisp
    /// builds.
    pub fn id_allocator(&self) -> Arc<AtomicU64> {
        self.id_allocator.clone()
    }

    pub fn port_pins(&self) -> PortPins {
        self.port_pins.clone()
    }

    /// The one structural lock — see the field doc. Every caller of
    /// [`topology`] holds it.
    pub fn create_lock(&self) -> Arc<tokio::sync::Mutex<()>> {
        self.create_lock.clone()
    }

    /// Stack `replaced`, the spec a rebuild of microgrid `id` just
    /// replaced, as its newest undo step.
    pub(crate) fn record_spec_undo(&self, id: u64, replaced: MicrogridSpec) {
        self.spec_undo
            .lock()
            .entry(id)
            .or_default()
            .record(replaced);
    }

    /// How deep microgrid `id`'s spec undo stacks are.
    pub fn spec_undo_depths(&self, id: u64) -> UndoDepths {
        self.spec_undo
            .lock()
            .get(&id)
            .map_or(UndoDepths { undo: 0, redo: 0 }, UndoHistory::depths)
    }

    /// Subscribe to `microgrid_registered` notifications.
    pub fn subscribe_microgrid_registered(&self) -> broadcast::Receiver<u64> {
        self.microgrid_registered.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An attached site reads the engine's clock and frequency.
    #[test]
    fn attached_sites_share_the_engine_clock_and_frequency() {
        let clock = Arc::new(tulisp_async::ManualClock::new());
        let base = crate::sim::sim_clock::headless_base();
        let engine = Engine::new(
            PathBuf::from("."),
            Metadata::default(),
            NowSource::sim(base, clock.clone()),
        );
        assert!(engine.is_headless());
        let site = MicrogridSite::new();
        engine.attach_site(&site);
        clock.advance(std::time::Duration::from_secs(5));
        assert_eq!(site.now(), base + chrono::Duration::seconds(5));
        engine.grid_frequency.write().current_hz = 49.5;
        assert_eq!(site.grid_state().frequency_hz, 49.5);
    }
}
