# Pill Colour and Zoom Tiers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the pill nodes read at every zoom: category colour as a 6 px edge bar and a tinted border, a faint live tint on the surface, and text that drops away as the canvas zooms out.

**Architecture:** `ui-assets/pill.js` keeps owning the look: its colours come from new `:root` tokens, three pure helpers decide border colour, surface colour and the level-of-detail tier, and `drawPill` takes a `lod` argument. `ui-assets/topology.js` tracks one tier per canvas from `network.getScale()` and redraws when it changes — the model, the pill size and the layout never change with zoom.

**Tech Stack:** Vanilla ES modules in `ui-assets/`, vis-network 9.1.10 (vendored), Playwright smoke script `tools/ui-smoke/live-topology.mjs`.

**Spec:** `docs/superpowers/specs/2026-08-21-pill-colour-design.md` (binding).

## Global Constraints

- Client-side only. No server / Rust changes.
- Commits: imperative subject, a body saying why, trailer exactly `Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>`; no Co-Authored-By / AI trailers; stage files by name (never `-A`/`.`/`-u`); never add `.nfs*` files.
- Branch `pill-colour` (checked out in `/vagrant/switchyard`, off `main`). Do not merge.
- Dev server serves `ui-assets/` from disk at `http://127.0.0.1:45109` (`UI=http://127.0.0.1:45109`); JS/CSS edits are live on reload. If it is down, see `AGENTS.md`; builds need `PROTOC=/tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad/protoc/bin/protoc`.
- RUN SMOKE = from `/vagrant/switchyard`: `SW_UI=$UI node tools/ui-smoke/live-topology.mjs 2>&1 | tee /tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad/smoke.log | grep -E "^(FAIL|ALL PASS|[0-9]+ FAILED)"` — a helper timeout throws with no FAIL line, so check the log tail if the grep prints nothing. Known occasional flake: none expected now; re-run once if a timing check fails and note it.
- Exact values (spec): bar 6 px flush with the pill's left edge, text starts 10 px right of it; border 1.5 px = `--pill-border` mixed 35 % toward the category colour; health ring 1.5 px, 2.5 px outside the border (red `--bad` error, amber `--standby` standby); surface `--pill-surface #242a33`, blended 7 % toward the flow hue when |hero| ≥ dead band, never when values are off; tiers `full` (scale ≥ 0.8), `hero` (0.4 ≤ scale < 0.8, hero only, Plex Mono 600 16 px), `marker` (< 0.4, no text), hysteresis 0.05.
- Tokens (values = today's literals): `--pill-surface #242a33`, `--pill-border #323a45`, `--pill-fg #d5dbe3`, `--pill-muted #7d848e`, `--pill-hover #b0b8c1`, `--flow-export #6bd9a5`, `--flow-import #79b8ff`, `--flow-dim #5a626d`, `--flow-export-dull #4f9a78`, `--flow-import-dull #5a87bd`, `--standby #c4ad55`; existing `--accent #79b8ff`, `--bad #e58275`.

---

## File map

| File | Responsibility |
|---|---|
| `ui-assets/style.css` | the tokens on `:root`; `.hover-card` rules use them |
| `ui-assets/pill.js` | `COLORS` from tokens; `borderColor`, `surfaceColor`, `lodFor`; bar/border/ring drawing; `drawPill(…, lod)`; `pillRenderer(model, onSize, getLod)` |
| `ui-assets/topology.js` | `catColor` in the model; per-canvas `lod` from `getScale()`; `debugLod()`, `debugSetScale()` |
| `tools/ui-smoke/live-topology.mjs` | unit tests for the helpers; e2e tier checks |

---

### Task 1: Palette tokens

**Files:**
- Modify: `ui-assets/style.css` (`:root` block near line 16; `.hover-card` rules near lines 2370–2410)
- Modify: `ui-assets/pill.js:8-23` (`COLORS`)
- Test: `tools/ui-smoke/live-topology.mjs` (unit block)

**Interfaces:**
- Produces: CSS custom properties listed in Global Constraints; `COLORS` keeps the same keys and values, now read from the tokens with the literal as fallback. Nothing visual changes.

- [ ] **Step 1: Failing unit test** — in the unit `page.evaluate` block (after the existing `pill` tests, before `return out;`):

```js
  // palette comes from :root tokens; values unchanged
  const css = (n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim();
  eq("token --pill-surface", css("--pill-surface"), "#242a33");
  eq("token --flow-export", css("--flow-export"), "#6bd9a5");
  eq("token --standby", css("--standby"), "#c4ad55");
  eq("COLORS.surface from token", pill.COLORS.surface, css("--pill-surface"));
  eq("COLORS.importDull from token", pill.COLORS.importDull, css("--flow-import-dull"));
  eq("COLORS.bad from token", pill.COLORS.bad, css("--bad"));
```
RUN SMOKE → the token `eq`s fail (empty strings).

- [ ] **Step 2: Tokens in style.css** — inside the `:root {` block, after `--bad: #e58275;`:

```css
  --standby: #c4ad55;
  /* Graph pills and the hover card (pill.js reads these once at load). */
  --pill-surface: #242a33;
  --pill-border: #323a45;
  --pill-fg: #d5dbe3;
  --pill-muted: #7d848e;
  --pill-hover: #b0b8c1;
  --flow-export: #6bd9a5;
  --flow-import: #79b8ff;
  --flow-dim: #5a626d;
  --flow-export-dull: #4f9a78;
  --flow-import-dull: #5a87bd;
```
Then in the `.hover-card` rules replace the literals: `background: #242a33` → `var(--pill-surface)`; `border: 1px solid #323a45` → `var(--pill-border)`; `color: #d5dbe3` → `var(--pill-fg)`; `.hc-chip-error` background → `var(--bad)`; `.hc-chip-standby` background and `.hc-stale` color → `var(--standby)`; `.hc-spark-line` stroke / `.hc-spark-end` fill → `var(--accent)`; `.hc-spark-zero` stroke, `.hc-bar` background, `.hc-foot` border-top → `var(--pill-border)`; `.hc-soc-fill` background → `var(--flow-export)`.

- [ ] **Step 3: `COLORS` from tokens** in `pill.js`:

```js
// Colours come from the :root tokens in style.css so a re-theme
// reaches the canvas; the literal is the fallback for a stylesheet
// without them (never a transparent pill).
function token(name, fallback) {
  if (typeof document === "undefined") return fallback;
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

export const COLORS = {
  export: token("--flow-export", "#6bd9a5"),
  import: token("--flow-import", "#79b8ff"),
  dim: token("--flow-dim", "#5a626d"),
  exportDull: token("--flow-export-dull", "#4f9a78"),
  importDull: token("--flow-import-dull", "#5a87bd"),
  surface: token("--pill-surface", "#242a33"),
  border: token("--pill-border", "#323a45"),
  fg: token("--pill-fg", "#d5dbe3"),
  muted: token("--pill-muted", "#7d848e"),
  accent: token("--accent", "#79b8ff"),
  hover: token("--pill-hover", "#b0b8c1"),
  bad: token("--bad", "#e58275"),
  standby: token("--standby", "#c4ad55"),
  socFill: token("--flow-export", "#6bd9a5"),
};
```

- [ ] **Step 4: RUN SMOKE** → `ALL PASS` (the existing colour assertions compare against the same literals, so they must still hold).

- [ ] **Step 5: Commit**

```bash
git add ui-assets/style.css ui-assets/pill.js tools/ui-smoke/live-topology.mjs
git commit -F - <<'EOF'
Move the pill and hover-card palette onto :root tokens

The canvas renderer and the hover card carried their own hex
literals, so a re-theme through the stylesheet never reached them.
The same values now live as custom properties next to the rest of
the palette; pill.js reads them once at load and keeps the literal as
a fallback. No visual change.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>
EOF
```

---

### Task 2: Edge bar, tinted border, live tint

**Files:**
- Modify: `ui-assets/pill.js` (`GEOM`, `pillModel` option rename, new helpers, `measurePill` textLeft, `drawPill`)
- Modify: `ui-assets/topology.js:447` (`dotColor:` → `catColor:`)
- Test: `tools/ui-smoke/live-topology.mjs` (unit: rename `dotColor` in `opts`/assertion; new helper tests)

**Interfaces:**
- Consumes: `COLORS` (Task 1).
- Produces: model field `catColor` (replaces `dotColor`); `borderColor(catColor) -> "#rrggbb"`; `surfaceColor(heroValue, deadBand, valuesOn) -> "#rrggbb"`; `mixHex(a, b, t)` exported for tests; `GEOM.bar = 6`, `GEOM.barGap = 10`, `textLeft = 16`.

- [ ] **Step 1: Failing unit tests** — in the unit block, change `dotColor: "#abcdef"` to `catColor: "#abcdef"` in `opts` and `eq("model dot", mInv.dotColor, …)` to `eq("model cat colour", mInv.catColor, "#abcdef")`; then add:

```js
  // bar + tinted border + live tint
  eq("mix 0", pill.mixHex("#000000", "#ffffff", 0), "#000000");
  eq("mix 1", pill.mixHex("#000000", "#ffffff", 1), "#ffffff");
  eq("mix half", pill.mixHex("#000000", "#ffffff", 0.5), "#808080");
  eq("border is 35 % category over border grey", pill.borderColor("#6fbf73"), pill.mixHex(pill.COLORS.border, "#6fbf73", 0.35));
  eq("surface neutral when dead", pill.surfaceColor(100, 300, true), pill.COLORS.surface);
  eq("surface neutral when null", pill.surfaceColor(null, 300, true), pill.COLORS.surface);
  eq("surface neutral with values off", pill.surfaceColor(-5000, 300, false), pill.COLORS.surface);
  eq("surface export tint", pill.surfaceColor(-5000, 300, true), pill.mixHex(pill.COLORS.surface, pill.COLORS.export, 0.07));
  eq("surface import tint", pill.surfaceColor(5000, 300, true), pill.mixHex(pill.COLORS.surface, pill.COLORS.import, 0.07));
  eq("text starts after the bar", pill.measurePill(ctx, pill.pillModel(inv, null, opts)).textLeft, 16);
```
RUN SMOKE → fails (`pill.mixHex is not a function`).

- [ ] **Step 2: Implement in `pill.js`**

Rename in `pillModel`: the option and field `dotColor` → `catColor` (signature `pillModel(c, live, { valuesOn, catColor, deadBand })`; update the comment above it). `GEOM`: replace `dot: 9, dotGap: 8,` with `bar: 6, barGap: 10,`. After the `finite` helper add:

```js
// Linear blend of two #rrggbb colours; t = 0 gives a, t = 1 gives b.
export function mixHex(a, b, t) {
  const ch = (h, i) => parseInt(h.slice(i, i + 2), 16);
  const out = [1, 3, 5].map((i) => Math.round(ch(a, i) + (ch(b, i) - ch(a, i)) * t));
  return `#${out.map((v) => Math.max(0, Math.min(255, v)).toString(16).padStart(2, "0")).join("")}`;
}

