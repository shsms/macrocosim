# Pill Nodes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the vis-network ellipse nodes on both graph canvases with a custom-drawn pill (name + `#id`, then hero active power and reactive / SoC), a `values` toggle, and a read-only hover card.

**Architecture:** A new `ui-assets/pill.js` holds a pure `pillModel()` (component + live sample → what to draw) and a canvas renderer (`measurePill`/`drawPill`/`pillRenderer`) plugged into vis-network's `shape: "custom"` / `ctxRenderer`. `topology.js` keeps its live map, 1 Hz flush, chevrons and microgrid guards, but builds nodes through the pill and stores a 60-entry power history per component for the hover card, which lives in a new `ui-assets/hovercard.js`. Fonts (IBM Plex Sans/Mono) are vendored.

**Tech Stack:** Vanilla ES modules served from `ui-assets/`, vis-network 9.1.10 (vendored), Playwright smoke script `tools/ui-smoke/live-topology.mjs` (run against the dev server: `SW_UI=http://127.0.0.1:PORT node tools/ui-smoke/live-topology.mjs`). No Rust changes.

**Spec:** `docs/superpowers/specs/2026-08-20-pill-nodes-design.md` (read it first; the amended interaction decisions in it are binding).

## Global Constraints

- Client-side only. No server / Rust changes.
- Commit convention: imperative subject, a body that says *why*, trailer exactly `Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>`. No `Co-Authored-By` or AI trailers. Stage files by name (`git add path ...`, never `-A`/`.`/`-u`). Never add `.nfs*` files.
- Branch: `live-topology` (already checked out in `/vagrant/switchyard`). Do not merge.
- Dev server: the UI is served from `ui-assets/` on disk, so JS/CSS edits are live on reload. Find the port in `/tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad/lt-endpoints.json` (key `ui`); `UI=$(python3 -c "import json;print(json.load(open('/tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad/lt-endpoints.json'))['ui'])")`. If it is down, see `AGENTS.md` for how to start one (`PROTOC=/tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad/protoc/bin/protoc` is needed for any cargo build).
- Tests run in the browser via Playwright. Tee every run to a scratch file and grep the file: `SW_UI=$UI node tools/ui-smoke/live-topology.mjs 2>&1 | tee /tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad/smoke.log | grep -E "^(FAIL|ALL PASS|[0-9]+ FAILED)"`. Below this is written as `RUN SMOKE`.
- Colours (from the spec): export `#6bd9a5`, import `#79b8ff`, dim `#5a626d`, reactive dull `#4f9a78` (export sign) / `#5a87bd` (import sign); pill surface `#242a33`, border `#323a45`, fg `#d5dbe3`, muted `#7d848e`; dot colours come from the `--cat-*` CSS tokens (already read by `colorFor` in topology.js); accent `#79b8ff`, bad `#e58275`, standby `#c4ad55`.
- Sizes: pill min width 96 px, max 200 px; name 11.5 px (values on) / 13 px (values off) IBM Plex Sans 500; id 10 px / 11 px IBM Plex Mono; hero 14 px Plex Mono 600; reactive 11 px Plex Mono 500; dot 9 px; SoC bar 40 x 5 px.
- Passive sign convention: +P consumption, +Q inductive. Colour by the sign of each quantity independently; values with |v| below the dead band `max(1 % of siteMaxRatedW, 50)` are dim.

---

## File map

| File | Responsibility |
|---|---|
| `ui-assets/vendor/fonts/*.woff2`, `ui-assets/vendor/fonts/LICENSE-OFL.txt` | vendored IBM Plex Sans 400/500/600 + Mono 400/500/600 |
| `ui-assets/style.css` | `@font-face` rules; `.hover-card` styles |
| `ui-assets/live.js` | keep `formatScaled`, `edgeFlow`; add `deadBandW(siteMaxRatedW)`; delete `liveLabelLines` |
| `ui-assets/pill.js` (new) | `pillModel`, `powerColor`, `reactiveColor`, `measurePill`, `drawPill`, `pillRenderer`, `invalidateMeasureCache`, `pillFontsReady` |
| `ui-assets/hovercard.js` (new) | `hoverCardModel` (pure), `createHoverCard` (DOM) |
| `ui-assets/topology.js` | nodes via pill; live map gains bounds/energy/history; `setValues`/`valuesOn`; size-refresh hook; hover card wiring; formula rings; `debugNodeModels` |
| `ui-assets/app.js`, `ui-assets/index.html` | toggle renamed `values`; help text |
| `ui-assets/explain.js` | passes `valuesOn: false` to `createGraphCanvas` |
| `tools/ui-smoke/live-topology.mjs` | unit tests for pill/hovercard models; e2e rewritten to models |
| `AGENTS.md` | mention `pill.js`, `hovercard.js`, fonts |

---

### Task 1: Vendor IBM Plex and declare the faces

**Files:**
- Create: `ui-assets/vendor/fonts/IBMPlexSans-{Regular,Medium,SemiBold}.woff2`, `ui-assets/vendor/fonts/IBMPlexMono-{Regular,Medium,SemiBold}.woff2`, `ui-assets/vendor/fonts/LICENSE-OFL.txt`
- Modify: `ui-assets/style.css:1` (prepend `@font-face` block before `:root`)

**Interfaces:**
- Produces: CSS font families `"IBM Plex Sans"` (weights 400/500/600) and `"IBM Plex Mono"` (400/500/600), served at `/assets/vendor/fonts/...`.

- [ ] **Step 1: Download the six woff2 files and the licence**

```bash
cd /vagrant/switchyard && mkdir -p ui-assets/vendor/fonts && cd ui-assets/vendor/fonts
for w in Regular Medium SemiBold; do
  curl -fsSL -o IBMPlexSans-$w.woff2 "https://cdn.jsdelivr.net/npm/@ibm/plex-sans@1.1.0/fonts/complete/woff2/IBMPlexSans-$w.woff2"
  curl -fsSL -o IBMPlexMono-$w.woff2 "https://cdn.jsdelivr.net/npm/@ibm/plex-mono@1.1.0/fonts/complete/woff2/IBMPlexMono-$w.woff2"
done
curl -fsSL -o LICENSE-OFL.txt "https://cdn.jsdelivr.net/npm/@ibm/plex-sans@1.1.0/LICENSE.txt"
ls -la && file *.woff2
```
Expected: six files of 50-120 kB each, `file` reports "Web Open Font Format (Version 2)". The licence is the SIL OFL 1.1.

- [ ] **Step 2: Add the `@font-face` rules at the top of `style.css`**

Insert before the `:root {` block:

```css
/* IBM Plex, vendored (SIL OFL 1.1, see vendor/fonts/LICENSE-OFL.txt).
   The graph canvas draws node text with these; the DOM never needs
   them at page load, so `font-display: swap` keeps first paint fast
   and pill.js explicitly loads the faces before measuring. */
@font-face { font-family: "IBM Plex Sans"; font-weight: 400; font-style: normal; font-display: swap; src: url("vendor/fonts/IBMPlexSans-Regular.woff2") format("woff2"); }
@font-face { font-family: "IBM Plex Sans"; font-weight: 500; font-style: normal; font-display: swap; src: url("vendor/fonts/IBMPlexSans-Medium.woff2") format("woff2"); }
@font-face { font-family: "IBM Plex Sans"; font-weight: 600; font-style: normal; font-display: swap; src: url("vendor/fonts/IBMPlexSans-SemiBold.woff2") format("woff2"); }
@font-face { font-family: "IBM Plex Mono"; font-weight: 400; font-style: normal; font-display: swap; src: url("vendor/fonts/IBMPlexMono-Regular.woff2") format("woff2"); }
@font-face { font-family: "IBM Plex Mono"; font-weight: 500; font-style: normal; font-display: swap; src: url("vendor/fonts/IBMPlexMono-Medium.woff2") format("woff2"); }
@font-face { font-family: "IBM Plex Mono"; font-weight: 600; font-style: normal; font-display: swap; src: url("vendor/fonts/IBMPlexMono-SemiBold.woff2") format("woff2"); }
```
(style.css is served from `/assets/style.css`, so the relative `vendor/fonts/...` URL resolves to `/assets/vendor/fonts/...`.)

- [ ] **Step 3: Verify the server serves them**

```bash
curl -sI "$UI/assets/vendor/fonts/IBMPlexMono-SemiBold.woff2" | head -3
```
Expected: `HTTP/1.1 200`. If the asset handler restricts extensions (check `src/ui/handlers/assets.rs` — if `.woff2` is not in its MIME table, that would be a server change; report back instead of editing Rust).

Then in Playwright:
```bash
cat > /tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad/fontcheck.mjs <<'EOF'
import { chromium } from "playwright";
const b = await chromium.launch({ args: ["--no-sandbox"] });
const p = await b.newPage();
await p.goto(process.env.SW_UI, { waitUntil: "networkidle" });
console.log(await p.evaluate(async () => {
  await document.fonts.load('600 14px "IBM Plex Mono"');
  await document.fonts.load('500 12px "IBM Plex Sans"');
  return { mono: document.fonts.check('600 14px "IBM Plex Mono"'), sans: document.fonts.check('500 12px "IBM Plex Sans"') };
}));
await b.close();
EOF
SW_UI=$UI node /tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad/fontcheck.mjs
```
Expected: `{ mono: true, sans: true }`.

- [ ] **Step 4: Commit**

```bash
cd /vagrant/switchyard
git add ui-assets/vendor/fonts/IBMPlexSans-Regular.woff2 ui-assets/vendor/fonts/IBMPlexSans-Medium.woff2 ui-assets/vendor/fonts/IBMPlexSans-SemiBold.woff2 ui-assets/vendor/fonts/IBMPlexMono-Regular.woff2 ui-assets/vendor/fonts/IBMPlexMono-Medium.woff2 ui-assets/vendor/fonts/IBMPlexMono-SemiBold.woff2 ui-assets/vendor/fonts/LICENSE-OFL.txt ui-assets/style.css
git commit -F - <<'EOF'
Vendor IBM Plex Sans and Mono for the graph canvas

The pill nodes set the component name in a humanist sans and the
numbers in a monospace with tabular figures; the system stacks vary
by OS and the canvas must measure text identically everywhere, so
the faces ship with the UI. Only the three weights the renderer uses
are included. OFL licence alongside.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>
EOF
```

