//! The simulation registry, scheduler, and shared environment.
//!
//! `MicrogridSite` owns every component, the parent → child topology, and the
//! external grid state (per-phase voltage, frequency) that components
//! query when computing AC quantities.
//!
//! On every `physics_tick_ms` interval, `tick_once` walks the
//! components in registration order (children first because Lisp
//! evaluates `:successors` before the surrounding `make-*` call) and
//! invokes `SimulatedComponent::tick` on each.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use tokio::sync::broadcast;

use crate::sim::EnergyAccum;
use crate::sim::component::OperationalMode;
use crate::sim::component::{Category, ComponentHandle, FIRST_AUTO_ID, SimulatedComponent};
use crate::sim::events::{EVENT_BUS_CAPACITY, SiteEvent};
use crate::sim::history::ComponentHistory;
use crate::sim::runtime::{CommandMode, ComponentRuntime, Health, TelemetryMode};
use crate::sim::scenario::ScenarioJournal;
use crate::sim::scenario_csv::CsvSinks;
use crate::sim::setpoints::{SetpointEvent, SetpointLog, SetpointOutcome};
use crate::timeout_tracker::TimeoutTracker;

mod history;
mod scenarios;

pub(crate) use scenarios::{ScenarioReport, ScenarioSummary};

/// Hard cap on per-component-per-metric ring buffer length. At the
/// fixed 1 Hz history sampling cadence (see `spawn_history_sampler`)
/// this works out to a 10-minute window per series — plenty for the
/// "what was my control app doing recently" use case.
const HISTORY_CAPACITY: usize = 600;

/// Cap on per-component setpoint-log length. Setpoint requests
/// arrive at the gRPC server's pace; a busy control app might land
/// 10/sec on one component. 1000 entries ≈ 100 s of dense traffic
/// or several minutes of typical use; older events evict.
const SETPOINT_LOG_CAPACITY: usize = 1000;

/// External AC environment shared by all AC components. Mirrors
/// microsim's `voltage-per-phase` / `ac-frequency` globals.
#[derive(Debug, Clone)]
pub struct GridState {
    pub voltage_per_phase: (f32, f32, f32),
    pub frequency_hz: f32,
}

impl Default for GridState {
    fn default() -> Self {
        Self {
            voltage_per_phase: (230.0, 230.0, 230.0),
            frequency_hz: 50.0,
        }
    }
}

#[derive(Clone)]
pub struct MicrogridSite {
    inner: Arc<MicrogridSiteInner>,
}

struct MicrogridSiteInner {
    /// Registration-order component list, snapshotted by every tick /
    /// sampler pass. The inner Arc makes a snapshot one refcount bump
    /// instead of a Vec clone; structural mutations (rare) copy-on-
    /// write via `Arc::make_mut`.
    components: RwLock<Arc<Vec<Arc<dyn SimulatedComponent>>>>,
    by_id: RwLock<HashMap<u64, Arc<dyn SimulatedComponent>>>,
    connections: RwLock<Vec<(u64, u64)>>,
    grid_state: RwLock<GridState>,
    physics_tick_ms: AtomicU64,
    /// *Process-wide* component-id allocator, cloned across every
    /// `MicrogridSite` in the enterprise so component ids stay
    /// globally unique across microgrids — matching the platform,
    /// where ids are enterprise-scoped. Two sites in the same registry
    /// share the same `Arc<AtomicU64>`; calling `next_id` on
    /// either advances the same counter.
    ///
    /// Single-site / legacy paths construct a fresh allocator
    /// per `MicrogridSite::new()` so they keep the prior
    /// per-site numbering behaviour without coordination — only
    /// the multi-microgrid path (`(make-microgrid …)` via the
    /// registry) wires sites to a shared allocator via
    /// `MicrogridSite::with_id_allocator`.
    next_id: Arc<AtomicU64>,
    /// Bumped by every STRUCTURAL mutation — register, connect,
    /// disconnect, remove, rename — and read by `Config::eval`, which
    /// regenerates the microgrid's managed file only when this
    /// counter moved; a transient poke like `set-meter-power` leaves
    /// it untouched and writes nothing, so it can't resurrect as
    /// config on the next reload. Distinct from `version`, which
    /// bumps on every eval as the UI's refetch signal.
    structural_version: AtomicU64,
    /// Bumped by `cancel_all_streams()`. Streaming tasks in server.rs
    /// compare against the value they captured at start and break when
    /// it has changed. Models a server-initiated graceful cancel of
    /// every active stream.
    stream_cancel_epoch: AtomicU64,
    /// Server-side artificial lag added to every sample's timestamp.
    /// When > 0, the protobuf message's timestamps are shifted into
    /// the past by this many milliseconds — modelling a server that
    /// delivers samples with stale timestamps.
    sample_lag_ms: AtomicU64,
    /// Per-component runtime mode flags (health, telemetry mode,
    /// command mode). Defaulted on register, mutated via the
    /// `set-component-*` Lisp defuns or directly from server.rs.
    runtime: RwLock<HashMap<u64, ComponentRuntime>>,
    /// Config-level operational mode per component (declared
    /// capability). Not a runtime knob: the runtime fault modes
    /// depend on it, never the other way around.
    operational_modes: RwLock<HashMap<u64, OperationalMode>>,
    /// User-facing name overrides set via `(rename-component …)`.
    /// Reads go through `display_name`; the component's intrinsic
    /// `SimulatedComponent::name()` stays as the auto-derived default.
    name_overrides: RwLock<HashMap<u64, String>>,
    /// Per-component telemetry history rings, populated by the
    /// `spawn_history_sampler` task. Read by the UI's `/api/history`
    /// endpoint. Cleared on `reset()` so a hot-reload starts charts
    /// fresh.
    histories: RwLock<HashMap<u64, ComponentHistory>>,
    /// Per-component cumulative-energy accumulators, advanced on every
    /// physics `tick_once` from the power the component settled on (so
    /// `EnergyWh` accrues in both the live server and the headless
    /// stepped runner, unlike the 1 Hz history sampler). A component in
    /// `Health::Error` has its cursor dropped so the faulted span isn't
    /// integrated; a healthy-but-telemetry-silent component keeps
    /// accruing. Only AC-active-power components get an entry — batteries
    /// (dc_power only) stay absent, matching the sparse `EnergyWh` metric.
    /// Cleared on `reset()`.
    component_energy: RwLock<HashMap<u64, EnergyAccum>>,
    /// Per-component log of incoming setpoint requests + outcome.
    /// Populated by the gRPC server handlers for SetActivePower /
    /// SetReactivePower / AugmentBounds; read by /api/setpoints for
    /// the UI's control inspector.
    setpoint_logs: RwLock<HashMap<u64, SetpointLog>>,
    /// Monotonic version counter; bumped via `bump_version` on every
    /// accepted /api/eval (and future programmatic mutations) so UI
    /// tabs know to refetch /api/topology.
    version: AtomicU64,
    /// Run generation — bumped by `reset()`, which a config hot-reload
    /// runs before rebuilding the site. Readers holding cumulative state
    /// derived from this site (the UI's aggregate energy totals) compare
    /// it to tell a fresh run (clear) from a topology mutation (keep).
    run_generation: AtomicU64,
    /// Broadcast bus for live UI subscribers. Senders are cheap to
    /// clone; receivers are obtained via `subscribe_events`.
    events: broadcast::Sender<SiteEvent>,
    /// Per-component setpoint expiry deadlines. Both the gRPC
    /// `SetElectricalComponentPower` handler and the `(set-power …)`
    /// Lisp defun add to this; a single tokio task in
    /// `Config::start_timeout_loop` polls for expirations and calls
    /// `reset_setpoint` on each. Living on MicrogridSite means the loop runs
    /// once per process regardless of which call sites schedule.
    timeout_tracker: TimeoutTracker,
    /// Scenario lifecycle + event journal. Scoped to the MicrogridSite
    /// rather than the Config because long-running scenarios
    /// outlive an `eval_file` call and the gRPC server reads from
    /// it via `MicrogridSite::scenario_*`.
    scenario: RwLock<ScenarioJournal>,
    /// Per-component CSV sinks active during the scenario.
    /// Populated by `(scenario-record-csv DIR)`; drained on
    /// `(scenario-stop-csv)` or implicitly by `scenario-stop`.
    /// Empty by default — recording is opt-in.
    scenario_csv: RwLock<CsvSinks>,
    /// Received-setpoint CSV sinks — one per envelope-bearing
    /// component, written event-driven from `log_setpoint`. Same
    /// open/close lifecycle as `scenario_csv`.
    scenario_setpoints_csv: RwLock<CsvSinks>,
    /// Effective-active-bounds CSV sinks — one per envelope-bearing
    /// component, sampled by `record_history_snapshot` at the same
    /// 1 Hz pass as telemetry. Same lifecycle as `scenario_csv`.
    scenario_bounds_csv: RwLock<CsvSinks>,
    /// Effective-reactive-bounds CSV sinks — the Q twin of
    /// `scenario_bounds_csv`, one per component with a Q axis
    /// (`reactive_bounds().is_some()`), sampled at the same pass.
    /// Same lifecycle as `scenario_csv`.
    scenario_reactive_bounds_csv: RwLock<CsvSinks>,
    /// Directory the active (or most recent) CSV recording wrote to,
    /// so the UI can list + offer the files for download. Set by
    /// `scenario_open_csv`; survives `scenario-stop-csv` so links work
    /// after a run ends.
    scenario_csv_dir: RwLock<Option<std::path::PathBuf>>,
    /// Optional handle on the grid frequency state. Wired
    /// in by `Config::new` so every MicrogridSite in the registry
    /// reads the same OU-driven frequency value (one AC grid →
    /// one frequency, by physics). Bootstrap MicrogridSites built
    /// outside that path (tags pass, unit tests) leave it `None`
    /// and fall back to the per-mg `grid_state.frequency_hz`.
    grid_frequency: RwLock<Option<crate::sim::frequency::SharedFrequency>>,
}

