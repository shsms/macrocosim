# Pill colour and zoom tiers — design

2026-08-21. Makes the pill nodes read at every zoom level: category
colour moves from a 9 px dot to a bar on the pill's edge and a tinted
border, live flow tints the surface, and the renderer drops text as
the canvas zooms out. Follows `2026-08-20-pill-nodes-design.md`; the
pill anatomy, model and hover card from that spec stay as they are.

Mockups reviewed in the brainstorming companion
(`.superpowers/brainstorm/161955-1787297638/content/`): screen 2
variant D was chosen, with today's surface colour kept.

## Problem

The pill redesign put the category colour on a 9 px dot and set the
pill surface (`#242a33`) only a few steps above the canvas
(`#1c2128`). On a large graph zoomed out, the dot is sub-pixel and
the pills barely register; the old colour-filled ellipses were louder
there. The zoomed-in look is right and must stay.

## Decisions

- **Category bar.** A 6 px bar on the pill's left edge, clipped to the
  rounded corner, in the component's category colour (the `--cat-*`
  token; inverters keep their subtype shade). The dot is removed; text
  starts 10 px right of the bar.
- **Tinted border.** 1.5 px, colour = the pill border grey mixed 35 %
  toward the category colour. Hidden components keep the `[4, 3]`
  dash. Selection (accent, 2 px), hover (`#b0b8c1`, 1.5 px) and the
  formula "subtracted" ring (red, 2 px) take precedence, as today.
- **Health ring** moves from around the dot to around the pill:
  1.5 px, 2.5 px outside the border, red for error, amber for standby.
- **Surface** stays `#242a33`. While the hero power is past the dead
  band it is blended 7 % toward the flow hue (export green / import
  blue; batteries by DC power); otherwise neutral. Values off: never
  tinted.
- **Zoom tiers (level of detail).** What is painted depends on the
  canvas scale; the model, the pill size and the layout do not:
  - `full` (scale ≥ 0.8): today's two rows.
  - `hero` (0.4 ≤ scale < 0.8): one row, the hero power only, Plex
    Mono 600 16 px, vertically centred; no name, id or aux.
  - `marker` (scale < 0.4): no text.
  - Hysteresis: a tier changes only when the scale crosses its
    threshold by 0.05 (e.g. `full` → `hero` below 0.75, `hero` →
    `full` above 0.85), so panning at a boundary does not flicker.
- **Formulas canvas** shares the renderer: it gets the bar, border and
  tiers; values are off there, so no tint and no hero row (the `hero`
  tier paints nothing but the pill, like `marker`).
- **Palette tokens.** The colours `pill.js` and the hover-card CSS
  hardcode become `:root` custom properties in `style.css`:
  `--pill-surface #242a33`, `--pill-border #323a45`, `--pill-fg
  #d5dbe3`, `--pill-muted #7d848e`, `--flow-export #6bd9a5`,
  `--flow-import #79b8ff`, `--flow-dim #5a626d`, `--flow-export-dull
  #4f9a78`, `--flow-import-dull #5a87bd`, `--pill-hover #b0b8c1`;
  accent, bad and standby reuse the existing `--accent`, `--bad` and a
  new `--standby #c4ad55`. `pill.js` reads them once via
  `getComputedStyle` (the `getCss` pattern in topology.js). No visual
  change from this step alone.

## Architecture (client-side only)

### pill.js

- `COLORS` is built from the tokens at module load.
- `GEOM`: `dot`/`dotGap` are replaced by `bar: 6` and `barGap: 10`;
  `textLeft = bar + barGap` where the bar sits flush with
  the pill's left edge (no left padding before it).
- `drawPill(ctx, x, y, model, state, lod)` — `lod` is `"full" |
  "hero" | "marker"`; default `"full"` so existing callers and unit
  tests keep working. `measurePill` is unchanged (sizes come from the
  full tier).
- Pure helpers, unit-tested: `borderColor(catColor)` (35 % mix),
  `surfaceColor(heroValue, deadBand)` (7 % blend or neutral),
  `lodFor(scale, previousLod)` (thresholds + hysteresis).
- `pillRenderer(model, onSize, getLod)` — `getLod` is a function the
  renderer calls on every draw to learn the current tier.

### topology.js

- Keeps one `lod` per canvas, recomputed in the `afterDrawing`
  handler (or on the vis `zoom` event) from `network.getScale()` via
  `lodFor`; when it changes, `network.redraw()` once. The renderer's
  `getLod` returns it. No `nodesDS.update`, no relayout on zoom.
- `nodeFor` passes the category colour into the model as today
  (`dotColor` is renamed `catColor`).
- Debug hook `debugLod()` for the smoke test.

### style.css / hovercard

- The tokens above on `:root`; `.hover-card` rules use them.

## Error handling / edge cases

- A missing token (older stylesheet) falls back to the current literal
  so the canvas never renders transparent.
- `lodFor` with a non-finite scale returns the previous tier.
- Tier switches never change `nodeDimensions`, so `knownSize`,
  bounding boxes and the width ratchet are untouched.

## Testing

- Unit (smoke, in-browser): `borderColor`, `surfaceColor` (dead band,
  sign, null, values-off), `lodFor` across thresholds with hysteresis
  in both directions, `measurePill` unchanged by `lod`.
- E2e: `network.moveTo({ scale })` through a `debugSetScale(s)` hook,
  then `debugLod()` reports `full`/`hero`/`marker` at 1.0 / 0.6 / 0.3
  and stays `hero` at 0.78 coming from 0.6; `debugNodeWidths()` and
  `debugNodeHeights()` identical across the three scales; existing
  pill, toggle, hover and chevron checks keep passing.
- Screenshots at 1.0×, 0.55×, 0.3× on the Berlin demo and on the
  Formulas canvas.

## Out of scope

- A filled-marker overview tier (rejected: the bar + tinted border
  read well enough at 0.3×).
- Edge colouring by category, legend changes, dashboard colours.
- Server changes.