// The pill border carries a hint of the category so the node still
// reads as "a battery" when the bar is a few pixels wide.
export function borderColor(catColor) {
  return mixHex(COLORS.border, catColor, 0.35);
}

// A faint flow tint on the surface: export leans green, import blue,
// a dead or unknown value stays neutral. Never with values off.
export function surfaceColor(heroValue, deadBand, valuesOn) {
  if (!valuesOn || !finite(heroValue) || Math.abs(heroValue) < deadBand) return COLORS.surface;
  return mixHex(COLORS.surface, heroValue < 0 ? COLORS.export : COLORS.import, 0.07);
}
```
The model needs the raw hero value for the tint: in `pillModel`, add `heroValue: finite(power) ? power : null` to the returned object (next to `hero`), and `deadBand` to the returned object too (the renderer has no other access to it). In `measurePill`: `const textLeft = GEOM.bar + GEOM.barGap;`. In `borderStyle`, the default branch becomes `{ color: borderColor(model.catColor), width: 1.5 }` (hidden keeps the dash; the width no longer varies). In `drawPill`:

```js
  // surface + border
  roundRect(ctx, left, top, d.width, d.height, GEOM.radius);
  ctx.fillStyle = surfaceColor(model.heroValue, model.deadBand, model.valuesOn);
  ctx.fill();
  const b = borderStyle(model, state);
  ctx.setLineDash(model.hidden ? [4, 3] : []);
  ctx.lineWidth = b.width;
  ctx.strokeStyle = b.color;
  ctx.stroke();
  ctx.setLineDash([]);
  // category bar on the left edge, clipped to the rounded corner
  ctx.save();
  roundRect(ctx, left, top, d.width, d.height, GEOM.radius);
  ctx.clip();
  ctx.fillStyle = model.catColor;
  ctx.fillRect(left, top, GEOM.bar, d.height);
  ctx.restore();
  // health ring around the whole pill
  if (model.health !== "ok") {
    roundRect(ctx, left - 2.5, top - 2.5, d.width + 5, d.height + 5, GEOM.radius + 2.5);
    ctx.lineWidth = 1.5;
    ctx.strokeStyle = model.health === "error" ? COLORS.bad : COLORS.standby;
    ctx.stroke();
  }
  // row 1: name + id
  const row1Y = top + GEOM.padY + f.row1H / 2;