impl MicrogridSite {
    pub fn new() -> Self {
        Self::with_id_allocator(Arc::new(AtomicU64::new(FIRST_AUTO_ID)))
    }

    /// Build a `MicrogridSite` that shares the supplied id
    /// allocator with whichever other sites already hold a clone.
    /// `(make-microgrid …)` uses this so every site in the
    /// registry draws auto-ids from one process-wide counter and
    /// the enterprise-wide id-uniqueness invariant holds without
    /// coordination on the lisp side.
    pub fn with_id_allocator(next_id: Arc<AtomicU64>) -> Self {
        Self {
            inner: Arc::new(MicrogridSiteInner {
                components: RwLock::new(Arc::new(Vec::new())),
                by_id: RwLock::new(HashMap::new()),
                connections: RwLock::new(Vec::new()),
                grid_state: RwLock::new(GridState::default()),
                physics_tick_ms: AtomicU64::new(100),
                next_id,
                runtime: RwLock::new(HashMap::new()),
                operational_modes: RwLock::new(HashMap::new()),
                name_overrides: RwLock::new(HashMap::new()),
                histories: RwLock::new(HashMap::new()),
                component_energy: RwLock::new(HashMap::new()),
                setpoint_logs: RwLock::new(HashMap::new()),
                version: AtomicU64::new(0),
                run_generation: AtomicU64::new(0),
                structural_version: AtomicU64::new(0),
                events: broadcast::channel(EVENT_BUS_CAPACITY).0,
                timeout_tracker: TimeoutTracker::new(),
                scenario: RwLock::new(ScenarioJournal::default()),
                scenario_csv: RwLock::new(CsvSinks::new()),
                scenario_setpoints_csv: RwLock::new(CsvSinks::new()),
                scenario_bounds_csv: RwLock::new(CsvSinks::new()),
                scenario_reactive_bounds_csv: RwLock::new(CsvSinks::new()),
                scenario_csv_dir: RwLock::new(None),
                grid_frequency: RwLock::new(None),
                stream_cancel_epoch: AtomicU64::new(0),
                sample_lag_ms: AtomicU64::new(0),
            }),
        }
    }

    /// Monotonic count of structural mutations (register / connect /
    /// disconnect / remove / rename) on this site. `Config::eval`
    /// snapshots it around an eval to decide whether the source is
    /// config worth persisting or a transient poke.
    pub fn structural_version(&self) -> u64 {
        self.inner.structural_version.load(Ordering::Acquire)
    }

    fn bump_structural(&self) {
        self.inner
            .structural_version
            .fetch_add(1, Ordering::Release);
    }

    /// Mark this site's *config* as moved without touching a
    /// component. The site's own mutators bump the structural
    /// version themselves; this is for edits that live one level up,
    /// on the microgrid's `MicrogridDef` — its name, its TSO label —
    /// which the managed file's `(make-microgrid …)` head carries and
    /// which therefore have to reach the same persist trigger.
    pub fn bump_structural_version(&self) {
        self.bump_structural();
    }

    /// Read the current sample-lag offset (ms). The server uses this
    /// to shift telemetry timestamps into the past, modelling a server
    /// that delivers samples with stale timestamps.
    pub fn sample_lag_ms(&self) -> u64 {
        self.inner.sample_lag_ms.load(Ordering::Acquire)
    }

    /// Set the sample-lag offset (ms). 0 = use the wall clock; > 0
    /// shifts every sample's timestamp into the past by that many ms.
    pub fn set_sample_lag_ms(&self, ms: u64) {
        self.inner.sample_lag_ms.store(ms, Ordering::Release);
    }

    /// Current stream-cancel epoch. Streaming tasks capture this on
    /// start; on each iteration they re-read it and break if it has
    /// changed since their start. Used by `cancel_all_streams()` to
    /// drop every active stream from the server side without killing
    /// the process.
    pub fn stream_cancel_epoch(&self) -> u64 {
        self.inner.stream_cancel_epoch.load(Ordering::Acquire)
    }

    /// Bump the stream-cancel epoch. Every currently-running stream
    /// task will see the change on its next iteration (≤ one stream
    /// interval) and exit cleanly. New clients reconnecting after will
    /// pick up the new epoch and stream normally.
    pub fn cancel_all_streams(&self) {
        self.inner
            .stream_cancel_epoch
            .fetch_add(1, Ordering::Release);
    }

    /// Wire this site to an grid frequency source. After this
    /// call, `grid_state()` reads `frequency_hz` from the shared OU
    /// state instead of the per-mg `GridState::frequency_hz` slot.
    /// Voltage stays per-mg.
    pub fn set_grid_frequency(&self, freq: crate::sim::frequency::SharedFrequency) {
        *self.inner.grid_frequency.write() = Some(freq);
    }

    /// Id of the microgrid's main / point-of-common-coupling meter:
    /// the meter fronting the grid connection point. Derived from the
    /// topology — the sole visible child of the single grid connection
    /// point, when that child is a meter — rather than a hand-set flag,
    /// mirroring how the microgrid-rs formula engine locates the grid
    /// meter, so it can never drift from the graph. `None` when the PCC
    /// meter is absent or ambiguous: no grid, more than one grid, a grid
    /// with no single child, or a child that isn't a meter (pure-PV /
    /// pure-battery / off-grid topologies have no grid at all). Duplicate
    /// edges collapse and hidden children are ignored — they aggregate
    /// off-graph (`graph_adapter` excludes them) — so neither makes the
    /// PCC ambiguous.
    ///
    /// The scenario reporter tracks this meter's active-power peak, and
    /// the UI's frequency tile samples its `frequency_hz` history — the
    /// latter a workaround for frequenz-microgrid 0.5.0's LogicalMeter
    /// not carrying a `Sample<Frequency>` formula through its actor.
    pub fn main_meter_id(&self) -> Option<u64> {
        let by_id = self.inner.by_id.read();
        let conns = self.inner.connections.read();
        // Exactly one grid connection point (>1 is an ambiguous PCC).
        let mut grids = by_id
            .iter()
            .filter(|(_, c)| c.category() == Category::Grid)
            .map(|(id, _)| *id);
        let grid = grids.next()?;
        if grids.next().is_some() {
            return None;
        }
        // Its children, distinct and visible: duplicate edges collapse
        // to the same id, and hidden components don't count. Require
        // exactly one, and that it's a meter.
        let mut children = conns
            .iter()
            .filter(|(parent, _)| *parent == grid)
            .map(|(_, child)| *child)
            .filter(|child| by_id.get(child).is_some_and(|c| !c.is_hidden()));
        let only = children.next()?;
        if children.any(|child| child != only) {
            return None;
        }
        by_id
            .get(&only)
            .filter(|c| c.category() == Category::Meter)
            .map(|_| only)
    }

    // ─── Setpoint timeouts ────────────────────────────────────────────
    //
    // Each accepted setpoint schedules a deadline on its own power
    // axis; on expiry the Config loop pulls the (id, axis) out via
    // `drain_expired_timeouts` and calls `reset_setpoint_axis` on the
    // component — the other axis's command keeps running.

    /// Schedule a setpoint expiry for `id`'s `axis` at
    /// `now + lifetime`. Replaces any previously-scheduled deadline
    /// for that (id, axis) — "latest set wins" per axis.
    pub fn add_timeout(
        &self,
        id: u64,
        axis: crate::timeout_tracker::SetpointAxis,
        lifetime: Duration,
    ) {
        self.inner.timeout_tracker.add(id, axis, lifetime);
    }

    /// Drain any deadlines that have elapsed and return their
    /// (id, axis) pairs. Called by `Config`'s timeout loop, which
    /// then calls `reset_setpoint_axis` on each.
    pub fn drain_expired_timeouts(&self) -> Vec<(u64, crate::timeout_tracker::SetpointAxis)> {
        self.inner.timeout_tracker.remove_expired()
    }

    // ─── Version counter + event broadcast bus ────────────────────────
    //
    // Every accepted /api/eval bumps `version`, which fires a
    // `TopologyChanged` on the broadcast bus. Live UI tabs listen
    // and refetch /api/topology on each bump.

    pub fn version(&self) -> u64 {
        self.inner.version.load(Ordering::Relaxed)
    }

