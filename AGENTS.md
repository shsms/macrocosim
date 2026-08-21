# switchyard

A Rust microgrid simulator with a Lisp-driven config DSL. Reimplementation of
microsim where component physics lives in Rust and Lisp's job
is wiring the topology + animating the environment.

## Layout

- `src/lib.rs` — module roots
- `src/sim/` — components + scheduler
  - `component.rs` — `SimulatedComponent` trait, `ComponentHandle`, `Telemetry`
  - `microgrid_site/` — per-microgrid registry, physics tick, grid state,
    topology (+ `history.rs` sampler, `scenarios.rs` event log)
  - `microgrids.rs` — enterprise registry + per-mg routing
  - `dispatch.rs` — enterprise dispatch store (per-`microgrid_id`, id
    allocator, lifecycle broadcast); backs the dispatch gRPC + UI
  - `bounds.rs` — `VecBounds`, `ComponentBounds` (rated + TTL
    augmentations, `validate_active_setpoint` 0-W-park gate)
  - `ramp.rs` — `CommandDelay` + `Ramp`
  - `decay.rs` — `bounded_exp_decay` + `soc_protected_bounds`, plus
    the SoC lifecycle helpers shared by battery/EV (`SocProtect`,
    `sanitize_soc_pct`, `integrate_soc_pct`)
  - `battery.rs`, `meter.rs`, `grid.rs`, `ev_charger.rs`,
    `inverter/{battery,solar}_inverter.rs`
  - `marker.rs` — no-physics categories (chp, wind turbine, steam
    boiler, power transformer, breaker); they classify the meters
    around them
  - `site_import.rs` — microgrid API site-export JSON → `(make-* …)` /
    `(connect …)` forms for `/api/microgrids/import`
  - `graph_adapter.rs` — lifts a site into
    `frequenz-microgrid-component-graph` nodes/edges (validation +
    the Formulas tab's explained-formula endpoint)
- `src/lisp/` — config DSL glue
  - `mod.rs` — `Config` (fields, accessors, reload)
  - `boot.rs` — `Config::new`: interpreter setup, defun registration,
    tulisp-async wiring, background loops
  - `defuns/` — every `register_*` installer, one file per topic
    (clock, scenarios, microgrids, metadata, runtime_modes, …)
  - `overrides.rs` / `snapshots.rs` — per-mg override-file persistence
    + snapshot save/load on `Config`
  - `make.rs` — `(make-*)` constructors via `AsPlist!`
  - `handle.rs` — `ComponentHandle` ↔ `Shared<dyn TulispAny>` round trip
- `src/ui/` — embedded web UI server
  - `mod.rs` — axum router + serve entry points
  - `handlers/` — HTTP handlers, one file per topic (topology, eval,
    scenarios, dispatches, …)
  - `state.rs` / `loopback.rs` / `events_ws.rs` — loopback client cache,
    gRPC loopback supervisor, WS event push
- `ui-assets/` — the SPA as hand-rolled ES modules (`app.js` is the
  entry; `topology.js`, `live.js`, `dashboard.js`, `inspect.js`,
  `repl.js`, `routing.js`, `dialogs.js`, `editor.js`, … own one
  concern each; `live.js` owns the live-overlay pure helpers: label
  text, number formatting, the dead band and edge flow; `pill.js`
  owns the node model and canvas renderer both graph canvases draw
  with, and the zoom tiers (full / hero / marker); `hovercard.js`
  the node hover card (pure model + DOM widget); `vendor/fonts/` the
  vendored IBM Plex faces (OFL))
- `tools/ui-smoke/` — Playwright smoke scripts run by hand against a
  live server (`SW_UI=http://127.0.0.1:PORT node
  tools/ui-smoke/live-topology.mjs`)
- `src/server.rs` — `Microgrid` gRPC service
- `src/assets_server.rs` — `PlatformAssets` gRPC service (shared port)
- `src/dispatch_server.rs` — `MicrogridDispatchService` gRPC service
  (store-and-serve dispatch API; CRUD + stream over `sim::dispatch`)
- `src/proto.rs` + `src/proto_conv.rs` — proto include + `Telemetry` →
  `MetricSample`s
- `src/timeout_tracker.rs` — request lifetime → `reset_setpoint` expiry
- `src/bin/switchyard.rs` — headless server
- `src/bin/swctl.rs` — clap-based client CLI
- `sim/common.lisp` — Lisp helpers (`every`, `cancel-timers`,
  `reset-state`); embedded into the binary with `defaults.lisp` +
  `scenarios.lisp` as the prelude
- `examples/berlin-demo.lisp` — self-contained demo world: topology
  + environment animation + the seven starter scenarios. Boot
  scripts are optional (`switchyard [script …]`); a bare boot loads
  worlds on demand via `(load …)` / the Microgrids tab, and
  `--state-dir` anchors journals / snapshots / relative paths

## Architectural rules

- **Lisp wires + animates the environment, Rust does physics.** Every
  component's tick / ramp / SoC derate is in Rust. Lisp's only verbs are
  `(make-*)` to build the graph and `(every …)` / `(run-with-timer …)` to
  perturb grid state or flip runtime knobs over time.
- **Inverter and battery couple only through the DC bus.** A real inverter
  and battery share an electrical bus, not data. `Battery::set_dc_power`
  clamps to its own SoC-derated bounds and keeps the ratio of accepted to
  pushed power; the inverter publishes its own push scaled by each child's
  `dc_accept_ratio` (zero when tripped or when no healthy child took the
  push), so a clipping battery shows on the inverter and the meters above
  it, and several inverters on one bus share the clip in proportion. The
  API gateway (server.rs) intersects bounds for setpoint validation —
  components never read each other's bounds.
- **Single physics tick, registration order = tick order.** `MicrogridSite::spawn_physics`
  runs one `tokio::time::interval` at `physics_tick_ms` and calls `tick()` on
  every component in registration order. Children register first because Lisp
  evaluates `:successors` before the surrounding `make-*`.
- **Telemetry stream cadence is anchored to a target timestamp.** `next_due +=
  step` then `sleep until next_due`; re-anchor only when behind. Per-stream
  `:stream-jitter-pct` perturbs each step; mean is exactly the configured
  interval.

## Build / run / test

```sh
cargo build
cargo test                                # unit tests for bounds/ramp/decay
cargo run --bin switchyard examples/berlin-demo.lisp
cargo run --bin swctl -- info
cargo run --bin swctl -- tree
cargo run --bin swctl -- stream 1001 --samples 5
cargo run --bin swctl -- set-power 1001 5000
```

Each registered microgrid binds its own gRPC port; the first
defaults to `[::1]:8800` and subsequent microgrids step by ten
(`:8810`, `:8820`, …). Override via `:grpc-port` on
`(make-microgrid …)`. swctl's `--addr` points the gRPC client at
the first microgrid by default; pass `--addr http://[::1]:8810`
etc. to reach others. The UI server binds `127.0.0.1:8801` by
default; override the port with `--ui-port N`, or pass
`--ephemeral-ports` to bind the UI and every gRPC / assets / dispatch
listener on OS-chosen ports (parallel CI instances). A routable
`--ui-bind` host is still on the roadmap. Add `--emit-endpoints=PATH`
to write the resolved addresses as one JSON line once bound (the
readiness signal).

`PlatformAssets` and `MicrogridDispatchService` each bind a single
shared listener (they're enterprise-wide, keyed by `microgrid_id` per
request): assets on `[::1]:9900`, dispatch on `[::1]:8900`. Override
via `(set-assets-socket-addr …)` / `(set-dispatch-socket-addr …)`.
Point the dispatch CLI at it with
`--url 'grpc://[::1]:8900?ssl=false' --auth-key any` (auth is ignored),
or use `swctl dispatch {list,create,pause,resume,delete,get}`. The
per-microgrid Dispatches UI sub-tab (`/api/mg/{id}/dispatches`) lists
them and can create / pause / resume / delete; all three write paths
(gRPC, UI, swctl) funnel through `DispatchStore::{create,set_active}`,
so construction + validation stay identical.

## Dependencies

- `tulisp = { version = "0.29", features = ["sync", "etags"] }` — the
  crates.io release (the git main-branch pin ended when 0.29.0
  shipped). Known 0.29.0 gap: a re-entrant `eval_string` underflows
  the eval-depth counter (fixed upstream) — `src/lisp/defuns/fs.rs`
  works around it with `eval_file`; drop the workaround on the next
  release bump.
- `tulisp-async = "0.1"` — same-ctx timer primitives (`run-with-timer`, `cancel-timer`,
  `sleep-for`). `TokioExecutor::new` calls `Handle::current()`, so
  `Config::new` must be invoked inside a running tokio runtime.
  `register` returns a `Handle`; the pre-tick hook owns one clone
  and ticks it each physics step — without that, no timer body
  ever runs (the same-ctx model has no background firing thread).
- Proto roots are vendored under `submodules/`:
  - `submodules/frequenz-api-microgrid` (pinned at v0.18.1) — override
    with `SWITCHYARD_PROTO_ROOT` for a private mirror.
  - `submodules/frequenz-api-assets` (pinned at v0.1.0).
  - `submodules/frequenz-api-dispatch` (pinned at v1.0.0) — dispatch
    v1; imports the same vendored common v1alpha8, so no common of
    its own.

## Adding a component type

1. New file under `src/sim/` implementing `SimulatedComponent`.
2. Add to `src/sim/mod.rs` re-exports.
3. Add a `%make-foo` defun in `src/lisp/make.rs` with `AsPlist!`-derived
   args, calling `site.register(...)`. Note the leading `%` —
   user-facing topology code calls `make-foo`, which dispatches here.
4. Add a `foo-defaults` plist + `(defun make-foo …)` wrapper to
   `sim/defaults.lisp`. The wrapper `apply`s `%make-foo` to the
   defaults plist `append`-ed in front of the caller's args; AsPlist's
   last-occurrence-wins resolution lets per-component plist values
   override the defaults.
5. (Optional) Override `subtype()` if proto needs `InverterType::Foo` / etc.
6. Register through `register_with_modes(...)` so the component gets
   the shared config + runtime kwargs: `:operational-mode` (config,
   persisted; derives the runtime knobs) plus `:health` /
   `:telemetry-mode` / `:command-mode` (runtime fault knobs, checked
   against the operational mode).

## Sample-config DSL convention

Two-layer split:
- `%make-*` — Rust primitives in `src/lisp/make.rs`. Pure plist
  parsing; every field arrives as a plist key, no defaults.
- `make-*` — Lisp wrappers in `sim/defaults.lisp` that prepend a
  `<cat>-defaults` plist and dispatch to `%make-*`.

Topology code uses `make-*` (defaults applied). To opt out of
defaults entirely for one call, invoke `%make-*` directly.
Per-component plist args win without any special handling — AsPlist!
takes the last occurrence of each key and the wrapper's defaults
appear first in the merged plist.

The prelude (common / defaults / scenarios) is compiled into the
binary via `include_str!`, so editing `sim/defaults.lisp` needs a
rebuild; a script that wants live defaults-editing can still
`(load "sim/defaults.lisp")` and `(watch-file …)` it explicitly.

## Lisp value adapters

- Runtime mode enums (`Health`, `TelemetryMode`, `CommandMode`) and
  the config-level `OperationalMode` take their lisp-side
  `TryFrom<TulispObject>` + `TulispConvertible` impls in
  `src/lisp/runtime_modes.rs`. **Symbols only** — `:health 'error`
  works, `:health "error"` errors with a type mismatch. Note the
  split: `OperationalMode` is microgrid CONFIG (persists via the
  overrides gate, drives the formula engine); the other three are
  runtime fault knobs that depend on it.
- `LispValue` (`src/lisp/value.rs`) — passthrough wrapper that lets a
  raw `TulispObject` ride through `AsPlist!` (works around the
  blanket-`From<T> for T` `Infallible` mismatch). Used for `:power`
  and `:sunlight%`, where the make-* dispatcher inspects the raw
  shape to pick between a constant and a `DynamicScalar`.

## Lisp gotchas (current tulisp-vm)

- **Timer bodies run on the calling ctx.** Same-ctx tulisp-async
  funcalls bodies on the parent `TulispContext`, so a lambda's
  lexical captures (`let*`-bound state, the surrounding closure
  environment) are preserved across firings. defuns/defvars/global
  setq results are visible as you'd expect.
- **`(every …)` callbacks fire on `Config`'s dedicated refresh
  loop, not on the physics tick.** `Config::spawn_lisp_refresh_loop`
  ticks on its own 100 ms grid, takes the interpreter lock once
  per pass, refreshes every microgrid's dynamic-scalar inputs,
  then drains the tulisp-async pending-firings mailbox. So a
  `(run-with-timer 0.05 …)` waits up to 100 ms before firing,
  and a zero-delay one fires on the next refresh pass. Tests
  that need a fire without spinning the loop call
  `cfg.refresh_once()` (synchronous wrapper for the same work).
  Physics ticks themselves are pure Rust now — they read the
  atomic scalars the refresh loop has cached and never touch the
  interpreter, so a long `/api/eval` no longer freezes the
  microgrid's beat.

## Adding a runtime knob

1. Field on the component config struct + plist arg in `src/lisp/make.rs`.
2. (If runtime-mutable) trait method override + `MicrogridSite` setter + Lisp defun
   in the matching `src/lisp/defuns/` file. Use `(every …)` or
   `(run-with-timer …)` from the config to script behaviour over time.
3. Demonstrate via a new line in `examples/berlin-demo.lisp` and
   verify via swctl.

## Testing an external bounds-driving app

Switchyard is used to test apps whose job is to push
`AugmentElectricalComponentBounds` and watch `power_bounds` react (a
GCP active-power limiter is the motivating case).

- **Both battery and solar inverters curtail to their effective
  (rated ∩ augmentation) bounds every tick.** `CommandDelay::poll`
  returns the armed setpoint on every tick, and `tick()` re-clamps it
  to the live envelope — so an external app narrowing a bound actually
  slews the inverter down at `ramp_rate`, and it recovers when the
  augmentation relaxes (tests: `late_augmentation_re_clamps_an_armed_setpoint`;
  `solar_inverter::tick`). Curtail-to-bounds already exists — don't
  reimplement it. A controller commands a setpoint **once**; it need
  not re-send, since the armed value persists and keeps curtailing.
- `set_active_setpoint` **hard-errors** a command outside the live
  (augmentation-narrowed) envelope — faithful to the real API gateway
  gating out-of-envelope setpoints. An EMS wanting "max within the cap"
  reads the bounds and commands within them.
- An inverter set to `:health 'error` (or `'standby`) **trips offline
  to zero output** *and* is dropped from the healthy `power_bounds`
  aggregate. A battery inverter clears its setpoint and awaits
  re-dispatch on recovery; a PV inverter resumes from sunlight.
- Drive sim state ad-hoc by POSTing lisp to
  `http://127.0.0.1:8801/api/eval`, e.g.
  `--data "(set-component-health 201 'error)"` → `{"ok":true,…}`.
- Switchyard's physics supports closed-loop bound tests today; the
  remaining gaps are ergonomic, not physical — scenario assertions,
  an in-sim controller/actor that reacts to live bounds, declarative
  signal profiles, and deterministic sim-time. See `todo.org` §I.

## Roadmap and deferred work

See `todo.org` for the forward-looking roadmap (scenario framework,
reactive plist values, integration tests, CI) and known open design
questions.