---

### Task 2: `pillModel` — the pure node model

**Files:**
- Create: `ui-assets/pill.js`
- Modify: `ui-assets/live.js` (add `deadBandW`, make `edgeFlow` use it; keep `liveLabelLines` for now — Task 4 deletes it)
- Test: `tools/ui-smoke/live-topology.mjs` (unit section)

**Interfaces:**
- Produces:
  - `deadBandW(siteMaxRatedW: number) -> number` in live.js — `max(0.01 * (siteMaxRatedW > 0 ? siteMaxRatedW : 10_000), 50)`.
  - `powerColor(value, deadBand) -> "#6bd9a5" | "#79b8ff" | "#5a626d"` (negative = export green, positive = import blue, null/NaN/|v|<deadBand = dim).
  - `reactiveColor(value, deadBand) -> "#4f9a78" | "#5a87bd" | "#5a626d"` (same rule, dull hues).
  - `pillModel(component, live, { valuesOn, dotColor, deadBand }) -> PillModel` where
    ```js
    // component: one entry of /api/topology `components`
    //   { id, name, category, subtype, hidden, health, provides_telemetry, ... }
    // live: { p, q, soc, dc } | null | undefined  (numbers or null)
    // PillModel:
    {
      id: 12,
      name: "Battery Inverter 1",   // truncated to 22 chars with "…" like shortLabel did
      fullName: "Battery Inverter 1",
      idText: "#12",
      dotColor: "#6fbf73",
      health: "ok" | "standby" | "error",
      hidden: false,
      valuesOn: true,               // density-mode flag (drives font sizes)
      hero: { text: "-19.93 kW", color: "#6bd9a5" } | null,
      aux: { kind: "reactive", text: "1.20 kVAr", color: "#5a87bd" }
         | { kind: "soc", pct: 85, text: "85%" }
         | null,
      highlight: "none",            // Task 4 sets "subtracted" for formula terms
    }
    ```
    Row 2 is drawn iff `valuesOn && (hero || aux)`. Rules: battery → hero from `dc`, aux SoC; ev-charger → hero from `p`, aux SoC; everything else → hero from `p`, aux reactive from `q` when finite. A missing hero beside a present aux renders as `{ text: "—", color: dim }`. `health` = `"standby"` when `provides_telemetry === false` and health is ok (same rule as today's `nodeStyleFor`).

- [ ] **Step 1: Write the failing unit tests**

In `tools/ui-smoke/live-topology.mjs`, inside the `page.evaluate` unit block, after the `edgeFlow` tests and before `return out;`, add:

```js
  // pill.js: the pure node model
  const pill = await import("/assets/pill.js");
  const EXP = "#6bd9a5", IMP = "#79b8ff", DIM = "#5a626d", EXPQ = "#4f9a78", IMPQ = "#5a87bd";
  eq("dead band floor", m.deadBandW(0), 100);        // 1 % of the 10 kW fallback
  eq("dead band 1 %", m.deadBandW(30000), 300);
  eq("dead band min 50", m.deadBandW(1000), 50);
  eq("powerColor export", pill.powerColor(-5000, 300), EXP);
  eq("powerColor import", pill.powerColor(5000, 300), IMP);
  eq("powerColor dead", pill.powerColor(120, 300), DIM);
  eq("powerColor null", pill.powerColor(null, 300), DIM);
  eq("reactiveColor lagging-with-import", pill.reactiveColor(800, 300), IMPQ);
  eq("reactiveColor leading", pill.reactiveColor(-800, 300), EXPQ);
  eq("reactiveColor dead", pill.reactiveColor(10, 300), DIM);
  const opts = { valuesOn: true, dotColor: "#abcdef", deadBand: 300 };
  const inv = { id: 12, name: "Battery Inverter 1", category: "inverter", subtype: "battery", hidden: false, health: "ok", provides_telemetry: true };
  const mInv = pill.pillModel(inv, { p: -19930, q: 1200, soc: null, dc: null }, opts);
  eq("model id text", mInv.idText, "#12");
  eq("model dot", mInv.dotColor, "#abcdef");
  eq("model hero", mInv.hero, { text: "-19.93 kW", color: EXP });
  eq("model aux reactive", mInv.aux, { kind: "reactive", text: "1.20 kVAr", color: IMPQ });
  eq("model health ok", mInv.health, "ok");
  eq("model highlight default", mInv.highlight, "none");
  const bat = { id: 1000, name: "bat-1000", category: "battery", subtype: null, hidden: false, health: "ok", provides_telemetry: true };
  const mBat = pill.pillModel(bat, { p: null, q: null, soc: 85.4, dc: -3000 }, opts);
  eq("battery hero is dc", mBat.hero, { text: "-3.00 kW", color: EXP });
  eq("battery aux is soc", mBat.aux, { kind: "soc", pct: 85, text: "85%" });
  const mBatSocOnly = pill.pillModel(bat, { p: null, q: null, soc: 40, dc: null }, opts);
  eq("battery without dc shows dash hero", mBatSocOnly.hero, { text: "—", color: DIM });
  const ev = { id: 7, name: "ev-7", category: "ev-charger", subtype: null, hidden: false, health: "ok", provides_telemetry: true };
  eq("ev aux is soc", pill.pillModel(ev, { p: 3000, q: 7, soc: 40, dc: null }, opts).aux, { kind: "soc", pct: 40, text: "40%" });
  const meter = { id: 2, name: "meter-2", category: "meter", subtype: null, hidden: true, health: "ok", provides_telemetry: true };
  const mMeter = pill.pillModel(meter, { p: 500, q: null, soc: null, dc: null }, opts);
  eq("meter p only", mMeter.aux, null);
  eq("meter hidden", mMeter.hidden, true);
  eq("no sample → no row 2", pill.pillModel(meter, null, opts).hero, null);
  eq("values off → no row 2 even with sample", pill.pillModel(meter, { p: 500, q: 1, soc: null, dc: null }, { ...opts, valuesOn: false }).hero, null);
  eq("values off flag", pill.pillModel(meter, null, { ...opts, valuesOn: false }).valuesOn, false);
  const standby = { ...meter, provides_telemetry: false };
  eq("standby health", pill.pillModel(standby, null, opts).health, "standby");
  eq("error health wins", pill.pillModel({ ...standby, health: "error" }, null, opts).health, "error");
  const longName = { ...meter, name: "A very long component name indeed" };
  eq("name truncated", pill.pillModel(longName, null, opts).name, "A very long componen…");
  eq("full name kept", pill.pillModel(longName, null, opts).fullName, "A very long component name indeed");
```

- [ ] **Step 2: Run to verify the new tests fail**

RUN SMOKE. Expected: the script throws inside `page.evaluate` (`/assets/pill.js` 404 → import fails). That is the failing state.

- [ ] **Step 3: Add `deadBandW` to live.js**

Replace the `const dead = Math.max(0.01 * max, 50);` line in `edgeFlow` and export the rule:

```js
// The "nothing is flowing" threshold shared by the chevrons and the
// pill colours: 1 % of the site's largest rated bound, never under
// 50 W. Falls back to a 10 kW site when nothing is rated.
export function deadBandW(siteMaxRatedW) {
  const max = siteMaxRatedW > 0 ? siteMaxRatedW : 10_000;
  return Math.max(0.01 * max, 50);
}
```
and inside `edgeFlow`: `const dead = deadBandW(siteMaxRatedW);` (keep `max` for the `norm` computation).

- [ ] **Step 4: Write `ui-assets/pill.js` (model half)**

```js
// The pill node: one model builder (pure) and one canvas renderer,
// shared by the Topology and Formulas canvases through vis-network's
// `shape: "custom"`. The model says *what* a node shows; the
// renderer (further down) says how it looks.

import { formatScaled } from "./live.js";

export const COLORS = {
  export: "#6bd9a5",
  import: "#79b8ff",
  dim: "#5a626d",
  exportDull: "#4f9a78",
  importDull: "#5a87bd",
  surface: "#242a33",
  border: "#323a45",
  fg: "#d5dbe3",
  muted: "#7d848e",
  accent: "#79b8ff",
  hover: "#b0b8c1",
  bad: "#e58275",
  standby: "#c4ad55",
  socFill: "#6bd9a5",
};

const finite = (v) => v != null && Number.isFinite(v);

// Consumption-positive: import blue, export green, dead band dim.
export function powerColor(value, deadBand) {
  if (!finite(value) || Math.abs(value) < deadBand) return COLORS.dim;
  return value < 0 ? COLORS.export : COLORS.import;
}

// Reactive power in duller versions of the same hues, by its own
// sign (+Q inductive). Beside the active colour, a matching hue reads
// as lagging and a contrasting one as leading.
export function reactiveColor(value, deadBand) {
  if (!finite(value) || Math.abs(value) < deadBand) return COLORS.dim;
  return value < 0 ? COLORS.exportDull : COLORS.importDull;
}

// Long names get shortened on the pill; the full name lives in the
// hover card / tooltip.
export function shortName(name) {
  return name.length > 22 ? `${name.slice(0, 20)}…` : name;
}

function effectiveHealth(c) {
  const health = c.health || "ok";
  return health === "ok" && c.provides_telemetry === false ? "standby" : health;
}

function socAux(soc) {
  if (!finite(soc)) return null;
  const pct = Math.round(soc);
  return { kind: "soc", pct, text: `${pct}%` };
}

// component: an /api/topology component; live: { p, q, soc, dc } or
// null; options: { valuesOn, dotColor, deadBand }.
export function pillModel(c, live, { valuesOn, dotColor, deadBand }) {
  let hero = null;
  let aux = null;
  if (valuesOn && live) {
    const power = c.category === "battery" ? live.dc : live.p;
    if (c.category === "battery" || c.category === "ev-charger") {
      aux = socAux(live.soc);
    } else if (finite(live.q)) {
      aux = { kind: "reactive", text: formatScaled(live.q, "VAr"), color: reactiveColor(live.q, deadBand) };
    }
    if (finite(power)) hero = { text: formatScaled(power, "W"), color: powerColor(power, deadBand) };
    else if (aux) hero = { text: "—", color: COLORS.dim };
  }
  return {
    id: c.id,
    name: shortName(c.name),
    fullName: c.name,
    idText: `#${c.id}`,
    dotColor,
    health: effectiveHealth(c),
    hidden: Boolean(c.hidden),
    valuesOn: Boolean(valuesOn),
    hero,
    aux,
    highlight: "none",
  };
}
```

- [ ] **Step 5: Run the smoke script; the new unit tests pass**

RUN SMOKE. Expected: no `FAIL unit:` lines. (E2e lines still pass — nothing on the canvas changed yet.)

- [ ] **Step 6: Commit**

```bash
git add ui-assets/pill.js ui-assets/live.js tools/ui-smoke/live-topology.mjs
git commit -F - <<'EOF'
Add the pill node model