    /// Increment the version counter and broadcast a
    /// `TopologyChanged` event. Returns the new version. Send errors
    /// (no live subscribers) are swallowed — the event is fire-and-
    /// forget by design.
    pub fn bump_version(&self) -> u64 {
        let v = self.inner.version.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self
            .inner
            .events
            .send(SiteEvent::TopologyChanged { version: v });
        v
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<SiteEvent> {
        self.inner.events.subscribe()
    }

    /// Broadcast a `ConfigError` on the site event bus. Used by the
    /// watcher's reload-failure path so UI subscribers can render a
    /// "config invalid" banner instead of seeing the post-reset
    /// empty site without explanation. Fire-and-forget — a send
    /// error means there are no live subscribers, which is fine.
    pub fn broadcast_config_error(&self, message: String) {
        let _ = self.inner.events.send(SiteEvent::ConfigError {
            ts_ms: chrono::Utc::now().timestamp_millis(),
            message,
        });
    }

    /// Broadcast one aggregated-stream sample from the loopback
    /// Microgrid client. The forwarder tasks in
    /// `ui::spawn_microgrid_loopback` call this for each
    /// `Sample<Q>` they receive; the SPA's WS reads them off
    /// `/ws/events`. Fire-and-forget for the same reason
    /// [`Self::broadcast_config_error`] is.
    pub fn broadcast_microgrid_sample(
        &self,
        stream: &'static str,
        quantity: &'static str,
        unit: &'static str,
        ts_ms: i64,
        value: Option<f32>,
    ) {
        let _ = self.inner.events.send(SiteEvent::MicrogridSample {
            stream,
            quantity,
            unit,
            ts_ms,
            value,
        });
    }

    // ─── Scheduler knobs + grid state ────────────────────────────────
    //
    // `physics_tick` is the cadence at which `spawn_physics` runs
    // every component's `tick`. `grid_state` is the environmental
    // state (per-phase voltage + frequency) that components read
    // during tick.

    pub fn next_id(&self) -> u64 {
        self.inner.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Move the shared auto-id allocator past `id`. Called for every
    /// explicit `:id` registration, so replayed overrides and imports
    /// advance the allocator the same way live imports do — a later
    /// auto-assigned id can never collide with a pinned one, in any
    /// microgrid sharing the allocator. Saturating so `u64::MAX`
    /// pins the allocator at the ceiling instead of overflowing.
    pub fn reserve_id(&self, id: u64) {
        self.inner
            .next_id
            .fetch_max(id.saturating_add(1), Ordering::SeqCst);
    }

    pub fn physics_tick(&self) -> Duration {
        Duration::from_millis(self.inner.physics_tick_ms.load(Ordering::Relaxed))
    }

    pub fn set_physics_tick_ms(&self, ms: u64) {
        self.inner.physics_tick_ms.store(ms, Ordering::Relaxed);
    }

    pub fn grid_state(&self) -> GridState {
        let mut state = self.inner.grid_state.read().clone();
        if let Some(freq) = self.inner.grid_frequency.read().as_ref() {
            state.frequency_hz = freq.read().read_hz();
        }
        state
    }

    pub fn set_grid_state(&self, state: GridState) {
        *self.inner.grid_state.write() = state;
    }

    // ─── Component registry + topology graph ─────────────────────────
    //
    // Components register via `register` / `register_arc` and land in
    // both `components` (registration order = tick order) and `by_id`
    // (for O(1) lookup). `connections` carries every parent→child
    // edge — `connections()` filters to visible, `hidden_connections`
    // returns the rest; `children_of` is the unfiltered walk that
    // aggregation paths use.

    pub fn register<C: SimulatedComponent + 'static>(&self, c: C) -> ComponentHandle {
        self.register_arc(Arc::new(c))
    }

    /// Register a component. Writes `components`, `by_id`, and
    /// `runtime` under separate short guards — NOT atomically. The
    /// standing invariant making that safe: every structural mutation
    /// (register / remove / connect, all reached via the `make-*` and
    /// editing defuns) runs while the caller holds the interpreter
    /// lock, so two structural writers never interleave; concurrent
    /// READERS (tick, gRPC, history) may observe a component in one
    /// map and not yet the other for the duration of this call, which
    /// every reader tolerates (`get` returning None / a skipped tick
    /// for one pass).
    pub fn register_arc(&self, c: Arc<dyn SimulatedComponent>) -> ComponentHandle {
        let id = c.id();
        Arc::make_mut(&mut *self.inner.components.write()).push(c.clone());
        self.inner.by_id.write().insert(id, c.clone());
        self.bump_structural();
        // Default runtime mode: every flag at "Normal" — i.e. emit
        // telemetry, accept commands, report physics-derived state.
        self.inner.runtime.write().entry(id).or_default();
        // A fresh component starts its history from zero: drop any leftover
        // energy accumulator, scenario baseline, chart history, and setpoint
        // ring under this id (a removal that raced the history sampler or a
        // gRPC setpoint log can leave a re-created entry behind, and a stale
        // energy cursor would integrate the removal-to-reregister gap).
        self.inner.component_energy.write().remove(&id);
        self.inner.scenario.write().energy_baseline_wh.remove(&id);
        self.inner.histories.write().remove(&id);
        self.inner.setpoint_logs.write().remove(&id);
        ComponentHandle::from_arc(c)
    }

    /// Add a `(parent, child)` edge. Returns false (and logs a
    /// warning) when the edge would close a cycle — power aggregation
    /// walks the graph recursively with no depth cap, so a cycle
    /// would recurse to a stack overflow that aborts the physics
    /// task. Self-edges count as cycles.
    pub fn connect(&self, parent: u64, child: u64) -> bool {
        let mut conns = self.inner.connections.write();
        // The new edge closes a loop iff `parent` is already
        // reachable FROM `child` (or it's a self-edge).
        if parent == child || reachable(&conns, child, parent) {
            log::warn!("connect({parent}, {child}) rejected: would create a cycle");
            return false;
        }
        conns.push((parent, child));
        drop(conns);
        self.bump_structural();
        true
    }

    /// Visible edges only — drops any edge whose parent or child is
    /// marked hidden. gRPC ListConnections and the UI topology graph
    /// both want this filtered view. Use [`Self::all_connections`]
    /// for aggregation paths that need the unfiltered set.
    pub fn connections(&self) -> Vec<(u64, u64)> {
        let by_id = self.inner.by_id.read();
        self.inner
            .connections
            .read()
            .iter()
            .filter(|(p, c)| {
                !by_id.get(p).map(|x| x.is_hidden()).unwrap_or(false)
                    && !by_id.get(c).map(|x| x.is_hidden()).unwrap_or(false)
            })
            .copied()
            .collect()
    }

    /// Every `(parent, child)` edge, hidden or not, in insertion
    /// order — an unfiltered clone of the raw connections graph.
    /// Unlike [`Self::children_of`] this isn't scoped to one parent;
    /// the microgrid-file renderer walks the whole graph to emit
    /// `:successors` / `(connect …)` forms.
    pub fn all_connections(&self) -> Vec<(u64, u64)> {
        self.inner.connections.read().clone()
    }

    /// Edges where at least one endpoint is hidden — the complement
    /// of [`Self::connections`]. The UI surfaces these as a separate
    /// `hidden_connections` field so a hidden meter's outgoing edges
    /// can be drawn dashed while still leaving the gRPC graph clean.
    pub fn hidden_connections(&self) -> Vec<(u64, u64)> {
        let by_id = self.inner.by_id.read();
        self.inner
            .connections
            .read()
            .iter()
            .filter(|(p, c)| {
                by_id.get(p).map(|x| x.is_hidden()).unwrap_or(false)
                    || by_id.get(c).map(|x| x.is_hidden()).unwrap_or(false)
            })
            .copied()
            .collect()
    }

    /// Every edge from `parent`, hidden or not. Used by aggregation
    /// paths (meter / inverter / `aggregate_child_bounds`) that need
    /// to walk the *physical* graph; the visible-only filter in
    /// [`Self::connections`] is for the user-facing surface.
    /// `connect` and `disconnect` flow through the same
    /// underlying vec, so anything wired up post-make from the UI /
    /// REPL automatically lands here.
    pub fn children_of(&self, parent: u64) -> Vec<u64> {
        self.inner
            .connections
            .read()
            .iter()
            .filter_map(|(p, c)| (*p == parent).then_some(*c))
            .collect()
    }

    /// Children of `parent` paired with each child's total parent
    /// count, gathered under a single connections-lock read. The
    /// meter aggregation walk runs per telemetry read (physics tick,
    /// history sampler, gRPC streams), so the edge list is scanned
    /// once per call here, not once per child.
    pub fn children_with_parent_counts(&self, parent: u64) -> Vec<(u64, usize)> {
        let conns = self.inner.connections.read();
        let mut parents: HashMap<u64, usize> = HashMap::new();
        for (_, c) in conns.iter() {
            *parents.entry(*c).or_insert(0) += 1;
        }
        conns
            .iter()
            .filter(|(p, _)| *p == parent)
            .map(|(_, c)| (*c, parents[c]))
            .collect()
    }

    /// Snapshot of the registration-order component list — one Arc
    /// refcount bump, no Vec copy.
    pub fn components(&self) -> Arc<Vec<Arc<dyn SimulatedComponent>>> {
        self.inner.components.read().clone()
    }

    /// Number of registered components, without snapshotting the list.
    pub fn component_count(&self) -> usize {
        self.inner.components.read().len()
    }

    pub fn get(&self, id: u64) -> Option<Arc<dyn SimulatedComponent>> {
        self.inner.by_id.read().get(&id).cloned()
    }

    /// Number of `(parent, child)` edges in the connections graph
    /// where `child == id`. A meter aggregating a child that's
    /// shared with a sibling meter (parallel paths) divides the
    /// child's flow by this count so the sum at the parent of those
    /// siblings doesn't double-count. Counts the raw `connections`
    /// vec, which includes hidden edges (only the `connections()` /
    /// `hidden_connections()` accessors filter) — so a connected
    /// hidden child counts like any other. Returns 0 only for a
    /// fully-unconnected child; callers treat that as "this meter is
    /// the sole consumer" by clamping with `.max(1)`.
    pub fn parent_count(&self, id: u64) -> usize {
        self.inner
            .connections
            .read()
            .iter()
            .filter(|(_, c)| *c == id)
            .count()
    }

    /// Sum the `effective_active_bounds()` of every direct child of
    /// `parent`. Returns `None` when `parent` has no children that
    /// expose bounds.
    ///
    /// The microgrid API gateway uses this to gate setpoints against
    /// the downstream physical envelope — a real inverter has no data
    /// link to its battery's BMS limits, but the gateway sees both
    /// telemetry streams and intersects them on the client's behalf.
    pub fn aggregate_child_bounds(&self, parent: u64) -> Option<crate::sim::bounds::VecBounds> {
        use crate::sim::bounds::VecBounds;
        let child_ids: Vec<u64> = self
            .inner
            .connections
            .read()
            .iter()
            .filter(|(p, _)| *p == parent)
            .map(|(_, c)| *c)
            .collect();
        if child_ids.is_empty() {
            return None;
        }
        let bounds: Vec<VecBounds> = child_ids
            .iter()
            .filter_map(|id| self.get(*id))
            .filter_map(|c| c.effective_active_bounds())
            .collect();
        if bounds.is_empty() {
            None
        } else {
            Some(VecBounds::sum_single(bounds))
        }
    }

    /// The active-power envelope a setpoint for `id` must fall within:
    /// the component's own effective AC bounds intersected with the
    /// summed DC bounds of its children. `None` when the component has
    /// no children exposing bounds — then only its own bounds apply
    /// (enforced by the component's `set_active_setpoint`).
    ///
    /// Both setpoint entry points gate against this so a command outside
    /// the intersection is rejected, not silently saturated by the
    /// battery: the gRPC `SetElectricalComponentPower` gateway
    /// ([`crate::server`]) and the `(set-active-power)` DSL
    /// (`lisp::defuns::setpoints`).
    pub fn active_setpoint_envelope(&self, id: u64) -> Option<crate::sim::bounds::VecBounds> {
        let child_env = self.aggregate_child_bounds(id)?;
        // A component with children bounds but no own bounds gates
        // on the children alone — intersecting with the empty
        // default would reject EVERY setpoint with a nonsense
        // "exceeds combined envelope []" message.
        match self.get(id)?.effective_active_bounds() {
            Some(own) => Some(own.intersect(&child_env)),
            None => Some(child_env),
        }
    }

    /// Sum the `reactive_bounds()` of every direct child of `parent`
    /// — the reactive twin of [`Self::aggregate_child_bounds`].
    /// Returns `None` when `parent` has no children that expose
    /// reactive bounds.
    ///
    /// In today's topologies that is always the answer: the only
    /// components reporting Q bounds are inverters, and an inverter's
    /// children are batteries, which carry no reactive axis at all
    /// (reactive power terminates at the inverter). The mirror exists
    /// so the gateway has the same shape on both axes, and it starts
    /// gating for real the moment a child type does report a Q
    /// envelope.
    pub fn aggregate_child_reactive_bounds(
        &self,
        parent: u64,
    ) -> Option<crate::sim::bounds::VecBounds> {
        use crate::sim::bounds::VecBounds;
        let child_ids: Vec<u64> = self
            .inner
            .connections
            .read()
            .iter()
            .filter(|(p, _)| *p == parent)
            .map(|(_, c)| *c)
            .collect();
        if child_ids.is_empty() {
            return None;
        }
        let bounds: Vec<VecBounds> = child_ids
            .iter()
            .filter_map(|id| self.get(*id))
            .filter_map(|c| c.reactive_bounds())
            .collect();
        if bounds.is_empty() {
            None
        } else {
            Some(VecBounds::sum_single(bounds))
        }
    }

    /// The reactive-power envelope a setpoint for `id` must fall
    /// within: the component's own Q band intersected with the summed
    /// Q bands of its children. `None` when no child exposes reactive
    /// bounds — then only the component's own band applies (enforced
    /// by the component's `set_reactive_setpoint`). See
    /// [`Self::aggregate_child_reactive_bounds`] for why `None` is the
    /// normal answer today.
    ///
    /// Both reactive setpoint entry points gate against this, mirroring
    /// [`Self::active_setpoint_envelope`]: the gRPC
    /// `SetElectricalComponentPower` gateway ([`crate::server`]) and
    /// the `(set-reactive-power)` DSL (`lisp::defuns::setpoints`).
    pub fn reactive_setpoint_envelope(&self, id: u64) -> Option<crate::sim::bounds::VecBounds> {
        let child_env = self.aggregate_child_reactive_bounds(id)?;
        // Same carve-out as the active side: a component with
        // children bounds but no Q band of its own gates on the
        // children alone rather than intersecting with an empty
        // default, which would reject every setpoint.
        match self.get(id)?.reactive_bounds() {
            Some(own) => Some(own.intersect(&child_env)),
            None => Some(child_env),
        }
    }

    /// Wipe every registered component. Called from `(reset-state)` in
    /// the config DSL on hot-reload. Also resets the id allocator so a
    /// reloaded config sees the same ids the previous load saw,
    /// matching microsim's `(setq comp--id--counter 1000)` behaviour.
    ///
    /// Reset *also* clears scenario-scoped state (the journal,
    /// per-component CSV sinks, the main-meter flag) so a hot-reload
    /// truly starts from scratch — leaving them in place leaked stale
    /// integrals against gone-and-reborn ids and blocked a reload
    /// from claiming a *different* meter as main.
    ///
    /// Grid state is environmental (set by the config's `every`
    /// timer); we deliberately keep it across reloads so the first
    /// tick after reload still has plausible per-phase voltage /
    /// frequency values.
    pub fn reset(&self) {
        // A reset starts a new run; see `run_generation`.
        self.inner.run_generation.fetch_add(1, Ordering::Relaxed);
        // Cancel active telemetry streams: each stream task holds an
        // Arc to its component from subscribe time, and a component
        // cleared below is never ticked again — without the epoch
        // bump the task keeps emitting the last snapshot on cadence
        // forever, indistinguishable from live data. Clients see
        // EOF/CANCELLED and reconnect onto the rebuilt registry.
        self.cancel_all_streams();
        *self.inner.components.write() = Arc::new(Vec::new());
        self.inner.by_id.write().clear();
        self.inner.connections.write().clear();
        self.inner.runtime.write().clear();
        self.inner.operational_modes.write().clear();
        self.inner.name_overrides.write().clear();
        self.inner.histories.write().clear();
        self.inner.component_energy.write().clear();
        self.inner.setpoint_logs.write().clear();
        *self.inner.scenario.write() = ScenarioJournal::default();
        // `clear()` drops every sink; each BufWriter flushes on drop.
        self.inner.scenario_csv.write().clear();
        self.inner.scenario_setpoints_csv.write().clear();
        self.inner.scenario_bounds_csv.write().clear();
        self.inner.scenario_reactive_bounds_csv.write().clear();
        // Deliberately do NOT rewind `next_id`: the allocator is shared
        // across every site in an enterprise, so a per-site reset (a lone
        // `(reset-microgrid)`) must not rewind the global counter while
        // other sites still hold live components at higher ids — the next
        // auto-allocation would then collide. A full `Config::reload`
        // resets the allocator explicitly when every site is rebuilt.
    }

    /// Remove a component from the registry and drop every edge that
    /// touches it (in or out). Returns true if the component was
    /// present. The Arc held by any in-flight gRPC stream task keeps
    /// the underlying component alive until the subscriber drops —
    /// the registry just stops handing it out from `get()`.
    pub fn remove_component(&self, id: u64) -> bool {
        let was_present = self.inner.by_id.write().remove(&id).is_some();
        if was_present {
            self.bump_structural();
        }
        Arc::make_mut(&mut *self.inner.components.write()).retain(|c| c.id() != id);
        self.inner
            .connections
            .write()
            .retain(|(p, c)| *p != id && *c != id);
        self.inner.histories.write().remove(&id);
        self.inner.component_energy.write().remove(&id);
        // The scenario's energy baseline too: a component re-registered
        // under this id restarts its accumulator at zero, and subtracting
        // the old baseline would read negative energy.
        self.inner.scenario.write().energy_baseline_wh.remove(&id);
        self.inner.runtime.write().remove(&id);
        self.inner.operational_modes.write().remove(&id);
        self.inner.setpoint_logs.write().remove(&id);
        self.inner.name_overrides.write().remove(&id);
        was_present
    }

    /// Drop every `(parent, child)` edge from the graph. Returns
    /// true if at least one edge was removed. Doesn't touch either
    /// endpoint's registration.
    ///
    /// Duplicates collapse — if `(connect …)` was called
    /// twice with the same pair, one disconnect removes both. The
    /// connections graph carries no positional identity, so there's
    /// no "remove only the first instance" semantics.
    pub fn disconnect(&self, parent: u64, child: u64) -> bool {
        let removed = {
            let mut edges = self.inner.connections.write();
            let before = edges.len();
            edges.retain(|(p, c)| !(*p == parent && *c == child));
            edges.len() < before
        };
        if removed {
            self.bump_structural();
        }
        removed
    }

    /// Override a component's display name. Reads via `display_name`;
    /// `SimulatedComponent::name()` is unchanged so internal log
    /// lines and physics-derived state keep their stable default.
    pub fn rename(&self, id: u64, name: String) {
        self.inner.name_overrides.write().insert(id, name);
        // Renames count as structural for persistence purposes — a
        // display name set from the UI should survive a reload.
        self.bump_structural();
    }

    /// The raw `name_overrides` entry for `id`, or `None` if the
    /// component has never been renamed. Unlike [`Self::display_name`]
    /// this never falls back to the component's auto-generated
    /// default — the microgrid-file renderer needs to know whether
    /// `:name` was actually set, not just what name currently reads.
    pub fn name_override(&self, id: u64) -> Option<String> {
        self.inner.name_overrides.read().get(&id).cloned()
    }

    /// User-facing display name — override if present, else the
    /// component's intrinsic `name()`. Returns `None` when the id
    /// isn't registered (and no override was placed for a since-
    /// removed component).
    pub fn display_name(&self, id: u64) -> Option<String> {
        if let Some(n) = self.inner.name_overrides.read().get(&id) {
            return Some(n.clone());
        }
        self.inner
            .by_id
            .read()
            .get(&id)
            .map(|c| c.name().to_string())
    }

    // ─── Per-component runtime modes ─────────────────────────────────
    //
    // Health / telemetry mode / command mode flags carried in
    // `runtime`. Defaulted on register; mutated via the
    // `set-component-*` Lisp defuns or gRPC. `runtime_of` returns
    // the current snapshot; the per-setter methods mutate in place.

    pub fn runtime_of(&self, id: u64) -> ComponentRuntime {
        self.inner
            .runtime
            .read()
            .get(&id)
            .copied()
            .unwrap_or_default()
    }

    /// Set a component's health, coupling its command mode along: an
    /// errored device is also unreachable for commands, and clearing
    /// the error restores normal handling.
    ///
    /// NOTE the two axes are deliberately NOT orthogonal across an
    /// Error→Ok cycle: `set_health(Ok)` forces `command = Normal`,
    /// clobbering an independently-set `Timeout` / `OverBound`. That
    /// clobber is required for recovery — without it the Error
    /// coupling sticks and the device stays uncommandable — so a
    /// script that wants a command fault to survive a health cycle
    /// must re-apply it after the recovery. (`Standby` leaves the
    /// command mode alone in both directions.)
    pub fn set_health(&self, id: u64, health: Health) {
        let mode = self.operational_mode(id);
        let mut runtime = self.inner.runtime.write();
        let entry = runtime.entry(id).or_default();
        entry.health = health;
        match health {
            Health::Error => entry.command = CommandMode::Error,
            // Recovery restores commands only when the declared
            // operational mode accepts them — health cannot grant a
            // capability the config denies.
            Health::Ok => {
                entry.command = if mode.accepts_control() {
                    CommandMode::Normal
                } else {
                    CommandMode::Error
                };
            }
            Health::Standby => {}
        }
    }

    /// Set a component's runtime telemetry mode. Rejected when the
    /// component's operational mode does not stream telemetry — the
    /// runtime knobs depend on the declared capability, so an
    /// inactive component can never be poked back to `normal`.
    pub fn set_telemetry_mode(&self, id: u64, mode: TelemetryMode) -> Result<(), String> {
        if mode == TelemetryMode::Normal && !self.operational_mode(id).provides_telemetry() {
            return Err(format!(
                "component {id} has operational mode {}, which streams no \
                 telemetry; cannot set telemetry-mode to normal",
                self.operational_mode(id)
            ));
        }
        self.inner.runtime.write().entry(id).or_default().telemetry = mode;
        Ok(())
    }

    /// Set a component's runtime command mode. Rejected when the
    /// component's operational mode does not accept control — same
    /// rule as [`Self::set_telemetry_mode`] — and when the component's
    /// health is `Error`: an errored device never accepts commands
    /// (`set_health` forces the channel shut; only recovery via
    /// `set_health(Ok)` re-opens it).
    pub fn set_command_mode(&self, id: u64, mode: CommandMode) -> Result<(), String> {
        if mode == CommandMode::Normal && !self.operational_mode(id).accepts_control() {
            return Err(format!(
                "component {id} has operational mode {}, which accepts no \
                 commands; cannot set command-mode to normal",
                self.operational_mode(id)
            ));
        }
        // Checked under the same write lock as the store, so a racing
        // set_health cannot slip between the check and the write.
        let mut runtime = self.inner.runtime.write();
        let entry = runtime.entry(id).or_default();
        if mode == CommandMode::Normal && entry.health == Health::Error {
            return Err(format!(
                "component {id} has health error, which keeps the command \
                 channel shut; set health to ok first"
            ));
        }
        entry.command = mode;
        Ok(())
    }

    // ─── Per-component operational mode (config) ─────────────────────

    /// The component's declared operational mode. Defaults to
    /// `Unspecified` (full capability) when never set.
    pub fn operational_mode(&self, id: u64) -> OperationalMode {
        self.inner
            .operational_modes
            .read()
            .get(&id)
            .copied()
            .unwrap_or_default()
    }

    /// Set a component's operational mode — a CONFIG change, so it
    /// bumps the structural version (the overrides gate persists the
    /// eval that did it). Rejected for an unregistered id: the change
    /// would persist, and a later component allocated on that id
    /// would silently inherit it.
    ///
    /// The runtime knobs are re-derived from the new mode: a mode
    /// without telemetry silences the stream, one without control
    /// errors the command channel, and regaining a capability
    /// restores the knob to `normal` — unless health is `Error`,
    /// which keeps the command channel erroring (an errored device
    /// never accepts commands, whatever the config says). Like the
    /// health ↔ command coupling in [`Self::set_health`], this
    /// deliberately clobbers independently-set fault knobs on the
    /// affected axes.
    pub fn set_operational_mode(&self, id: u64, mode: OperationalMode) -> Result<(), String> {
        if self.get(id).is_none() {
            return Err(format!("component {id} not found"));
        }
        self.inner.operational_modes.write().insert(id, mode);
        {
            let mut runtime = self.inner.runtime.write();
            let entry = runtime.entry(id).or_default();
            entry.telemetry = if mode.provides_telemetry() {
                TelemetryMode::Normal
            } else {
                TelemetryMode::Silent
            };
            entry.command = if mode.accepts_control() && entry.health != Health::Error {
                CommandMode::Normal
            } else {
                CommandMode::Error
            };
        }
        self.bump_structural();
        Ok(())
    }

    // ─── Physics tick ────────────────────────────────────────────────
    //
    // `tick_once` runs one synchronous pass over every component;
    // `spawn_physics` is the long-running task that does it on a
    // `tokio::time::interval`. Pre-tick hook fires first so Lisp-
    // driven inputs resolve once per tick before any `tick()` reads
    // an atomic.

    /// Tick every registered component once. Children are stored before
    /// parents, so a single forward pass updates leaves before the
    /// meters that aggregate them.
    ///
    /// Pure Rust — does NOT enter the Lisp interpreter. Lambda-bound
    /// component inputs (`:power`, `:sunlight%`, …) are refreshed
    /// by `Config`'s dedicated lisp-refresh task on its own 100 ms
    /// cadence; this method only reads the atomic scalars those
    /// refreshes leave behind. Tests that need a synchronous refresh
    /// before driving `tick_once` should call `Config::refresh_once`.
    pub fn tick_once(&self, now: DateTime<Utc>, dt: Duration) {
        let components = self.inner.components.read().clone();
        for c in components.iter() {
            c.tick(self, now, dt);
        }
        self.integrate_energy(&components, now);
    }

    /// Advance each component's cumulative-energy accumulator from the
    /// power it settled on this tick. Runs on every `tick_once`, so
    /// `EnergyWh` accrues identically in the live server and the headless
    /// stepped runner. A component in `Health::Error` has its cursor
    /// dropped — a faulted device isn't moving real metered energy; a
    /// healthy but telemetry-silent component keeps accruing, since the
    /// physics power is real whether or not a sample is being streamed.
    /// Components carrying no AC active power (batteries) never get an
    /// entry, matching the sparse `EnergyWh` history metric.
    fn integrate_energy(&self, components: &[Arc<dyn SimulatedComponent>], now: DateTime<Utc>) {
        // Health first, under one runtime-lock read for the whole
        // pass (not held across the power reads — `active_power_w`
        // re-enters `connections` / `by_id` locks for meters).
        let healths: Vec<Health> = {
            let runtime = self.inner.runtime.read();
            components
                .iter()
                .map(|c| runtime.get(&c.id()).copied().unwrap_or_default().health)
                .collect()
        };
        // Gather power with no energy lock held, keeping the
        // `component_energy` critical section lock-clean.
        let samples: Vec<(u64, Option<f32>, Health)> = components
            .iter()
            .zip(healths)
            .map(|(c, health)| (c.id(), c.active_power_w(self), health))
            .collect();
        let ts_ms = now.timestamp_millis();
        let mut acc = self.inner.component_energy.write();
        for (id, power, health) in samples {
            let Some(power_w) = power else { continue };
            let e = acc.entry(id).or_default();
            if health == Health::Error {
                e.reset_cursor();
            } else {
                e.advance(power_w, ts_ms);
            }
        }
    }

    /// The running cumulative AC energy (Wh) a component has moved since
    /// its first tick, or `None` for a component carrying no AC active
    /// power (e.g. a battery) that never started integrating. Populated
    /// on the physics tick, so it reads back in both live and stepped
    /// runs (see `integrate_energy`).
    pub fn component_energy_wh(&self, id: u64) -> Option<f64> {
        self.inner
            .component_energy
            .read()
            .get(&id)
            .map(|e| e.total_wh)
    }

    /// The cumulative AC energy (Wh) a component has moved since the
    /// current scenario started: the running total minus the baseline
    /// `scenario_start` snapshotted. Equal to the full total when no
    /// scenario has started (empty baseline) or when the component
    /// first ticked mid-scenario (no baseline entry). `None` when the
    /// component has no accumulator entry (see `component_energy_wh`).
    pub fn component_energy_since_scenario_wh(&self, id: u64) -> Option<f64> {
        let total = self.component_energy_wh(id)?;
        let base = self
            .inner
            .scenario
            .read()
            .energy_baseline_wh
            .get(&id)
            .copied()
            .unwrap_or(0.0);
        Some(total - base)
    }

    /// The current run generation. `reset()` bumps it; a changed value
    /// tells a reader that cumulative state gathered before the reset
    /// belongs to a previous run.
    pub fn run_generation(&self) -> u64 {
        self.inner.run_generation.load(Ordering::Relaxed)
    }

    /// Spawn the physics loop. Returns immediately. The loop holds an
    /// `Arc` clone of the MicrogridSite, so the MicrogridSite cannot drop until the
    /// task exits — and right now there is no exit path. That's fine
    /// for the long-running binary but means tests that need a clean
    /// shutdown should call `tick_once` directly instead.
    pub fn spawn_physics(self) {
        tokio::spawn(async move {
            let mut last = Utc::now();
            let mut interval = tokio::time::interval(self.physics_tick());
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let now = Utc::now();
                let dt = (now - last)
                    .to_std()
                    .unwrap_or_else(|_| Duration::from_millis(0));
                last = now;
                self.tick_once(now, dt);
                // Re-read the tick interval each iteration so config
                // changes take effect without a restart. interval_at
                // (first tick a full period out) — a plain interval's
                // immediate first tick would fire one extra pass on
                // every cadence change.
                let target = self.physics_tick();
                if interval.period() != target {
                    interval =
                        tokio::time::interval_at(tokio::time::Instant::now() + target, target);
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                }
            }
        });
    }