```
Remove the dot drawing; replace the remaining `dotY` uses with `row1Y`.

- [ ] **Step 3: topology.js** — `dotColor: colorFor(c),` → `catColor: colorFor(c),`. Grep `dotColor` across `ui-assets/` and `tools/` → no hits.

- [ ] **Step 4: RUN SMOKE** → `ALL PASS`. Screenshot with the existing scratch script `/tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad/shot.mjs` (read it for env switches; default mode is values-on topology) and view the PNG with the Read tool: bar flush left, clipped by the corner; border faintly category-coloured; exporting nodes a hair greener, importing a hair bluer; hidden node dashed; no dot.

- [ ] **Step 5: Commit**

```bash
git add ui-assets/pill.js ui-assets/topology.js tools/ui-smoke/live-topology.mjs
git commit -F - <<'EOF'
Carry the category on the pill's edge and border

A 9 px dot vanishes on a zoomed-out graph, and a surface a few steps
above the canvas does not read as a node. The category now runs as a
6 px bar down the pill's left edge and tints the border, and live
flow tints the surface a little toward its hue, so a pill reads as
"battery, exporting" at sizes where no text is legible. The health
ring moves from the dot to the pill.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>
EOF
```

---

### Task 3: Zoom tiers

**Files:**
- Modify: `ui-assets/pill.js` (`lodFor`, `drawPill(…, lod)`, `pillRenderer(model, onSize, getLod)`)
- Modify: `ui-assets/topology.js` (per-canvas `lod`; zoom handling; `nodeFor` passes `getLod`; debug hooks)
- Test: `tools/ui-smoke/live-topology.mjs` (unit `lodFor`; e2e tiers)

**Interfaces:**
- Consumes: `drawPill`, `pillRenderer` from Task 2.
- Produces: `lodFor(scale, prev) -> "full" | "hero" | "marker"`; `drawPill(ctx, x, y, model, state, lod = "full")`; `pillRenderer(model, onSize, getLod)` where `getLod()` returns the current tier; topology API `debugLod()` and `debugSetScale(scale)` (calls `network.moveTo({ scale, animation: false })`).

- [ ] **Step 1: Failing unit tests** (unit block):

```js
  // level of detail by canvas scale, with 0.05 hysteresis
  eq("lod full at 1", pill.lodFor(1.0, "full"), "full");
  eq("lod hero at 0.6", pill.lodFor(0.6, "full"), "hero");
  eq("lod marker at 0.3", pill.lodFor(0.3, "hero"), "marker");
  eq("lod stays full just under 0.8", pill.lodFor(0.78, "full"), "full");
  eq("lod drops to hero under 0.75", pill.lodFor(0.74, "full"), "hero");
  eq("lod stays hero just over 0.8", pill.lodFor(0.82, "hero"), "hero");
  eq("lod back to full over 0.85", pill.lodFor(0.86, "hero"), "full");
  eq("lod stays marker just over 0.4", pill.lodFor(0.42, "marker"), "marker");
  eq("lod hero over 0.45", pill.lodFor(0.46, "marker"), "hero");
  eq("lod keeps prev on NaN", pill.lodFor(Number.NaN, "hero"), "hero");
  eq("lod no prev picks by threshold", pill.lodFor(0.5, undefined), "hero");
  // drawing at a tier never changes the measured size
  const mLod = pill.pillModel(inv, { p: -19930, q: 1200, soc: null, dc: null }, opts);
  const dFull = pill.measurePill(ctx, mLod);
  pill.drawPill(ctx, 0, 0, mLod, { selected: false, hover: false }, "marker");
  eq("marker draw keeps dims", pill.measurePill(ctx, mLod), dFull);