pillModel turns a topology component plus its last live sample into
what the node shows: name and id, a hero power (DC for batteries),
and either reactive power or SoC. Colours follow each quantity's own
sign under the passive convention, dimmed below the same dead band
the chevrons use, so a pill and its edge can never disagree. Pure,
so the smoke script unit-tests it without a canvas.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>
EOF
```

---

### Task 3: The canvas renderer

**Files:**
- Modify: `ui-assets/pill.js` (append renderer half)
- Test: `tools/ui-smoke/live-topology.mjs` (unit section — measure with a real 2D context)

**Interfaces:**
- Produces:
  - `pillFontsReady: Promise<void>` — resolves once the faces are loaded (or immediately when `document.fonts` is missing).
  - `invalidateMeasureCache(): void`.
  - `measurePill(ctx, model) -> { width, height, name, textLeft, row1H, row2H }` — `width` in [96, 200]; `name` is the (possibly further-truncated) name that fits.
  - `drawPill(ctx, x, y, model, state)` with `state = { selected: bool, hover: bool }`; `(x, y)` is the centre (vis convention).
  - `pillRenderer(model, onSize) -> ctxRenderer` — the function vis calls as `ctxRenderer({ ctx, id, x, y, state, style, label })`; returns `{ drawNode, nodeDimensions: { width, height } }` and calls `onSize(id, width, height)` every draw.

- [ ] **Step 1: Write the failing unit tests**

Append to the unit `page.evaluate` block (before `return out;`):

```js
  // renderer: measured sizes, content-derived and clamped
  await pill.pillFontsReady;
  const ctx = document.createElement("canvas").getContext("2d");
  const dShort = pill.measurePill(ctx, pill.pillModel({ ...meter, name: "m" }, null, { ...opts, valuesOn: false }));
  const dLong = pill.measurePill(ctx, pill.pillModel(inv, { p: -19930, q: 1200, soc: null, dc: null }, opts));
  const dHuge = pill.measurePill(ctx, pill.pillModel(longName, { p: -19930, q: -12500, soc: null, dc: null }, opts));
  eq("min width", dShort.width, 96);
  out.push({ name: "long wider than short", ok: dLong.width > dShort.width, got: `${dLong.width} vs ${dShort.width}` });
  out.push({ name: "max width clamp", ok: dHuge.width <= 200 && dHuge.width >= 150, got: String(dHuge.width) });
  out.push({ name: "clamped name re-truncated", ok: dHuge.name.endsWith("…") && dHuge.name.length < 21, got: dHuge.name });
  out.push({ name: "two rows taller than one", ok: dLong.height > dShort.height, got: `${dLong.height} vs ${dShort.height}` });
  const dOff = pill.measurePill(ctx, pill.pillModel(inv, null, { ...opts, valuesOn: false }));
  eq("values-off height single row", dOff.height, dShort.height);
  // pillRenderer contract
  const sizes = [];
  const r = pill.pillRenderer(pill.pillModel(inv, null, { ...opts, valuesOn: false }), (id, w, h) => sizes.push([id, w, h]));
  const res = r({ ctx, id: 12, x: 0, y: 0, state: { selected: false, hover: false }, style: {}, label: "" });
  eq("renderer reports dimensions", res.nodeDimensions, { width: dOff.width, height: dOff.height });
  eq("renderer onSize", sizes, [[12, dOff.width, dOff.height]]);
  out.push({ name: "renderer drawNode is callable", ok: typeof res.drawNode === "function" && (res.drawNode(), true) });
```
Note: a values-on node without a sample is also a single-row pill, but at the smaller (11.5 px) name size, so it is *not* the same height as a values-off pill; the renderer test deliberately uses the same values-off model as `dOff`.

- [ ] **Step 2: Run to verify failure**

RUN SMOKE. Expected: evaluate throws (`pill.measurePill is not a function`).

- [ ] **Step 3: Append the renderer to `pill.js`**

```js
// ── renderer ────────────────────────────────────────────────────
// Everything below draws in canvas units; vis-network applies the
// zoom and device-pixel scaling, so 14 px here is 14 px at scale 1.

export const FONT_SANS = '"IBM Plex Sans", system-ui, -apple-system, "Segoe UI", sans-serif';
export const FONT_MONO = '"IBM Plex Mono", ui-monospace, SFMono-Regular, Menlo, monospace';

const GEOM = {
  minWidth: 96,
  maxWidth: 200,
  padX: 10,
  padY: 6,
  dot: 9,
  dotGap: 8,
  idGap: 6,
  rowGap: 3,
  dividerGap: 8,
  radius: 10,
  socBarW: 40,
  socBarH: 5,
  socGap: 6,
};

function fonts(model) {
  return model.valuesOn
    ? { name: `500 11.5px ${FONT_SANS}`, id: `400 10px ${FONT_MONO}`, row1H: 14 }
    : { name: `500 13px ${FONT_SANS}`, id: `400 11px ${FONT_MONO}`, row1H: 16 };
}
const FONT_HERO = `600 14px ${FONT_MONO}`;
const FONT_AUX = `500 11px ${FONT_MONO}`;
const ROW2_H = 17;

// Canvas text is not a DOM use, so the browser never loads a web
// font for it on its own; ask for the faces explicitly.
export const pillFontsReady =
  typeof document !== "undefined" && document.fonts
    ? Promise.all([
        document.fonts.load('500 11.5px "IBM Plex Sans"'),
        document.fonts.load('600 14px "IBM Plex Mono"'),
        document.fonts.load('400 10px "IBM Plex Mono"'),
      ]).then(() => undefined, () => undefined)
    : Promise.resolve();

const measureCache = new Map();
export function invalidateMeasureCache() {
  measureCache.clear();
}

function textWidth(ctx, font, text) {
  ctx.font = font;
  return ctx.measureText(text).width;
}

function hasRow2(model) {
  return Boolean(model.valuesOn && (model.hero || model.aux));
}

function auxWidth(ctx, aux) {
  if (!aux) return 0;
  if (aux.kind === "soc") return GEOM.socBarW + GEOM.socGap + textWidth(ctx, FONT_AUX, aux.text);
  return textWidth(ctx, FONT_AUX, aux.text);
}

// Measures the pill for `model`. Cached by the strings that affect
// size; hover/selection never change a pill's size.
export function measurePill(ctx, model) {
  const key = [model.valuesOn ? 1 : 0, model.name, model.idText, model.hero?.text ?? "", model.aux?.kind ?? "", model.aux?.text ?? ""].join("");
  const hit = measureCache.get(key);
  if (hit) return hit;
  const f = fonts(model);
  const textLeft = GEOM.padX + GEOM.dot + GEOM.dotGap;
  const row2 = hasRow2(model);
  let row2W = 0;
  if (row2) {
    row2W = textWidth(ctx, FONT_HERO, model.hero ? model.hero.text : "—");
    if (model.aux) row2W += GEOM.dividerGap * 2 + 1 + auxWidth(ctx, model.aux);
  }
  const idW = textWidth(ctx, f.id, model.idText);
  // Row 1 may need to give way: the pill must stay inside the
  // layout's column separation, so the name truncates until it fits.
  let name = model.name;
  let row1W = textWidth(ctx, f.name, name) + GEOM.idGap + idW;
  const budget = GEOM.maxWidth - textLeft - GEOM.padX;
  while (row1W > budget && name.length > 4) {
    name = `${name.replace(/…$/, "").slice(0, -1)}…`;
    row1W = textWidth(ctx, f.name, name) + GEOM.idGap + idW;
  }
  const content = Math.max(row1W, row2W);
  const width = Math.round(Math.min(GEOM.maxWidth, Math.max(GEOM.minWidth, textLeft + content + GEOM.padX)));
  const height = GEOM.padY * 2 + f.row1H + (row2 ? GEOM.rowGap + ROW2_H : 0);
  const dims = { width, height, name, textLeft, row1H: f.row1H, row2H: row2 ? ROW2_H : 0 };
  measureCache.set(key, dims);
  return dims;
}

function roundRect(ctx, x, y, w, h, r) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

function borderStyle(model, state) {
  if (model.highlight === "subtracted") return { color: COLORS.bad, width: 2 };
  if (state.selected) return { color: COLORS.accent, width: 2 };
  if (state.hover) return { color: COLORS.hover, width: 1.5 };
  return { color: COLORS.border, width: model.hidden ? 1.5 : 1 };
}

