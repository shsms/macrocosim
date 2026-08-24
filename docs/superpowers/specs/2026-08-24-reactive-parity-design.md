# Reactive power parity — design

2026-08-24. Brings reactive power (Q) support to the same level as
active power (P): one unified per-axis control path, reactive bounds
that can be augmented over the API, meters that can carry reactive
loads, and Q visible in queries, CSVs, scenarios and the UI. Fixes
the reported failures ("reactive bounds setting from the API fails",
"meters can have power overrides, but not reactive power") and four
open todo items (#445 live-P config bounds, #474 signed-apparent
dc_power_w, #537 meter Q aggregation, #998 PV trip).

## Problem

The P and Q control paths grew separately. Setpoints have parity
(`set-active-power` / `set-reactive-power`, one gRPC RPC for both
axes), but everything around them does not:

- `AugmentElectricalComponentBounds` hard-rejects every metric except
  `AC_POWER_ACTIVE` (`server.rs:672-677`), and there is nothing to
  route a reactive augmentation to — the trait has no
  `augment_reactive_bounds` and the Q envelope has no TTL'd queue.
  The only runtime Q-envelope knobs (`set-reactive-pf-limit`,
  `set-reactive-apparent-va`) are permanent, whole-capability
  replacements, Lisp-only.
- Reactive setpoints skip the site-level gateway envelope check that
  active setpoints get (`server.rs:264-273`, `setpoints.rs:115-124`).
- Static config bounds advertise Q sampled at the **live** operating
  point (`proto_conv.rs:146-156`): an idle PF-limited inverter
  advertises `(0, 0)` and a caching controller concludes it cannot do
  Q. Grid and EV charger — which have no Q capability at all —
  advertise a fake `±p_max`.
- Meters drive P three ways (`:power` kwarg, `set-meter-power`
  value/lambda/symbol, `drive-meter`) and Q not at all; worse, a meter
  with a P override reports Q = 0 even when its children produce Q.
- Telemetry Q bounds are a single `(lo, hi)` tuple where P carries
  multi-band `VecBounds`.
- The battery consumes a Q share from its inverters, passes it through
  unclamped, and folds it into a signed-apparent `dc_power_w` — a DC
  device that physically stores no reactive power.
- Q is invisible to scripted controllers (no query defuns), to
  scenario recordings (no reactive-bounds CSV, no Q stats in the
  report), and to most of the UI (no hover envelope bar, dashboard
  ignores `reactive_power_var`, no knobs).
- Envelope semantics agree more than the todo suggested: BOTH axes
  already re-clamp an armed target into their own live envelope every
  tick (the armed command re-polls; pinned non-destructively at
  `battery_inverter.rs:528-558`). The genuinely open d5b case is a
  CHILD battery's SoC envelope tightening under an armed inverter
  target — visible only to the site gateway, not to the component.

## Decisions

- **One per-axis control path: `PowerAxis`.** A new `src/sim/axis.rs`
  struct owns the full control machinery for one axis of one
  component: `CommandDelay` + slew `Ramp` + optional capability caps
  (the PF-limit / apparent-VA pair — set for Q axes, `None` for P)
  + a `ComponentBounds`-style TTL augmentation queue over `VecBounds`
  (both axes). Per tick it promotes delayed commands, re-clamps the
  **retained armed request** (never overwriting it — tighten →
  follow, re-widen → restore, as pinned today) into the live
  tracking envelope, slews and publishes. This codifies existing
  behavior on both axes; it changes no test outcome. The child-SoC
  half of d5b (a battery's envelope closing under an armed inverter
  target, visible only at the gateway) stays an open todo.
  The tracking envelope is static/rated bounds ∩
  caps-at-the-other-axis's-value ∩ live augmentations ∩ an optional
  **per-tick dynamic envelope hook** the owning component supplies
  (the EV charger's SoC derate; nothing for the inverters).
  Accept-time **validation** uses a separate envelope that excludes
  the dynamic hook (the EV charger deliberately validates against
  rated ∩ augmentations only, so accepts don't bounce as the cell
  tops up). Two park rules carry over as axis contracts: an empty
  intersection parks the output at 0, and an unarmed idle target is
  never clamped INTO a zero-excluding envelope (a `[5 kW, 22 kW]`
  augmentation must not make an idle charger charge). The idle
  target is per-component — 0 for battery inverter and EV charger,
  the live sunlight availability for solar (unarmed PV tracks the
  sun; `reset` parks solar at `min_avail`, not 0).
  `ReactivePath` dissolves into a `PowerAxis` with caps; the
  inverters' and EV charger's P plumbing (delay + ramp +
  `ComponentBounds`) becomes a `PowerAxis` without caps. Existing
  behaviors — `trip()`, `override_published`, per-axis reset,
  accept-validation, runtime cap setters — become axis methods.
  Battery and meter get no axes (they take no setpoints).
- **Q bounds become `VecBounds` end to end.** `Telemetry.
  reactive_power_bounds` widens from `Option<(f32, f32)>` to
  `Option<VecBounds>`; `reactive_bounds()` follows; `proto_conv`
  streams the multi-band shape the same way it does for P. Every
  single-value reader collapses Q exactly as it collapses P on the
  same surface: history/WS scalars take the first band (event names
  `reactive_power_lower_bound_var` / `reactive_power_upper_bound_var`
  keep their shape), the bounds CSV takes the outer hull (first-band
  lower / last-band upper), and `Telemetry::metric_value`
  (scenario-expect) takes the envelope extremes, matching its own
  active arms. `set-reactive-power`'s CLAMP
  clamps into `reactive_setpoint_envelope` via the same
  `VecBounds::clamp` the active CLAMP uses.
- **Reactive augmentation over the API.** The trait gains
  `augment_reactive_bounds(create_ts, VecBounds, lifetime)` (default
  no-op; implemented by both inverters via their Q axis).
  `AugmentElectricalComponentBounds` accepts `AC_POWER_REACTIVE` and
  routes there, reusing `validate_augmentation` unchanged. Metrics
  other than active/reactive AC power still get the invalid-argument
  rejection.
- **Gateway parity.** `MicrogridSite::reactive_setpoint_envelope(id)`
  mirrors `active_setpoint_envelope`; both gRPC `do_set_power`
  (reactive arm) and `(set-reactive-power)` run the same gateway
  cross-check the active path has, and `CLAMP` clamps into the same
  envelope.
- **Config bounds advertise the capability hull.** The static
  `AcPowerReactive` config bound in `ListElectricalComponents` is the
  widest Q the device could ever deliver — the max over P of the caps
  envelope (kVA cap → ±VA at P = 0; PF cap → ±k·P_rated at rated P;
  both → the widest point of their intersection; NEITHER cap set →
  the `|Q| ≤ |P|` fallback cone, whose hull is ±P_rated). Never zero
  for a capable device, independent of the operating point (closes
  todo #445). Components without a Q axis advertise `(0, 0)` — no
  more fake `±p_max` for grid and EV charger. Naming note:
  `:reactive-pf-limit` is the ratio k = |Q|/|P| (as today), while
  the meter driver's `:power-factor` is true cos φ — both meanings
  stay, and the docs say so in one sentence.
- **EV chargers stay P-only, but honestly so.** No Q axis; they
  advertise `(0, 0)` config bounds AND publish
  `reactive_power_var: Some(0.0)` in telemetry, so per-component
  telemetry, the WS feed, and the UI hover readout stay truthful
  about Q being settled at 0 rather than silently omitted. Any
  P-only AC component follows the same rule. (Corrected 2026-08-24,
  SP3 mutation-check: the telemetry zero is *not* load-bearing for
  upstream formula convergence — the aggregation's own
  `COALESCE(..., 0.0)` already resolves a component that never
  streams Q, confirmed against the vendored `frequenz-microgrid`
  crate sources.)
- **The battery loses its Q axis.** Reactive power terminates on the
  inverter's AC side: inverters no longer push a Q share to children,
  `set_dc_active_reactive` is removed from the trait (its only real
  consumer was the battery), `dc_power_w` becomes pure active DC
  power (closes todo #474), and battery telemetry carries no Q (it
  already carries none). `dc_accept_ratio` stays P-only and correct
  by construction. The inverter's health / no-healthy-children gates
  keep zeroing the PUBLISHED Q — only the child push disappears, so
  a dead DC bus still means zero VArs. `dc_current_a` changes
  meaning with `dc_power_w` (pure P over voltage); the two doc
  contracts describing the signed-apparent fold (`battery.rs:59-66`,
  `battery.rs:237-244`) are rewritten with it; and the scenario
  charge/discharge Wh integrals get *smaller* (correct) when Q ≠ 0 —
  a number change, not a regression.
- **Meters carry reactive loads.** `Meter` gains a reactive source
  slot next to `power_source`, an enum:
  - `Var(DynamicScalar)` — constant / lambda / symbol, exactly like
    `:power`;
  - `PowerFactor { pf, leading }` — Q derived from the meter's
    **live P** at read time: `|P| · tan(acos(pf))`, negated when
    `leading` (PF is always positive; lagging/inductive/+Q is the
    default, matching the passive-sign convention the hover card
    uses).
  Constructor kwargs `:reactive-power` and `:power-factor` (+
  `:leading t`) are mutually exclusive — both at once is a config
  error. Constants persist into generated blocks via
  `constructor_kwargs` the same way `:power` does (floats via
  `lisp_float32`; `:leading t` renders like `:hidden t`; both new
  kwargs join the render round-trip test); lambda/symbol sources are
  runtime state, not persisted. The constructed-vs-poked freeze
  applies: runtime `set-meter-reactive-power` /
  `set-meter-power-factor` never change what `constructor_kwargs`
  renders, exactly like `set-meter-power` vs `:power`. And
  `has_unrenderable_source` ORs in the reactive slot, so Adopt and
  the save-warning path warn about a dynamic Q source they cannot
  write down.
- **Trait + DSL surface for meter Q.** `set_reactive_power_override`,
  `set_reactive_power_source`, `takes_reactive_power_override`
  (defaults false/no-op, Meter implements); defuns
  `(set-meter-reactive-power ID VALUE)` and
  `(set-meter-power-factor ID PF &optional LEADING)`; scenario
  wrappers `(drive-meter-reactive ID SOURCE)` and
  `(drive-meter-pf ID PF &optional LEADING)` compiling to the defuns
  the way `drive-meter` does.
- **Meter Q aggregation fix (todo #537).** A meter with a P override
  no longer zeroes Q: it reports its own reactive source when one is
  set, else sums its children. A CHP's Q therefore lives on its
  neighbor meter, the same pattern as its P.
- **Readout.** Query defuns `(component-reactive-power ID)`,
  `(component-reactive-bound-lower ID)` /
  `(component-reactive-bound-upper ID)` beside their active twins;
  scenario recording writes `<id>-reactive-bounds.csv` for every
  component with a Q axis (same shape as the active bounds CSV); the
  scenario report gains peak main-meter Q and the site PF at that
  peak. No reactive energy (varh) accumulator — deliberately out.
- **PV health-trip parity (todo #998).** The solar inverter's
  health-trip arm calls `trip()` on its **reactive axis only** (snap
  the Q ramp, drop the armed Q command), matching the battery
  inverter. The P side keeps its armed curtailment across a trip —
  reviewed and ruled intended earlier, not overturned here; #998's
  own alternative (keep armed, snap actual) is rejected in favor of
  the full Q trip.
- **UI.** Hover card draws the Q envelope bar + marker it already
  computes (`envelopeBar` gains a unit parameter — it hardcodes
  "W" today and would mislabel VArs). Dashboard adds
  `reactive_power_var` to its tier sets and WS dispatch, plus one
  site tile — grid Q with derived site PF — fed by a new loopback
  subscription to the upstream client's typed `ReactivePower` grid
  formula stream (a `ReactivePower`-typed forwarder beside the
  monomorphic `Power` one: `.as_volt_amperes_reactive()`, quantity
  "ReactivePower", unit "var", deliberately absent from
  `energy_stream_for` so no varh sneaks in). Inspector: inverters
  get a `set-reactive-power`
  setpoint knob beside the existing cap knobs; meters get
  `reactive power (VAr or expr)` and `power factor (+ leading)`
  knobs. Edge chevrons stay P/DC-based.
- **Not added, on purpose:** no gRPC route for
  `set-reactive-pf-limit` / `set-reactive-apparent-va` — capability
  mutation is a simulator poke (like `set-meter-power`), not a
  controller API.

## Architecture

### Sub-project 1 — the axis and the API (core)

- `src/sim/axis.rs` (new): `PowerAxis` as decided above; unit-tested
  standalone (promote → re-clamp → slew, caps ∩ augmentation ∩
  static, TTL expiry, trip/override/reset).
- `src/sim/reactive.rs` shrinks to the caps math
  (`ReactiveCapability`, `q_bounds_at`, hull computation) consumed by
  `PowerAxis`; `ReactivePath` is deleted.
- Inverters and EV charger rebuild their P (and for inverters, Q)
  control paths on `PowerAxis`; behavior pinned by the existing
  setpoint/ramp/reactive tests, updated where d5b's re-clamp changes
  outcomes (an armed P target outside a tightened envelope now
  follows the envelope).
- Battery: Q axis removed (`set_dc_active_reactive` gone,
  `dc_power_w` pure P); inverter push path sends P only.
- `server.rs`: augment RPC accepts `AC_POWER_REACTIVE`;
  `reactive_setpoint_envelope` gateway checks in both gRPC and
  `setpoints.rs`.
- `proto_conv.rs`: hull-based static Q config bounds; honest zeros;
  multi-band Q sample bounds.
- Solar trip fix.

### Sub-project 2 — meter reactive loads and readout

- `Meter` reactive source enum + kwargs + persistence
  (`constructor_kwargs`, round-trip test).
- Trait methods, defuns, scenario wrappers as decided.
- Aggregation fix; query defuns; reactive-bounds CSV; scenario report
  Q stats.
- Python client parity: the meter builder gains
  `reactive_power` / `power_factor(+leading)` kwargs and the
  scripting layer a reactive `DrivenSignal` twin of `Meter.power`
  (`python/src/switchyard/build.py`).
- `scenarios/README.md`, `AGENTS.md` documentation.

### Sub-project 3 — UI and loopback

- Hover envelope bar; dashboard tier/WS Q + grid-Q/site-PF tile;
  loopback `ReactivePower` grid formula subscription; inspector
  knobs; ui-smoke checks.

## Error handling

- `:reactive-power` + `:power-factor` together → constructor error
  naming both kwargs.
- `:power-factor` outside `(0, 1]` → error (0 would divide by zero in
  the tan; 1 means Q = 0 and is allowed).
- A reactive augmentation on a component without a Q axis is ACKed as
  a no-op — the same (known) behavior the active side has
  (todo #1007); not made worse, not fixed here.
- `set-meter-reactive-power` / `set-meter-power-factor` on a
  non-meter → the same not-found/unsupported errors the active
  drivers give.
- NaN/non-finite VAr values rejected everywhere finite P values are.

## Testing

- Axis unit tests as above, including the d5b re-clamp behavior on
  the P axis (pinned explicitly, since it is a deliberate behavior
  change).
- gRPC: augment-reactive round trip (bounds visible in streamed
  telemetry, expire on TTL); reactive setpoint gateway rejection;
  config-bounds hull values for PF-only, kVA-only, both,
  **neither-cap (fallback cone)**, and no-axis components.
- Axis contracts: park-at-0 on empty intersection; unarmed idle
  target not clamped into a zero-excluding envelope; solar idle
  target tracks availability and reset parks at `min_avail`;
  validation-vs-tracking envelope split (EV SoC derate clips output
  but never rejects accepts).
- Formula convergence: a grid-Q formula over a site containing an
  EV charger converges, exercising the loopback stream end to end.
  (Corrected 2026-08-24, SP3 mutation-check: convergence does not
  depend on the EV's telemetry zero — see the note above.)
- Meter: PF sign tests (lagging/leading), live-P tracking, override
  vs children aggregation, restart round-trip of the new kwargs.
- Scenario: drive-meter-reactive / drive-meter-pf end to end with
  `:metric 'reactive-power` checks; reactive-bounds CSV columns.
- UI smoke: dashboard tile present, hover card Q bar drawn, knobs
  post the right forms.

## Out of scope

- Reactive energy (varh) accumulation.
- Per-phase reactive setpoints (d4).
- The Formulas subview for Q (the external component-graph crate is
  P-only).
- EV charger reactive capability.
- Q-based edge flow rendering.
- A gRPC surface for capability (PF/kVA cap) mutation.

## Appendix — parity survey (2026-08-24, abridged)

Key file:line facts the design rests on:

- Augment rejection: `server.rs:664-677`; wire proto has
  `AcPowerReactive = 30`.
- Gateway asymmetry: `server.rs:264-273` (P-only check),
  `setpoints.rs:74-76,115-124`; no `reactive_setpoint_envelope` in
  `microgrid_site/mod.rs` (only `active_setpoint_envelope`, :705).
- Live-P config bounds: `proto_conv.rs:128-157`; inverter
  `reactive_bounds()` samples measured/actual P
  (`battery_inverter.rs:295-297`, `solar_inverter.rs:277-279`).
- Meter: single P-only `power_source` (`meter.rs:27`);
  `aggregate_reactive` zeroes on override (`meter.rs:93-101`); no
  reactive kwarg (`make.rs:53-70`); no reactive driver defun
  (`load_drivers.rs`).
- Battery Q pass-through: `battery.rs:186,260-265`; signed-apparent
  `dc_power_w`: `battery.rs:214`; `set_dc_active_reactive` default
  drops Q: `component.rs:508-510`.
- Q bounds tuple vs P `VecBounds`: `component.rs:203-208`.
- ReactivePath (inverters only): `reactive.rs:112-208`,
  `battery_inverter.rs:60`, `solar_inverter.rs:80`; EV charger has no
  Q axis (`ev_charger.rs`, no `set_reactive_setpoint`).
- Q re-clamps every tick where P does not: noted at todo d5b
  (`todo.org:1279-1285`).
- Readout gaps: `queries.rs:49-70` (P-only); bounds CSV active-only
  (`microgrid_site/scenarios.rs:154-158`, `history.rs:69`); scenario
  report P/Wh-only (`microgrid_site/scenarios.rs:47-83`).
- UI: hover card computes Q envelope but draws no bar
  (`hovercard.js:77-87,150-151`); dashboard has no
  `reactive_power_var` anywhere (`dashboard.js:317-320,483-491`);
  loopback subscribes only `AcPowerActive` formula streams
  (`src/ui/loopback.rs:237-274` — upstream client supports typed
  `ReactivePower`); no setpoint/driver knobs (`inspect.js:154-162`).
- Already at parity (unchanged): setpoint defuns + RPC
  (`setpoints.rs`, `server.rs:195-314`), per-axis TTL/reset
  (`component.rs:365-376`), scenario assertion metrics
  (`scenarios.rs:229-244`), meter tree walk shared for both axes
  (`meter.rs:74-101`), inspect.js live charts (`inspect.js:17-37`).
