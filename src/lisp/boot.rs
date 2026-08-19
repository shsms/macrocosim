//! `Config` bootstrap and lifecycle: build the interpreter, eval
//! the config file, spawn the long-lived loops (Lisp refresh + the
//! request-timeout sweep + scenario auto-advance), and the hot-
//! reload + tags-pass entry points.
//!
//! Everything in this file is an `impl Config { ... }` (or a free
//! helper Config relies on) so that the heavy bootstrap logic
//! doesn't sit in the parent module alongside the trivial getters.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{RecommendedWatcher, Watcher};
use parking_lot::{Mutex, RwLock};
use tokio::sync::broadcast;
use tulisp::{Error, SharedMut, TulispContext};

use crate::sim::MicrogridSite;
use crate::sim::microgrids::SiteRouter;

use super::{Config, Metadata, defuns};

impl Config {
    /// Build a config from one script file — the single-script
    /// convenience for embedders and tests. The state dir is the
    /// script's directory, which keeps journals / snapshots /
    /// runtime-created stubs next to the script — the entry-config
    /// contract this constructor has always had. (The binary's CLI
    /// goes through [`Config::new_with`], whose default is the cwd.)
    /// Returns the formatted lisp error on parse / eval failure —
    /// caller decides whether to panic (binary boot) or surface in
    /// the UI (hot reload). On error no background loops have been
    /// spawned and nothing is ticking (the binary spawns each site's
    /// physics only after a successful `new`); sites the failed eval
    /// already registered are dropped with the registry Arcs.
    pub fn new(filename: &str) -> Result<Self, String> {
        let scripts = vec![filename.to_string()];
        let state_dir = script_parent_dir(filename);
        Self::new_inner(scripts, state_dir, false)
    }

    /// Build a config from zero or more boot scripts. With none, the
    /// engine boots bare: empty registry, full DSL live via the
    /// embedded prelude — topologies then arrive at runtime through
    /// `(load …)` / the REPL / the HTTP create + import endpoints.
    ///
    /// `state_dir` anchors everything persistent (overrides journals,
    /// `snapshots/`, runtime-created microgrid stubs) and the
    /// relative-path resolution of `(load …)` / `(file-exists-p …)`.
    /// `None` falls back to the process cwd — one anchor regardless
    /// of where the scripts live, so a `(load …)` typed into the
    /// REPL resolves the same whether the world came from a boot
    /// script or a runtime load.
    pub fn new_with(scripts: &[String], state_dir: Option<PathBuf>) -> Result<Self, String> {
        let state_dir = state_dir.unwrap_or_else(|| PathBuf::from("."));
        Self::new_inner(scripts.to_vec(), state_dir, false)
    }

    /// Build a *headless* `Config` for deterministic, faster-than-real-
    /// time scenario runs. Timers and scenario time run on a hand-
    /// advanced [`ManualClock`](tulisp_async::ManualClock) (returned
    /// alongside), and the background loops (physics, lisp refresh, the
    /// request-timeout sweep, scenario auto-advance, the frequency
    /// driver) are NOT spawned — the caller steps the simulation itself
    /// via [`Config::sim_step`]. Used for CI scenario assertions.
    pub fn new_headless(filename: &str) -> Result<(Self, Arc<tulisp_async::ManualClock>), String> {
        let scripts = vec![filename.to_string()];
        let state_dir = script_parent_dir(filename);
        let cfg = Self::new_inner(scripts, state_dir, true)?;
        // A stepped run has no UI and no REPL — nothing can load a
        // topology later, so an empty registry here is a dead run,
        // not a bare boot waiting for input.
        if cfg.microgrids.lock().is_empty() {
            return Err("config loaded but no (make-microgrid …) form ran — \
                 a headless run needs its config to register a microgrid"
                .to_string());
        }
        let clock = cfg
            .sim_clock
            .clone()
            .expect("headless Config always has a sim clock");
        Ok((cfg, clock))
    }