// Draws the pill centred on (x, y). `state` is vis's
// { selected, hover }; a formula "subtracted" highlight lives in the
// model itself (it is not a vis selection).
export function drawPill(ctx, x, y, model, state) {
  const d = measurePill(ctx, model);
  const f = fonts(model);
  const left = x - d.width / 2;
  const top = y - d.height / 2;
  // surface + border
  roundRect(ctx, left, top, d.width, d.height, GEOM.radius);
  ctx.fillStyle = COLORS.surface;
  ctx.fill();
  const b = borderStyle(model, state);
  ctx.setLineDash(model.hidden ? [4, 3] : []);
  ctx.lineWidth = b.width;
  ctx.strokeStyle = b.color;
  ctx.stroke();
  ctx.setLineDash([]);
  // category dot + health ring
  const dotX = left + GEOM.padX + GEOM.dot / 2;
  const dotY = top + GEOM.padY + f.row1H / 2;
  ctx.beginPath();
  ctx.arc(dotX, dotY, GEOM.dot / 2, 0, Math.PI * 2);
  ctx.fillStyle = model.dotColor;
  ctx.fill();
  if (model.health !== "ok") {
    ctx.beginPath();
    ctx.arc(dotX, dotY, GEOM.dot / 2 + 2.5, 0, Math.PI * 2);
    ctx.lineWidth = 1.5;
    ctx.strokeStyle = model.health === "error" ? COLORS.bad : COLORS.standby;
    ctx.stroke();
  }
  // row 1: name + id
  ctx.textBaseline = "middle";
  ctx.textAlign = "left";
  let tx = left + d.textLeft;
  ctx.font = f.name;
  ctx.fillStyle = COLORS.fg;
  ctx.fillText(d.name, tx, dotY);
  tx += ctx.measureText(d.name).width + GEOM.idGap;
  ctx.font = f.id;
  ctx.fillStyle = COLORS.muted;
  ctx.fillText(model.idText, tx, dotY + 0.5);
  if (!d.row2H) return;
  // row 2: hero | aux
  const row2Y = top + GEOM.padY + f.row1H + GEOM.rowGap + ROW2_H / 2;
  tx = left + d.textLeft;
  ctx.font = FONT_HERO;
  const hero = model.hero || { text: "—", color: COLORS.dim };
  ctx.fillStyle = hero.color;
  ctx.fillText(hero.text, tx, row2Y);
  if (!model.aux) return;
  tx += ctx.measureText(hero.text).width + GEOM.dividerGap;
  ctx.fillStyle = COLORS.border;
  ctx.fillRect(Math.round(tx), row2Y - 6, 1, 12);
  tx += 1 + GEOM.dividerGap;
  if (model.aux.kind === "soc") {
    const barY = row2Y - GEOM.socBarH / 2;
    roundRect(ctx, tx, barY, GEOM.socBarW, GEOM.socBarH, GEOM.socBarH / 2);
    ctx.fillStyle = COLORS.border;
    ctx.fill();
    const fillW = Math.max(0, Math.min(GEOM.socBarW, (GEOM.socBarW * model.aux.pct) / 100));
    if (fillW > 0) {
      roundRect(ctx, tx, barY, fillW, GEOM.socBarH, GEOM.socBarH / 2);
      ctx.fillStyle = COLORS.socFill;
      ctx.fill();
    }
    tx += GEOM.socBarW + GEOM.socGap;
    ctx.font = FONT_AUX;
    ctx.fillStyle = COLORS.fg;
    ctx.fillText(model.aux.text, tx, row2Y);
  } else {
    ctx.font = FONT_AUX;
    ctx.fillStyle = model.aux.color;
    ctx.fillText(model.aux.text, tx, row2Y);
  }
}

// The vis-network `ctxRenderer` for one node. vis calls it on every
// draw with { ctx, id, x, y, state, style, label }; `onSize` lets the
// canvas owner notice size changes (vis applies `nodeDimensions` one
// draw late — see topology.js).
export function pillRenderer(model, onSize) {
  return ({ ctx, id, x, y, state }) => {
    const d = measurePill(ctx, model);
    if (onSize) onSize(id, d.width, d.height);
    return {
      drawNode() {
        drawPill(ctx, x, y, model, state || { selected: false, hover: false });
      },
      nodeDimensions: { width: d.width, height: d.height },
    };
  };
}
```

- [ ] **Step 4: Run the smoke script**

RUN SMOKE. Expected: no `FAIL` lines. If "max width clamp" fails because `dHuge.width < 150`, confirm the fonts actually loaded (Task 1 Step 3) before blaming the code.

- [ ] **Step 5: Commit**

```bash
git add ui-assets/pill.js tools/ui-smoke/live-topology.mjs
git commit -F - <<'EOF'
Draw the pill node on the canvas

measurePill sizes a pill from its content — the name truncates
further when the id and metrics would push it past the layout's
column separation, and short names get a floor so the dot never
crowds them. drawPill paints the surface, border states (selected,
hover, hidden dashes, formula-subtracted), the category dot with its
health ring, and the two rows. pillRenderer adapts it to vis-network's
custom shape contract and reports every measured size so the owner
can work around vis applying custom dimensions a draw late.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>
EOF
```

---

### Task 4: Switch both canvases to pills

**Files:**
- Modify: `ui-assets/topology.js` (imports; delete `LIVE_NODE_*`, `liveNodeConstraints`, `liveLabel`, `shortLabel`, `lighten`, `nodeStyleFor`; add `nodeFor`; `buildVisData`; `flushLive`; `afterDrawing`; `restoreRedHighlights`/`highlight`; `setLive`; debug hooks)
- Modify: `ui-assets/live.js` (delete `liveLabelLines`)
- Modify: `ui-assets/explain.js` (adapter `valuesOn: false`; export `formulaCanvas` if not already)
- Modify: `tools/ui-smoke/live-topology.mjs` (drop `liveLabelLines` unit tests; e2e to models)

**Interfaces:**
- Consumes: `pillModel`, `pillRenderer`, `invalidateMeasureCache`, `pillFontsReady` from pill.js; `deadBandW` from live.js.
- Produces (topology public API, used by Tasks 5-7 and app.js):
  - `topology.debugNodeModels() -> PillModel[]` (one per node, from the DataSet).
  - `topology.highlight(ids, subtractedIds)` unchanged signature; subtracted nodes now carry `model.highlight = "subtracted"`.
  - `createGraphCanvas(containerId, adapter)` gains adapter options `adapter.valuesOn` (boolean, default `true`; the Formulas instance passes `false`) and `adapter.tooltip` (default `true` keeps vis `title`; the topology instance passes `false`).
  - `setLive` / `liveOn` keep their names in this task (renamed in Task 5).

- [ ] **Step 1: Rewrite the e2e label assertions to models (failing first)**

In `tools/ui-smoke/live-topology.mjs`:
1. Delete the seven `liveLabelLines` unit `eq(...)` lines (the block starting `// liveLabelLines: one metric per line`).
2. Replace `getLabels` and the `hasLiveLine` helper:
```js
const getModels = () =>
  page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugNodeModels();
  });
const hasValues = (ms) => ms.some((m) => m.hero);
```
3. Replace the `labels` wait and the three label checks:
```js
const models = await waitFor(async () => {
  const ms = await getModels();
  return hasValues(ms) ? ms : null;
});
check("e2e: some node shows a power hero", models.some((m) => m.hero && /-?\d+(\.\d+)? (W|kW|MW)/.test(m.hero.text)), JSON.stringify(models));
check("e2e: battery shows DC power hero and SoC aux", models.some((m) => /^bat-\d+$/.test(m.fullName) && m.hero && m.aux?.kind === "soc"), JSON.stringify(models));
check("e2e: inverter shows reactive aux", models.some((m) => /^inv-/.test(m.fullName) && m.aux?.kind === "reactive" && /VAr/.test(m.aux.text)), JSON.stringify(models));
check("e2e: every node carries its #id", models.every((m) => m.idText === `#${m.id}`), JSON.stringify(models));
```
4. Width checks: keep the `widthsA`/`widthsB` stability check and add right after it:
```js
check("e2e: widths are content-derived (not all equal)", new Set(widthsA).size > 1, JSON.stringify(widthsA));
check("e2e: widths inside [96, 200]", widthsA.every((w) => w >= 96 && w <= 200), JSON.stringify(widthsA));
```
5. Refresh section: `const afterRefresh = { models: await getModels(), edges: await getEdges() };` and `check("e2e: values survive a topology refresh", hasValues(afterRefresh.models), JSON.stringify(afterRefresh.models));`.
6. Toggle section: replace `labels: topology.debugLiveLabels()` with `models: topology.debugNodeModels()`, the wait condition with `st.on === false && !hasValues(st.models)`, and the first check with `check("e2e: toggle off clears row 2", off.models.every((m) => !m.hero && !m.aux && m.valuesOn === false), JSON.stringify(off.models));`.
7. Add a Formulas-canvas check before the toggle section:
```js
// ── e2e: formulas canvas uses the same pills, values off ─────────
await page.click('#mg-subtoggle .mode-btn[data-subview="formulas"]');
const formulaModels = await waitFor(async () => {
  const ms = await page.evaluate(async () => {
    const { formulaCanvas } = await import("/assets/explain.js");
    return formulaCanvas().debugNodeModels();
  });
  return ms.length ? ms : null;
});
check("e2e: formulas canvas shows #id on every node, values off", formulaModels.every((m) => m.idText === `#${m.id}` && m.valuesOn === false), JSON.stringify(formulaModels));
await page.click('#mg-subtoggle .mode-btn[data-subview="topology"]');
```
(Check `grep -n "function formulaCanvas" ui-assets/explain.js`; if it is not exported, add `export`.)

RUN SMOKE → expected: throws on `debugNodeModels is not a function`.

- [ ] **Step 2: Replace node construction in topology.js**

Imports: change `import { edgeFlow, liveLabelLines } from "./live.js";` to
```js
import { deadBandW, edgeFlow } from "./live.js";
import { invalidateMeasureCache, pillFontsReady, pillModel, pillRenderer } from "./pill.js";
```
Delete `lighten`, `shortLabel`, `LIVE_NODE_WIDTH`, `LIVE_NODE_HEIGHT`, `liveNodeConstraints` and the whole existing `nodeStyleFor`. Keep `colorFor`, `CATEGORY_COLOR`, `INVERTER_SUBTYPE_COLOR`, `LIVE_KEY`, `EDGE_LIVE_COLOR`, `edgeRestStyle`.

Inside `createGraphCanvas`, in the state block after `let liveMg = null;`, add:
```js
  // Pill sizes vis has not applied yet. vis-network takes a custom
  // shape's `nodeDimensions` into account only on the draw *after*
  // the one that reported them, and only when the node is flagged
  // for refresh — so the renderer reports every size, and
  // afterDrawing re-stamps the nodes whose size moved (an id-only
  // update flags the refresh) and schedules a measured relayout.
  const knownSize = new Map(); // id -> "w x h" last reported
  const sizeDirty = new Set();
  function noteSize(id, w, h) {
    const key = `${w}x${h}`;
    if (knownSize.get(id) === key) return;
    knownSize.set(id, key);
    sizeDirty.add(id);
  }
  const valuesDefault = adapter.valuesOn !== false;
