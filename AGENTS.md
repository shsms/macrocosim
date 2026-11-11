# macrocosim

A Rust microgrid simulator for testing downstream control apps. Component
physics lives in Rust; a Python script, a program of its own, declares the
topology (a JSON topology document the engine builds) and animates the
environment over the HTTP API. Reimplementation of microsim.

## Layout

- `src/lib.rs` — module roots
- `src/engine.rs` — `Engine`: the simulator state every server and handler
  shares (registry, dispatch store, enterprise `Metadata` in
  `engine/metadata.rs`, grid frequency, the source of "now", the structural
  lock, the spec undo stacks). `engine/topology.rs` is every structural change
  to a microgrid, each taking a `Structural` guard on that lock
  (`Engine::structural`): `create` builds and registers one, `rebuild` swaps in
  a new spec with carry (a component carries when its defaults-filled spec is
  unchanged under the same id, its name, health and modes aside) and moves the
  gRPC server when `grpc_port` changes, `edit` is a rebuild that also stacks the
  replaced spec for undo and marks a file-backed microgrid unsaved, `unload`
  removes one (registry entry first, then its runtime, recording and CSV sinks,
  dispatches and undo stacks, then the `microgrid_unloaded` broadcast), and
  `declare` applies a client's declaration: it creates each listed microgrid
  that is not loaded, rebuilds with carry each loaded one whose spec changed (an
  edit: undo step, unsaved mark), leaves one whose spec is unchanged, unloads
  nothing and checks no source, then starts the runtime of each loaded one it
  lists. Every change keeps component ids unique across the engine. A registry
  entry's `spec` is the spec the microgrid was last built from, with every id
  filled in
- `src/spec.rs` — the topology document (`TopologyDoc`,
  `MicrogridSpec`, `ComponentSpec`): one flat JSON document of
  microgrids, components and connections; `spec/defaults.rs` the
  per-type `Defaults` table (`GET /api/defaults`) and `fill`;
  `spec/order.rs` the children-first walk and cycle check the build and
  the site import share; `spec/build.rs` `build_topology` (checks a spec
  and builds its components without touching a site) and `runtime_from_modes`;
  `spec/schema.rs` the JSON Schema, derived at run time
- `src/version.rs` — `VERSION`, the crate version in PEP 440 form
  (`0.2.0-alpha.0` is `0.2.0a0`), the form the Python package carries
- `src/undo.rs` — `UndoHistory<T>`, the per-microgrid undo / redo
  stack (capped at 20 steps), generic over what one step stores
- `src/python.rs` — finds the Python interpreter the `.py` export runs on
  (`--python`, `$MACROCOSIM_PYTHON`, beside the binary, `PATH`) and checks it:
  3.11 or later with the `macrocosim` package of the binary's version. A passed
  check is kept for the engine's life; a failed one runs again on the next
  export. The `.py` export (`GET /api/mg/{mg}/spec.py`) pipes the stored spec
  into `python -P -m macrocosim.export`, one export at a time
- `src/runtime.rs` — `MicrogridRuntimes`: the one startup path per
  microgrid (physics, history, gRPC server, UI loopback), its
  started / failed status, and the handles of the tasks it started:
  `stop` ends them all and forgets the runtime, `restart_grpc` moves
  the gRPC server to a new port
