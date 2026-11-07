//! Lisp glue: load the config DSL, register the `make-*` functions
//! against a `MicrogridSite`, and act as the runtime entry point for
//! the gRPC server (which reads the same sites through their
//! gateways).
//!
//! The `Config` struct is intentionally thin — the simulation state
//! lives in `MicrogridSite`, the lisp interpreter is just the configuration
//! frontend. Behaviour is fanned out across child modules:
//!
//! - `boot` — `Config::new`, the long-lived lisp refresh loop, the
//!   tags-table pass, hot-reload + watch.
//! - `overrides` — `eval` and the file regeneration it triggers.
//! - `snapshots` — `save_snapshot_for` / `load_snapshot_for` against
//!   a microgrid's own file.
//! - `undo` — the per-microgrid undo / redo stacks over managed
//!   microgrid files.
//! - `defuns` — every `ctx.defun(...)` installer the config DSL
//!   exposes.

mod boot;
pub mod csv_profile;
mod defuns;
pub mod handle;
pub mod make;
pub mod microgrid_file;
mod overrides;
pub(crate) mod renames;
mod snapshots;
mod undo;

#[cfg(test)]
mod test_support;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::broadcast;
use tulisp::{SharedMut, TulispContext};

use crate::engine::Engine;
use crate::sim::MicrogridSite;
use crate::sim::microgrids::{CurrentMicrogrid, SharedSiteRouter};

pub use crate::engine::Metadata;
pub use boot::{LoadAsError, LoadError};
pub use snapshots::SnapshotError;
pub use undo::UndoDepths;

/// Escape `"` and `\` inside a Lisp string literal, and strip
/// control characters (incl. newlines), so a value can never break
/// out of its quotes in a file we later re-eval. Used by every
/// writer that renders text into Lisp source: the microgrid-file
/// and enterprise-file renderers, and the site-import form
/// renderer.
pub(crate) fn escape_lisp_string(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .flat_map(|c| match c {
            '\\' => vec!['\\', '\\'],
            '"' => vec!['\\', '"'],
            c => vec![c],
        })
        .collect()
}

/// Component categories that carry a `<cat>-defaults` plist in
/// `sim/defaults.lisp`. Walked in this order by `/api/defaults` and
/// by the `enterprise.lisp` renderer, so both see the same set and
/// the file's order is stable. A new category needs an entry here
/// AND a `(setq <cat>-defaults '(…))` block in `sim/defaults.lisp`.
pub(crate) const DEFAULT_CATEGORIES: &[&str] = &[
    "grid",
    "meter",
    "battery",
    "battery-inverter",
    "solar-inverter",
    "ev-charger",
    "steam-boiler",
    // One shared plist for all marker categories (chp, wind turbine,
    // power transformer, breaker).
    "marker",
];

/// Hash of a file's text, as stored in
/// [`Config::written_hashes`]. `DefaultHasher` is not stable across
/// Rust releases — fine here, the map only ever lives inside one
/// running process.
fn content_hash(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    content.hash(&mut h);
    h.finish()
}