    fn new_inner(scripts: Vec<String>, state_dir: PathBuf, headless: bool) -> Result<Self, String> {
        use std::sync::atomic::AtomicU64;
        let mut ctx = TulispContext::new();
        let enterprise_id_allocator =
            Arc::new(AtomicU64::new(crate::sim::component::FIRST_AUTO_ID));
        let site = MicrogridSite::with_id_allocator(enterprise_id_allocator.clone());
        let metadata = Arc::new(RwLock::new(Metadata::default()));
        let extra_watches = Arc::new(Mutex::new(HashSet::new()));
        let clock = crate::sim::clock::new_clock();
        let scenarios = crate::sim::scenarios::new_registry();
        let microgrids = crate::sim::microgrids::new_registry();
        let dispatches = crate::sim::dispatch::new_store();
        let current_microgrid = crate::sim::microgrids::new_current_microgrid();
        let router = SiteRouter::new(microgrids.clone(), current_microgrid.clone(), site.clone());
        // Capacity = 1024 to absorb a mass-create burst (e.g. a
        // script POST'ing /api/microgrids/create a few hundred times
        // back-to-back) without lagging the WS event pump's
        // receiver. Even on Lagged the pump re-snapshots the
        // registry and back-fills forwarders, so capacity tuning is
        // belt-and-suspenders — but a fresh subscriber spinning up
        // mid-burst still benefits from the extra slack.
        let microgrid_registered = Arc::new(broadcast::channel(1024).0);
        // Enterprise-wide grid frequency state — one OU process drives
        // every MicrogridSite in the registry so they share the
        // physically-correct same frequency. The driver task is
        // spawned below; bootstrap site + future make-microgrid forms
        // both attach to this slot.
        let grid_frequency = crate::sim::frequency::new_shared();
        site.set_grid_frequency(grid_frequency.clone());
        // The wall-clock background loops (frequency driver, timeout
        // sweep, lisp refresh) are spawned only after the config
        // evals successfully, below — a failed `Config::new` must
        // not leave orphan loops ticking. A headless run never
        // spawns them: it drives every tick itself, and the loops
        // would race the stepped driver.

        // tulisp canonicalizes the load path, which requires the
        // directory to exist — create it up front so a fresh
        // `--state-dir` fails with a clear message (every other
        // consumer create_dir_all's lazily anyway).
        std::fs::create_dir_all(&state_dir)
            .map_err(|e| format!("state dir {} cannot be created: {e}", state_dir.display()))?;
        ctx.set_load_path(Some(&state_dir))
            .map_err(|e| format!("set_load_path({}): {e}", state_dir.display()))?;

        // Headless builds run the timer queue + scenario time on a
        // hand-advanced ManualClock so a scenario can be stepped
        // deterministically and faster than real time; live builds use
        // the wall clock and the background loops below.
        let sim_clock = if headless {
            Some(Arc::new(tulisp_async::ManualClock::new()))
        } else {
            None
        };
        let now = match &sim_clock {
            Some(clock) => crate::sim::sim_clock::NowSource::sim(
                crate::sim::sim_clock::headless_base(),
                clock.clone(),
            ),
            None => crate::sim::sim_clock::NowSource::wall(),
        };

        defuns::register_runtime(
            &mut ctx,
            router.clone(),
            metadata.clone(),
            state_dir.clone(),
            microgrids.clone(),
            now.clone(),
        );
        defuns::register_clock(&mut ctx, clock.clone());
        defuns::register_watches(&mut ctx, state_dir.clone(), extra_watches.clone());
        defuns::register_scenarios(&mut ctx, scenarios.clone());
        defuns::register_microgrids(
            &mut ctx,
            microgrids.clone(),
            router.clone(),
            current_microgrid.clone(),
            enterprise_id_allocator.clone(),
            microgrid_registered.clone(),
            grid_frequency.clone(),
        );
        defuns::register_frequency(&mut ctx, grid_frequency.clone());

        // tulisp-async gives the config DSL access to run-with-timer,
        // cancel-timer, sleep-for and friends, used to drive
        // *environment* animation (per-tick voltage / frequency
        // perturbations, scheduled events). Component logic stays in
        // Rust; lisp's only job is wiring + scripting the site
        // around it. Must be called inside a tokio runtime —
        // TokioExecutor::new captures Handle::current().
        //
        // The returned `Handle` is what the dedicated
        // `spawn_lisp_refresh_loop` task ticks at 100 ms cadence
        // to fire pending timer firings. Without it the mailbox
        // would just accumulate.
        let executor = Arc::new(tulisp_async::TokioExecutor::new());
        let timer_handle = match &sim_clock {
            Some(clock) => tulisp_async::register_with_clock(&mut ctx, executor, clock.clone()),
            None => tulisp_async::register(&mut ctx, executor),
        };

        // Embedded scenario DSL prelude. The vocabulary a config needs —
        // `at` / `check` / `controller` / `drive-*` / `timeline` /
        // `define-controller`, the `make-*` defaults, and the `scenario--run`
        // runner — lives in three lisp files that config.lisp `(load …)`s
        // today; bake them in so a binary shipping no lisp still has them.
        // Loaded after the Rust defuns + tulisp-async (so `every` /
        // `run-with-timer` exist) and before the user config, so its
        // `(define-scenario …)` / `(make-* …)` forms resolve. Order matters:
        // common before scenarios (the runner calls `define-controller`).
        // Additive — a config that still `(load …)`s these files just
        // redefines them on top.
        //
        // TODO: once tulisp supports embedding files (an eval-with-filename
        // path), evaluate each file under its real path so a prelude error
        // cites the real source file rather than the shared `<eval_string>`
        // bucket (todo §D9).
        for src in [
            include_str!("../../sim/common.lisp"),
            include_str!("../../sim/defaults.lisp"),
            include_str!("../../sim/scenarios.lisp"),
        ] {
            if let Err(e) = ctx.eval_string(src) {
                return Err(format!(
                    "embedded prelude failed to load: {}",
                    e.format(&ctx)
                ));
            }
        }

        for script in &scripts {
            if let Err(e) = ctx.eval_file(script) {
                let formatted = e.format(&ctx);
                log::error!("Tulisp error in {script}:\n{formatted}");
                return Err(formatted);
            }
        }
        warn_orphaned_chp_defaults(&mut ctx);

        // An empty registry is a legitimate live state: a bare boot
        // (no scripts) serves the UI + REPL and topologies arrive on
        // demand via `(load …)` or the create/import endpoints. Warn
        // when scripts WERE given and registered nothing — that is
        // almost certainly a config mistake, but with the UI up the
        // user can still recover interactively. The headless path
        // hard-errors instead (see `new_headless`).
        if microgrids.lock().is_empty() {
            if scripts.is_empty() {
                log::info!(
                    "bare boot: no microgrids registered — load a script from \
                     the UI or REPL, e.g. (load \"examples/berlin-demo.lisp\")"
                );
            } else {
                log::warn!(
                    "boot scripts registered no microgrids — no gRPC servers \
                     will be up until a (make-microgrid …) form runs"
                );
            }
        }

        // Validate every registered site, not the (always-empty)
        // bootstrap: components live in the per-mg sites created by
        // `(make-microgrid …)`. The UI reads validation per request
        // via /api/topology; these calls exist for the boot log.
        for (id, entry) in microgrids.lock().iter() {
            log_topology_validation(&entry.site, &format!("boot (microgrid {id})"));
        }

        // Wall-clock background loops, spawned only now that the
        // config evaluated: an eval error returns above, and loops
        // spawned earlier would keep ticking (and keep the registry
        // Arcs alive) with no handle to stop them.
        if !headless {
            crate::sim::frequency::spawn_driver(grid_frequency.clone());
            // One-per-process loop that walks every registered
            // MicrogridSite's TimeoutTracker and calls reset_setpoint
            // on each elapsed entry. Both gRPC's
            // SetElectricalComponentPower and the Lisp
            // `(set-active-power …)` defun add to the tracker; this
            // loop is what makes their request-lifetime semantics
            // visible.
            Self::start_timeout_loop(microgrids.clone());
        }

        let ctx = SharedMut::new(ctx);

        // Lisp refresh loop. One tokio task at 100 ms cadence holds
        // the interpreter lock once per pass, walks every registered
        // microgrid's components calling `refresh_inputs` (which
        // re-resolves any lambda-bound `:power` / `:sunlight%` / …
        // into `DynamicScalar`'s atomic), and drains the
        // tulisp-async timer mailbox so `(every …)` / `(run-with-
        // timer …)` callbacks fire.
        //
        // Decoupling this from the per-site physics tick means:
        //  - Physics ticks are lock-free; a long-running /api/eval
        //    no longer stalls every microgrid's per-second physics.
        //  - The refresh ticks at its own cadence (100 ms by
        //    default), so lambda-bound inputs lag at most one
        //    refresh interval behind their underlying lisp source.
        //    For a 15-min sine curve that's a 0.005% phase shift —
        //    negligible.
        //  - `Config::refresh_once` exposes the same work
        //    synchronously for tests that drive `tick_once` and
        //    expect the lambda result to be visible immediately.
        if !headless {
            Self::spawn_lisp_refresh_loop(microgrids.clone(), ctx.clone(), timer_handle.clone());
        }
        // The scenario runners (todo §J2) replace the old day-stage
        // auto-advance task; until they land, registered scenarios are
        // introspectable but not yet runnable from the registry.

        Ok(Self {
            // Canonicalized so the same file spelled differently
            // (relative argv vs an absolute runtime (load …)) dedups
            // to one replay entry; the scripts eval'd above, so they
            // exist and canonicalize cleanly.
            loaded_files: Arc::new(Mutex::new(
                scripts
                    .iter()
                    .map(|s| {
                        let p = PathBuf::from(s);
                        p.canonicalize().unwrap_or(p)
                    })
                    .collect(),
            )),
            state_dir,
            ctx,
            site,
            metadata,
            extra_watches,
            clock,
            scenarios,
            microgrids,
            dispatches,
            router,
            current_microgrid,
            enterprise_id_allocator,
            import_lock: Arc::new(tokio::sync::Mutex::new(())),
            microgrid_registered,
            timer_handle,
            now,
            sim_clock,
        })
    }