```
There is an existing `let redHighlighted = [];` — keep that single declaration (it is now "ids whose model carries the subtracted ring").

Replace `liveLabel` with the node builder:
```js
  // The vis node for a component: a custom-drawn pill carrying the
  // component's last live sample (when values are on and a sample
  // exists). `label` stays for vis's accessibility/export paths and
  // is never drawn; `title` (the vis tooltip) only on canvases
  // without a hover card.
  function nodeFor(c) {
    const model = pillModel(c, liveEnabled ? liveValues.get(c.id) : null, {
      valuesOn: liveEnabled && valuesDefault,
      dotColor: colorFor(c),
      deadBand: deadBandW(maxAbsBoundW),
    });
    if (redHighlighted.includes(c.id)) model.highlight = "subtracted";
    const node = {
      id: c.id,
      label: c.name,
      shape: "custom",
      ctxRenderer: pillRenderer(model, noteSize),
      pillModel: model,
    };
    if (adapter.tooltip !== false) node.title = `#${c.id} — ${c.name}`;
    return node;
  }
```

`buildVisData`: the per-component body becomes
```js
    const nodes = data.components.map((c) => {
      componentById.set(c.id, c);
      return nodeFor(c);
    });
```

`restoreRedHighlights`:
```js
  function restoreRedHighlights() {
    if (!redHighlighted.length) return;
    const ids = redHighlighted.filter((id) => componentById.has(id));
    redHighlighted = [];
    nodesDS.update(ids.map((id) => nodeFor(componentById.get(id))));
  }
```
`highlight(ids, subtractedIds)`: replace the `nodesDS.update(subs.map(... style.color ...))` block with
```js
      if (subs.length) {
        redHighlighted = subs;
        nodesDS.update(subs.map((id) => nodeFor(componentById.get(id))));
      }
```
(set `redHighlighted` *before* calling `nodeFor`, which reads it).

`flushLive`: replace the node loop with
```js
    const nodeUpdates = [];
    for (const id of liveDirty) {
      const c = componentById.get(id);
      if (c) nodeUpdates.push(nodeFor(c));
    }
```
and drop `linesChanged` plus the trailing `if (linesChanged ...) pendingMeasuredRelayout = true;` (size changes are now caught by `noteSize` + afterDrawing).

`afterDrawing` handler: add as the first statement inside the callback
```js
        if (sizeDirty.size) {
          const ids = [...sizeDirty];
          sizeDirty.clear();
          // An id-only update flags vis's refresh so the next draw
          // adopts the reported dimensions; that draw then runs the
          // measured relayout below with real boxes.
          nodesDS.update(ids.map((id) => ({ id })));
          if (!manualArrangement) pendingMeasuredRelayout = true;
          return;
        }
```
Right after the `network.on("afterDrawing", ...)` registration add the font hook:
```js
      // Fonts may land after the first paint: re-measure everything
      // and let the size-dirty path relayout.
      pillFontsReady.then(() => {
        if (!nodesDS) return;
        invalidateMeasureCache();
        knownSize.clear();
        nodesDS.update(nodesDS.getIds().map((id) => ({ id })));
      });
```

`setLive(on)`: replace the `nodesDS.update(nodesDS.get().map(... label ...))` with
```js
      if (nodesDS) {
        nodesDS.update(
          nodesDS.getIds().filter((id) => componentById.has(id)).map((id) => nodeFor(componentById.get(id))),
        );
      }
```

Debug hooks: replace `debugLiveLabels` with
```js
    /// Smoke-test hook: every node's pill model as drawn.
    debugNodeModels() {
      return nodesDS ? nodesDS.get().map((n) => n.pillModel) : [];
    },
```
and update `debugNodeWidths`'s comment to "content-derived, stable across flushes".

Formulas canvas: in `ui-assets/explain.js` add `valuesOn: false,` to the adapter object passed to `createGraphCanvas("formula-topology", { ... })`. Topology instance (bottom of topology.js): add `tooltip: false,` to its adapter object. Update the header comment list at the top of topology.js (`topology.highlight(ids, subtractedIds)`).

- [ ] **Step 3: Delete `liveLabelLines` from live.js**

Remove the function and its comment block; update the file header comment to "Pure helpers for the live topology overlay: number formatting, the dead band and edge flow attributes."

- [ ] **Step 4: Run the smoke script and look at the canvas**

RUN SMOKE. Expected: `ALL PASS`. Then a screenshot for eyes-on verification:
```bash
cat > /tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad/shot.mjs <<'EOF'
import { chromium } from "playwright";
const b = await chromium.launch({ args: ["--no-sandbox"] });
const p = await (await b.newContext({ viewport: { width: 1600, height: 950 }, deviceScaleFactor: 2 })).newPage();
await p.goto(process.env.SW_UI, { waitUntil: "networkidle" });
await p.click('.mglist-card:has-text("Berlin demo")');
await p.click('#mg-subtoggle .mode-btn[data-subview="topology"]');
await new Promise((r) => setTimeout(r, 3500));
if (process.env.CLICK_VALUES) { await p.click("#topology-controls .pill.active:not(.snap-btn):not(.layout-btn)"); await new Promise((r) => setTimeout(r, 800)); }
await p.screenshot({ path: process.env.OUT || "/tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad/pills.png" });
await b.close();
EOF
SW_UI=$UI node /tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad/shot.mjs
```
Open the PNG with the Read tool. Check: pills (not ellipses), dot left, name + dim id, hero numbers coloured, reactive dull, battery SoC bar, no overlapping nodes, edges attach at pill borders, chevrons present.

- [ ] **Step 5: Commit**

```bash
git add ui-assets/topology.js ui-assets/live.js ui-assets/explain.js tools/ui-smoke/live-topology.mjs
git commit -F - <<'EOF'
Draw graph nodes as pills on both canvases

Replaces the ellipse nodes and their fixed live-mode box with the
custom pill renderer. Each node's vis entry carries its pill model;
flushes, refreshes and the formula highlight all rebuild the node
through one nodeFor() so every path agrees. vis-network applies a
custom shape's dimensions a draw late, so the renderer reports sizes
and afterDrawing refreshes the nodes whose size moved before the
measured relayout. The Formulas canvas uses the same pills with
values off, which puts the formula's component ids on its nodes.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>
EOF
```

---

### Task 5: The `values` toggle, sampling that never stops

**Files:**
- Modify: `ui-assets/topology.js` (`applySample` no longer gated; `setLive`→`setValues`, `liveOn`→`valuesOn`; live map gains bounds/energy/ts/hist)
- Modify: `ui-assets/app.js:131-135,379-380`, `ui-assets/index.html:289-291,591`
- Test: `tools/ui-smoke/live-topology.mjs`

**Interfaces:**
- Produces:
  - `topology.setValues(on: boolean)`, `topology.valuesOn() -> boolean` (replacing `setLive`/`liveOn`; `app.js` uses the new names; the button class becomes `.values-btn`).
  - Live entry shape (consumed by Tasks 6-7): `{ p, q, soc, dc, energy, pLo, pHi, qLo, qHi, ts, hist }` where `ts` is the latest sample's `ts_ms` and `hist` is an array of `[ts_ms, watts]` capped at 60 entries (oldest first; batteries record `dc_power_w`, everything else `active_power_w`).
  - `topology.debugLiveEntry(id) -> entry | null` (smoke hook).

- [ ] **Step 1: Failing tests**

In the smoke script's toggle section, change `.live-btn` → `.values-btn` (all places), `topology.liveOn()` → `topology.valuesOn()` (both places), and add after the "liveOn() reports off" check (rename it "valuesOn() reports off"):
```js
// Sampling continues with values off: the map keeps filling so the
// hover card and the sparkline are complete when values come back.
await new Promise((r) => setTimeout(r, 2500));
const entryOff = await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  return topology.debugLiveEntry(100);
});
check("e2e: sampling continues while values are off", entryOff && Number.isFinite(entryOff.p) && entryOff.hist.length >= 2, JSON.stringify(entryOff));
check("e2e: live entry carries bounds and timestamp", entryOff && Number.isFinite(entryOff.ts) && Number.isFinite(entryOff.pLo) && Number.isFinite(entryOff.pHi), JSON.stringify(entryOff));
await page.click("#topology-controls .values-btn");
const backOn = await waitFor(async () => {
  const ms = await getModels();
  return hasValues(ms) ? ms : null;
});
check("e2e: toggle on restores row 2", hasValues(backOn));
await page.click("#topology-controls .values-btn"); // back off for the reload test below
```
RUN SMOKE → expected failure: `.values-btn` not found.

- [ ] **Step 2: topology.js changes**

`liveEntry(id)`: the initial object becomes
```js
      e = { p: null, q: null, soc: null, dc: null, energy: null, pLo: null, pHi: null, qLo: null, qHi: null, ts: null, hist: [] };
```
`applySample(ev)`: remove the `if (!liveEnabled) return;` line and rewrite the metric handling after `const e = liveEntry(ev.id);`:
```js
      e.ts = ev.ts_ms ?? Date.now();
      let drawn = true;
      switch (ev.metric) {
        case "active_power_w": e.p = ev.value; break;
        case "reactive_power_var": e.q = ev.value; break;
        case "soc_pct": e.soc = ev.value; break;
        case "dc_power_w": e.dc = ev.value; break;
        case "energy_wh": e.energy = ev.value; drawn = false; break;
        case "active_power_lower_bound_w": e.pLo = ev.value; drawn = false; break;
        case "active_power_upper_bound_w": e.pHi = ev.value; drawn = false; break;
        case "reactive_power_lower_bound_var": e.qLo = ev.value; drawn = false; break;
        case "reactive_power_upper_bound_var": e.qHi = ev.value; drawn = false; break;
        default: return;
      }
      if (ev.metric === "active_power_lower_bound_w" || ev.metric === "active_power_upper_bound_w") {
        maxAbsBoundW = Math.max(maxAbsBoundW, Math.abs(ev.value));
      }
      // 60 s power history for the hover sparkline; batteries are
      // judged by their DC side.
      const histMetric = componentById.get(ev.id)?.category === "battery" ? "dc_power_w" : "active_power_w";
      if (ev.metric === histMetric && Number.isFinite(ev.value)) {
        e.hist.push([e.ts, ev.value]);
        if (e.hist.length > 60) e.hist.splice(0, e.hist.length - 60);
      }
      if (!drawn) return;
      liveDirty.add(ev.id);
      armLiveFlush();