/// Renders an f64 so tulisp reads it back as a float (always with a
/// decimal point). Whole numbers of any magnitude get the `.1`
/// form — a bare integer token above i64::MAX would not even parse.
pub(crate) fn lisp_float(v: f64) -> String {
    if v == v.trunc() && v.is_finite() {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// [`lisp_float`] for a value that is natively `f32` — every
/// component's config is. Formatting the `f32` directly gives the
/// shortest decimal that round-trips *as an f32*; widening to f64
/// first would print the f64 nearest to it instead, turning a
/// configured `0.35` into `0.3499999940395355` in the generated
/// block. Reading it back as an f32 yields the same bits either way,
/// but only one of the two is a file a person can stand to read.
pub(crate) fn lisp_float32(v: f32) -> String {
    if v == v.trunc() && v.is_finite() {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// A number of seconds from Lisp as a `Duration`: fractions allowed;
/// negative, NaN, infinite and overflowing values are refused with an
/// error naming `what`, the keyword or defun that took it.
pub(crate) fn secs_duration(
    what: impl std::fmt::Display,
    secs: f64,
) -> Result<Duration, tulisp::Error> {
    Duration::try_from_secs_f64(secs).map_err(|_| {
        tulisp::Error::invalid_argument(format!(
            "{what} must be a non-negative number of seconds, got {secs}"
        ))
    })
}

#[derive(Clone)]
pub struct Config {
    /// The simulator state the interpreter drives. Everything that
    /// does not evaluate Lisp reads it directly; see
    /// [`crate::engine`].
    pub(crate) engine: Engine,
    /// Every file the loader has evaluated — boot scripts, runtime
    /// `(load …)`s, created microgrid files — canonicalized and
    /// dedup'd, in first-load order.
    ///
    /// Two jobs, neither of them "the reload list": it is the set of
    /// files worth WATCHING (a driver script that registers no
    /// microgrid still wants hot-reload), and it carries the ORDER
    /// [`Config::registered_sources`] replays in, since the registry
    /// is a BTreeMap and iterating it yields id order. What a
    /// whole-world reload replays is derived from the registry, not
    /// from here.
    pub(crate) source_files: Arc<Mutex<Vec<PathBuf>>>,
    pub(crate) ctx: SharedMut<TulispContext>,
    pub(crate) site: MicrogridSite,
    /// Additional files the config has registered via `(watch-file …)`.
    /// `Config::watch` adds each to the live notify watcher so edits to
    /// e.g. `sim/defaults.lisp` trigger the same reload as edits to
    /// the entry-point config. Set semantics — duplicate registrations
    /// (from re-runs of the config during reload) are no-ops.
    pub(crate) extra_watches: Arc<Mutex<HashSet<PathBuf>>>,
    /// Multi-stage scenario registry — what `(define-scenario …)`
    /// writes to and what the UI's Scenarios mode + /api/scenarios
    /// read from. See `crate::sim::scenarios` for the data model.
    pub(crate) scenarios: crate::sim::scenarios::SharedScenarios,
    /// Dynamic site lookup the lisp defuns capture. Resolves to
    /// the current microgrid's site at call time, falling back
    /// to the first registry entry and finally to the bootstrap
    /// site allocated in `Config::new`. See
    /// [`crate::sim::microgrids::SiteRouter`].
    pub(crate) router: SharedSiteRouter,
    /// The microgrid file whose load is in flight, or `None` outside
    /// a load. `load_file` sets it for the duration of the eval so
    /// every `(make-microgrid …)` form the file runs knows which file
    /// it belongs to (and so a second file claiming a taken id is
    /// detectable). Ambient state like `current_microgrid` — only
    /// flipped under the interpreter lock. See
    /// [`crate::sim::microgrids::with_loading`].
    pub(crate) loading: crate::sim::microgrids::LoadingSlot,
    /// Active microgrid id, written by /api/mg/{id}/eval and the
    /// scenario per-microgrid replay. `None` defers to the
    /// router's fallback (first registry entry).
    pub(crate) current_microgrid: CurrentMicrogrid,
    /// tulisp-async timer handle. The Lisp refresh loop ticks it at
    /// 100 ms cadence to fire `(run-with-timer …)` / `(every …)`
    /// callbacks; `Config::refresh_once` ticks it synchronously for
    /// tests that drive ticks deterministically.
    pub(crate) timer_handle: tulisp_async::Handle,
    /// Hash of the bytes macrocosim itself last wrote to each file
    /// it manages (managed microgrid files, `enterprise.lisp`). The
    /// file watcher compares an event's file content against this
    /// map and ignores a match, so a save macrocosim performed does
    /// not bounce back as a reload of the file it just wrote.
    pub(crate) written_hashes: Arc<Mutex<HashMap<PathBuf, u64>>>,
    /// Per-microgrid undo / redo stacks over managed microgrid
    /// files. A structural edit pushes the block the file carried
    /// before it; `/api/mg/{id}/undo` pops it back. See
    /// [`crate::lisp::undo`].
    pub(crate) undo: undo::SharedUndo,
    /// Present only for a headless `Config` (built by
    /// [`Config::new_headless`]): the hand-advanced clock that drives
    /// the timer queue and `now`. `None` for a live `Config`, whose
    /// timers run on the wall clock and whose background loops tick it.
    pub(crate) sim_clock: Option<Arc<tulisp_async::ManualClock>>,
}

impl Config {
    /// The simulator state this interpreter drives.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Shared scenarios registry — `(define-scenario …)` writes
    /// here, the UI Scenarios mode + /api/scenarios read here, and
    /// the auto-advance task mutates the per-entry runtime state.
    pub fn scenarios(&self) -> crate::sim::scenarios::SharedScenarios {
        self.scenarios.clone()
    }

    /// See [`Engine::import_lock`].
    pub(crate) fn import_lock(&self) -> Arc<tokio::sync::Mutex<()>> {
        self.engine.import_lock()
    }

    /// See [`Engine::create_lock`].
    pub(crate) fn create_lock(&self) -> Arc<tokio::sync::Mutex<()>> {
        self.engine.create_lock()
    }

    /// See [`Engine::microgrids`].
    pub fn microgrids(&self) -> crate::sim::microgrids::SharedMicrogrids {
        self.engine.microgrids()
    }

    /// See [`Engine::dispatches`].
    pub fn dispatches(&self) -> crate::sim::dispatch::SharedDispatchStore {
        self.engine.dispatches()
    }

    /// See [`Engine::id_allocator`].
    pub fn enterprise_id_allocator(&self) -> Arc<std::sync::atomic::AtomicU64> {
        self.engine.id_allocator()
    }

    /// See [`Engine::port_pins`].
    pub fn port_pins(&self) -> crate::sim::microgrids::PortPins {
        self.engine.port_pins()
    }

    /// A headless `Config` is stepped by `sim_step` on its own clock.
    pub fn is_headless(&self) -> bool {
        self.sim_clock.is_some()
    }

    /// See [`Engine::subscribe_microgrid_registered`].
    pub fn subscribe_microgrid_registered(&self) -> broadcast::Receiver<u64> {
        self.engine.subscribe_microgrid_registered()
    }

    /// Mutable handle on the active microgrid id. Per-microgrid
    /// HTTP routes (`/api/mg/{id}/eval` and friends) flip this via
    /// `with_microgrid` so the lisp defuns + the override-file
    /// path resolve to the URL's microgrid.
    pub fn current_microgrid_handle(&self) -> CurrentMicrogrid {
        self.current_microgrid.clone()
    }

    /// See [`Engine::clock_handle`].
    pub fn clock_handle(&self) -> crate::sim::clock::SharedClock {
        self.engine.clock_handle()
    }

    /// Clone of the tulisp interpreter handle. Exposed so the scenario
    /// runner (todo §J2) can funcall scenario section forms from
    /// outside `lisp::Config::eval`; everything else inside the
    /// crate should reach for `eval` / `eval_silent` instead.
    pub fn interpreter(&self) -> SharedMut<TulispContext> {
        self.ctx.clone()
    }

    /// Every name the interpreter defines, with what tulisp's `describe` says
    /// of it, in no particular order.
    pub fn symbols(&self) -> Vec<(String, tulisp::symbols::SymbolInfo)> {
        let ctx = self.ctx.borrow();
        ctx.symbols()
            .map(|(name, info)| (name.to_string(), info))
            .collect()
    }

    /// See [`Engine::metadata`].
    pub fn metadata(&self) -> Metadata {
        self.engine.metadata()
    }

    /// See [`Engine::assets_socket_addr`].
    pub fn assets_socket_addr(&self) -> String {
        self.engine.assets_socket_addr()
    }

    /// See [`Engine::dispatch_socket_addr`].
    pub fn dispatch_socket_addr(&self) -> String {
        self.engine.dispatch_socket_addr()
    }

    /// See [`Engine::reserved_ports`].
    pub fn reserved_ports(&self) -> Vec<u16> {
        self.engine.reserved_ports()
    }

    /// Resolve the site the ambient scope points at. Follows
    /// `current_microgrid`, whose contract requires the interpreter
    /// lock — callers outside an eval (HTTP handlers) resolve a
    /// microgrid's site from the registry instead.
    pub fn site(&self) -> MicrogridSite {
        self.router.site()
    }

    /// The bootstrap site legacy single-site configs run on — see
    /// [`crate::sim::microgrids::SiteRouter::bootstrap_site`].
    pub fn bootstrap_site(&self) -> MicrogridSite {
        self.router.bootstrap_site()
    }

    /// See [`Engine::tz_name`].
    pub fn tz_name(&self) -> &'static str {
        self.engine.tz_name()
    }

    /// See [`Engine::state_dir`].
    pub fn state_dir(&self) -> &std::path::Path {
        self.engine.state_dir()
    }

    /// Directory holding the managed per-microgrid `<id>.lisp`
    /// files, under the state dir. The HTTP create endpoint writes
    /// one here per microgrid it mints; restoring one after a restart
    /// is a manual `(load …)` / CLI argument — nothing scans this
    /// directory.
    pub fn microgrids_dir(&self) -> PathBuf {
        self.engine.state_dir.join("microgrids")
    }

    /// The enterprise-wide state file: enterprise id, timezone,
    /// request lifetime, both socket addresses and every `*-defaults`
    /// plist. Evaluated at boot before any microgrid file and
    /// regenerated by [`Config::persist_enterprise`].
    pub fn enterprise_path(&self) -> PathBuf {
        self.engine.state_dir.join("enterprise.lisp")
    }

    /// Remember that macrocosim wrote `content` to `path`, so the
    /// file watcher can tell its own save apart from a human edit.
    /// Keyed by the canonical path, which is what the watcher asks
    /// with.
    pub(crate) fn record_self_write(&self, path: &Path, content: &str) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.written_hashes
            .lock()
            .insert(path, content_hash(content));
    }

    /// Was `content` exactly what macrocosim last wrote to `path`?
    /// If so, FORGET that write and answer true. `path` must be
    /// canonical, the key [`Self::record_self_write`] stores.
    ///
    /// The file watcher asks this before reloading, so a save
    /// macrocosim performed does not bounce back as a reload. The
    /// answer is deliberately one-shot: a remembered hash that
    /// stayed remembered would go on suppressing forever, so an
    /// operator reverting the file to exactly that content by hand
    /// (an editor undo, a `git checkout --`) would be ignored and
    /// the world would stay on whatever it had drifted to. Forgetting
    /// on the first match costs at most one extra reload of content
    /// identical to what is already live — a no-op — and never a
    /// missed edit.
    pub(crate) fn take_self_write(&self, path: &Path, content: &str) -> bool {
        let mut hashes = self.written_hashes.lock();
        if hashes.get(path) == Some(&content_hash(content)) {
            hashes.remove(path);
            return true;
        }
        false
    }

    /// Remember that the loader evaluated `path`, keeping the order
    /// in which files first arrived. Called by the loader once it has
    /// STARTED evaluating a file — on success, and on a failure too,
    /// since a partly-evaluated file may have registered a microgrid
    /// or armed timers and is then part of the world. Whether the
    /// file registered a microgrid doesn't matter either: a driver
    /// script that only arms timers is worth watching and worth
    /// re-evaluating on its own. A file that never evaluated (missing,
    /// unreadable) is not recorded.
    /// See [`Config::source_files`].
    pub(crate) fn note_source_file(&self, path: &Path) {
        let mut files = self.source_files.lock();
        if !files.iter().any(|p| p == path) {
            files.push(path.to_path_buf());
        }
    }

    /// Drop `path` from the loader's record — for a file that is
    /// being deleted, so the watch set and the reload list stop
    /// naming a path that no longer exists.
    pub(crate) fn forget_source_file(&self, path: &Path) {
        // Canonicalized the way `note_source_file`'s caller recorded
        // it, which is why this runs while the file still exists.
        let canonical = path.canonicalize();
        let canonical = canonical.as_deref().unwrap_or(path);
        self.source_files.lock().retain(|p| p != canonical);
    }

    /// Every file the loader has evaluated, in first-load order.
    /// This is the *watch* set (plus `enterprise.lisp` and the
    /// `(watch-file …)` registrations) and the set of paths a single
    /// file edit may be reloaded on its own — not the whole-world
    /// reload list, which is [`Config::registered_sources`].
    pub(crate) fn loader_visited_files(&self) -> Vec<PathBuf> {
        self.source_files.lock().clone()
    }

    /// The ids of the microgrids `path` backs, ascending. Resolved
    /// and canonicalized the way the loader resolves it, so a
    /// state-dir-relative spelling (what the load endpoint receives)
    /// matches the absolute one the registry stores.
    pub fn microgrids_backed_by(&self, path: &Path) -> Vec<u64> {
        let resolved = self.resolve_in_state_dir(path);
        let canonical = resolved.canonicalize().unwrap_or(resolved);
        self.engine
            .microgrids
            .lock()
            .iter()
            .filter(|(_, e)| e.source.as_deref() == Some(canonical.as_path()))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Every file that backs at least one registered microgrid, each
    /// listed once, in the order the files first arrived.
    ///
    /// This IS the whole-world reload list: the world is the set of
    /// live microgrids, and each microgrid names the file it came
    /// from, so there is no replay list to drift out of sync with the
    /// registry. A file whose microgrids are all gone drops out by
    /// itself; a microgrid typed into the REPL has no source and
    /// contributes nothing to reload. A driver-only script is not
    /// here either — it is re-run when its own file changes, not
    /// when some other file does.
    pub(crate) fn registered_sources(&self) -> Vec<PathBuf> {
        let live: Vec<PathBuf> = self
            .engine
            .microgrids
            .lock()
            .values()
            .filter_map(|e| e.source.clone())
            .collect();
        let order = self.source_files.lock();
        let mut out: Vec<PathBuf> = order.iter().filter(|p| live.contains(p)).cloned().collect();
        // A source the loader never saw — the create endpoint writes
        // a file and registers its microgrid in one step, without
        // going through `load_file` — has no recorded order; append
        // it after the ones that do, in registry (id) order.
        for path in live {
            if !out.contains(&path) {
                out.push(path);
            }
        }
        out
    }
}