    /// Trigger one refresh + timer-drain pass synchronously. Mirrors
    /// what the background loop does once per 100 ms, but on the
    /// caller's thread — tests reach for this when they need a
    /// `(run-with-timer 0 …)` fire to be visible before the next
    /// `tick_once`, or a lambda-bound `:power` value to resolve
    /// before reading `aggregate_power_w`.
    ///
    /// Acquires the interpreter lock, walks every registered
    /// microgrid's components calling `refresh_inputs`, then drains
    /// the timer mailbox once. Tests that drive `tick_once` directly
    /// call this first so lambda-bound `:power` / `:sunlight%` /
    /// `(run-with-timer 0 …)` values are visible before the synthetic
    /// physics tick.
    pub fn refresh_once(&self) {
        let mut guard = self.ctx.borrow_mut();
        let sites: Vec<MicrogridSite> = self
            .microgrids
            .lock()
            .values()
            .map(|e| e.site.clone())
            .collect();
        for site in sites {
            for c in site.components().iter() {
                c.refresh_inputs(&mut guard);
            }
        }
        self.timer_handle.tick(&mut guard);
    }

    /// Advance a headless simulation by `dt`: move the sim clock
    /// forward, re-resolve lisp-driven inputs + fire now-due timers
    /// ([`refresh_once`](Self::refresh_once)), then tick physics on
    /// every site at the new sim-time. Deterministic and as fast as the
    /// caller loops it — no real-time sleeping. No-op (logged) on a live
    /// `Config` (one built by [`Config::new`] rather than
    /// [`Config::new_headless`]).
    pub fn sim_step(&self, dt: Duration) {
        let Some(clock) = self.sim_clock.as_ref() else {
            log::warn!("Config::sim_step called on a live Config; ignored");
            return;
        };
        clock.advance(dt);
        // Timers + dynamic inputs first (a `timeline` source reads
        // scenario-elapsed, now at the advanced sim-time), then physics.
        self.refresh_once();
        let now = self.now.now();
        let sites: Vec<MicrogridSite> = self
            .microgrids
            .lock()
            .values()
            .map(|e| e.site.clone())
            .collect();
        for site in sites {
            site.tick_once(now, dt);
        }
    }

    /// Step a headless simulation to `until` sim-time in `dt` increments.
    /// Returns the number of steps taken. No-op on a live `Config`.
    pub fn sim_run(&self, until: Duration, dt: Duration) -> u64 {
        if self.sim_clock.is_none() {
            log::warn!("Config::sim_run called on a live Config; ignored");
            return 0;
        }
        let mut steps = 0;
        let mut elapsed = Duration::ZERO;
        while elapsed < until {
            self.sim_step(dt);
            elapsed += dt;
            steps += 1;
        }
        steps
    }