- `src/sim/` — components + scheduler
  - `component.rs` — `SimulatedComponent` trait, `ComponentHandle`,
    `Telemetry`; `component/` holds the capability traits
    (`Controllable`, `DcStorage`, `ReactiveLimits`, and the knob
    traits `MeterDrive`, `SunlightDrive`, `SteamDrive`, `EvPort`),
    each reached through an accessor on `SimulatedComponent`
  - `scalar.rs` — `Scalar`, an `f32` in an atomic: a knob the physics
    tick reads and a write replaces (a meter's power, a boiler's
    demand, a driven sunlight)
  - `microgrid_site/` — per-microgrid registry, physics tick, grid state,
    topology (+ `history.rs` sampler, `recording.rs` the recording
    journal's start / stop / event / report and its CSV sinks,
    `swap.rs` the rebuild with carry: `swap_topology` installs a built
    topology in place, restoring each matching component's
    `PhysicsState` and keeping its history and energy, while weather,
    grid state, sample lag and the UI's aggregate energy, keyed on the
    `energy_epoch` only `reset()` moves, stay)
  - `recording.rs` — `RecordingJournal`, the event ring and the
    metric accumulators a recording's report reads; `recording_csv.rs`
    its per-component CSV sinks
  - `microgrids.rs` — the enterprise registry: `MicrogridEntry`
    (definition, site, source file, `unsaved`, spec), id and port
    allocation
  - `dispatch.rs` — enterprise dispatch store (per-`microgrid_id`, id
    allocator, lifecycle broadcast); backs the dispatch gRPC + UI
  - `bounds.rs` — `VecBounds`, `ComponentBounds` (the list of TTL
    augmentations a `GatewayAxis` holds)
  - `ramp.rs` — `CommandDelay` + `Ramp`
  - `gateway.rs` (+ `gateway/{step,window}.rs`) — `MicrogridGateway`,
    one per microgrid, held by `MicrogridSite`: every Microgrid API
    rule (setpoint validation, request lifetimes, augmentations, the
    gateway delay and ramp, the SoC window and its share). Reached
    through `site.gateway()`; `site.bounds_of` / `site.telemetry_of`
    are the one bounds read every consumer reports
  - `gateway_axis.rs` — `GatewayAxis`, the API half of one power axis
  - `device_axis.rs` — `DeviceAxis`, the hardware half: a FIFO device
    delay and the physical clamp
  - `decay.rs` — `bounded_exp_decay` + `soc_protected_bounds`, plus
    the SoC lifecycle helpers: `SocProtect` is the gateway's SoC
    window, while `sanitize_soc_pct` and `integrate_soc_pct` are
    shared by the battery and the connected car
  - `battery.rs`, `meter.rs`, `grid.rs`, `ev_charger.rs`,
    `inverter/{battery,solar}_inverter.rs`, `steam_boiler.rs`
  - `physics_state.rs` — `PhysicsState`, what a rebuilt component
    keeps (battery SoC, the plugged car, boiler pressure), read and
    written through `SimulatedComponent::physics_state` /
    `restore_physics_state`
  - `ev_presets.rs` — the connected car: preset catalog, per-plug
    overrides, taper, draw law; owned by the charger, never a site
    component
  - `marker.rs` — no-physics categories (chp, wind turbine, power
    transformer, breaker); they classify the meters around them and
    pass their children's power up to the meter above
  - `sim_clock.rs` — `NowSource` (the wall clock, or a fixed base plus
    a `ManualClock` the physics tests advance by hand)
  - `site_import.rs` — microgrid API site-export JSON → the
    `MicrogridSpec` `/api/microgrids/import` creates, keeping the
    export's component ids
  - `graph_adapter.rs` — lifts a site into
    `frequenz-microgrid-component-graph` nodes/edges (validation +
    the formula endpoint behind the formula explorer panel)
- `src/ui/` — embedded web UI server
  - `mod.rs` — axum router + serve entry points
  - `api.rs` — `ApiError`, the extractor wrappers (`Json`, `Query`,
    `Path`, `WsUpgrade`, `Mg`) and the JSON fallbacks
  - `handlers/` — HTTP handlers, one file per topic (topology, spec,
    control, recording, dispatches, …)
  - `state.rs` / `loopback.rs` / `events_ws.rs` — loopback client cache,
    gRPC loopback supervisor, WS event push (one forwarder per
    microgrid, reconciled against the registry on every registered or
    unloaded notice, so a microgrid created again under an
    unloaded id is followed)
  - `app_state.rs` — `AppState`, the router's state: the `Engine`, the
    `MicrogridRuntimes` and their loopback slots, the `.py` export's interpreter
    (`Export`); a handler takes the part it uses, such as `State<Engine>`
- `ui-assets/` — the SPA as hand-rolled ES modules (`app.js` is the entry;
  `topology.js`, `live.js`, `metrics-store.js`, `metrics-panel.js`,
  `inspect.js`, `routing.js`, `events-ws.js` (the events socket's pump, the Logs
  panel's head and its backfill), `chrome.js` (the top bar: the zone and theme
  chips, the system pulse), `unsaved-chip.js` (a microgrid's unsaved chip, with
  no DOM), `dialogs.js`, `editor.js`, … own one concern each; `http.js` the
  failed-response reader (`errorText`, `getJson`) and `bgFetch`; `html.js`
  `escapeHtml`, with no DOM so the node tests can import it (`app.js` re-exports
  it); `notices.js` toasts, form errors and the UI's log lines; `connection.js`
  whether the server can be reached, and its banner; `logs.js` the logs panel's
  rows; `storage.js` guarded localStorage; `zone.js` the display zone (the
  browser's local zone or UTC, from the top bar's zone chip) every shown time
  goes through: the formatters, `<time>` elements it re-formats on a switch,
  uPlot's `tzDate`, and the wall-time conversion for `datetime-local` fields;
  imported as `* as zone`, so call sites read `zone.fmtTime`, `zone.tzDate`;
  `theme.js` the colour theme (auto follows the OS, or light or dark from the
  top bar's theme chip), set as `data-theme` on `<html>`; code that draws with
  colours read from the tokens (the canvas, the charts) re-reads them in a
  `theme.onChange` listener; `spec-edit.js` the editor's structural edits (read
  the selected microgrid's spec, change it, `PUT` it, retrying once on a 409);
  `ids.js` the ids new components take (the lowest free ones from 5000);
  `paste-forms.js` the DOM-free splice of a copied subgraph into a spec under
  fresh ids, children before parents; `metrics-store.js` holds the
  derived-stream rings + the PF helpers and `metrics-panel.js` the floating
  charts panel that reads them; `chart-lib.js` the uPlot gate every chart
  builder asks first (a note in the slot when the vendored global did not load);
  `live.js` owns the live-overlay pure helpers: label text, number formatting,
  the dead band and edge flow; `pill.js` owns the node model and canvas renderer
  both graph canvases draw with, and the zoom tiers (full / hero / marker);
  `hovercard.js` the node hover card (pure model + DOM widget); `side-panel.js`
  the card shell every panel (inspector, formulas, metrics, weather, logs,
  Defaults, Report) opens in — static-markup cards are listed in its
  `STATIC_PANELS` with their scroll band, which a card's measured floor
  (`floorOf`) holds at its min-height; per-panel width, spawn corner and first
  dock edge in `PANEL_DEFAULTS`; a card docks into the bottom or right strip
  (`#dock-bottom`, `#dock-right`) the first time it opens (the logs at the
  bottom, the rest on the right) and keeps the user's own dock or float choice
  after that (`STRIPS` holds each strip's ids, axis, sizes and the room it
  leaves the rest of main; `dockPanel(name, edge)` / `floatPanel` /
  `layoutStrip(edge)`; persisted under `mc-panel-dock-<name>` and
  `mc-strip-<edge>`); `splitter.js` the drag-to-resize handshake the dock strips
  use; `strip-model.js` their DOM-free arithmetic (shares, order, size clamp);
  `panel-geometry.js` the floating cards' DOM-free placement arithmetic (cascade
  slot pick); `vendor/fonts/` the vendored IBM Plex faces (OFL))
  - Reactive power reads at parity with active power across the SPA:
    the hover card draws a Q envelope bar under the P one (same
    `hovercard.js` `envelopeBar`, labelled in VAr); the metrics
    panel's Reactive power card charts the `grid_reactive_power`,
    `pv_reactive_power` and `battery_reactive_power` aggregate
    streams, with each chip reading out PF against its own P stream
    and an optional dashed PF overlay on a right-hand scale (both
    from `metrics-store.js` `pfValue` / `pfText`); the inspector's
    knob table offers an inverter's reactive limits and a meter's
    reactive power or power factor (with a `leading` checkbox).
