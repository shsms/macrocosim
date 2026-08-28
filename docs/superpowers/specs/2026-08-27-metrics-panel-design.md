# Metrics panel — design

Date: 2026-08-27
Status: approved (design review in chat; mockup reviewed)
Todo entry: "Dashboard space rethink: attention panel over tier rows"
(todo.org) — resolved by this design more aggressively than the entry
sketched: the Dashboard subview is removed outright, not refilled.

## Context and scope

The Dashboard subview has two halves: seven derived-metric tiles
(value + hand-rolled SVG sparkline) and per-component tier rows
(battery pairs, PV inverters, EV chargers, CHP). Both are superseded:
the tiles by a floating **metrics panel** on the Topology canvas (the
formula-explorer pattern), the rows by the canvas itself (live values
on nodes, hovercard envelopes) and the inspector.

Decisions from the design review:

- The Dashboard subview is dropped entirely — nav tab, markup, row
  modules. No attention panel replaces the rows in this round; if one
  is wanted later it will be a new panel, not a subview.
- The tier rows are dropped without replacement (explicit decision;
  the "canvas can't scan 30 batteries by SoC" concern from the todo
  entry is accepted as a loss for now).
- The tiles' formula-tree doorway (`formulas.js`, hover tooltip +
  click → `formula-tree` panel) is dropped; the formula explorer
  panel is the one formula view.
- The reactive card grows per-source series: grid, PV, and battery
  reactive power — the same logical-meter formulas as the power
  streams, metric `AcPowerReactive`.
- Both panel toggles (formula explorer + metrics) live in the
  canvas-controls bar, not the top header.

Out of scope: varh (reactive-energy) accumulation; per-component
attention/alerting (the "Dashboard alert strip" and "power-flow card"
todo entries stand on their own); frequency via the logical meter
(still impossible upstream — the workaround below survives).

## 1. Server: two new reactive streams

`src/ui/loopback.rs subscribe_power_forwarders` adds two
`subscribe_reactive_forwarder` calls beside the existing grid one:

- `pv_reactive_power` = `lm.pv::<metric::AcPowerReactive>(None)`
- `battery_reactive_power` = `lm.battery::<metric::AcPowerReactive>(None)`

Verified against frequenz-microgrid: `battery`/`pv` are
metric-generic like `grid`, and the actor's typed-sender arm for
ReactivePower already exists (the grid Q stream uses it). Note
`BatteryPool::power` (the `battery_pool_power` stream) is AC — the
old tile's "aggregated DC" meta line was wrong and dies with it — so
per-source PF derives honestly from existing P streams + new Q
streams, all AC, with no extra stream.

The new streams ride the existing infrastructure untouched:
`publish_reactive` (quantity "ReactivePower", unit "var"), the
`/api/microgrid/latest` snapshot, the `/api/microgrid/history` ring,
and the `microgrid_sample` WS frames are all stream-name-generic.
`energy_stream_for` maps neither (no varh, as with grid Q). Streams
whose category is absent skip with one info log, as today.

## 2. Client: `metrics-panel.js` replaces `dashboard.js`

A new module owning the panel (shell tenant `metrics-btn`) and the
sample store. The store is `dashboard.js`'s moved wholesale: the
900-slot × 1 Hz Float32Array ring per stream (15 min), `applySample`,
`backfill` (history + latest reseed), `startAutoReseed` /
`stopAutoReseed` (5 s `/microgrid/latest` poll + visibilitychange),
`resetStream`. Two changes:

- **Gate**: everywhere the store checked "Dashboard subview visible"
  (`onDashboard()` in startAutoReseed; `applyMode`/init backfill
  triggers) it now checks `isPanelOpen("metrics-btn")`. WS
  `microgrid_sample` frames still land in the ring while the panel is
  open; when it is closed nothing fetches or paints. Opening the
  panel runs `backfill()` (history refill + latest reseed), so a ring
  that idled while closed restarts clean rather than showing a gap.
- **Paint**: instead of `data-stream` DOM lookups + sparkline SVG,
  the store notifies the panel renderer (chips + charts below). The
  tile-painting code, `renderSpark`, and the element caches die.