```
`flushLive`: the guard `if (!liveEnabled || ...)` stays — with values off nothing is drawn, but the map fills.
Rename `setLive` → `setValues`, `liveOn` → `valuesOn` in the returned API (keep the bodies). Add:
```js
    /// Smoke-test hook: one component's live entry.
    debugLiveEntry(id) {
      return liveValues.get(id) ?? null;
    },
```
Update the header comment list at the top of topology.js (`topology.setValues / valuesOn`) and the `applySample` doc comment ("records the value even while values are off").

- [ ] **Step 3: app.js + index.html**

app.js: `.live-btn` → `.values-btn` (both places), `canvas.setLive` → `canvas.setValues` (both the guard and the call), `topology.liveOn()` → `topology.valuesOn()`.
index.html lines 289-291:
```html
        <span class="ctl-label">show</span>
        <button type="button" class="pill values-btn active"
                title="Show live power, reactive power / SoC on nodes and flow chevrons on edges. Off: name and id only.">values</button>
```
Help list (line ~591): `<li><strong>Values</strong> — nodes show current power (and reactive power or SoC), edges a chevron in the direction power actually flows, sized by magnitude. The <em>values</em> pill (top right) toggles the numbers; off, nodes show name and id only. Hovering a node opens a detail card.</li>` (the hover sentence anticipates Task 7).

- [ ] **Step 4: Run the smoke script**

RUN SMOKE. Expected: `ALL PASS`.

- [ ] **Step 5: Commit**

```bash
git add ui-assets/topology.js ui-assets/app.js ui-assets/index.html tools/ui-smoke/live-topology.mjs
git commit -F - <<'EOF'
Rename the toggle to "values" and keep sampling while it is off

With pills the toggle no longer switches between two node shapes; it
hides the metric row. Samples are recorded regardless so the hover
card and the sparkline buffer are complete the instant values come
back, and the live map now keeps the envelope bounds, energy and
timestamps the card needs.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>
EOF
```

---

### Task 6: Hover card model (pure)

**Files:**
- Create: `ui-assets/hovercard.js`
- Test: `tools/ui-smoke/live-topology.mjs` (unit)

**Interfaces:**
- Produces `hoverCardModel(input) -> CardModel`:
  ```js
  // input
  {
    component,                 // /api/topology component
    live,                      // live entry from Task 5 or null
    parents: ["meter-2"],      // names
    children: ["bat-1000"],
    lastCommand: { kind: "power", value: "-2000", ts: 1787252975347, accepted: true, reason: "" } | null,
    nowMs: 1787252990000,
    deadBand: 300,
  }
  // CardModel
  {
    title: "inv-bat-1001", idLine: "#1001 · inverter / battery", health: "ok",
    power: { label: "Active power", text: "-19.93 kW", color, lo: -30000, hi: 30000, value: -19930 } | null,
    reactive: { label: "Reactive power", text: "1.20 kVAr", color, lo, hi, value } | null,
    pf: { text: "PF 0.99 lagging" } | null,
    energy: { text: "12.40 kWh since start" } | null,
    soc: { pct: 85, text: "85%" } | null,
    dc: { label: "DC power", text, color, lo: null, hi: null, value } | null,   // batteries only
    spark: [[ts, w], ...],
    lastCommand: { text: "power -2000 · 15 s ago · accepted" } | null,
    wiring: { parents: "meter-2", children: "bat-1000" },   // "—" when empty
    freshness: { text: "updated 2 s ago", stale: false },     // stale when > 5 s; "no data yet" when live null
  }
  ```
  PF = |P| / sqrt(P² + Q²), 2 decimals; `lagging` when `sign(P) === sign(Q)`, `leading` otherwise; null when either is missing or |P| below dead band. Batteries: `power` null, `dc` set, `pf` null.

- [ ] **Step 1: Failing unit tests**

Append to the unit block:
```js
  // hovercard.js: the pure card model
  const hc = await import("/assets/hovercard.js");
  const now = 1787252990000;
  const liveInv = { p: -19930, q: 1200, soc: null, dc: null, energy: 12400, pLo: -30000, pHi: 30000, qLo: -5000, qHi: 5000, ts: now - 2000, hist: [[now - 3000, -19000], [now - 2000, -19930]] };
  const card = hc.hoverCardModel({ component: inv, live: liveInv, parents: ["meter-2"], children: ["bat-1000"], lastCommand: { kind: "power", value: "-2000", ts: now - 15000, accepted: true, reason: "" }, nowMs: now, deadBand: 300 });
  eq("card title", card.title, "Battery Inverter 1");
  eq("card id line", card.idLine, "#12 · inverter / battery");
  eq("card power text", card.power.text, "-19.93 kW");
  eq("card power envelope", [card.power.lo, card.power.hi, card.power.value], [-30000, 30000, -19930]);
  eq("card pf leading (opposite signs)", card.pf.text, "PF 1.00 leading");
  eq("card energy", card.energy.text, "12.40 kWh since start");
  eq("card last command", card.lastCommand.text, "power -2000 · 15 s ago · accepted");
  eq("card wiring", card.wiring, { parents: "meter-2", children: "bat-1000" });
  eq("card freshness", card.freshness, { text: "updated 2 s ago", stale: false });
  eq("card spark", card.spark, liveInv.hist);
  const lag = hc.hoverCardModel({ component: inv, live: { ...liveInv, p: 8000, q: 6000 }, parents: [], children: [], lastCommand: null, nowMs: now, deadBand: 300 });
  eq("card pf lagging (same signs)", lag.pf.text, "PF 0.80 lagging");
  eq("card wiring empty", lag.wiring, { parents: "—", children: "—" });
  eq("card no command", lag.lastCommand, null);
  const stale = hc.hoverCardModel({ component: inv, live: { ...liveInv, ts: now - 9000 }, parents: [], children: [], lastCommand: null, nowMs: now, deadBand: 300 });
  eq("card stale", stale.freshness, { text: "updated 9 s ago", stale: true });
  const none = hc.hoverCardModel({ component: inv, live: null, parents: [], children: [], lastCommand: null, nowMs: now, deadBand: 300 });
  eq("card no data", none.freshness, { text: "no data yet", stale: true });
  eq("card no data → no pf", none.pf, null);
  const batCard = hc.hoverCardModel({ component: bat, live: { ...liveInv, p: null, q: null, dc: -3000, soc: 85.4 }, parents: ["inv-bat-1001"], children: [], lastCommand: null, nowMs: now, deadBand: 300 });
  eq("battery card soc", batCard.soc, { pct: 85, text: "85%" });
  eq("battery card dc", batCard.dc.text, "-3.00 kW");
  eq("battery card no ac power section", batCard.power, null);
  eq("battery card no pf", batCard.pf, null);
  eq("rejected command", hc.hoverCardModel({ component: inv, live: liveInv, parents: [], children: [], lastCommand: { kind: "power", value: "5", ts: now - 1000, accepted: false, reason: "out of bounds" }, nowMs: now, deadBand: 300 }).lastCommand.text, "power 5 · 1 s ago · rejected: out of bounds");
```
RUN SMOKE → fails on import of `/assets/hovercard.js`.

- [ ] **Step 2: Write `ui-assets/hovercard.js` (model half)**

```js
// The node hover card: a pure model builder (what the card says) and
// a DOM widget (further down) that shows it beside a pill. Read-only
// — every action stays in the inspector.

import { formatScaled } from "./live.js";
import { powerColor, reactiveColor } from "./pill.js";

const finite = (v) => v != null && Number.isFinite(v);

function agoText(ms) {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} s ago`;
  const m = Math.floor(s / 60);
  return m < 60 ? `${m} min ago` : `${Math.floor(m / 60)} h ago`;
}

function powerSection(label, value, lo, hi, deadBand) {
  if (!finite(value)) return null;
  return {
    label,
    text: formatScaled(value, "W"),
    color: powerColor(value, deadBand),
    lo: finite(lo) ? lo : null,
    hi: finite(hi) ? hi : null,
    value,
  };
}

// |P| / sqrt(P² + Q²); lagging when P and Q share a sign (passive
// convention: +Q inductive), leading otherwise.
function powerFactor(p, q, deadBand) {
  if (!finite(p) || !finite(q) || Math.abs(p) < deadBand) return null;
  const pf = Math.abs(p) / Math.hypot(p, q);
  const lagging = q === 0 || Math.sign(p) === Math.sign(q);
  return { text: `PF ${pf.toFixed(2)} ${lagging ? "lagging" : "leading"}` };
}

export function hoverCardModel({ component: c, live, parents, children, lastCommand, nowMs, deadBand }) {
  const battery = c.category === "battery";
  const hasLive = Boolean(live);
  const showSoc = battery || c.category === "ev-charger";
  const soc = hasLive && showSoc && finite(live.soc) ? { pct: Math.round(live.soc), text: `${Math.round(live.soc)}%` } : null;
  const energy = hasLive && finite(live.energy) ? { text: `${formatScaled(live.energy, "Wh")} since start` } : null;
  let freshness;
  if (!hasLive || !finite(live.ts)) freshness = { text: "no data yet", stale: true };
  else {
    const age = nowMs - live.ts;
    freshness = { text: `updated ${agoText(age)}`, stale: age > 5000 };
  }
  let command = null;
  if (lastCommand) {
    const outcome = lastCommand.accepted ? "accepted" : `rejected: ${lastCommand.reason || "unknown"}`;
    command = { text: `${String(lastCommand.kind).replace("_", " ")} ${lastCommand.value} · ${agoText(nowMs - lastCommand.ts)} · ${outcome}` };
  }
  return {
    title: c.name,
    idLine: `#${c.id} · ${c.category}${c.subtype ? ` / ${c.subtype}` : ""}`,
    health: (c.health || "ok") === "ok" && c.provides_telemetry === false ? "standby" : c.health || "ok",
    power: hasLive && !battery ? powerSection("Active power", live.p, live.pLo, live.pHi, deadBand) : null,
    reactive:
      hasLive && !battery && finite(live.q)
        ? {
            label: "Reactive power",
            text: formatScaled(live.q, "VAr"),
            color: reactiveColor(live.q, deadBand),
            lo: finite(live.qLo) ? live.qLo : null,
            hi: finite(live.qHi) ? live.qHi : null,
            value: live.q,
          }
        : null,
    pf: hasLive && !battery ? powerFactor(live.p, live.q, deadBand) : null,
    energy,
    soc,
    dc: hasLive && battery ? powerSection("DC power", live.dc, null, null, deadBand) : null,
    spark: hasLive ? live.hist.slice() : [],
    lastCommand: command,
    wiring: {
      parents: parents.length ? parents.join(", ") : "—",
      children: children.length ? children.join(", ") : "—",
    },
    freshness,
  };
}
```
Note `formatScaled(12400, "Wh")` yields `12.40 kWh` — the ladder is unit-agnostic.

- [ ] **Step 3: Run; unit tests pass. Commit**

RUN SMOKE → `ALL PASS`.
```bash
git add ui-assets/hovercard.js tools/ui-smoke/live-topology.mjs
git commit -F - <<'EOF'
Add the hover card model