- `tools/ui-smoke/` — Playwright smoke scripts against a live server
  (`MACROCOSIM_UI=http://127.0.0.1:PORT node tools/ui-smoke/live-topology.mjs`
  points it at a server you already run). The e2e half drives the smoke's own
  site (`tools/ui-smoke/site.py`, microgrid 2200) from a scratch state dir, and
  sends gRPC setpoints through the `macroctl` binary named by `MACROCTL`. `make
  ui-e2e` builds both binaries, boots one server, runs that site against it and
  runs the smoke, and the `ui-e2e` job in `.github/workflows/ci.yml` does the
  same on pushes to main and PRs targeting main.
- `src/server.rs` — `Microgrid` gRPC service
- `src/assets_server.rs` — `PlatformAssets` gRPC service (shared port)
- `src/dispatch_server.rs` — `MicrogridDispatchService` gRPC service
  (store-and-serve dispatch API; CRUD + stream over `sim::dispatch`)
- `src/proto.rs` + `src/proto_conv.rs` — proto include + `Telemetry` →
  `MetricSample`s
- `src/timeout_tracker.rs` — request-lifetime deadlines on the site
  clock, owned by the gateway and expired on the physics tick
- `src/bin/macrocosim.rs` — headless server
- `src/bin/macroctl.rs` — clap-based client CLI
- `examples/starter_site.py` — the demo world as a Python script (`python
  examples/starter_site.py` against a running server): `MICROGRID`, the
  animation, a `main` that declares and animates it, and the seven scenarios as
  async functions; `examples/scenario_driving.py` imports it for a 30-minute
  recorded stress run. `python/pyproject.toml` puts `examples/` on ty's and
  ruff's first-party paths, since the scripts import each other
- `python/` — the `macrocosim` Python package: the spec dataclasses (`_spec.py`,
  where each `make_*` constructor is its dataclass), the knob handles
  (`_components.py`, `_signals.py`), recording, `launch`, `connect` and its
  `Connection` (`declare`, `unload`, `microgrids`), and the exporter
  (`export.py`)

## Architectural rules

- **A script declares the topology and animates the environment, Rust does
  physics.** Every component's tick / ramp / SoC is in Rust. A script declares
  its microgrids through `connect` (or `launch`), as a topology document the
  engine builds, and writes knobs over the HTTP API from ordinary asyncio loops.
- **Module style.** Rust 2018: a new module with children is `foo.rs`
  beside a `foo/` directory, never `foo/mod.rs`. `sim/`,
  `sim/inverter/`, `sim/microgrid_site/`, `ui/` and `ui/handlers/`
  still use `mod.rs`.
- **One gateway per microgrid holds every Microgrid API rule.** The
  gRPC service talks to `site.gateway()`; the components are plain
  hardware with one command input per power axis
  (`Controllable::set_command`). The gateway validates (0 is always
  accepted; NaN never), arms request lifetimes and stamps augmentations
  on the site clock, expires both on the physics tick (`gateway.step`
  runs in `tick_once` before the components), and refuses a command
  whose site was reset or rebuilt since its lookup ("site was reset").
  The protocol layer — fault gates, over-bound fault injection, the
  setpoint journal, lifetime windows — stays in `server.rs`.