    /// Run a registered scenario on the headless sim clock for its
    /// declared `:length`, stepping by `dt`. Returns the number of
    /// steps. The stepped runner (todo §J2): compiles the scenario via
    /// the Lisp `scenario--run` (so its cue / check timers fire on the
    /// sim clock) then drives `sim_run`, deterministically and faster
    /// than real time. Pair with `scenario-expect` + `scenario report
    /// --assert` for a CI gate. Errors on a live `Config`, an unknown
    /// scenario, or one without a `:length`.
    pub fn run_scenario_stepped(&self, name: &str, dt: Duration) -> Result<u64, String> {
        if self.sim_clock.is_none() {
            return Err("run_scenario_stepped requires a headless Config".to_string());
        }
        let length_s = self
            .scenarios
            .lock()
            .get(name)
            .ok_or_else(|| format!("no scenario named {name:?}"))?
            .length_s
            .ok_or_else(|| format!("scenario {name:?} has no :length for a stepped run"))?;
        crate::sim::scenarios::start(&self.ctx, &self.scenarios, name)?;
        Ok(self.sim_run(Duration::from_secs_f64(length_s), dt))
    }

    /// The current microgrid's scenario report (peak / charge / SoC
    /// stats + the `scenario-expect` pass/fail ledger) at sim/wall now,
    /// serialized to JSON — the same shape `GET /api/scenario/report`
    /// returns. Public + JSON so an out-of-crate stepped runner
    /// (`swctl scenario run --stepped`) can print + assert on it
    /// without a server or exposing the internal report type.
    pub fn scenario_report_json(&self) -> serde_json::Value {
        serde_json::to_value(self.site().scenario_report(self.now.now())).unwrap_or_default()
    }

