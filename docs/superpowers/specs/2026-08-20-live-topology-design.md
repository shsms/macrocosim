# Live topology — design

2026-08-20. Live power values and flow direction on the Topology
canvas, so the screen that draws the site also shows what the site is
doing.

## Problem

All live data (power, reactive power, SoC) lives in the Dashboard and
the inspector; the Topology canvas draws static name bubbles. An
operator watching a site cross-references two screens or clicks nodes
one at a time. The canvas should carry the live numbers and the flow
itself.

## Decisions (settled during brainstorming)

- **Node content**: live metrics as extra label lines, one metric
  per line, bare numbers. Batteries report no AC power, so they show
  `SoC 85%` then their DC power (`-3.00 kW`); EV chargers show AC
  power then `SoC 40%`; everything else shows AC power then reactive
  power (`1.20 kVAr`) when the component reports it. No mini-cards.
- **Nodes don't resize with values**: while live is on every node
  gets a fixed label box (132 × 54 px — the widest name, three
  lines), so a value growing from `0.0 W` to `-19.93 kW` or a line
  appearing never changes a node's size or shifts the layout. Off
  restores the natural sizing.
- **Edges carry both structure and flow, on separate channels**:
  the existing muted end arrowhead keeps encoding parent→child
  wiring, unchanged. A new mid-edge chevron in the accent color
  encodes live flow: direction = physical flow (export points toward
  the parent), size/width from magnitude, absent when the edge is
  dead. The channels differ in position, color, and presence, so
  they can't be confused.
- **Sign convention**: switchyard is consumption-positive; the
  chevron maps to *physical* flow so the picture matches an
  electrician's intuition. A battery's chevron flips between charge
  and discharge.
- **Data source**: the existing 1 Hz history-sampler WS broadcast —
  it already carries `(id, metric, value)` for every component
  (ActivePowerW, ReactivePowerVar, SocPct, …). No new endpoint, no
  server changes, no added load.
- **Toggle, default on**: a `live` pill beside the layout picker
  (topology only; the Formulas canvas stays static). State persists
  in localStorage. Off = today's canvas exactly.
- Noted for later, out of scope here: reactive power in the
  Dashboard tiles/rows (recorded in the todo backlog).

## Architecture

All client-side, all in `ui-assets/`.

### Live state (topology.js)

A module-level map + dirty-set inside the `topology` IIFE:

```
liveValues: Map<id, { p: number|null, q: number|null, soc: number|null }>
liveDirty:  Set<id>
```

- `topology.applySample(ev)` — new entry point called from app.js's
  existing WS `microgrid_sample` routing (alongside the dashboard
  modules): updates `liveValues` for `active_power_w`,
  `reactive_power_var`, `soc_pct` metrics, adds the id to
  `liveDirty`. Ignored when the toggle is off.
- A 1 Hz flush (armed only while dirty ids exist AND the topology
  subview is visible AND live is on) builds ONE
  `nodesDS.update([...])` for the dirty nodes' labels and ONE
  `edgesDS.update([...])` for their edges, then clears the set. One
  redraw per tick regardless of component count.
- While the subview is hidden the flush stays parked; samples keep
  accumulating in `liveValues`, and the first visible flush catches
  up (subview-enter calls the flush once).
- `apply()` (topology refresh) prunes `liveValues` to the current
  component set. A component with no sample yet renders its
  structural label only — never stale or placeholder numbers.
- Health rendering, selection, drag/snap, layouts: untouched.

### Node labels

`liveLabelLines({ category, p, q, soc, dc })` — pure function
returning the metric lines in display order ([] until a sample
arrives; the node then keeps its structural one-line label):

- battery: `SoC <n>%`, then DC power (batteries emit `dc_power_w`
  and `soc_pct`, never `active_power_w`).
- ev-charger: AC power, then `SoC <n>%`.
- everything else: AC power, then reactive (`<n> kVAr`) when
  reported.
- power values use the same W → kW → MW ladder as the dashboard
  (shared `formatScaled`, not a copy).

vis-network multi-line labels are plain `\n` text; the second line
uses the existing label font (scales with zoom like the name).

### Edge flow (chevrons)

`edgeFlow(childPower, parentCount, siteMaxRatedW)` — pure function
returning the vis edge attributes:

- Flow on a parent→child edge = the child's active power divided by
  its parent count (the meter aggregation sharing rule, so parallel
  paths split visually too).
- Direction: consumption (positive) points the chevron toward the
  child; generation/export (negative) toward the parent. Implemented
  with vis-network's `arrows.middle` and a negative `scaleFactor`
  for the flipped direction — one attribute update, no custom
  drawing.
- Magnitude: chevron `scaleFactor` magnitude and edge `width` scale
  with `sqrt(|flow| / siteMaxRatedW)`, clamped to [1.5, 6] px
  width (1.5 is the canvas's default edge width).
  `siteMaxRatedW` = the max absolute rated bound across the site's
  components (recomputed on topology refresh; fallback 10 kW when
  nothing is rated).
- Dead band: below max(1% of `siteMaxRatedW`, 50 W) the chevron is
  disabled and the edge renders exactly as today — dead legs look
  dead.
- The structural end arrowhead is never modified.

### Toggle (index.html + topology.js)

- A `live` pill button in `#topology-controls` next to the existing
  layout pills, `active` styling like the snap toggle.
- Off: flush parked, labels revert to name-only, chevrons removed
  (one bulk update), `topology.applySample` becomes a no-op.
- Persisted under `switchyard-topology-live` (default on when the
  key is absent).

### app.js wiring

The WS sample router gains one line: forward per-component metric
samples to `topology.applySample(ev)` (same place the row modules
receive them). No change to the WS protocol or subscription.

## Error handling / edge cases

- WS drop or reconnect: values freeze at last-known; on reconnect
  the stream resumes and overwrites. (The data-freshness indicator
  in the UX backlog would cover signaling this globally; not this
  branch.)
- Component removed mid-session: pruned from `liveValues` on the
  next topology refresh; vis removes its edges with it.
- Toggling live swaps every node between the fixed live box and
  natural sizing, so it triggers one measured relayout
  (`pendingMeasuredRelayout`); within a mode node sizes are constant.
- Hidden components render dashed on this canvas and get live
  labels/chevrons like any other node.

## Testing

- Unit (pure functions, in a small JS test or exercised via
  Playwright `page.evaluate`): `nodeLabel` formatting per category
  (power only / power+reactive / battery SoC, null-handling),
  `edgeFlow` direction/sign mapping, dead-band, width clamp,
  parent-count sharing.
- End-to-end (Playwright tour against the berlin-demo config):
  live on → some node's label carries a `W`/`kW` line, the battery
  shows `SoC` then DC power on separate lines, an inverter shows
  power then reactive on separate lines, node widths are identical
  across flushes, some edge has a middle chevron, and the hidden
  consumer meter's edge (always consuming, independent of PV
  sunlight) points at the child; a no-op eval's topology refresh
  leaves labels and chevrons in place; toggle off → labels are
  name-only and no middle arrows; toggle state survives reload.
- Perf: the single parked 1 Hz flush timer makes more than one
  DataSet update per second structurally impossible; no separate
  assertion.

## Out of scope

- Reactive power in the Dashboard (backlog).
- Animated flow dashes (rejected: continuous redraw cost).
- Server-side changes of any kind.
- Formulas-canvas live values.
- Data-freshness/stale indicators (separate backlog item).