    // ─── Setpoint event log ──────────────────────────────────────────
    //
    // Per-component rolling log of accepted / rejected setpoint
    // requests. Populated by the gRPC handlers; read by the UI's
    // /api/setpoints inspector. Each `log_setpoint` also broadcasts
    // on the event bus for live UI updates.

    /// Append a setpoint event to the per-component log + broadcast
    /// it on the site event bus so live UI inspectors update without
    /// a refetch. Auto-creates the ring on first push; bounded to
    /// `SETPOINT_LOG_CAPACITY` entries (oldest evict).
    pub fn log_setpoint(&self, id: u64, event: SetpointEvent) {
        let ts_ms = event.ts.timestamp_millis();
        let kind = event.kind.as_str();
        let value = event.value;
        let (accepted, reason) = match &event.outcome {
            SetpointOutcome::Accepted { .. } => (true, None),
            SetpointOutcome::Rejected { reason } => (false, Some(reason.clone())),
        };
        // Scenario recording first (the ring push consumes the event).
        // Event-driven rather than sampled: a control app can issue
        // several requests between two 1 Hz passes and a replay wants
        // every one of them.
        if let Some(sink) = self.inner.scenario_setpoints_csv.write().get_mut(&id)
            && let Err(e) = sink.write_setpoint_row(&event)
        {
            log::warn!("setpoints CSV write failed for {id}: {e}");
        }
        self.inner
            .setpoint_logs
            .write()
            .entry(id)
            .or_insert_with(|| SetpointLog::new(SETPOINT_LOG_CAPACITY))
            .push(event);
        let _ = self.inner.events.send(SiteEvent::Setpoint {
            id,
            ts_ms,
            setpoint_kind: kind,
            value,
            accepted,
            reason,
        });
    }