    /// Spawn the Lisp refresh + timer-drain loop. Runs at 100 ms
    /// cadence on its own tokio task; sole acquirer of the
    /// interpreter lock for refresh purposes (eval still contends
    /// at the same lock, but only when it's not held by us). See
    /// the comment block in `Config::new` for the design rationale.
    fn spawn_lisp_refresh_loop(
        registry: crate::sim::microgrids::SharedMicrogrids,
        ctx: SharedMut<TulispContext>,
        timer_handle: tulisp_async::Handle,
    ) {
        tokio::spawn(async move {
            // First tick at +100 ms so tests that boot a Config +
            // drive `tick_once` synchronously don't race the loop.
            let start = tokio::time::Instant::now() + Duration::from_millis(100);
            let mut tick = tokio::time::interval_at(start, Duration::from_millis(100));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                // Snapshot the per-mg sites outside the ctx lock, then
                // take the lock once and run the full refresh pass.
                // A long /api/eval grabbing the same lock will delay
                // this iteration but won't block physics: each site's
                // `spawn_physics` task ticks lock-free against the
                // atomics last published by `refresh_inputs`.
                let sites: Vec<MicrogridSite> =
                    registry.lock().values().map(|e| e.site.clone()).collect();
                let mut guard = ctx.borrow_mut();
                for site in &sites {
                    for c in site.components().iter() {
                        c.refresh_inputs(&mut guard);
                    }
                }
                timer_handle.tick(&mut guard);
            }
        });
    }

    fn start_timeout_loop(registry: crate::sim::microgrids::SharedMicrogrids) {
        tokio::spawn(async move {
            // `interval` + `Skip` keeps the cadence on the nominal
            // 100 ms grid even when one iteration overruns (a Lisp
            // reset_setpoint that grabs the interpreter lock against
            // a long /api/eval can take real time). The previous
            // `sleep(100ms)` drifted upward under load — each
            // iteration's clock started AFTER the work finished.
            //
            // `interval_at` rather than `interval` so the *first*
            // tick lands at +100 ms instead of immediately. Tests
            // that arm a deadline + check `drain_expired_timeouts`
            // synchronously rely on the BG task not racing them at
            // t=0.
            let start = tokio::time::Instant::now() + Duration::from_millis(100);
            let mut tick = tokio::time::interval_at(start, Duration::from_millis(100));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                // Snapshot the per-mg sites under the lock, then drain
                // outside the lock so a slow component callback can't
                // hold registry-wide reads.
                let sites: Vec<MicrogridSite> =
                    registry.lock().values().map(|e| e.site.clone()).collect();
                for site in sites {
                    for (id, axis) in site.drain_expired_timeouts() {
                        log::info!(
                            "Request timeout for component {id} ({axis:?}) — resetting that axis"
                        );
                        if let Some(c) = site.get(id) {
                            c.reset_setpoint_axis(axis);
                        }
                    }
                }
            }
        });
    }

    /// Build a TAGS table for every file in `roots` and every
    /// file each transitively `(load …)`s. Drives tulisp's
    /// parse-with-etags path: every `(defun NAME …)` form across
    /// the file tree becomes one entry, and every Rust-side
    /// `ctx.defun("name", …)` call from `register_runtime` /
    /// `tulisp_async::register` adds an entry pointing at the
    /// Rust source location — so `M-.` on `(set-meter-power …)`
    /// or `(run-with-timer …)` jumps straight into the Rust
    /// implementation.
    ///
    /// Static, but must run inside a tokio runtime —
    /// `tulisp_async::TokioExecutor::new` captures
    /// `Handle::current()`. The etags binary wraps `main` with
    /// `#[tokio::main]` for that.
    ///
    /// The load path is set from the first root's parent
    /// directory (the canonical config); roots beyond the first
    /// can `(load …)` files relative to it just like config.lisp
    /// would.
    pub fn tags_table(roots: &[&str]) -> Result<String, Error> {
        let mut ctx = TulispContext::new();
        let site = MicrogridSite::new();
        let metadata = Arc::new(RwLock::new(Metadata::default()));
        // Throwaway router for the TAGS pass — no microgrids
        // registered, so SiteRouter::site falls through to the
        // bootstrap site and every defun captures that one.
        let microgrids = crate::sim::microgrids::new_registry();
        let current = crate::sim::microgrids::new_current_microgrid();
        let router = SiteRouter::new(microgrids.clone(), current, site.clone());

        let load_dir: PathBuf = roots
            .first()
            .and_then(|r| Path::new(r).parent())
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        ctx.set_load_path(Some(&load_dir))
            .map_err(|e| Error::os_error(format!("set_load_path({}): {e}", load_dir.display())))?;

        defuns::register_runtime(
            &mut ctx,
            router,
            metadata,
            load_dir,
            microgrids,
            crate::sim::sim_clock::NowSource::wall(),
        );
        // The Handle is unused here — tags_table is a one-shot parse
        // pass, no timers ever fire — but `register` still installs
        // the four builtins so that `(run-with-timer …)` etc. show up
        // in the generated TAGS file.
        let _ = tulisp_async::register(&mut ctx, Arc::new(tulisp_async::TokioExecutor::new()));

        ctx.tags_table(Some(roots))
    }

    /// Reset every site and timer, then replay the loaded-file list
    /// in load order. Returns the formatted lisp error on failure —
    /// files before the failing one have been rebuilt, the failing
    /// and later files' microgrids stay reset-empty; the next
    /// successful reload converges the whole world again.
    pub fn reload(&self) -> Result<(), String> {
        let mut ctx = self.ctx.borrow_mut();
        self.reload_locked(&mut ctx)
    }

    /// `reload` body against an already-held interpreter guard — for
    /// callers (e.g. a scoped overrides-file replace) that must hold
    /// the lock across surrounding work; re-borrowing inside would
    /// deadlock.
    pub(super) fn reload_locked(&self, ctx: &mut TulispContext) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        let start = std::time::Instant::now();
        self.site.reset();
        // Reset the enterprise-wide id allocator too — every site
        // is about to be rebuilt by the re-eval of config.lisp,
        // and we want auto-allocated ids to keep starting at
        // FIRST_AUTO_ID across reloads (the comment on
        // MicrogridSite.next_id justifies why).
        self.enterprise_id_allocator
            .store(crate::sim::component::FIRST_AUTO_ID, Ordering::Relaxed);
        // Keep the registry: the per-mg runtimes (physics tick, history
        // sampler, gRPC server, loopback client) each hold their entry's
        // site handle, and the re-eval'd (make-microgrid …) forms reuse
        // those sites in place — dropping the entries would orphan every
        // runtime on a site the registry no longer hands out. Reset each
        // site up front so a microgrid the new config no longer declares
        // ends up empty (its runtimes keep running against the empty
        // site — they can't be torn down without a restart — but it
        // stops ticking stale components).
        for entry in self.microgrids.lock().values() {
            entry.site.reset();
        }
        // Cancel every live timer centrally before the replay: each
        // replayed file re-registers its own `every` blocks, so this
        // is what keeps timer hygiene ORDER-INDEPENDENT. A per-script
        // `(cancel-timers)` call could not be: replayed after another
        // script, it would cancel that script's just-re-registered
        // timers, permanently freezing the earlier world's animation.
        // Top-level eval_string here is fine — no caller of reload
        // sits inside an eval frame (the 0.29.0 hazard is re-entrant
        // eval_string only).
        if let Err(e) = ctx.eval_string("(cancel-timers)") {
            log::warn!("reload: cancel-timers failed: {}", e.format(ctx));
        }
        // Replay every loaded file in load order: the boot scripts
        // plus everything that arrived at runtime — `(load …)` evals
        // and the stubs the create/import endpoints wrote. The world
        // is the sum of loaded files, not of one entry config, so the
        // replay list is the source of truth here. A file that
        // vanished since it was loaded is skipped with a warning (its
        // microgrids stay reset-empty) rather than aborting the whole
        // reload.
        let files: Vec<PathBuf> = self.loaded_files.lock().clone();
        for file in &files {
            if !file.exists() {
                log::warn!(
                    "reload: {} no longer exists; skipping (its microgrids stay empty)",
                    file.display()
                );
                continue;
            }
            if let Err(e) = ctx.eval_file(&file.to_string_lossy()) {
                let formatted = e.format(ctx);
                log::error!("Tulisp error in {}:\n{formatted}", file.display());
                return Err(formatted);
            }
        }
        warn_orphaned_chp_defaults(ctx);
        if self.microgrids.lock().is_empty() {
            log::warn!("reload left the registry empty — no loaded file registers a microgrid");
        }
        // Tell UI subscribers the MicrogridSite rebuilt. Catches the
        // "removed the only pending entry" case where remove_pending
        // reloads but has no surviving entries to bump-version
        // through eval_with_affects. Bump every registered microgrid
        // so per-mg UI subscribers all see the rebuild — the router-
        // resolved `cfg.site()` reads from the first registry entry,
        // not the bootstrap site we reset above.
        for (id, entry) in self.microgrids.lock().iter() {
            log_topology_validation(&entry.site, &format!("reload (microgrid {id})"));
            entry.site.bump_version();
        }
        log::info!(
            "Reloaded config in {:.1}ms",
            start.elapsed().as_secs_f64() * 1000.0
        );
        Ok(())
    }

    pub async fn watch(self) {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        // notify can fail at construction (out of inotify slots —
        // `fs.inotify.max_user_watches` exhausted by an IDE running
        // alongside) or at watch-registration time (file vanished).
        // Either case kills hot-reload but the rest of the binary
        // should keep serving; log and bail out of just this task.
        let mut watcher = match RecommendedWatcher::new(
            move |res| {
                futures::executor::block_on(async {
                    let _ = tx.send(res).await;
                });
            },
            notify::Config::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                log::error!("watch: notify init failed: {e}; hot-reload disabled");
                return;
            }
        };
        // Watch the boot scripts plus every `(watch-file …)`
        // registration. Snapshotted now; files loaded at runtime take
        // effect on the next process restart (the live notify watcher
        // isn't held across reloads, by design — keeps the watch
        // lifecycle simple; `(watch-file PATH)` in a script opts it
        // in explicitly).
        let mut watched = 0usize;
        let mut requested = 0usize;
        let boot_files: Vec<PathBuf> = self.loaded_files.lock().clone();
        for path in boot_files.iter().chain(self.extra_watches.lock().iter()) {
            requested += 1;
            match watcher.watch(path, notify::RecursiveMode::NonRecursive) {
                Ok(()) => watched += 1,
                Err(e) => log::warn!("watch {}: {}", path.display(), e),
            }
        }
        if watched == 0 {
            if requested == 0 {
                log::info!("watch: nothing to watch (bare boot); hot-reload idle");
            } else {
                log::error!("watch: no watch could be registered; hot-reload disabled");
            }
            return;
        }

        // Debounce window. Editors typically fire several notify
        // events for a single save (write + close-after-write +
        // plugin reformat); we coalesce anything arriving within
        // this window into one reload. 150 ms is comfortably above
        // the inotify event-batch latency on a busy machine and
        // still feels instant to a human editing.
        const DEBOUNCE: Duration = Duration::from_millis(150);

        while let Some(res) = rx.recv().await {
            let event = match res {
                Ok(e) => e,
                Err(e) => {
                    log::error!("watch error: {:?}", e);
                    return;
                }
            };
            if !matches!(event.kind, notify::EventKind::Modify(_)) {
                continue;
            }
            // After the first Modify, drain any further events that
            // arrive within DEBOUNCE; each additional event restarts
            // the window. Once the window goes quiet, fire one reload.
            // Reload errors are logged by `reload()` and surfaced on
            // the site event bus so the UI can show a banner; the
            // loop intentionally keeps going so a typo doesn't kill
            // the live-edit feedback path.
            loop {
                match tokio::time::timeout(DEBOUNCE, rx.recv()).await {
                    Ok(Some(Ok(_))) => continue,
                    Ok(Some(Err(e))) => {
                        log::error!("watch error: {:?}", e);
                        return;
                    }
                    Ok(None) => return,
                    Err(_) => break,
                }
            }
            if let Err(msg) = self.reload() {
                // Fan out to every REGISTRY site: the WS event pump
                // only spawns forwarders for registry entries, so a
                // broadcast on the bootstrap site's bus would reach
                // no browser. A reload failure concerns every
                // microgrid (all were reset), so every view gets it.
                for entry in self.microgrids.lock().values() {
                    entry.site.broadcast_config_error(msg.clone());
                }
            }
        }
    }
}