- **Lock order.** Outermost first:
  1. the engine's structural lock, a `tokio::sync::Mutex`: create,
     import, a declaration, a spec `PUT`, undo and an unload hold it,
     and every `topology` function takes its `Structural` guard. The
     HTTP routes run each change in its own task
     (`handlers::detached`), so a client that hangs up cannot leave one
     half applied;
  2. the runtimes' `start_lock` (a `tokio::sync::Mutex`, which `start`, `stop`
     and `restart_grpc` take);
  3. the gateway lock;
  4. the registry (`parking_lot::Mutex`), taken briefly and never held across an
     `.await`, then the runtimes' map;
  5. component locks.

  Knob writes take no structural lock: a write that finds its
  component id gone answers 404. The gateway lock is never held across
  an `.await`.
- **An axis is a `GatewayAxis` in the gateway plus a `DeviceAxis` in
  the component.** The chain is gateway delay → gateway ramp → device
  delay → output. Each step the gateway targets the armed command
  (else the component's `Controllable::idle_value`), clamps it to
  validation envelope ∩ `Controllable::physical_band`, narrows that
  toward 0 by (battery inverter, P) its window share without crossing
  0, ramps, and never leaves the ramp outside the physical band or the
  share — a narrowing is followed at once, a widening is climbed at
  the ramp rate. The validation envelope is the rated band (P) or the
  `ReactiveCapability` at the last measured P (Q) ∩ live
  augmentations; `reactive_pf_limit` sets `k` in `|Q| ≤ k × |P|` — a
  ratio of apparent quantities, not true power factor. The device
  output is the delayed command clamped to the rated band and the
  current physical band, so a direct `Controllable::set_command` is
  capped too. A health trip snaps both to 0, empties the delay line,
  and clears the command unless
  `Controllable::keeps_command_through_fault` (PV active, a charger
  with `resume_on_recovery`). The EV charger's axis produces the
  *limit* it offers the plugged car, not the draw.
- **The battery is hardware from 0 % to 100 %; the SoC window is the
  gateway's.** The battery's `tick` clamps the summed inverter pushes
  to the rated band, refusing charge at 100 % and discharge at 0 %.
  The gateway throttles each battery's bounds from its SoC
  (`soc_lower_pct`, `soc_upper_pct`, `soc_protect_margin_pct`, taper base 1.2,
  floor 0.3) and, each step, shares the room between the inverters
  pushing into it in proportion to their pushes (same sign only), so a
  running setpoint tapers and holds inside the window within one
  device delay. Inverters still publish their push scaled by each
  child's `DcStorage::dc_accept_ratio`, which stays 1 inside the
  window; without the gateway a battery charges to 100 %. Every
  inverter pushing into a battery gets the same ratio, so inverters on
  one bus share a clip in proportion to their pushes. An inverter
  publishes 0 (P and Q) when it is tripped or when no healthy child
  took its push. Reactive power terminates at the inverter: a DC bus
  carries no Q. A meter's own reactive source is the real thing:
  mutually-exclusive `reactive_power_var` or `power_factor` +
  `leading` (true cos φ in `(0, 1]`, deriving `Q = P·tan(acos(pf))`
  off the meter's own live P, negated when leading).
- **Single physics tick, registration order = tick order.** `MicrogridSite::spawn_physics`
  runs one `tokio::time::interval` at `physics_tick_ms` and calls `tick()` on
  every component in registration order. The spec build registers children
  before their parents (`spec::order`).
- **Telemetry stream cadence is anchored to a target timestamp.** `next_due +=
  step` then `sleep until next_due`; re-anchor only when behind. Per-stream
  `stream_jitter_pct` perturbs each step; mean is exactly the configured
  interval.
- **Site weather is one singleton per microgrid, not per-component.**
  `POST /api/mg/{mg}/weather` installs a parametric clear-sky day (a sunrise/sunset window, a sine peaking at `peak_pct`)
  plus an optional ambient cloud generator; a later `POST` retunes any
  of it in place, and its `pass_cloud` scripts one deterministic cloud;
  `GET` reads the sky back (`src/ui/handlers/weather.rs`,
  `src/sim/weather.rs`). A solar inverter with no `sunlight_pct` follows
  the site's weather (`weather_lag_s` / `weather_jitter_pct` lag and
  roughen the sample it reads — with no explicit lag each inverter gets
  a stable id-derived 0–60 s offset so a cloud sweeps across a multi-PV
  site, and `weather_lag_s` 0 opts out; `array_peak_w` sizes the DC
  array whichever source the sunlight comes from); a driven
  `sunlight_pct` makes it manual instead, and the `drive` route's
  `clear_sunlight` is the way back to following the weather. A `seed`
  in the `POST` rebuilds the weather, and `DELETE` removes it.

## HTTP API

- Whole-site routes live under `/api/` (microgrids, import, defaults, spec
  schema, logs, frequency). `GET /api/defaults` serves the spec `Defaults`
  table, read-only. `POST /api/microgrids` creates the microgrids of a topology
  document (201 `{"microgrids": [{id, grpc_port, runtime}]}`), and `POST
  /api/microgrids/import` a microgrid from a site export; both live in memory
  only, and are `unsaved`. `POST
  /api/microgrids?existing=rebuild&client_version=V` is a declaration
  (`topology::declare`): 200 `{"microgrids": [{id, grpc_port, runtime,
  outcome}]}`, `outcome` being `created`, `rebuilt` or `unchanged`; every
  microgrid needs an id. A `client_version` that is not the binary's is a 400,
  checked before the rest of the query and the body are read. These two are the
  one place a query parameter changes what a route does rather than filtering or
  tuning its reply.
- `GET /api/mg/{mg}/spec` answers a microgrid's stored spec, and
  `PUT` of a `MicrogridSpec` rebuilds it with carry (200 with the stored
  spec; 409 for a component id or a gRPC port another microgrid
  holds, 422 for an invalid spec or a component without an id). A new
  `grpc_port` moves the microgrid's gRPC server there.
  `DELETE /api/mg/{mg}` unloads a microgrid (204): its streams end, its
  gRPC port is free within about 2 s, its dispatches are deleted with
  a `Deleted` event each, its id and component ids are free, and the
  events socket sends `microgrid_unloaded` for it, on which the SPA
  drops its card and leaves a view of it.
  A `PUT` stacks the replaced spec on the microgrid's undo stack and
  marks a microgrid with a source file `unsaved`. `POST
  /api/mg/{mg}/undo` and `/redo` walk that stack, rebuilding it with
  carry from each step (`topology::step_history`).
  `GET /api/spec/schema` serves the topology document's JSON Schema.
- The typed knob routes:
  `POST /api/mg/{mg}/component/{id}/drive` (meter power and reactive
  power, sunlight, SoC, steam demand and pressure, and an inverter's
  `reactive_apparent_va` and `reactive_pf_limit`), `.../status`
  (health, telemetry and command modes), `.../runtime`
  (`operational_mode`), `POST` and `DELETE .../component/{id}/ev`
  (plug a catalog car in, unplug it), `POST /api/mg/{mg}/sample-lag`,
  `POST /api/mg/{mg}/streams/cancel` and `POST /api/mg/{mg}/grid`
  (`voltage_per_phase_v`). Each changes the live value, never a stored
  spec, and sends a `knob_changed` event. `GET` and `POST
  /api/frequency` read and drive the frequency every microgrid shares:
  `hz` sets it now, `model` changes the keys of the base model it
  gives, `override` drives the frequency with a model whose missing
  keys come from the base model, and `clear_override` drops that
  override.
- `POST /api/mg/{mg}/recording/start` (`name`, `csv`), `.../stop` and
  `.../event` (`kind`, `payload`) record a microgrid: a start is 409
  while a recording runs. A rebuild ends a recording and keeps its
  report for one stop (`ended_by: "rebuild"`); an unload ends it with
  the microgrid. CSV files go to
  `<state_dir>/recordings/<mg>/<name>-<%Y%m%dT%H%M%SZ>/` (`-2`, `-3`, …
  added when that directory exists).
  `GET .../recording` (the summary), `/events`, `/report`, `/csv` and
  `/csv/{file}` read it.
- Everything about one microgrid lives under `/api/mg/{mg}/`, served
  by one nested router; the `Mg` extractor resolves `{mg}` and
  answers 404 `microgrid {mg} not registered` itself.
- A path names a resource; a verb appears only for an action (start,
  stop, import, undo, redo, drive, a dispatch's active). Ids that pick
  a microgrid, component or dispatch are path segments; other query
  parameters filter, window or tune the reply (`metric`, `window_s`,
  `since`, `limit`, `width`, `dir`).
- Field names follow [`docs/names-and-units.md`](docs/names-and-units.md):
  the unit is in the name (`power_w`, `soc_pct`, `remaining_s`), points
  in time are RFC 3339 (`ts`, `start`, `created_at`), chart series are
  epoch seconds (`t_s`), and a generic `value` has a `unit` beside it.
  A request body or query with a field the route does not know is
  refused (422 for a JSON body, 400 for a query string).
- Every failure is JSON `{"error": "..."}` with a 4xx/5xx status
  (`ApiError` in `src/ui/api.rs`); a route may add fields beside it.
  An unknown path answers 404 `no route for {METHOD} {path}`. A known
  path asked with the wrong method answers 405 with the same `no
  route for …` text.
  Success bodies carry no `ok`; an action with nothing to return
  answers 204.

## Build / run / test

```sh
cargo build
cargo test
cd python && uv sync && cd ..
cargo run --bin macrocosim -- --state-dir /tmp/mc --python python/.venv/bin/python
python/.venv/bin/python examples/starter_site.py   # in another terminal
cargo run --bin macroctl -- info
cargo run --bin macroctl -- tree
cargo run --bin macroctl -- stream 1001 --samples 5
cargo run --bin macroctl -- set-power 1001 5000
```

The state directory (`--state-dir`, the current directory by default) must not
hold 0.1's `enterprise.lisp` or `microgrids/*.lisp`, and a `.lisp` argument is
refused too: the binary exits naming the files. A `.py` argument is refused with
a pointer to running the script with `python`; the binary takes no positional
argument.

The Rust tests that need the venv use `python/.venv/bin/python` (run `uv sync`
in `python/` first); they are skipped with a note when it is missing, and fail
instead under `MACROCOSIM_REQUIRE_PYTHON=1`, which CI sets. In `python/`,
`MACROCOSIM_BIN=../target/debug/macrocosim MACROCOSIM_REQUIRE_LIVE=1 uv run
pytest` runs the client's live tests against this checkout's binary.

`ui-assets/` changes: `make ui-test` runs biome over `ui-assets` (config in
`biome.json`; `check` also runs the organizeImports assist, so an unsorted
import or export specifier list fails as an error, and `noDescendingSpecificity`
is off for `style.css` in the `overrides` block until its rules are reordered
with a browser to check), then every `tools/*-test.mjs` plus `boot-smoke.mjs`,
which imports app.js under a DOM shim and catches TDZ / cycle / bad-export
breakage a curl-200 can't see. `make ui-e2e` runs the Playwright smoke in
`tools/ui-smoke/` against a scratch server on OS-chosen ports, with
`tools/ui-smoke/site.py` running against it as a separate process (it needs
`uv sync` in `python/` first), the way CI does; `make ui-e2e-deps` installs its
one dependency, Playwright with a matching Chromium, into the gitignored
`node_modules`.

UI input convention: a numeric field that commits on Enter (inspector
knobs, weather config fields) must hide the browser's native spinner
arrows (`appearance: textfield` + the `-webkit-*-spin-button` rules in
`style.css`) and ignore wheel scrolling (a non-passive `wheel` listener
calling `preventDefault`, Firefox steps a focused number field on a
wheel) — either affordance changes the value without committing it, and
the next poll or blur silently reverts. Arrow KEYS keep stepping: those
are deliberate keyboard edits, one Enter from a commit. Fields that
commit via a button (dialogs, pass-a-cloud) may keep both.

UI style convention: colours, text sizes, spacing, radii, shadows and layers
come from the tokens on `:root` in `style.css` (`--text-*`, `--space-*`,
`--radius-*`, `--z-*`, and the colour roles). Every colour token has a value in
the `:root[data-theme="light"]` block too, and comfortable density overrides the
size and spacing tokens on `:root[data-density="comfortable"]`.
`tools/css-vars-test.mjs` fails on a colour literal outside the token blocks, a
light value missing, or a raw length in a spacing, radius or z-index
declaration.

UI controls convention: a button is `.btn` (the secondary kind) with
`.btn-primary`, `.btn-quiet` or `.btn-danger` for the other kinds, `.btn-sm` for
the small size and `.btn-icon` for a glyph-only one (×, ⤓, ✕); a field is
`.field` (`.field-sm` for a field set inline with text or other controls, as in
panel rows and the dispatch dialog's inline choices; `.field-num` for a short
number); a form's inline error is `.form-error`. A form or dialog has at most
one primary button. Toggles keep their own components (`.pill`, `.pulse-chip`,
`.mode-btn`, …). A panel's toggle, a button in one of the bars, carries
`aria-pressed` and has an accent border and text while its panel is open.
`tools/controls-test.mjs` lists the allowed button classes and fails on a class
the kit replaced.

UI layout convention: two bars sit above main, by scope. The top bar
(`body > header`) holds what is site-wide: the running chip, the system pulse
(`#pulse`), the Logs, Defaults, Report and help buttons, and the theme, density
and zone chips. The microgrid bar (`#mg-header`, hidden on the list) holds what
belongs to the selected microgrid, its panel buttons included. The canvas strip
(`#topology-controls`) holds only the canvas's own controls. Both bars wrap in a
narrow window; the smoke checks the layout at 1024px wide.

UI error convention: a form or dialog shows a server error in its own
`.form-error` through `showFormError` (`notices.js`) and stays open; a new try
clears it with `clearFormError`, and closing a dialog clears its errors. A
form's own check of its input shows through `showInputError`, which does not
log. Any other action the user triggered toasts with `notify`, and an error
toast stays until it is closed. A background loop fetches through
`bgFetch(source, …)` (`http.js`), or `getJson` / `mgFetch` given a source: a
request that cannot reach the server raises the "server unreachable" banner
(`connection.js`) until one gets through, and an HTTP error is logged once per
failure streak of that source. A dropped WebSocket writes "disconnected" into
the header status, which its reopen clears. Each of these reaches the logs
panel as a `ui: ` line too, except the input checks. localStorage is read and
written through `storage.js`, which keeps values in memory when the browser
blocks it; `theme.js`, which reads the window it is given, and the pre-paint
script in `index.html` keep their own guards.

Each registered microgrid binds its own gRPC port; the first
defaults to `[::1]:8800` and subsequent microgrids step by ten
(`:8810`, `:8820`, …), skipping the assets and dispatch ports; an
explicit `grpc_port` on either is refused. Set it with `grpc_port`
in the microgrid's spec. `topology::create` starts each microgrid's
runtime through `MicrogridRuntimes`, at boot and later. A
bind failure, or a server that ends, marks only that microgrid failed
and the process stays up (a boot-time bind failure still exits the
binary); a microgrid unloaded and created again starts afresh, and a
spec `PUT` with a new `grpc_port` rebinds the microgrid there.
`/api/microgrids` entries carry `runtime: {status, grpc_addr, error} |
null`, and create and import wait for the start and report it; the
microgrid card shows the address and a "failed" chip. The UI's loopback
client reads a private copy of the Microgrid service through an
in-memory channel, so a microgrid's live data reaches the UI whether or
not its public port is bound.
macroctl's global `--microgrid-id` selects the microgrid for the
gRPC commands, `dispatch`, `pool` and `dashboard`; the default is the
lowest id. `--addr` names the gRPC server directly; with
`--microgrid-id` the lookup still runs and the two must agree. The
UI server binds `127.0.0.1:8801` by default; override the
port with `--ui-port N`, or pass `--ephemeral-ports` to bind the UI
and every gRPC / assets / dispatch listener on OS-chosen ports
(parallel CI instances). A routable `--ui-bind` host is still on the
roadmap. Add `--emit-endpoints=PATH` to write the resolved addresses
as one JSON line once bound (the readiness signal); the line carries
`version`, the binary's version in PEP 440 form, so a client can tell
a mismatched binary.

`macrocosim --topology topology.json` boots the microgrids of a topology
document: each is built from its spec with the file as its source, and a bad
document exits the binary. `--enterprise-id`, `--assets-addr`,
`--dispatch-addr`, `--request-lifetime SECONDS` and `--augment-lifetime SECONDS`
set the enterprise settings.

`PlatformAssets` and `MicrogridDispatchService` each bind a single
shared listener (they're enterprise-wide, keyed by `microgrid_id` per
request): assets on `[::1]:9900`, dispatch on `[::1]:8900`. Override
with `--assets-addr` / `--dispatch-addr`.
Point the dispatch CLI at it with
`--url 'grpc://[::1]:8900?ssl=false' --auth-key any` (auth is ignored),
or use `macroctl dispatch {list,create,pause,resume,delete,get}`. The
per-microgrid Dispatches UI sub-tab (`/api/mg/{id}/dispatches`) lists
them and can create / pause / resume / delete; all three write paths
(gRPC, UI, macroctl) funnel through `DispatchStore::{create,set_active}`,
so construction + validation stay identical.

## Dependencies

- Proto roots are vendored under `submodules/`:
  - `submodules/frequenz-api-microgrid` (pinned at v0.18.1) — override
    with `MACROCOSIM_PROTO_ROOT` for a private mirror.
  - `submodules/frequenz-api-assets` (pinned at v0.1.0).
  - `submodules/frequenz-api-dispatch` (pinned at v1.0.0) — dispatch
    v1; imports the same vendored common v1alpha8, so no common of
    its own.
- `schemars` is pinned to one minor version: a release may change the
  JSON Schema it writes for the topology document.

## Adding a component type

1. New file under `src/sim/` implementing `SimulatedComponent`.
   A component with a capability implements its trait from
   `src/sim/component/` and overrides the matching accessor with
   `Some(self)`: a controllable one implements `Controllable`
   (`has_axis`, `set_command`, `physical_band`, plus the defaulted
   `idle_value`, `initial_value`, `keeps_command_through_fault`,
   `bounds_follow_physical_band`, `gateway_settings`) and keeps one
   `DeviceAxis` per axis; the gateway supplies validation,
   lifetimes, augmentations and the ramp. A component with state a
   rebuild should keep implements `physics_state` /
   `restore_physics_state`. Add the component to the capability table
   test in `src/sim/component.rs`.
2. Add to `src/sim/mod.rs` re-exports.
3. Add its spec: a `component_spec!` struct and a `ComponentSpec`
   variant in `src/spec.rs`, its arm in `src/spec/build.rs` (the checks
   and the construction), and its row in `src/spec/defaults.rs`.
4. (Optional) Override `subtype()` if proto needs `InverterType::Foo` / etc.
5. Map its export category in `src/sim/site_import.rs` when the
   Microgrid API has one.
6. Add it to the Python client: its spec dataclass and `make_*`
   constructor in `python/src/macrocosim/_spec.py`, its handle in
   `_components.py`; the exporter reads the dataclasses, so it needs no
   table of its own.

## Adding a runtime knob

1. A field on the component config struct and its spec field, built in
   `src/spec/build.rs`.
2. (If runtime-mutable) a method on the matching capability trait, and
   a field on the typed route that writes it (`DriveRequest` in
   `src/ui/handlers/control.rs`, or a route of its own) that reaches it
   through the accessor and broadcasts `note_knob_changed`; its unit
   in `knob_unit` (`src/sim/events.rs`); its read-back in
   `knobs_for` (`src/ui/handlers/component.rs`) and the inspector's
   `KNOBS_BY_CATEGORY` (`ui-assets/inspect.js`). A new capability also
   needs its own trait, an accessor on `SimulatedComponent`, and a row
   in the capability table test.
3. The Python client's knob (`python/src/macrocosim/_signals.py`,
   `_components.py`), and a line in `examples/starter_site.py` if the
   demo should show it.

## EV chargers

The charger and the car it charges are two different things
(`src/sim/ev_charger.rs`, `src/sim/ev_presets.rs`). The charger is
config: its spec takes `phases` (1 or 3 — its own wiring, which both
divides the offered limit into a per-phase current and caps how many of
the car's phases can be used) and `idle` (`paused`, the default, offers
nothing with no command standing; `full` offers the full rating). It
has no pack of its own, and the site import drops the battery bounds
from an export's charger.

The car is runtime state: `POST /api/mg/{mg}/component/{id}/ev` plugs a catalog
car in (`preset`, with `soc_pct`, `target_soc_pct`, `phases`, `max_current_a`,
`capacity_wh`, `taper_start_pct`, `taper_floor_pct` overrides), `DELETE` unplugs
it, `GET` reads it and the catalog. A `soc_pct` drive on a charger moves the
plugged car's SoC and is refused when nothing is plugged in. Being runtime
state, the plugged car is never part of the charger's spec; a script that should
start with a car plugged in plugs it from its `main`. A rebuild that keeps the
charger keeps its car (`PhysicsState`).

## Testing an external bounds-driving app

Macrocosim is used to test apps whose job is to push
`AugmentElectricalComponentBounds` and watch `power_bounds` react (a
GCP active-power limiter is the motivating case).

- **Every gateway-owned axis curtails to its effective (rated ∩
  augmentation) bounds every step.** The gateway re-clamps the
  standing command to the live envelope on each physics tick, so an
  external app narrowing a bound slews the output down at the ramp
  rate, and it recovers when the augmentation relaxes (tests:
  `late_augmentation_re_clamps_an_armed_setpoint`;
  `armed_target_follows_a_tightening_envelope_and_restores`). A
  controller commands a setpoint **once**; it need not re-send.
- The gateway **hard-errors** a command outside the live
  (augmentation-narrowed) setpoint envelope, faithful to the real API
  gateway. A component with children is judged by its reported bounds
  ∩ the children's. A childless one is judged by its validation
  envelope (the rated band, or the reactive capability at the live P,
  ∩ augmentations); a physical band such as the boiler's heat need is
  left out, so a command above it is accepted and held to the need. An
  EMS wanting "max within the cap" reads the bounds and commands
  within them.
- An inverter set to health `error` (or `standby`) **trips offline
  to zero output** *and* is dropped from the healthy `power_bounds`
  aggregate. A battery inverter clears its setpoint and awaits
  re-dispatch on recovery; a PV inverter resumes from sunlight; an EV
  charger trips like the battery inverter by default, and
  `resume_on_recovery` makes it keep its armed command through the
  fault and ramp back to it on recovery instead; a steam boiler trips
  like the battery inverter too, while its gas burner keeps holding
  pressure at target. A request lifetime expiring on any of them
  (granularity: the physics tick, on the site clock) retargets the
  ramp instead of snapping it: with a `ramp_rate_w_per_s` the P output
  moves back to idle at that rate (a PV inverter's idle is its sunlight
  floor), without one it still gets there in one tick. The inverters' Q
  axes ramp at `reactive_ramp_rate_var_per_s`, 2000 VAr/s unless set. A
  charger's car survives a trip; by default charging resumes with the
  next command, and with `resume_on_recovery` it ramps back on its own.
  Every command reaches the output after the gateway delay plus a
  device delay (`device_delay_s`, 0.1 s unless set), each of which lets
  a command out up to the smaller of 5 ms and half the delay early to
  allow for a tick landing early; a command still in the device's
  delay line is dropped by a trip, never replayed.
- Only a component with a gateway axis on the requested side stores an
  augmentation — both inverters on P and Q, the EV charger and the
  steam boiler on P. Every other component or axis (grid, meter,
  battery; the charger's and boiler's Q) answers `UNIMPLEMENTED`, as
  the setpoint path already does for a component that takes no
  setpoint.
- Drive sim state ad hoc over the typed routes, e.g. `curl -X POST
  http://127.0.0.1:8801/api/mg/2200/component/201/status -H
  'content-type: application/json' -d '{"health": "error"}'` (204; a
  refused write answers 400 `{"error":"…"}`), or from Python with
  `macrocosim.connect`.
- Macrocosim's physics supports closed-loop bound tests today; the
  remaining gaps are ergonomic, not physical — an in-sim
  controller/actor that reacts to live bounds and deterministic
  sim-time. See `todo.org` §I.

## Roadmap and deferred work

See `todo.org` for the forward-looking roadmap (browser-driven UI
tests, additional gRPC services, physics realism upgrades, CI
end-to-end testing) and known open design questions.