`gridFrequency` in `chrome.js` survives as-is conceptually — it
synthesizes the `grid_frequency` stream from the main meter's
per-component `frequency_hz` (600 s backfill cap, WS `sample`
forwarding) because the logical meter still can't carry a Frequency
formula. It retargets from `dashboardTiles` to the new store, and its
backfill now runs on panel open.

### Panel layout

Header row: "Metrics" title + a `window` pill group — `1m / 5m /
10m` — that drives the x-range of all charts (a draw-time slice of
the ring; the ring always holds 15 min). Below, three foldable cards
(the inspector's fold markup, chevron, and localStorage persistence
pattern):

- **Power** — series: `grid_power`, `battery_pool_power`, `pv_power`,
  `consumer_power`, `producer_power`. The battery envelope
  (`battery_pool_bounds_lower`/`_upper`) draws as a translucent band
  behind the battery series.
- **Reactive power** — series: `grid_reactive_power`,
  `pv_reactive_power`, `battery_reactive_power`. A **PF overlay**
  toggle (default off, persisted) adds per-source PF lines, dashed,
  on a second right-hand axis (~0.9–1.0); PF = |P|/hypot(P, Q) from
  the matching P and Q rings, computed at draw time (leading/lagging
  per the existing sign convention).
- **Frequency** — series: `grid_frequency`.

Series colors follow the category palette so chart lines mean what
the canvas already means: grid `--cat-grid`, battery `--cat-battery`,
PV `--cat-inverter-solar`, consumer `--flow-import`, producer
`--flow-export`.

### Charts

uPlot (already vendored; the inspector's Charts card is the
precedent), one per card, sized to the panel width: graduated y-axis
with faint gridlines, no x-axis labels beyond uPlot's defaults, a
dashed y=0 line when the window crosses zero, unit scaling as the
tiles had (`formatScaled` family). Data feeds straight off the rings
via `setData` on a rAF-coalesced scheduler (the `makeRenderScheduler`
pattern), so a burst of WS samples costs one redraw. Charts rebuild
(not just redata) on window change, series toggle, and card unfold —
folded cards hold no live uPlot (same lazy pattern as the inspector's
first-unfold chart build).

### Chips

Under each chart, one chip per series: color dot + stream short name
+ live value (mono, tabular-nums). Reactive chips append their PF
readout ("PF 0.98 lag"). A chip is also the series toggle — clicking
flips the series in the chart; the **off state keeps full-brightness
text** (values stay readable — mockup decision): flat/transparent
chip ground, hollow dot, thin strike through the name. Toggle state
persists per stream.

Folded cards keep their headline instantaneous value on the header's
`fold-summary` slot — Power: grid P; Reactive: grid Q + site PF;
Frequency: Hz — so a fully folded panel is a three-line live readout.
Fold-summary values update live while the panel is open (cheap text
writes, no chart).

### Persistence

localStorage, same try/catch discipline as the inspector: per-card
fold state, per-series chip toggles, PF overlay, window choice, panel
drag position (`sw-panel-pos-metrics-btn` via the shell).

## 3. Demolition and wiring

