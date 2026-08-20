# Pill nodes — design

2026-08-20. A custom-drawn node for both graph canvases (Topology and
Formulas), replacing the vis-network ellipses, with the live overlay
redesigned around it. Supersedes the node-content and node-sizing
decisions of `2026-08-20-live-topology-design.md`; the data path,
flush, chevrons, microgrid guards and toggle persistence from that
spec stay as they are.

Mockup reviewed and approved: Claude artifact "Switchyard Node Studies"
(variants D, E, F are the ones adopted).

## Problem

The live overlay was retrofitted onto ellipse nodes: vis sizes an
ellipse to circumscribe its text box, so nodes are balloons with
small, uniform monospace text; name and metrics share one size,
weight and color; zero and 20 kW look identical; a fixed box was
needed to stop values from resizing nodes. The canvas does not read
as live, and the Formulas canvas shows names while formulas speak
component ids.

## Decisions

- **One node design, drawn by us.** vis-network `shape: "custom"`
  with a `ctxRenderer` draws a rounded pill; vis keeps layouts,
  drag/snap, selection, edges and chevrons unchanged. The ellipse
  styling (`nodeStyleFor`'s shape/margins/constraints) and the
  fixed-box live-mode constraints are deleted.
- **Pill anatomy (variant D).** Left: a 9 px category dot (health
  error = red ring around the dot; standby = amber ring; hidden
  component = dashed pill border as today). Body, two rows:
  - row 1: component name (IBM Plex Sans 500, 11.5 px, foreground)
    and `#id` (IBM Plex Mono 10 px, dim) beside it;
  - row 2 (values on, when samples exist): active power as the hero
    (Plex Mono 600, 14 px, tabular) then, behind a hairline
    divider, either reactive power (Plex Mono 500, 11 px) or, for
    batteries and EV chargers, a 40×5 px SoC bar with the percentage.
  Batteries use DC power as their power value (they report no AC
  power). Width comes from the measured content (min 96 px, max
  200 px — the layout's column separation, as the old
  `widthConstraint.maximum` was; the name truncates further if row 1
  would exceed it); height from the rows present. No fixed box.
- **Values off (variant F).** Row 2 omitted; the pill collapses to
  one row and the name steps up to 13 px with the id at 11 px — the
  name becomes the hero when nothing else is there. This is both the
  topology canvas's density mode and the Formulas canvas's node.
- **Color = direction + life.** Passive sign convention (verified in
  the Frequenz common proto: +P consumption, +Q inductive). Active
  power: export (negative) `#6bd9a5`, import (positive) `#79b8ff`,
  zero/below dead band dim `#5a626d`. Reactive power: the same hues
  desaturated (`#4f9a78` / `#5a87bd`) by *its own* sign, zero dim
  (same absolute dead band as P: max(1 % of `siteMaxRatedW`, 50)) —
  so Q matching P's hue reads as lagging and a contrasting hue as
  leading, with no legend. Idle nodes (no flow) keep their surface;
  only the numbers dim.
- **Toggle.** The `live` pill becomes `values` (same
  localStorage key `switchyard-topology-live`, default on). Off =
  values-off pills, not ellipses. Sampling never stops: the toggle
  only decides whether row 2 is drawn, so the hover card and the
  sparkline buffer are complete the moment values are switched back
  on (`applySample` is no longer gated by the toggle; the 1 Hz flush
  is).
- **Formulas canvas** uses the same renderer in values-off mode. The
  formula-hover highlight maps to rings: accent ring for a
  referenced term, red ring for a subtracted one (today's blue/red
  node highlight semantics, re-expressed on the pill).
- **Hover card (variant E)**, read-only, 300 px, anchored below the
  pill, never covering its wired neighbours: header (dot, name,
  `#id · category/subtype`, health chip); 60 s active-power
  sparkline from a per-component 60-entry ring buffer kept in
  `liveValues` (seeded on first hover from the existing
  `history?window_s=300` endpoint when the buffer is still short);
  active power on its envelope bar (lower…upper with the current
  marker); reactive power with its allowed band; `PF 0.99
  leading|lagging` computed from P and Q; energy since start;
  category extras (battery: SoC bar and DC power; PV: none yet —
  protect band, capacity, DC envelope and sunlight need a
  component-detail endpoint and are deferred with the per-phase
  metrics); last command (kind, value, age, accepted/rejected with
  reason) from a cached fetch of `/api/setpoints`; wiring (parents /
  children by name); footer: freshness ("updated N s ago" — the
  stale indicator when the WS drops) and "click for inspector".
  Per-phase P/Q/V/I is deferred until the WS sampler carries
  per-phase metrics. The card is topology-only; the Formulas canvas
  keeps vis's `title` tooltip
  (`#id — name`) and the topology canvas drops `title`, so there is
  never a vis tooltip and a card on screen together.
- **Hover card lifecycle.** `pointer-events: none` (the card must
  never take the pointer from the node, or hovering flickers); shows
  after a 250 ms dwell and aborts its fetches on blur, so sweeping
  across the canvas fires nothing; hidden on blur, drag start,
  pan/zoom, click (the inspector takes over), right-click / context
  menu, subview change and any refresh that removes its node.
- **Dot colour** comes from the existing `--cat-*` tokens in
  style.css (read via `getComputedStyle` once), so the dot, the
  sidebar and the legend agree; the mockup's hexes are not a second
  palette.
- **Fonts.** IBM Plex Sans (400/500/600) and IBM Plex Mono
  (400/500/600) vendored as woff2 under `ui-assets/vendor/fonts/`
  with `@font-face` in style.css; the canvas renderer waits for
  `document.fonts.ready` before its first measure so nothing is laid
  out in a fallback face and then jumps. If the network already
  exists when the fonts land (fast first paint), the measure cache is
  invalidated, every node is re-stamped so vis re-measures, and one
  measured relayout runs — the same path `setValues` uses.

## Architecture (client-side only; no server changes)

### `ui-assets/pill.js` — the renderer (new, pure-ish)

- `measurePill(ctx, model) -> { width, height, rows }` — measures
  text with the canvas 2D context; cached per label string + state.
- `drawPill(ctx, x, y, model, state)` — draws surface, border (normal
  / selected accent / hover / formula-referenced accent ring /
  subtracted red ring / hidden dashed), dot with health ring, rows.
- `pillModel(component, live, valuesOn) -> model` — pure: name,
  id, category color, health, and the row-2 segments with their
  colors; batteries take `dc` as power; reactive or SoC segment by
  category; dead-band → dim. Unit-testable without a canvas.
- The vis node object becomes
  `{ id, shape: "custom", ctxRenderer: pillRenderer(model), ... }`;
  `ctxRenderer` returns `{ drawNode, nodeDimensions }` so vis uses
  our size for hit-testing and layout (`getBoundingBox` stays
  truthful, so the tidy layout and `pendingMeasuredRelayout` keep
  working unchanged).

### topology.js changes

- `nodeStyleFor` → builds the pill model via `pillModel` and the
  custom renderer; colorFor/health logic reused for the dot/ring.
- `flushLive` updates dirty nodes by replacing their `ctxRenderer`
  model (one `nodesDS.update` batch per second as today); vis
  `label` and, on the topology canvas, `title` are dropped.
- `setLive` → `setValues`: re-stamps every node's model with
  `valuesOn`, triggers one measured relayout (row count changes).
- Hover: vis `hoverNode`/`blurNode` events drive an HTML hover card
  element positioned from `network.canvasToDOM(node position)`;
  content from the live map, the component summary, the energy and
  history endpoints already used by the inspector, and a cached
  `/api/setpoints?id=` fetch (60 s TTL). Hidden while dragging.
- Formulas canvas: same factory, `valuesOn: false`, formula
  highlight → ring states.
- Removed: ellipse shape settings, `widthConstraint`/
  `heightConstraint`, `LIVE_NODE_*`, `liveNodeConstraints`, the
  multi-line label machinery (`liveLabel`/`liveLabelLines` are
  replaced by `pillModel`; `live.js` keeps `formatScaled` and
  `edgeFlow`).

### Edge interplay

Unchanged: chevrons and rest style from the live-topology spec.
Pill color and chevron color agree by construction (both derive from
the sign of the child's power).

## Error handling / edge cases

- Name longer than the pill's max width (e.g. 28 chars): truncate
  with an ellipsis at ~22 chars as `shortLabel` does today; the full
  name lives in the hover card and the `title`.
- Fonts not yet loaded: first measure waits for `document.fonts.ready`;
  if the vendored files fail to load, the fallback stack
  (`system-ui` / `ui-monospace`) still renders and only metrics
  differ.
- Devicepixel ratio: canvas text is drawn in CSS pixels; vis handles
  DPR scaling.
- Hover card near the canvas edge flips above the pill; it hides on
  drag start and on any topology refresh that removes its node.
- Values off / Formulas canvas: no samples are consulted; model
  building skips the live map entirely.

## Testing

- Unit (in-browser via the existing Playwright smoke pattern):
  `pillModel` — row-2 composition per category (meter: P+Q; battery:
  DC power + SoC; EV: P + SoC; grid/CHP: P only when reported), hue
  selection for P and Q by sign incl. dead band → dim, values-off
  model has no row 2, health → ring mapping.
- E2e (extend `tools/ui-smoke/live-topology.mjs`): the label-based
  assertions (`debugLabels`, "label carries a kW / SoC line") are
  rewritten against a new `debugNodeModels()` hook that returns each
  node's `pillModel`; nodes report content-derived widths that differ between short and long names
  and stay stable across flushes; values toggle collapses pills to
  one row and back; hovering a pill shows the card with name, `#id`,
  PF line and freshness; the Formulas canvas shows `#id` on every
  node and the hover-highlight ring on a referenced term; existing
  chevron/refresh/microgrid-switch assertions keep passing.
- Visual: one screenshot per canvas state (values on, values off,
  formulas with highlight, hover card) read during verification.

## Out of scope

- Per-phase metrics in the hover (needs sampler support).
- Sparklines on the pill itself (hover only).
- Dashboard cards with trend (variant B) — separate design, noted
  in the UX backlog.
- Server changes of any kind.