hoverCardModel composes everything the node hover card says — power
on its envelope, reactive power with its band, a spelled-out power
factor with lagging/leading derived from the signs, energy since
start, SoC and DC power for batteries, the last command's outcome,
wiring and a freshness line — from the live entry and the topology,
with no DOM, so the wording is unit-tested.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>
EOF
```

---

### Task 7: Hover card widget and canvas wiring

**Files:**
- Modify: `ui-assets/hovercard.js` (append `createHoverCard`)
- Modify: `ui-assets/topology.js` (hover wiring; `/api/setpoints` cache; history seed)
- Modify: `ui-assets/style.css` (append `.hover-card` rules)
- Test: `tools/ui-smoke/live-topology.mjs` (e2e)

**Interfaces:**
- Consumes: `hoverCardModel` (Task 6), live entries (Task 5), `topology.parentsOf/childrenOf` (existing), `mgPath` from routing.js.
- Produces: `createHoverCard() -> { show(model, anchor), hide(), visible(), text() }` where `anchor = { x, y, width, height }` in page (DOM) pixels of the pill; the card is appended to `document.body` once, positioned below the anchor (or above when it would leave the viewport), 300 px wide, `pointer-events: none`.
- `topology.debugHoverCard() -> { visible, text } | null` and `topology.debugNodeScreenRect(id) -> { x, y, width, height } | null` smoke hooks.
- Adapter option `adapter.hoverCard: true` (topology instance only).

- [ ] **Step 1: Failing e2e test**

Add before the toggle section of the smoke script:
```js
// ── e2e: hover card ──────────────────────────────────────────────
const hoverAt = await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  return topology.debugNodeScreenRect(1001); // inv-bat-1001
});
await page.mouse.move(hoverAt.x + hoverAt.width / 2, hoverAt.y + hoverAt.height / 2);
const cardState = await waitFor(async () => {
  const s = await page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugHoverCard();
  });
  return s && s.visible ? s : null;
}, 5000);
check("e2e: hover card names the component", /inv-bat-1001/.test(cardState.text) && /#1001/.test(cardState.text), cardState.text);
check("e2e: hover card has a PF line", /PF \d\.\d\d (lagging|leading)/.test(cardState.text), cardState.text);
check("e2e: hover card has freshness", /updated \d+ s ago|no data yet/.test(cardState.text), cardState.text);
check("e2e: hover card is inert to the pointer", await page.evaluate(() => getComputedStyle(document.querySelector(".hover-card")).pointerEvents === "none"));
await page.mouse.move(5, 5);
const hiddenCard = await waitFor(async () => {
  const s = await page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugHoverCard();
  });
  return s && !s.visible ? s : null;
}, 3000);
check("e2e: hover card hides on blur", hiddenCard.visible === false);
```
RUN SMOKE → fails (`debugNodeScreenRect is not a function`).

- [ ] **Step 2: Append the widget to `hovercard.js`**

```js
// ── widget ──────────────────────────────────────────────────────