- **`dashboard.js` deleted.** Tiles store → `metrics-panel.js`;
  `batteryPairs`, `pvRows`, `evRows`, `chpRows`, `envelopeBar`,
  `makeRowModule`, `sitePfText` (superseded by per-source PF; its
  unit tests move to the new module's PF helper) all die.
- **`formulas.js` deleted** (loadFormulas, tile tooltips,
  setupFormulaTileClicks, the `formula-tree` tenant). `formula-ast.js`
  and the formula explorer are untouched.
- **index.html**: the `#dashboard` block and the Dashboard nav
  `mode-btn` go; the canvas-controls bar gains a `panels` group —
  `<span class="ctl-label">panels</span>` + `formulas` + `metrics`
  pills; `#formula-btn` leaves the top header. Help text updates.
- **Buttons**: the pills stay `side-panel.js`-synced toggles. Panel
  names are the button ids: `formula-btn` keeps its id (and tenant
  name) across the move; the metrics pill is `metrics-btn`.
  Esc/open-stack semantics unchanged.
- **routing.js**: `dashboard` leaves `VALID_SUBVIEWS`, and the
  default subview — today `"dashboard"` at every fallback site —
  becomes `"topology"`; a stale `dashboard` hash or stored subview
  resolves to `topology` (the `formulas`-redirect precedent). The `applyMode` dashboard branches (backfill
  trigger, row-module seed gating) and the row-module imports go.
  `refreshTopology`'s row `refresh(data, …)` fan-out goes;
  `gridFrequency.applyTopology` stays.
- **repl.js WS dispatch**: `kind === "sample"` drops the four
  row-module `applySample` calls, keeps `gridFrequency`;
  `kind === "microgrid_sample"` routes to the metrics store instead
  of `dashboardTiles`.
- **app.js**: `dashboardTiles.backfill()`/`startAutoReseed()` call
  sites retarget to the metrics store (reseed starts at init as
  today, self-gated on panel-open).
- **style.css**: `.dash-*`, tier-row, and the panel-dock
  dashboard-grid-column special case go; new rules for the panel
  header, cards-in-panel, chips, chart sizing.
- **`side-panel.js`**: the "drag disabled on the dashboard subview"
  special case (and the dock's dashboard grid-column CSS) dies;
  panels are pure floats everywhere. Comment prose naming the
  dashboard formula tree updates.
- **AGENTS.md**: the module map and site-PF/dashboard-rows prose
  update.
- **Server intact elsewhere**: the aggregate `*_energy` streams
  (`grid_energy` etc.) are API/WS surface with no UI consumer —
  scenario energy checks use the per-component path — and stay as
  they are. `swctl dashboard` reads `/topology` +
  `/microgrid/latest`, both unchanged; the CLI command keeps its
  name.

Knock-on: with the dashboard gone, nothing renders `soc_pct` rows or
tier health pills outside the canvas/inspector — accepted (see
Context). The `microgrid/latest`/`history` endpoints and WS stream
names are unchanged, so `swctl`/API consumers are unaffected; the two
new streams are additive.

## Testing

- Server (`src/ui/tests.rs` + `tests/ui_http.rs`): the loopback
  builds against a topology with PV + batteries → `/microgrid/latest`
  and `/microgrid/history` carry `pv_reactive_power` and
  `battery_reactive_power` with quantity "ReactivePower"/unit "var";
  absent-category topologies skip them without error.
- PF helper: pure-function unit tests (sign convention,
  leading/lagging tagging, unity clamp) replacing the `sitePfText`
  tests.
- JS: `tools/boot-smoke.mjs` keeps the module graph loading
  (dashboard.js/formulas.js deletion + new module change the import
  graph); `tools/formula-ast-test.mjs` unaffected.
- `tools/ui-smoke/live-topology.mjs`: its dashboard sections (the
  `dashboard.js` import and the Dashboard-subview click-through)
  are rewritten against the metrics panel or deleted.
- In-browser: chart/chip/fold behavior rides the pending
  browser-test harness, as with the inspector and formula panels;
  until then, live verification via the Playwright tooling used for
  the formula-explorer branch.

## Addendum — follow-up fixes (2026-08-27, same branch)

Decisions from the post-implementation review session. Root causes
were established live (Berlin demo vs the two-meters-under-grid
formula-test sites) before any of these were designed.

### A. `grid_frequency` becomes a real loopback stream

frequenz-microgrid 0.6.0 — already the locked version — carries the
Frequency sender arm (`TypedFormulaResponseSender::Frequency`;
`AcFrequency` is a CoalesceFormula and the crate's own tests call
`lm.grid::<AcFrequency>()`). The loopback comment claiming frequency
can't stream is stale. So: a frequency forwarder on
`lm.grid::<metric::AcFrequency>()` publishes `grid_frequency`
(quantity "Frequency", unit "Hz", `as_hertz`) onto the same
latest/history/WS paths as every other stream. Works on any site
shape — the old `main_meter_id` path required exactly one
grid → one meter and silently died on multi-feeder sites.

Deleted outright: the chrome.js `gridFrequency` synthesis (backfill
cap, WS forwarding, `applyTopology`), its call sites in routing.js /
repl.js / metrics-panel.js, `metricsStore.resetStream` (that feeder
was its only caller), and `main_meter_id` in the topology payload +
its test assertion.

### B. Scenario reporter consumes the grid formula streams

The journal's main-meter metrics re-key onto the loopback's
`grid_power` / `grid_reactive_power` samples, fed from the publish
path via a site hook (the loopback already holds `&MicrogridSite`).
`record_sample`'s `main_meter_id` parameter, history.rs's main-meter
P/Q pass, and `MicrogridSite::main_meter_id()` (with its tests) are
deleted. P/Q pairing for PF-at-peak keeps the last-seen P in the
journal and evaluates on Q samples — the same convention the client
uses.

Report fields rename to match the new meaning: `peak_main_meter_w`
→ `peak_grid_w`, `peak_main_meter_var` → `peak_grid_var`,
`main_meter_window_averages` → `grid_window_averages`;
`main_meter_id` leaves the report. `site_pf_at_peak_var` keeps its
name. panels.js, swctl, and the scenario/ui_http tests follow.

Accepted trade-offs (explicit): peaks now read the 1 Hz resampled
formula stream instead of raw telemetry ticks; a ~300 ms
loopback-rebuild blind window exists after mid-scenario topology
edits; formula semantics (fabricated 0.0 on silent legs) now apply
to scenario metrics too.

### C. GCP inspector: Charts card only

For the grid category the inspector renders only the Charts card —
no Component, Power, or Setpoints cards — and its one chart is the
site `grid_frequency` stream read from `metricsStore`
(`backfill()` on build + `subscribe()` for live growth), not the
GCP's own per-component history: the sim's Grid deliberately
publishes no telemetry, which is why the old chart was always empty.

### D. Panels are independent floats

`#panel-dock`'s flex column made every open panel share the dock's
height — opening a second panel shrank the first even when dragged
elsewhere (drag is a transform; layout still stacked). Open panels
become absolutely positioned floats in the dock's area: a small
cascade offset staggers panels that have no stored position, each
panel's height is viewport-bounded with its own internal scroll, and
opening/closing one never resizes another.

### E. Drag hygiene

The live drag clamp keeps the grab strip below the header (it could
previously be dragged under the fixed header and become
unreachable), and every `openPanel` sanitizes the stored position —
a position that would put the strip off-screen or under the header
is clamped back into view and re-persisted.

### F. PF: no change

PF is calculated client-side (|P| / hypot(P, Q)) and works; "PF —"
appears only at P = Q = 0 where PF is undefined — the idle state of
the formula-test sites that prompted the report.

### G. Panel sizing and drag floor (revised during user testing)

Two revisions to §D/§E after live acceptance:

- **Height is a cap, not a setting.** The native resize gripper sets
  a persisted max-height cap (`sw-panel-size-<name>`) while the
  panel's height stays content-hugging (`height: auto`): a panel can
  never be stretched past its content (the manual height converts to
  the cap when the gesture settles and the card snaps to content
  size), and expanding/collapsing a card auto-grows/shrinks the
  panel up to min(cap, dock). Shrinking below content scrolls
  `.panel-content`.
- **The drag floor is the dock's top edge**, not the header's bottom
  — the pulse bar and microgrid header sit below the header and
  could still cover a dragged strip. The dock's top clears all
  chrome by construction; both the live drag clamp and the open-time
  sanitize floor there, measured live.

### H. Inverter Q-envelope flicker

The inspector's Power card flickered its Q graduation once a second
under a setpoint loop: every accepted setpoint re-fetches the
component snapshot, whose reactive envelope was null for any
component whose children expose no Q bounds
(`reactive_setpoint_envelope` short-circuits before consulting the
component's OWN bounds), clobbering the stream-fed window. Fixed in
three layers: the snapshot's envelope falls back to the component's
own bounds (reporting only — the setpoint gates are untouched);
`applySnapshot` preserves stream-fed bounds over snapshot nulls (the
same preservation liveVal already had); and `.env-ends` keeps one
line of height when empty so transients can't resize the panel. The
graduation's VALUES tracking P is the capability physics (Q window
derives from P under a PF/apparent cap) and stays.