```
And the e2e, placed after the "formulas canvas" section and before the hover section:

```js
// ── e2e: zoom tiers ───────────────────────────────────────────────
const lodAt = (s) =>
  page.evaluate(async (scale) => {
    const { topology } = await import("/assets/topology.js");
    topology.debugSetScale(scale);
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    return { lod: topology.debugLod(), widths: topology.debugNodeWidths(), heights: topology.debugNodeHeights() };
  }, s);
const atFull = await lodAt(1.0);
const atHero = await lodAt(0.6);
const atMarker = await lodAt(0.3);
check("e2e: lod full at 1.0", atFull.lod === "full", atFull.lod);
check("e2e: lod hero at 0.6", atHero.lod === "hero", atHero.lod);
check("e2e: lod marker at 0.3", atMarker.lod === "marker", atMarker.lod);
const atEdge = await lodAt(0.78); // coming from marker→hero→0.78 must stay hero
check("e2e: hysteresis holds hero at 0.78", atEdge.lod === "hero", atEdge.lod);
check("e2e: tiers keep node widths", JSON.stringify(atFull.widths) === JSON.stringify(atMarker.widths), `${JSON.stringify(atFull.widths)} vs ${JSON.stringify(atMarker.widths)}`);
check("e2e: tiers keep node heights", JSON.stringify(atFull.heights) === JSON.stringify(atMarker.heights));
await lodAt(1.0);
await page.evaluate(async () => { const { topology } = await import("/assets/topology.js"); topology.fit(); });
```
(`atEdge` path: the previous call left the tier at `marker`; 0.78 from `marker` goes to `hero` (above 0.45) and must not reach `full` (needs > 0.85).) RUN SMOKE → fails (`pill.lodFor is not a function`).

- [ ] **Step 2: `pill.js`**

```js
// Level of detail by canvas scale. Text is unreadable below ~0.8, so
// the pill drops to its hero number, and below 0.4 to no text at all.
// A tier changes only once the scale is 0.05 past its threshold, so
// panning at a boundary does not flicker.
const LOD_FULL = 0.8;
const LOD_HERO = 0.4;
const LOD_HYST = 0.05;
export function lodFor(scale, prev) {
  if (!Number.isFinite(scale)) return prev ?? "full";
  const up = (t) => t + LOD_HYST;
  const down = (t) => t - LOD_HYST;
  if (prev === "full") return scale >= down(LOD_FULL) ? "full" : scale >= down(LOD_HERO) ? "hero" : "marker";
  if (prev === "hero") return scale >= up(LOD_FULL) ? "full" : scale >= down(LOD_HERO) ? "hero" : "marker";
  if (prev === "marker") return scale >= up(LOD_FULL) ? "full" : scale >= up(LOD_HERO) ? "hero" : "marker";
  return scale >= LOD_FULL ? "full" : scale >= LOD_HERO ? "hero" : "marker";
}
const FONT_HERO_ONLY = `600 16px ${FONT_MONO}`;
```
`drawPill(ctx, x, y, model, state, lod = "full")`: after the health ring, `if (lod === "marker") return;` and

```js
  if (lod === "hero") {
    // one centred row: the hero power only (Formulas canvas has no
    // hero, so it shows the bare pill)
    if (!model.hero) return;
    ctx.textBaseline = "middle";
    ctx.textAlign = "left";
    ctx.font = FONT_HERO_ONLY;
    ctx.fillStyle = model.hero.color;
    ctx.fillText(model.hero.text, left + d.textLeft, top + d.height / 2 + 1);
    return;
  }