function esc(s) {
  return String(s).replace(/[&<>"']/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[ch]);
}

function envelopeBar(section) {
  if (!section) return "";
  let marker = "";
  if (section.lo != null && section.hi != null && section.hi > section.lo) {
    const pct = Math.max(0, Math.min(100, ((section.value - section.lo) / (section.hi - section.lo)) * 100));
    marker = `<div class="hc-bar"><div class="hc-bar-marker" style="left:${pct.toFixed(1)}%;background:${section.color}"></div></div>
      <div class="hc-bar-ends"><span>${esc(formatScaled(section.lo, "W"))}</span><span>${esc(formatScaled(section.hi, "W"))}</span></div>`;
  }
  return `<div class="hc-row"><span class="hc-label">${esc(section.label)}</span><span class="hc-value" style="color:${section.color}">${esc(section.text)}</span></div>${marker}`;
}

function sparkSvg(points) {
  if (points.length < 2) return '<div class="hc-spark hc-spark-empty">collecting…</div>';
  const W = 276, H = 36;
  const ys = points.map((p) => p[1]);
  const lo = Math.min(0, ...ys), hi = Math.max(0, ...ys);
  const span = hi - lo || 1;
  const x = (i) => ((i / (points.length - 1)) * W).toFixed(1);
  const y = (v) => (H - ((v - lo) / span) * H).toFixed(1);
  const d = points.map((p, i) => `${i ? "L" : "M"}${x(i)},${y(p[1])}`).join(" ");
  const zero = y(0);
  return `<svg class="hc-spark" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" aria-hidden="true">
    <line x1="0" y1="${zero}" x2="${W}" y2="${zero}" class="hc-spark-zero"/>
    <path d="${d}" class="hc-spark-line"/>
    <circle cx="${x(points.length - 1)}" cy="${y(ys[ys.length - 1])}" r="2.5" class="hc-spark-end"/>
  </svg>`;
}

function render(m) {
  const healthChip = m.health === "ok" ? "" : `<span class="hc-chip hc-chip-${esc(m.health)}">${esc(m.health)}</span>`;
  const soc = m.soc
    ? `<div class="hc-row"><span class="hc-label">SoC</span><span class="hc-value">${esc(m.soc.text)}</span></div>
       <div class="hc-bar hc-soc"><div class="hc-soc-fill" style="width:${m.soc.pct}%"></div></div>`
    : "";
  const row = (label, value, cls = "") => `<div class="hc-row"><span class="hc-label">${esc(label)}</span><span class="hc-value ${cls}">${esc(value)}</span></div>`;
  return `
    <div class="hc-head"><span class="hc-title">${esc(m.title)}</span>${healthChip}</div>
    <div class="hc-id">${esc(m.idLine)}</div>
    ${sparkSvg(m.spark)}
    ${envelopeBar(m.power)}${envelopeBar(m.dc)}
    ${m.reactive ? `<div class="hc-row"><span class="hc-label">${esc(m.reactive.label)}</span><span class="hc-value" style="color:${m.reactive.color}">${esc(m.reactive.text)}</span></div>` : ""}
    ${m.pf ? `<div class="hc-row hc-pf">${esc(m.pf.text)}</div>` : ""}
    ${soc}
    ${m.energy ? row("Energy", m.energy.text) : ""}
    ${m.lastCommand ? row("Last command", m.lastCommand.text, "hc-cmd") : ""}
    ${row("Parents", m.wiring.parents)}
    ${row("Children", m.wiring.children)}
    <div class="hc-foot"><span class="${m.freshness.stale ? "hc-stale" : ""}">${esc(m.freshness.text)}</span><span>click for inspector</span></div>`;
}

// One card per canvas, appended lazily. `anchor` is the pill's rect
// in page pixels; the card sits 8 px below it, or above when below
// would leave the viewport, and never takes the pointer.
export function createHoverCard() {
  let el = null;
  function ensure() {
    if (el) return el;
    el = document.createElement("div");
    el.className = "hover-card";
    el.setAttribute("aria-hidden", "true");
    el.hidden = true;
    document.body.appendChild(el);
    return el;
  }
  return {
    show(model, anchor) {
      const node = ensure();
      node.innerHTML = render(model);
      node.hidden = false;
      const W = 300;
      const left = Math.max(8, Math.min(window.innerWidth - W - 8, anchor.x + anchor.width / 2 - W / 2));
      node.style.left = `${left}px`;
      node.style.top = "0px";
      const h = node.offsetHeight;
      const below = anchor.y + anchor.height + 8;
      node.style.top = `${below + h > window.innerHeight - 8 ? Math.max(8, anchor.y - 8 - h) : below}px`;
    },
    hide() {
      if (el) el.hidden = true;
    },
    visible() {
      return Boolean(el && !el.hidden);
    },
    text() {
      return el && !el.hidden ? el.textContent : "";
    },
  };
}
```

- [ ] **Step 3: CSS (append to style.css)**

```css
/* Node hover card (topology canvas). Read-only and inert to the
   pointer: moving the mouse onto it must not blur the node. */
.hover-card {
  position: fixed;
  z-index: 60;
  width: 300px;
  box-sizing: border-box;
  padding: 10px 12px;
  border-radius: 10px;
  background: #242a33;
  border: 1px solid #323a45;
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
  color: #d5dbe3;
  font-family: "IBM Plex Sans", var(--font-sans);
  font-size: 12px;
  line-height: 1.4;
  pointer-events: none;
}
.hover-card .hc-head { display: flex; align-items: center; gap: 8px; }
.hover-card .hc-title { font-weight: 600; font-size: 13px; }
.hover-card .hc-id { color: var(--muted); font-family: "IBM Plex Mono", var(--font-mono); font-size: 11px; margin-bottom: 6px; }
.hover-card .hc-chip { font-size: 10px; padding: 1px 6px; border-radius: 8px; text-transform: uppercase; letter-spacing: 0.04em; }
.hover-card .hc-chip-error { background: #e58275; color: #1c2128; }
.hover-card .hc-chip-standby { background: #c4ad55; color: #1c2128; }
.hover-card .hc-spark { display: block; width: 100%; height: 36px; margin: 2px 0 6px; }
.hover-card .hc-spark-empty { color: var(--muted); font-size: 11px; line-height: 36px; text-align: center; }
.hover-card .hc-spark-line { fill: none; stroke: #79b8ff; stroke-width: 1.5; }
.hover-card .hc-spark-zero { stroke: #323a45; stroke-width: 1; }
.hover-card .hc-spark-end { fill: #79b8ff; }
.hover-card .hc-row { display: flex; justify-content: space-between; gap: 12px; padding: 2px 0; }
.hover-card .hc-label { color: var(--muted); }
.hover-card .hc-value { font-family: "IBM Plex Mono", var(--font-mono); font-variant-numeric: tabular-nums; text-align: right; }
.hover-card .hc-cmd { font-size: 11px; }
.hover-card .hc-pf { justify-content: flex-end; font-family: "IBM Plex Mono", var(--font-mono); color: var(--muted); font-size: 11px; }
.hover-card .hc-bar { position: relative; height: 4px; border-radius: 2px; background: #323a45; margin: 2px 0; }
.hover-card .hc-bar-marker { position: absolute; top: -2px; width: 2px; height: 8px; border-radius: 1px; transform: translateX(-1px); }
.hover-card .hc-bar-ends { display: flex; justify-content: space-between; color: var(--muted); font-size: 10px; font-family: "IBM Plex Mono", var(--font-mono); margin-bottom: 4px; }
.hover-card .hc-soc-fill { height: 100%; border-radius: 2px; background: #6bd9a5; }
.hover-card .hc-foot { display: flex; justify-content: space-between; margin-top: 6px; padding-top: 6px; border-top: 1px solid #323a45; color: var(--muted); font-size: 10.5px; }
.hover-card .hc-stale { color: #c4ad55; }
```

- [ ] **Step 4: Wire it in topology.js**

Imports: `import { createHoverCard, hoverCardModel } from "./hovercard.js";` and add `mgPath` to the routing.js import.

State (inside `createGraphCanvas`, after the size-dirty block):
```js
  // Hover card: one per canvas (only the topology canvas asks for
  // one), shown after a short dwell so a sweep across the graph
  // fires nothing, hidden by any gesture.
  const hoverCard = adapter.hoverCard ? createHoverCard() : null;
  let hoverTimer = null;
  let hoverId = null;
  let hoverAbort = null;
  // /api/setpoints per component, cached for 60 s.
  const setpointCache = new Map(); // id -> { at, last }

  async function lastCommandFor(id, signal) {
    const hit = setpointCache.get(id);
    if (hit && Date.now() - hit.at < 60_000) return hit.last;
    try {
      const res = await fetch(`/api/setpoints?id=${id}&window_s=600`, { signal });
      const data = await res.json();
      const e = data.events?.[data.events.length - 1];
      const last = e
        ? { kind: e.kind, value: e.value, ts: e.ts, accepted: e.outcome?.kind === "accepted", reason: e.outcome?.reason || "" }
        : null;
      setpointCache.set(id, { at: Date.now(), last });
      return last;
    } catch {
      return hit ? hit.last : null;
    }
  }

  function nodeScreenRect(id) {
    const box = network.getBoundingBox(id);
    if (!box) return null;
    const tl = network.canvasToDOM({ x: box.left, y: box.top });
    const br = network.canvasToDOM({ x: box.right, y: box.bottom });
    const host = container().getBoundingClientRect();
    return { x: host.left + tl.x, y: host.top + tl.y, width: br.x - tl.x, height: br.y - tl.y };
  }

  function hideHover() {
    if (hoverTimer) clearTimeout(hoverTimer);
    hoverTimer = null;
    hoverId = null;
    if (hoverAbort) hoverAbort.abort();
    hoverAbort = null;
    if (hoverCard) hoverCard.hide();
  }

  // Seeds a short sparkline from the server's history the first
  // time a node is hovered (the live buffer takes 60 s to fill).
  async function seedHistory(id, c, entry, signal) {
    if (!entry || entry.hist.length >= 10) return;
    const metric = c.category === "battery" ? "dc_power_w" : "active_power_w";
    try {
      const res = await fetch(`${mgPath("history")}?id=${id}&metric=${metric}&window_s=60`, { signal });
      const data = await res.json();
      const firstLive = entry.hist[0]?.[0] ?? Number.POSITIVE_INFINITY;
      const seeded = (data.samples || []).filter((s) => Number.isFinite(s[1]) && s[0] < firstLive);
      entry.hist = [...seeded, ...entry.hist].slice(-60);
    } catch {
      // no seed — the live buffer fills on its own
    }
  }

  async function showHover(id) {
    const c = componentById.get(id);
    if (!c || !hoverCard || !network) return;
    hoverAbort = new AbortController();
    const { signal } = hoverAbort;
    const entry = liveValues.get(id) ?? null;
    await seedHistory(id, c, entry, signal);
    const lastCommand = await lastCommandFor(id, signal);
    if (signal.aborted || hoverId !== id) return;
    const rect = nodeScreenRect(id);
    if (!rect) return;
    const nameOf = (nid) => componentById.get(nid)?.name ?? `#${nid}`;
    hoverCard.show(
      hoverCardModel({
        component: c,
        live: entry,
        parents: network.getConnectedNodes(id, "from").map(nameOf),
        children: network.getConnectedNodes(id, "to").map(nameOf),
        lastCommand,
        nowMs: Date.now(),
        deadBand: deadBandW(maxAbsBoundW),
      }),
      rect,
    );
  }
```
(`parentsOf`/`childrenOf` on the public API already use `network.getConnectedNodes(id, "from" | "to")` — check and reuse the same calls.)

Events, registered next to the other `network.on` calls inside the `if (!network)` branch:
```js
      if (hoverCard) {
        network.on("hoverNode", ({ node }) => {
          hideHover();
          hoverId = node;
          hoverTimer = setTimeout(() => {
            hoverTimer = null;
            if (hoverId === node) showHover(node);
          }, 250);
        });
        network.on("blurNode", hideHover);
        for (const ev of ["dragStart", "zoom", "dragging", "click", "oncontext", "doubleClick"]) {
          network.on(ev, hideHover);
        }
        document.addEventListener("visibilitychange", hideHover);
      }
```
In `apply()`, after `buildVisData`: `if (hoverId != null && !componentById.has(hoverId)) hideHover();`.
In `flushLive()`: at the top, `if (visibleSubview() !== "topology") { hideHover(); return; }` replaces the plain `return` for the subview guard; at the end, after the DataSet updates, `if (hoverCard && hoverCard.visible() && hoverId != null) showHover(hoverId);` so the card's numbers and freshness follow the 1 Hz flush (the setpoint lookup is cached; the history seed is skipped once the buffer has 10+ points).

Debug hooks on the API:
```js
    /// Smoke-test hooks for the hover card.
    debugNodeScreenRect(id) {
      return network ? nodeScreenRect(id) : null;
    },
    debugHoverCard() {
      return hoverCard ? { visible: hoverCard.visible(), text: hoverCard.text() } : null;
    },
```
Topology instance adapter (bottom of the file): add `hoverCard: true,`.

- [ ] **Step 5: Run the smoke script, screenshot with the card open**

RUN SMOKE → `ALL PASS`. Then extend `shot.mjs` (Task 4) with an env switch: when `HOVER=1`, evaluate `topology.debugNodeScreenRect(1001)`, `page.mouse.move` to its centre, wait 1500 ms, then screenshot to `OUT=.../hover.png`. Read the PNG: card below the pill, sparkline drawn, PF line, freshness footer, card not covering the hovered pill.

- [ ] **Step 6: Commit**

```bash
git add ui-assets/hovercard.js ui-assets/topology.js ui-assets/style.css tools/ui-smoke/live-topology.mjs
git commit -F - <<'EOF'
Show a read-only hover card on topology nodes

Hovering a pill for a quarter second opens a card with the 60 s power
sparkline, power on its envelope, reactive power and power factor,
energy, SoC and DC power for batteries, the last command's outcome,
wiring and a freshness line. It is inert to the pointer and hides on
every gesture, so it never competes with the inspector; the setpoint
lookup is cached per component and the history seed happens once.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>
EOF
```

---

### Task 8: Docs, spec touch-up and a final visual pass

**Files:**
- Modify: `AGENTS.md:50-56`, `docs/superpowers/specs/2026-08-20-pill-nodes-design.md` (hover-card extras line)
- Verify: screenshots of the four states

- [ ] **Step 1: AGENTS.md**

In the `ui-assets/` bullet, after `live.js`, add: "`pill.js` owns the node model and canvas renderer both graph canvases draw with; `hovercard.js` the node hover card (pure model + DOM widget); `vendor/fonts/` the vendored IBM Plex faces (OFL)." Update the `live.js` sentence to "number formatting, the dead band and edge flow".

- [ ] **Step 2: Spec touch-up**

The spec's hover-card bullet promises "battery: SoC bar against its protect band, DC power on its envelope, capacity and stored kWh; PV: sunlight %". None of those values reach the client (there is no per-component detail endpoint, and the spec forbids server changes). Replace that parenthetical with "(battery: SoC bar and DC power; PV: none yet — protect band, capacity, DC envelope and sunlight need a component-detail endpoint and are deferred with the per-phase metrics)". Also drop "TTL remaining" (setpoint events carry no TTL) so the line reads "last command (kind, value, age, accepted/rejected with reason)".

- [ ] **Step 3: Visual pass**

Take four screenshots with `shot.mjs` variants: values on; values off (`CLICK_VALUES=1`); the Formulas subview with a formula term hovered (click the formulas mode button, then `page.hover('#formula-view .formula-ref')`); topology with the hover card (`HOVER=1`). Read each. Things that must hold: no node overlaps another in the default layout; edges and chevrons meet pill borders; values-off pills show only name + id at the larger size; the Formulas canvas shows `#id` and a red ring on a subtracted term's node while hovered (a referenced term shows the accent ring).

- [ ] **Step 4: Full smoke run, then commit**

RUN SMOKE → `ALL PASS`.
```bash
git add AGENTS.md docs/superpowers/specs/2026-08-20-pill-nodes-design.md
git commit -F - <<'EOF'
Document the pill renderer and trim the hover card's promises

AGENTS.md points at pill.js, hovercard.js and the vendored fonts. The
spec's hover card listed battery capacity, protect band, DC envelope
and PV sunlight, which no endpoint exposes to the client; they are
deferred alongside the per-phase metrics rather than implied.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>
EOF
```
