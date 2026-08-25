# Component inspector redesign — design

Date: 2026-08-25
Status: approved (design review in chat; mocks at
https://claude.ai/code/artifact/ec94c02a-12f3-4b44-9190-9c7d9a280655)
Todo entry: "Inspector redesign for component selection" (todo.org)

## Scope

Redesign the floating component inspector (today: `inspect.js
renderInspect`, one flat string-templated `<dl>`) and add the server
read-back surface that fixes its write-only knobs. Three problems from
the todo entry, all in scope:

1. **Blind knobs** — knobs are write-only and clear on submit. Fix:
   read-back (current value, envelope, active setpoint) with **fully
   live** updates, via a new snapshot endpoint + WS event.
2. **Triple-serving panel** — the `#inspector` card hosts node
   inspection, formula trees, the Defaults editor, and the scenario
   report via `dataset.panel`. Fix: a generic side-panel shell with
   tenants; only the component inspector is redesigned, the other
   tenants are re-hosted unchanged.
3. **No reactive envelope display** — bounds stream but the inspector
   doesn't show them. Fix: envelope bars (hovercard treatment) for P
   and Q.

Out of scope: E2 schema-driven form generation (stays deferred per its
own entry), redesign of formula/Defaults/report tenants, site-level
controls (captured as its own todo entry), "Inspector setpoint
ergonomics" and "pin and compare" UX entries.

## Server: read-back surface

### `GET /api/mg/{mg}/component?id=N` (and legacy `/api/component`)

One snapshot payload with everything the panel needs on open:

- **Knobs**: per-knob current value; for expression-driven
  `DynamicScalar` knobs additionally the printed Lisp source form.
  Coverage is exactly today's knob set (`KNOBS_BY_CATEGORY` +
  `knobsFor`): meter power / reactive power / power factor (incl. the
  `leading` flag), inverter reactive setting + PF limit + apparent VA,
  solar sunlight. The endpoint only reads state components already
  store — no new stored state.
- **Setpoints**: active setpoint per axis with remaining TTL (ms).
- **Augmentations**: whether an augmentation is active per axis.
- **Envelope**: current P/Q bounds, so bars paint without waiting for
  the next bound `Sample`.
- 404 on unknown component or microgrid.

### `SiteEvent::KnobChanged { id, knob, value, expr }`

Emitted at the point of state change — the site-level setter methods
the defuns call, NOT the HTTP layer — so REPL, scenario, and UI writes
all broadcast. Fire-and-forget on the existing broadcast bus, like
`Setpoint`. (`TopologyChanged` only fires on `/api/eval`, so it cannot
carry scenario-driven knob changes; this event closes that gap.)

Envelope liveness needs no new surface: bounds already stream as
`Sample` metrics (`active_power_lower_bound_w`, …) and `topology.js`
caches them. Setpoint liveness rides the existing `Setpoint` event;
the TTL countdown is client-side, corrected by each event.

## Client: panel split

- New shell module (`side-panel.js`) owns the floating `#inspector`
  card: open/close, `dataset.panel`, chrome-button highlight sync,
  Esc/×/tab-switch teardown. API: `openPanel(name, renderFn,
  teardownFn)` / `closePanel()`.
- Tenants: component inspector (redesigned), formula tree, Defaults
  editor, scenario report (all three re-hosted with their current
  rendering verbatim).
- Teardown is per-tenant: inspector tears down charts + WS
  subscription; the report tears down its poll timer. Today's
  `clearSide()` kills both unconditionally — that coupling goes away.
- `inspect.js` shrinks to the component-inspector tenant; shared
  `evalQuoted` / `jsToLispString` helpers move to a neutral module.

## Client: the redesigned inspector

Sections, top to bottom (mocks show the final look):

- **Header**: name edit-in-place (unchanged), id, category chip in the
  category colour, health chip, "augmented" badge when active.
- **Graph**: the declared operational mode — config-level, part of the
  graph declaration, so it sits apart from the runtime knobs.
- **Simulation**: health / telemetry / commands — the runtime fault
  injectors. Commands row hidden for categories without a setpoint
  surface (as today).
- **Power**: envelope bars for P and Q (hovercard bar treatment): live
  tick, active-setpoint marker + text row with TTL countdown; then the
  knobs.
- **Charts**: foldable, folded by default; history fetch and
  live-chart wiring deferred to first unfold; unfold state remembered
  per session (localStorage).
- **Recent setpoints**: as today.
- **Connections**: collapsed one-line footer (n parents · m children),
  expands on click to the parent/child lists with disconnect buttons.

Interaction decisions:

- **Segmented chips replace the state dropdowns** (mode, health,
  telemetry, commands): every option visible, one click to switch,
  reusing the `#mg-subtoggle` toggle vocabulary. Active chip is
  colour-coded — blue normal, green ok, yellow/red for injected
  faults — so a component left in `silent` or `timeout` is visible at
  a glance. Cost: `mode` and `telemetry` wrap to two rows in the
  420 px panel.
- **Edit-in-place knobs**: inputs pre-filled with the live value;
  `KnobChanged` refreshes them while unfocused; focus freezes the
  field; Enter applies (existing `evalQuoted` setter path), Esc/blur
  reverts to live. Expression-driven knobs render the printed source
  with an "expr" chip and the resolved per-tick value beneath; editing
  the text edits the expression. The PF `leading` checkbox reflects
  read state.
- **Rejected write**: toast (as today) + input snaps back to the live
  value instead of clearing.

## Testing

Per the scope call, Rust-side only (browser tests arrive with the B1
Playwright harness, whose todo entry now lists the inspector):

- `/api/component` handler tests beside the existing `src/ui` handler
  tests: snapshot shape per category (inverter knobs; meter expression
  knob printed back; PF flag; setpoint with remaining TTL; envelope),
  404s.
- `KnobChanged` emission tests following the `SetpointEvent` patterns
  in `ui/tests.rs`, including setters driven from scenario/REPL eval
  paths, not just HTTP.
- JS stays within the existing biome gate; panel behaviour verified by
  hand in the running app.

## Error handling

Degrade per-section; never break the panel (the existing charts /
setpoints discipline):

- Snapshot fetch fails → knobs render as today's blank write-only
  inputs with an "unavailable" hint; other sections unaffected.
- WS reconnect or lagged receiver → re-fetch the snapshot (mirrors
  the topology refetch), with the existing stale-generation guard on
  rapid reselection.
- Unprintable expression source → placeholder label; the input still
  accepts a replacement expression.
- TTL countdown drift → corrected by each `Setpoint` WS event.