```
`pillRenderer(model, onSize, getLod)`: `drawPill(ctx, x, y, model, state || {…}, getLod ? getLod() : "full")`.

- [ ] **Step 3: `topology.js`** — state next to `knownSize`:

```js
  // Level of detail for every pill on this canvas, from the camera
  // scale. Only what is painted changes with it: sizes, layout and
  // the DataSet never do, so a zoom costs one redraw.
  let lod = "full";
  function syncLod() {
    if (!network) return;
    const next = lodFor(network.getScale(), lod);
    if (next === lod) return;
    lod = next;
    network.redraw();
  }
```
Import `lodFor` from `./pill.js`. In `nodeFor`: `ctxRenderer: pillRenderer(drawn, noteSize, () => lod)`. Register `network.on("zoom", syncLod);` next to the other handlers, and call `syncLod()` at the top of the `afterDrawing` handler (fit() and moveTo() change the scale without a `zoom` event; `syncLod` is a no-op when the tier is unchanged, so this does not loop). API hooks:

```js
    /// Smoke-test hooks: the current level-of-detail tier, and a way
    /// to set the camera scale without a wheel gesture.
    debugLod() {
      return lod;
    },
    debugSetScale(scale) {
      if (!network) return;
      network.moveTo({ scale, animation: false });
      syncLod();
    },
```

- [ ] **Step 4: RUN SMOKE** → `ALL PASS`. Screenshots: extend `shot.mjs` with `SCALE=0.55` / `SCALE=0.3` (evaluate `topology.debugSetScale(Number(process.env.SCALE))` after the canvas settles) and take the three; view each: at 0.55 one bold number per pill, at 0.3 bars and tinted borders only, nothing clipped, the Formulas canvas (switch subview) showing bare pills at 0.55.

- [ ] **Step 5: Docs + commit** — `AGENTS.md` `pill.js` mention: append "and the zoom tiers (full / hero / marker)". Then:

```bash
git add ui-assets/pill.js ui-assets/topology.js tools/ui-smoke/live-topology.mjs AGENTS.md
git commit -F - <<'EOF'
Drop pill text as the canvas zooms out

Below about 0.8× the name and id are unreadable and below 0.4× so is
the number, yet every pill still painted both. The renderer now takes
a level-of-detail tier from the camera scale — two rows, the hero
number alone, or no text — with 0.05 of hysteresis so panning on a
threshold does not flicker. Sizes and layout do not depend on the
tier, so a zoom is one redraw and nothing moves.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>
EOF
```