/// `chp-defaults` was folded into `marker-defaults` when CHP became
/// a marker category. A persisted overrides file (or a hand-written
/// config) that still sets it evals fine, but `make-chp` no longer
/// reads it — warn instead of silently dropping the customization.
fn warn_orphaned_chp_defaults(ctx: &mut TulispContext) {
    if let Ok(v) = ctx.eval_string("(and (boundp 'chp-defaults) chp-defaults)")
        && !v.null()
    {
        log::warn!(
            "chp-defaults is set but no longer read: CHP defaults \
             moved into marker-defaults. Put the customization there."
        );
    }
}

/// The script's directory, for the single-script constructors that
/// anchor state next to the script. `Path::parent()` returns
/// `Some("")` for bare filenames like "config.lisp" — tulisp rejects
/// empty paths, so those fall back to the cwd.
fn script_parent_dir(script: &str) -> PathBuf {
    Path::new(script)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Run the component-graph validator on the current `MicrogridSite` and
/// log the outcome. `phase` is one of "boot" / "reload" so the log
/// line tags which path triggered the check.
///
/// Log-only, not fatal. Empty worlds (no components yet) skip
/// because the graph crate requires exactly one
/// `GridConnectionPoint` and rejects empty graphs — test fixtures
/// that wire up `Config` against `""` would otherwise fail.
/// Non-empty worlds that fail validation surface as a `log::warn!`
/// in the simulator log; the pulse bar's graph pill gets its ⚠ from
/// `/api/topology`, which validates the requested site per request.
///
/// On success the log line includes a one-line summary so a dev
/// reading the log can confirm switchyard parsed the topology the
/// same way `frequenz-microgrid` would.
fn log_topology_validation(site: &MicrogridSite, phase: &str) {
    let (nodes, edges) = crate::sim::graph_adapter::snapshot(site);
    let visible_count = nodes.len();
    if visible_count == 0 {
        log::debug!("graph: {phase} skipped (no visible components)");
        return;
    }
    match crate::sim::graph_adapter::build_from(nodes, edges) {
        Ok(graph) => {
            // `graph.components()` yields nodes that survived
            // pass-through elision. With no pass-through categories
            // in switchyard's model yet this equals visible_count;
            // we log both so the gap is visible the day we add a
            // transformer / breaker / converter.
            let logical_count = graph.components().count();
            log::info!(
                "graph: {phase} validated ({visible_count} visible, {logical_count} after pass-through elision)"
            );
        }
        Err(e) => {
            log::warn!(
                "graph: {phase} validation failed — {visible_count} visible components rejected by frequenz-microgrid-component-graph: {e}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Config;
    use super::super::test_support::{config_with, next_unique};

    /// A bare boot (no scripts) is a legitimate live state: empty
    /// registry, DSL live, topologies arrive on demand.
    #[test]
    fn bare_boot_has_empty_registry() {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "switchyard-bare-{}-{}",
            std::process::id(),
            next_unique(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cfg = rt
            .block_on(async { Config::new_with(&[], Some(dir)) })
            .expect("bare boot must succeed");
        std::mem::forget(rt);
        assert!(cfg.microgrids().lock().is_empty());
        // The DSL is live: a runtime eval can still build a world.
        let out = cfg
            .eval("(make-microgrid :id 7 :grpc-port 8807 :topology (lambda () nil))")
            .expect("runtime make-microgrid");
        assert!(!out.is_empty());
        assert!(cfg.microgrids().lock().contains_key(&7));
    }

    /// Reload replays the loaded-file list, so a file recorded after
    /// boot (a runtime `(load …)` or a create-endpoint stub) brings
    /// its microgrid back instead of being forgotten.
    #[test]
    fn reload_replays_recorded_files() {
        let (cfg, dir) = config_with("nil");
        let extra = dir.join("extra-mg.lisp");
        std::fs::write(
            &extra,
            "(make-microgrid :id 42 :grpc-port 8842 :topology (lambda () nil))",
        )
        .unwrap();
        cfg.record_loaded_file(extra);
        cfg.reload().expect("reload");
        let reg = cfg.microgrids();
        let reg = reg.lock();
        assert!(reg.contains_key(&42), "recorded file must be replayed");
        assert!(reg.len() >= 2, "boot script's microgrid must survive too");
    }

    /// `Config::new` returns Err on lisp eval failure rather than
    /// silently logging — the binary panics with a useful message
    /// and tests get a clear assertion target rather than a
    /// half-built MicrogridSite.
    #[test]
    fn config_new_returns_err_on_bad_lisp() {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "switchyard-cfg-bad-{}-{}",
            std::process::id(),
            next_unique(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.lisp");
        std::fs::write(&path, "(this-is-not-a-defun-anywhere 42)").unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let res = rt.block_on(async { Config::new(path.to_str().unwrap()) });
        std::mem::forget(rt);
        let err = match res {
            Ok(_) => panic!("expected lisp error for undefined fn"),
            Err(e) => e,
        };
        assert!(
            err.contains("this-is-not-a-defun-anywhere"),
            "error should name the offending symbol: {err}",
        );
    }

    /// A headless `Config` runs physics, timers, and scenario time on a
    /// hand-advanced sim clock: `sim_run` drives a 60 s scenario in
    /// near-zero wall time, the timer fires at sim t=30, and the battery
    /// SoC integrates on sim-time (3.6 kW into 1 kWh for 60 s = +6%).
    #[test]
    fn headless_sim_runs_physics_and_timers_on_sim_time() {
        use std::time::Duration;
        let body = "(set-enterprise-id 1)
(make-microgrid :id 9 :grpc-port 18901 :topology
  (lambda ()
    (%make-grid-connection-point :id 1
      :successors (list (%make-meter :id 2
        :successors (list (%make-battery-inverter :id 3 :rated-lower -5000.0 :rated-upper 5000.0
          :successors (list (%make-battery :id 4 :rated-lower -5000.0 :rated-upper 5000.0
            :capacity 1000.0 :initial-soc 50.0)))))))))
(setq fired 0)
(run-with-timer 30 nil (lambda () (setq fired 1)))
(scenario-start \"sim\")
(set-active-power 3 3600.0 600000)";
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "switchyard-headless-{}-{}",
            std::process::id(),
            next_unique(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.lisp");
        std::fs::write(&path, body).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (cfg, _clock) = rt
            .block_on(async { Config::new_headless(path.to_str().unwrap()) })
            .expect("headless config builds");
        std::mem::forget(rt);

        let start = std::time::Instant::now();
        let steps = cfg.sim_run(Duration::from_secs(60), Duration::from_secs(1));
        let wall = start.elapsed();

        assert_eq!(steps, 60);
        // 60 sim-seconds elapsed in near-zero wall time.
        let elapsed: f64 = cfg
            .eval_silent("(scenario-elapsed)")
            .unwrap()
            .parse()
            .unwrap();
        assert!((elapsed - 60.0).abs() < 2.0, "scenario-elapsed = {elapsed}");
        assert!(wall < Duration::from_secs(5), "headless wall time {wall:?}");
        // Timer fired at sim t=30.
        assert_eq!(cfg.eval_silent("fired").unwrap(), "1");
        // Physics integrated SoC on sim-time: 50% -> ~56%. Asserted via
        // the scenario-expect check itself (I2), exercising the whole
        // stack.
        assert_eq!(
            cfg.eval_silent("(scenario-expect :component 4 :metric 'soc :approx 56.0 :tol 1.0)")
                .unwrap(),
            "t",
            "battery SoC should integrate on sim-time to ~56%",
        );
        // Energy accrues on the physics tick too, so an energy
        // scenario-expect resolves in a headless stepped run (regression:
        // it used to read the never-populated history layer and always
        // fail). Meter 2 imports 3600 W for ~60 s -> ~60 Wh.
        assert_eq!(
            cfg.eval_silent("(scenario-expect :component 2 :metric 'energy :min 40.0 :max 70.0)")
                .unwrap(),
            "t",
            "meter energy should integrate on sim-time in a stepped run",
        );
    }

    /// The shipped demo script boots cleanly and its scenarios
    /// register — a guard against a `define-scenario` schema change
    /// silently breaking the example, since nothing else exercises
    /// the real shipped script end to end.
    #[test]
    fn default_config_boots_and_registers_library_scenarios() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (cfg, _clock) = rt
            .block_on(async { Config::new_headless("examples/berlin-demo.lisp") })
            .expect("shipped berlin-demo.lisp boots headless");
        std::mem::forget(rt);
        let names: Vec<String> = cfg.scenarios().lock().keys().cloned().collect();
        assert!(
            names.contains(&"peak-evening-load".to_string()),
            "expected the starter library to register; got {names:?}"
        );
        assert!(
            names.len() >= 7,
            "expected >=7 starter scenarios, got {names:?}"
        );
    }

    /// The stepped runner drives a registered scenario end-to-end on
    /// the sim clock: `define-scenario` sections (a `timeline` drive +
    /// timed `check`s) compile through `scenario--run`, the checks fire
    /// as sim-time timers, and `run_scenario_stepped` advances the
    /// clock for the declared `:length`. Deterministic + faster than
    /// real time — the J2 CI gate.
    #[test]
    fn stepped_runner_runs_a_registered_scenario() {
        use std::time::Duration;
        // Copy the scenario DSL the config (load …)s.
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "switchyard-stepped-{}-{}",
            std::process::id(),
            next_unique(),
        ));
        let sim = dir.join("sim");
        std::fs::create_dir_all(&sim).unwrap();
        for f in ["common.lisp", "scenarios.lisp"] {
            std::fs::copy(format!("sim/{f}"), sim.join(f)).unwrap();
        }
        let body = "(set-enterprise-id 1)
(load \"sim/common.lisp\")
(load \"sim/scenarios.lisp\")
(make-microgrid :id 9 :grpc-port 18903 :topology
  (lambda ()
    (%make-grid-connection-point :id 1
      :successors (list (%make-meter :id 2 :power 0.0)))))
(define-scenario :name \"ramp\"
  :schedule 'relative :clock 'stepped :length \"60s\" :seed 7
  :drive (list (drive-meter 2 (timeline (hold 1000.0 :for 30)
                                        (ramp :to 5000.0 :over 30))))
  :expect (list (check \"10s\" :component 2 :metric 'active-power
                       :approx 1000.0 :tol 200.0)
                (check \"59s\" :component 2 :metric 'active-power
                       :approx 5000.0 :tol 800.0)))";
        let path = dir.join("config.lisp");
        std::fs::write(&path, body).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (cfg, _clock) = rt
            .block_on(async { Config::new_headless(path.to_str().unwrap()) })
            .expect("headless config builds");
        std::mem::forget(rt);

        let start = std::time::Instant::now();
        let steps = cfg
            .run_scenario_stepped("ramp", Duration::from_secs(1))
            .expect("scenario runs");
        let wall = start.elapsed();
        assert_eq!(steps, 60);
        assert!(wall < Duration::from_secs(5), "stepped wall time {wall:?}");

        // Both timed checks fired and passed; none failed.
        let report = cfg.site().scenario_report(chrono::Utc::now());
        assert_eq!(report.checks_passed, 2, "report: {report:?}");
        assert_eq!(report.checks_failed, 0, "report: {report:?}");

        // A scenario without :length can't be stepped; an unknown one
        // errors too.
        assert!(
            cfg.run_scenario_stepped("nope", Duration::from_secs(1))
                .is_err()
        );
    }

    /// `Config::refresh_once` drains tulisp-async's pending-timer
    /// queue. Without that, run-with-timer would just accumulate
    /// PendingTasks (same-ctx model — nothing fires them
    /// asynchronously). A zero-delay one-shot timer plus one
    /// refresh is the tightest expression of the contract.
    #[test]
    fn refresh_once_drains_pending_timers() {
        let (cfg, _dir) = config_with(
            "(set-microgrid-id 9)
             (setq fired 0)
             (run-with-timer 0 nil (lambda () (setq fired 1)))",
        );
        cfg.refresh_once();
        assert_eq!(cfg.eval_silent("fired").unwrap(), "1");
    }

    /// `reload()` must rebuild each microgrid's topology on the SAME
    /// site the boot-time runtimes hold — minting fresh sites would
    /// leave physics ticking (and gRPC serving) orphaned pre-reload
    /// state while the registry's new sites never tick.
    #[test]
    fn reload_rebuilds_topology_on_the_same_site() {
        let (cfg, _dir) = config_with(
            "(set-microgrid-id 9)
             (%make-grid-connection-point :id 1)",
        );
        // The handle a boot-spawned physics task / gRPC server holds.
        let live_site = cfg.microgrids().lock().get(&9).unwrap().site.clone();
        assert!(live_site.get(1).is_some());

        cfg.reload().expect("reload succeeds");

        // The re-eval'd topology landed on the SAME site: the
        // pre-reload handle sees the rebuilt component, and the
        // registry still carries exactly one entry for id 9.
        assert!(
            live_site.get(1).is_some(),
            "pre-reload site handle must see the rebuilt topology",
        );
        let reg = cfg.microgrids();
        let r = reg.lock();
        assert_eq!(r.len(), 1);
        assert!(r.get(&9).unwrap().site.get(1).is_some());
    }
}