    /// Read the recent setpoint events for one component.  Returns
    /// owned events so the caller can release the lock immediately.
    /// Empty Vec when the component has no recorded setpoints yet —
    /// either because it's new or because no client has set anything.
    pub fn setpoints_window(&self, id: u64, since: DateTime<Utc>) -> Vec<SetpointEvent> {
        self.inner
            .setpoint_logs
            .read()
            .get(&id)
            .map(|log| log.iter_window(since).cloned().collect())
            .unwrap_or_default()
    }
}

impl Default for MicrogridSite {
    fn default() -> Self {
        Self::new()
    }
}

/// Is `to` reachable from `from` over the directed `(parent, child)`
/// edge list? Iterative DFS with a visited set — used by `connect` to
/// reject cycle-creating edges before they can blow the aggregation
/// walk's stack.
fn reachable(edges: &[(u64, u64)], from: u64, to: u64) -> bool {
    let mut stack = vec![from];
    let mut visited = std::collections::HashSet::new();
    while let Some(node) = stack.pop() {
        if node == to {
            return true;
        }
        if !visited.insert(node) {
            continue;
        }
        for (p, c) in edges {
            if *p == node {
                stack.push(*c);
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The energy integrator reads `active_power_w()` where every
    /// other consumer reads `telemetry().active_power_w`; the trait
    /// doc requires overrides to read the same source their
    /// `telemetry()` reads. Pin that for every real component so a
    /// later telemetry-source change can't silently drift the energy
    /// accounting from the reported power stream.
    #[test]
    fn active_power_w_matches_telemetry() {
        use crate::sim::inverter::battery_inverter::BatteryInverterConfig;
        use crate::sim::inverter::solar_inverter::SolarInverterConfig;
        use crate::sim::{
            Battery, EvCharger, Meter,
            battery::BatteryConfig,
            ev_charger::EvChargerConfig,
            inverter::{BatteryInverter, SolarInverter},
        };

        let w = MicrogridSite::new();
        let sec = Duration::from_secs(1);
        w.register(Battery::new(1, sec, BatteryConfig::default()));
        w.register(EvCharger::new(2, sec, EvChargerConfig::default()));
        w.register(BatteryInverter::new(
            3,
            sec,
            BatteryInverterConfig::default(),
        ));
        w.register(SolarInverter::new(4, sec, SolarInverterConfig::default()));
        // A meter aggregating the solar inverter, so its override
        // exercises the children walk.
        w.register(Meter::new(5, sec, None, None, 0.0, false));
        w.connect(3, 1);
        w.connect(5, 4);

        // Drive nonzero flows: command the battery inverter and EV,
        // let the PV free-run from default sunlight, then tick.
        w.get(3).unwrap().set_active_setpoint(5_000.0).unwrap();
        w.get(2).unwrap().set_active_setpoint(3_000.0).unwrap();
        let mut now = Utc::now();
        for _ in 0..30 {
            now += chrono::Duration::milliseconds(100);
            w.tick_once(now, Duration::from_millis(100));
        }

        for id in 1..=5 {
            let c = w.get(id).unwrap();
            assert_eq!(
                c.active_power_w(&w),
                c.telemetry(&w).active_power_w,
                "component {id}: active_power_w() drifted from telemetry()"
            );
        }
        // And the flows really are nonzero, so the equality above
        // compared real values, not None == None everywhere. (PV
        // generation is negative by convention: rated is [-30 kW, 0].)
        assert!(w.get(4).unwrap().active_power_w(&w).unwrap_or(0.0) < 0.0);
        assert!(w.get(2).unwrap().active_power_w(&w).unwrap_or(0.0) > 0.0);
    }

    #[test]
    fn set_health_couples_command_mode() {
        let w = MicrogridSite::new();
        // Erroring a component makes it unreachable for commands too.
        w.set_health(5, Health::Error);
        assert_eq!(w.runtime_of(5).health, Health::Error);
        assert_eq!(w.runtime_of(5).command, CommandMode::Error);
        // Clearing the error restores normal command handling.
        w.set_health(5, Health::Ok);
        assert_eq!(w.runtime_of(5).command, CommandMode::Normal);
        // Standby refuses via the health check but leaves command mode alone.
        w.set_command_mode(5, CommandMode::Timeout).unwrap();
        w.set_health(5, Health::Standby);
        assert_eq!(w.runtime_of(5).command, CommandMode::Timeout);
    }

    /// Setting the operational mode (config) derives the runtime
    /// knobs; regaining a capability restores the knob to normal.
    #[test]
    fn operational_mode_derives_runtime_knobs() {
        let w = MicrogridSite::new();
        w.register(Stub::new(5));
        assert_eq!(w.operational_mode(5), OperationalMode::Unspecified);

        w.set_operational_mode(5, OperationalMode::Inactive)
            .unwrap();
        assert_eq!(w.runtime_of(5).telemetry, TelemetryMode::Silent);
        assert_eq!(w.runtime_of(5).command, CommandMode::Error);

        w.set_operational_mode(5, OperationalMode::TelemetryOnly)
            .unwrap();
        assert_eq!(w.runtime_of(5).telemetry, TelemetryMode::Normal);
        assert_eq!(w.runtime_of(5).command, CommandMode::Error);

        w.set_operational_mode(5, OperationalMode::ControlAndTelemetry)
            .unwrap();
        assert_eq!(w.runtime_of(5).telemetry, TelemetryMode::Normal);
        assert_eq!(w.runtime_of(5).command, CommandMode::Normal);
    }

    /// The runtime knobs depend on the operational mode: a capability
    /// the mode forbids cannot be poked back to normal, but fault
    /// values are always allowed.
    #[test]
    fn runtime_knobs_validate_against_operational_mode() {
        let w = MicrogridSite::new();
        w.register(Stub::new(5));
        w.set_operational_mode(5, OperationalMode::Inactive)
            .unwrap();
        assert!(w.set_telemetry_mode(5, TelemetryMode::Normal).is_err());
        assert!(w.set_command_mode(5, CommandMode::Normal).is_err());
        // Fault values stay settable — they only deepen the outage.
        w.set_telemetry_mode(5, TelemetryMode::Closed).unwrap();
        w.set_command_mode(5, CommandMode::Timeout).unwrap();

        w.set_operational_mode(5, OperationalMode::ControlOnly)
            .unwrap();
        assert!(w.set_telemetry_mode(5, TelemetryMode::Normal).is_err());
        w.set_command_mode(5, CommandMode::Normal).unwrap();
    }

    /// A config-level mode change bumps the structural version, so
    /// the overrides gate persists the eval that made it.
    #[test]
    fn operational_mode_changes_are_structural() {
        let w = MicrogridSite::new();
        w.register(Stub::new(5));
        let before = w.structural_version();
        w.set_operational_mode(5, OperationalMode::ControlOnly)
            .unwrap();
        assert!(w.structural_version() > before);
        // An unregistered id is rejected — the change would persist
        // and a later component on that id would inherit it.
        assert!(
            w.set_operational_mode(99, OperationalMode::Inactive)
                .is_err()
        );
    }

    /// Health and operational mode both gate the command channel:
    /// health recovery cannot grant control the mode denies, and a
    /// mode change cannot re-enable an errored device.
    #[test]
    fn health_and_operational_mode_couple_on_commands() {
        let w = MicrogridSite::new();
        w.register(Stub::new(5));

        // Control-only mode + health cycle: recovery keeps commands
        // normal (the mode accepts control) …
        w.set_operational_mode(5, OperationalMode::ControlOnly)
            .unwrap();
        w.set_health(5, Health::Error);
        w.set_health(5, Health::Ok);
        assert_eq!(w.runtime_of(5).command, CommandMode::Normal);

        // … but an inactive mode wins over recovery.
        w.set_operational_mode(5, OperationalMode::Inactive)
            .unwrap();
        w.set_health(5, Health::Error);
        w.set_health(5, Health::Ok);
        assert_eq!(w.runtime_of(5).command, CommandMode::Error);

        // And a mode change never re-enables an errored device.
        w.set_health(5, Health::Error);
        w.set_operational_mode(5, OperationalMode::ControlAndTelemetry)
            .unwrap();
        assert_eq!(w.runtime_of(5).command, CommandMode::Error);
    }

    /// Cycle-creating edges are rejected — the aggregation walk has
    /// no depth cap, so an accepted cycle would recurse the physics
    /// task to a stack overflow. Self-edges count.
    #[test]
    fn connect_rejects_cycle_creating_edges() {
        let w = MicrogridSite::new();
        assert!(w.connect(1, 2));
        assert!(w.connect(2, 3));
        // Closing the loop at any distance is refused...
        assert!(!w.connect(3, 1), "3 -> 1 closes 1->2->3");
        assert!(!w.connect(2, 1), "2 -> 1 closes 1->2");
        assert!(!w.connect(4, 4), "self-edge");
        // ...and the rejected edges never landed. (No components are
        // registered, so nothing is hidden and `connections()` shows
        // the full edge list.)
        assert_eq!(w.connections().len(), 2);
        // Unrelated edges still connect fine.
        assert!(w.connect(3, 4));
    }

    /// Two meters can list the same inverter as a successor and both
    /// edges land in the connections graph (a parallel-meter
    /// setup). `aggregate_child_bounds` from either parent finds its
    /// own children independently — no double-counting at the bounds
    /// layer.
    #[test]
    fn shared_child_under_two_parents() {
        let w = MicrogridSite::new();
        w.connect(2, 100);
        w.connect(3, 100);
        let conns = w.connections();
        assert_eq!(conns.len(), 2);
        assert!(conns.contains(&(2, 100)));
        assert!(conns.contains(&(3, 100)));
        // No registered component for id 100 in this lightweight
        // test, so aggregate_child_bounds returns None — we're
        // checking the connection-graph shape, not the bounds math.
        assert!(w.aggregate_child_bounds(2).is_none());
        assert!(w.aggregate_child_bounds(3).is_none());
    }

    /// `children_of` is the unfiltered list of edges from a parent.
    /// Hidden-aware filtering happens at the `connections()` /
    /// `hidden_connections()` boundary using registered components'
    /// `is_hidden()`; this helper is the raw graph walk used by
    /// aggregation paths that need to include hidden children.
    #[test]
    fn children_of_returns_every_edge_from_parent() {
        let w = MicrogridSite::new();
        w.connect(2, 100);
        w.connect(2, 101);
        assert_eq!(w.children_of(2), vec![100, 101]);
        w.disconnect(2, 100);
        assert_eq!(w.children_of(2), vec![101]);
    }

    /// `parent_count` reflects how many edges in the connections
    /// graph terminate on a given child. Meter aggregation divides
    /// by this so a child shared by N parents contributes 1/N to
    /// each.
    #[test]
    fn parent_count_reports_edge_count() {
        let w = MicrogridSite::new();
        assert_eq!(w.parent_count(100), 0); // unconnected
        w.connect(2, 100);
        assert_eq!(w.parent_count(100), 1);
        w.connect(3, 100);
        assert_eq!(w.parent_count(100), 2);
        // unrelated child unaffected
        assert_eq!(w.parent_count(101), 0);
    }

    // `tick_once` used to invoke a pre-tick hook installed by
    // `Config::new` to refresh Lisp-driven component inputs. That
    // hook moved off the per-site tick to a dedicated Lisp-refresh
    // tokio task on `Config`, decoupling physics from the
    // interpreter lock. The ordering test the old shape relied on
    // no longer makes sense — physics is pure Rust now and the
    // refresh runs at its own cadence — so the test was deleted
    // along with the hook field.

    /// `bump_version` advances the counter and broadcasts a
    /// `TopologyChanged` event with the new version. Used by
    /// `Config::eval` after every eval so UI tabs refetch.
    #[tokio::test]
    async fn bump_version_broadcasts_event() {
        let w = MicrogridSite::new();
        let mut rx = w.subscribe_events();
        assert_eq!(w.version(), 0);
        let v = w.bump_version();
        assert_eq!(v, 1);
        assert_eq!(w.version(), 1);
        match rx.recv().await.unwrap() {
            crate::sim::events::SiteEvent::TopologyChanged { version } => {
                assert_eq!(version, 1);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    /// Components used as stubs in the mutation-method tests below.
    /// All they need to do is identify themselves; physics is irrelevant.
    struct Stub {
        id: u64,
        name: String,
        category: Category,
        hidden: bool,
    }
    impl Stub {
        fn new(id: u64) -> Self {
            Self {
                id,
                name: format!("stub-{id}"),
                category: Category::Meter,
                hidden: false,
            }
        }
        fn with_category(id: u64, category: Category) -> Self {
            Self {
                category,
                ..Self::new(id)
            }
        }
        fn hidden(id: u64, category: Category) -> Self {
            Self {
                hidden: true,
                ..Self::with_category(id, category)
            }
        }
    }
    impl std::fmt::Display for Stub {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.name)
        }
    }
    impl SimulatedComponent for Stub {
        fn id(&self) -> u64 {
            self.id
        }
        fn category(&self) -> crate::sim::Category {
            self.category
        }
        fn is_hidden(&self) -> bool {
            self.hidden
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn stream_interval(&self) -> Duration {
            Duration::from_secs(1)
        }
        fn tick(&self, _: &MicrogridSite, _: DateTime<Utc>, _: Duration) {}
        fn telemetry(&self, _: &MicrogridSite) -> crate::sim::Telemetry {
            crate::sim::Telemetry::default()
        }
        fn make_fn(&self) -> &'static str {
            "%make-test-stub"
        }
        fn constructor_kwargs(&self) -> Vec<(&'static str, String)> {
            Vec::new()
        }
    }

    #[test]
    fn remove_component_drops_registry_and_edges() {
        let w = MicrogridSite::new();
        w.register(Stub::new(1));
        w.register(Stub::new(2));
        w.register(Stub::new(3));
        w.connect(1, 2);
        w.connect(2, 3);
        w.connect(1, 3);

        assert!(w.remove_component(2));
        assert!(w.get(2).is_none());
        assert!(w.get(1).is_some());
        let edges = w.connections();
        // Both edges touching id 2 went away; the 1→3 direct edge stays.
        assert_eq!(edges, vec![(1, 3)]);
        // Removing a missing id is a no-op that returns false.
        assert!(!w.remove_component(99));
    }

    #[test]
    fn disconnect_drops_one_edge_keeps_endpoints() {
        let w = MicrogridSite::new();
        w.register(Stub::new(1));
        w.register(Stub::new(2));
        w.connect(1, 2);
        w.connect(1, 2); // duplicate
        assert!(w.disconnect(1, 2));
        // First call drops both copies (retain semantics).
        assert!(w.connections().is_empty());
        assert!(w.get(1).is_some());
        assert!(w.get(2).is_some());
        // Second disconnect on the same edge returns false.
        assert!(!w.disconnect(1, 2));
    }

    #[test]
    fn rename_overrides_display_name_only() {
        let w = MicrogridSite::new();
        w.register(Stub::new(7));
        assert_eq!(w.display_name(7).as_deref(), Some("stub-7"));
        w.rename(7, "frontside-meter".into());
        assert_eq!(w.display_name(7).as_deref(), Some("frontside-meter"));
        // The component's intrinsic name() is untouched.
        assert_eq!(w.get(7).unwrap().name(), "stub-7");
    }

    /// `name_override` returns only an actual rename, never the
    /// component's auto default; `all_connections` is the raw,
    /// unfiltered, insertion-ordered edge list — unlike `connections`
    /// it doesn't drop edges touching a hidden component.
    #[test]
    fn name_override_and_all_connections_are_raw() {
        let site = MicrogridSite::new();
        site.register(crate::sim::Meter::new(
            1,
            std::time::Duration::from_secs(1),
            None,
            None,
            0.0,
            false,
        ));
        site.register(crate::sim::Meter::new(
            2,
            std::time::Duration::from_secs(1),
            None,
            None,
            0.0,
            true,
        ));
        site.connect(1, 2);
        assert_eq!(
            site.name_override(1),
            None,
            "auto default is not an override"
        );
        site.rename(1, "main".into());
        assert_eq!(site.name_override(1).as_deref(), Some("main"));
        // Hidden endpoint edges are still listed, in insertion order.
        assert_eq!(site.all_connections(), vec![(1, 2)]);
        assert!(site.connections().is_empty(), "visible-only stays filtered");
    }

    /// `reset()` clears history alongside the rest of the MicrogridSite so a
    /// hot-reload starts charts fresh — old component-id histories
    /// don't linger as orphan entries.
    #[test]
    fn reset_clears_history() {
        let w = MicrogridSite::new();
        // Push directly via the public API by way of a minimal stub.
        w.inner.histories.write().insert(
            42,
            crate::sim::history::ComponentHistory::new(HISTORY_CAPACITY),
        );
        w.reset();
        assert!(w.inner.histories.read().is_empty());
    }

    /// A per-site `reset()` must not rewind the enterprise-wide id
    /// allocator shared across sites — otherwise resetting one microgrid
    /// hands out ids that collide with components still live on another.
    #[test]
    fn reset_does_not_rewind_a_shared_id_allocator() {
        let alloc = Arc::new(AtomicU64::new(FIRST_AUTO_ID));
        let site_a = MicrogridSite::with_id_allocator(alloc.clone());
        let site_b = MicrogridSite::with_id_allocator(alloc.clone());
        // Site A advances the shared counter.
        assert_eq!(site_a.next_id(), FIRST_AUTO_ID);
        assert_eq!(site_a.next_id(), FIRST_AUTO_ID + 1);
        // Resetting B must leave the shared counter where A left it.
        site_b.reset();
        assert_eq!(
            site_a.next_id(),
            FIRST_AUTO_ID + 2,
            "a per-site reset rewound the shared id allocator"
        );
    }

    /// An errored device never accepts commands: `set_command_mode`
    /// rejects `Normal` while health is `Error`, whatever the
    /// operational mode says; recovery goes through `set_health(Ok)`.
    #[test]
    fn command_normal_is_rejected_while_health_is_error() {
        let w = MicrogridSite::new();
        w.register(Stub::new(1));
        w.set_health(1, Health::Error);
        assert!(w.set_command_mode(1, CommandMode::Normal).is_err());
        assert_eq!(w.runtime_of(1).command, CommandMode::Error);
        // Non-normal modes stay settable (e.g. scripted Timeout).
        assert!(w.set_command_mode(1, CommandMode::Timeout).is_ok());
        // Recovery re-opens the channel.
        w.set_health(1, Health::Ok);
        assert!(w.set_command_mode(1, CommandMode::Normal).is_ok());
    }

    /// `reserve_id` moves the shared allocator past a pinned id, so a
    /// replayed `(make-* :id N)` in one microgrid can never collide
    /// with a later auto-assigned id in a sibling.
    #[test]
    fn reserve_id_advances_the_shared_allocator() {
        let alloc = Arc::new(AtomicU64::new(FIRST_AUTO_ID));
        let site_a = MicrogridSite::with_id_allocator(alloc.clone());
        let site_b = MicrogridSite::with_id_allocator(alloc);
        site_a.reserve_id(2000);
        assert_eq!(site_b.next_id(), 2001);
        // Reserving below the watermark is a no-op.
        site_a.reserve_id(5);
        assert_eq!(site_b.next_id(), 2002);
        // The ceiling saturates instead of wrapping to 0.
        site_a.reserve_id(u64::MAX);
        assert_eq!(site_b.next_id(), u64::MAX);
    }

    /// Beyond histories, `reset()` also flushes the scenario journal,
    /// the setpoint logs, and any open CSV sinks. Leaving these across a
    /// hot-reload leaks stale integrals against ids that have since been
    /// re-registered.
    #[test]
    fn reset_clears_scenario_and_setpoints() {
        use crate::sim::setpoints::{SetpointEvent, SetpointKind, SetpointOutcome};
        let w = MicrogridSite::new();
        w.register(Stub::new(1));
        w.log_setpoint(
            1,
            SetpointEvent {
                ts: Utc::now(),
                kind: SetpointKind::ActivePower,
                value: 1234.0,
                ttl_s: Some(60),
                outcome: SetpointOutcome::Accepted {
                    effective_value: Some(1234.0),
                },
            },
        );
        w.scenario_start("smoke".into(), Utc::now());
        w.scenario_record("k".into(), "v".into(), Utc::now());

        w.reset();

        assert!(
            w.inner.setpoint_logs.read().is_empty(),
            "setpoint_logs must clear",
        );
        assert!(
            w.inner.scenario.read().started_at.is_none(),
            "scenario journal must reset",
        );
        assert_eq!(w.inner.scenario.read().event_count(), 0);
    }

    /// The main / point-of-common-coupling meter is derived from the
    /// topology, not a hand-set flag: it's the sole visible child of the
    /// grid connection point, and must itself be a meter. It follows the
    /// graph and is ambiguous (None) if the grid has ≠ 1 distinct child.
    #[test]
    fn main_meter_id_is_sole_meter_child_of_grid() {
        let w = MicrogridSite::new();
        w.register(Stub::with_category(1, Category::Grid));
        w.register(Stub::with_category(2, Category::Meter));
        w.register(Stub::with_category(3, Category::Battery));
        // No edges yet → nothing fronts the grid.
        assert_eq!(w.main_meter_id(), None);
        // grid(1) → meter(2), meter(2) → battery(3): grid's only child
        // is meter 2, so it's the main meter.
        w.connect(1, 2);
        w.connect(2, 3);
        assert_eq!(w.main_meter_id(), Some(2));
        // A duplicate grid→meter edge collapses to one distinct child.
        w.connect(1, 2);
        assert_eq!(w.main_meter_id(), Some(2));
        // A second distinct child of the grid makes the PCC ambiguous.
        w.register(Stub::with_category(4, Category::Battery));
        w.connect(1, 4);
        assert_eq!(w.main_meter_id(), None);
        // Remove the extra child → unambiguous single meter child again.
        w.disconnect(1, 4);
        assert_eq!(w.main_meter_id(), Some(2));
        // Removing the grid re-derives to None with no explicit clearing.
        w.remove_component(1);
        assert_eq!(w.main_meter_id(), None);
    }

    /// A grid whose sole child is not a meter has no PCC meter.
    #[test]
    fn main_meter_id_none_when_sole_grid_child_isnt_a_meter() {
        let w = MicrogridSite::new();
        w.register(Stub::with_category(1, Category::Grid));
        w.register(Stub::with_category(2, Category::Battery));
        w.connect(1, 2);
        assert_eq!(w.main_meter_id(), None);
    }

    /// A hidden component wired under the grid aggregates off-graph, so
    /// it doesn't count as a sibling — the visible meter is still main.
    #[test]
    fn main_meter_id_ignores_hidden_grid_child() {
        let w = MicrogridSite::new();
        w.register(Stub::with_category(1, Category::Grid));
        w.register(Stub::with_category(2, Category::Meter));
        w.register(Stub::hidden(5, Category::Meter));
        w.connect(1, 2);
        w.connect(1, 5);
        assert_eq!(w.main_meter_id(), Some(2));
    }

    /// More than one grid connection point is an ambiguous PCC → None.
    #[test]
    fn main_meter_id_none_with_multiple_grids() {
        let w = MicrogridSite::new();
        w.register(Stub::with_category(1, Category::Grid));
        w.register(Stub::with_category(2, Category::Meter));
        w.register(Stub::with_category(3, Category::Grid));
        w.register(Stub::with_category(4, Category::Meter));
        w.connect(1, 2);
        w.connect(3, 4);
        assert_eq!(w.main_meter_id(), None);
    }
}
