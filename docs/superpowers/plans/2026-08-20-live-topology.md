# Live Topology Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Live power/reactive/SoC values on topology nodes and physical-flow chevrons on edges, fed by the existing 1 Hz WS sample broadcast, behind a persisted default-on toggle.

**Architecture:** All client-side in `ui-assets/`. A new pure-function module `live.js` (label text, flow attributes, shared power-format ladder) is consumed by `topology.js`, which keeps an `id → {p,q,soc}` map fed from the existing WS `sample` events and flushes label/edge updates in one batched DataSet update per second. No server changes of any kind.

**Tech Stack:** vanilla ES modules, vis-network (already vendored), Playwright (installed globally on this VM) for unit-in-browser + e2e verification.

**Spec:** `docs/superpowers/specs/2026-08-20-live-topology-design.md`

## Global Constraints

- **No server/Rust changes.** The branch must touch only `ui-assets/` (and the test script under `tools/`).
- Commit messages: imperative subject, short body saying why, trailer exactly `Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>`. NO Co-Authored-By / AI-attribution trailers.
- `npx @biomejs/biome check ui-assets/*.js` and `node --check <file>` must pass before every commit.
- Power format ladder must stay byte-identical to the dashboard's: `|v| >= 1e6 → (v/1e6).toFixed(2) + " M" + unit`, `>= 1e3 → toFixed(2) + " k" + unit`, else `v.toFixed(1) + " " + unit`; null/non-finite → `"—"`.
- Sign convention: switchyard is consumption-positive. Chevrons show *physical* flow: negative (export/generation) points toward the parent.
- localStorage key: `switchyard-topology-live`; absent = on, `"0"` = off.
- WS per-component samples arrive as `ev.kind === "sample"` with `{id, metric, ts_ms, value}`; metric names: `active_power_w`, `reactive_power_var`, `soc_pct`, `active_power_lower_bound_w`, `active_power_upper_bound_w`.

**Test server** (all tasks): a running switchyard with the berlin demo. Reuse one instance across tasks:

```bash
S=/tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad
export PROTOC=$S/protoc/bin/protoc   # protoc is not installed system-wide
rm -f $S/lt-endpoints.json
(cargo run --quiet --bin switchyard -- examples/berlin-demo.lisp \
  --state-dir $S/sw-state --ephemeral-ports \
  --emit-endpoints=$S/lt-endpoints.json > $S/lt-server.log 2>&1 &)
timeout 300 bash -c "until [ -s $S/lt-endpoints.json ]; do sleep 2; done"
export SW_UI="http://$(python3 -c "import json;print(json.load(open('$S/lt-endpoints.json'))['ui'])")"
curl -sf "$SW_UI/api/microgrids" >/dev/null && echo SERVER-UP
```

The dev build serves `ui-assets/` from disk, so JS edits apply on browser reload without restarting the server. If a server is already running from earlier work, just set `SW_UI` to it.

---

### Task 1: Pure helpers module (`live.js`) with in-browser unit tests

**Files:**
- Create: `ui-assets/live.js`
- Create: `tools/ui-smoke/live-topology.mjs`

**Interfaces:**
- Consumes: nothing (pure module).
- Produces (later tasks import these from `./live.js`):
  - `formatScaled(value, unit) -> string` — the ladder above; `"—"` for null/undefined/NaN/Infinity.
  - `liveLabelLine({ category, p, q, soc }) -> string | null` — the node's second label line; `null` when `p` is null (no line at all).
  - `edgeFlow(childPowerW, parentCount, siteMaxRatedW) -> { chevron: boolean, towardParent: boolean, width: number, scale: number }`.

- [ ] **Step 1: Write the failing test script**

Create `tools/ui-smoke/live-topology.mjs`:

```js
// Live-topology smoke: in-browser unit tests for ui-assets/live.js
// plus (later tasks) e2e assertions against a running switchyard.
// Run: SW_UI=http://127.0.0.1:PORT node tools/ui-smoke/live-topology.mjs
import { chromium } from "playwright";

const BASE = process.env.SW_UI;
if (!BASE) throw new Error("set SW_UI to a running switchyard UI, e.g. http://127.0.0.1:8801");

let failures = 0;
const check = (name, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"} ${name}${ok ? "" : ` — ${detail}`}`);
  if (!ok) failures++;
};

const browser = await chromium.launch({ args: ["--no-sandbox"] });
const page = await (await browser.newContext({ viewport: { width: 1600, height: 950 } })).newPage();
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));
await page.goto(BASE, { waitUntil: "networkidle" });

// ── unit tests: import the module in the browser ──────────────────
const unit = await page.evaluate(async () => {
  const m = await import("/assets/live.js");
  const out = [];
  const eq = (name, got, want) =>
    out.push({ name, ok: Object.is(got, want) || JSON.stringify(got) === JSON.stringify(want), got: JSON.stringify(got), want: JSON.stringify(want) });

  // formatScaled: the dashboard ladder, byte-identical
  eq("fmt W", m.formatScaled(107.3, "W"), "107.3 W");
  eq("fmt kW", m.formatScaled(-24000, "W"), "-24.00 kW");
  eq("fmt MW", m.formatScaled(1500000, "W"), "1.50 MW");
  eq("fmt kVAr", m.formatScaled(1200, "VAr"), "1.20 kVAr");
  eq("fmt null", m.formatScaled(null, "W"), "—");
  eq("fmt NaN", m.formatScaled(Number.NaN, "W"), "—");

  // liveLabelLine
  eq("line inverter p+q", m.liveLabelLine({ category: "inverter", p: -24000, q: 1200, soc: null }), "-24.00 kW · 1.20 kVAr");
  eq("line meter p only", m.liveLabelLine({ category: "meter", p: 500, q: null, soc: null }), "500.0 W");
  eq("line battery soc", m.liveLabelLine({ category: "battery", p: 0, q: null, soc: 85.2 }), "0.0 W · SoC 85%");
  eq("line ev soc", m.liveLabelLine({ category: "ev-charger", p: 3000, q: null, soc: 40 }), "3.00 kW · SoC 40%");
  eq("line battery no soc yet", m.liveLabelLine({ category: "battery", p: 0, q: null, soc: null }), "0.0 W");
  eq("line no sample", m.liveLabelLine({ category: "meter", p: null, q: null, soc: null }), null);

  // edgeFlow: dead band, direction, sharing, clamps
  eq("flow dead", m.edgeFlow(10, 1, 30000).chevron, false);
  eq("flow consume", m.edgeFlow(5000, 1, 30000).towardParent, false);
  eq("flow export", m.edgeFlow(-5000, 1, 30000).towardParent, true);
  eq("flow shared halves", m.edgeFlow(-5000, 2, 30000).chevron, true);
  out.push({ name: "flow shared magnitude", ok: m.edgeFlow(-5000, 2, 30000).scale < m.edgeFlow(-5000, 1, 30000).scale });
  out.push({ name: "flow width clamp hi", ok: m.edgeFlow(-10e6, 1, 30000).width <= 6 });
  out.push({ name: "flow width clamp lo", ok: m.edgeFlow(-400, 1, 30000).width >= 1 });
  eq("flow zero parents treated as 1", m.edgeFlow(-5000, 0, 30000).chevron, true);
  eq("flow fallback max", m.edgeFlow(-5000, 1, 0).chevron, true);
  return out;
});
for (const t of unit) check(`unit: ${t.name}`, t.ok, `got ${t.got} want ${t.want}`);

check("no page errors", errors.length === 0, JSON.stringify(errors));
await browser.close();
if (failures) { console.error(`${failures} FAILED`); process.exit(1); }
console.log("ALL PASS");
```

- [ ] **Step 2: Run it to make sure it fails**

Run: `node tools/ui-smoke/live-topology.mjs` (with `SW_UI` set)
Expected: FAIL — the `import("/assets/live.js")` rejects (404), the evaluate throws, script exits non-zero.

- [ ] **Step 3: Implement `ui-assets/live.js`**

```js
// Pure helpers for the live topology overlay: label text and edge
// flow attributes. No DOM, no vis-network — unit-testable alone.

// W → kW → MW ladder, shared with the dashboard's fmt() so every
// power readout in the app scales identically.
export function formatScaled(value, unit) {
  if (value == null || !Number.isFinite(value)) return "—";
  const a = Math.abs(value);
  if (a >= 1e6) return `${(value / 1e6).toFixed(2)} M${unit}`;
  if (a >= 1e3) return `${(value / 1e3).toFixed(2)} k${unit}`;
  return `${value.toFixed(1)} ${unit}`;
}

// The node's live second line, or null when no power sample has
// arrived yet (the node then keeps its structural one-line label).
// Batteries and EV chargers show SoC instead of reactive power;
// everything else appends reactive only when the component reports
// it.
export function liveLabelLine({ category, p, q, soc }) {
  if (p == null || !Number.isFinite(p)) return null;
  const power = formatScaled(p, "W");
  if (category === "battery" || category === "ev-charger") {
    if (soc == null || !Number.isFinite(soc)) return power;
    return `${power} · SoC ${soc.toFixed(0)}%`;
  }
  if (q == null || !Number.isFinite(q)) return power;
  return `${power} · ${formatScaled(q, "VAr")}`;
}

// Flow attributes for a parent→child edge. `childPowerW` is the
// child's active power (consumption-positive); the edge's share is
// 1/parentCount (the meter aggregation rule, so parallel paths
// split visually too). The chevron shows *physical* flow: export
// (negative) points toward the parent. Below the dead band the
// chevron disappears so dead legs look dead.
export function edgeFlow(childPowerW, parentCount, siteMaxRatedW) {
  const max = siteMaxRatedW > 0 ? siteMaxRatedW : 10_000;
  const flow = (childPowerW ?? 0) / Math.max(parentCount, 1);
  const dead = Math.max(0.01 * max, 50);
  if (!Number.isFinite(flow) || Math.abs(flow) < dead) {
    return { chevron: false, towardParent: false, width: 1.5, scale: 0 };
  }
  const norm = Math.min(1, Math.sqrt(Math.abs(flow) / max));
  return {
    chevron: true,
    towardParent: flow < 0,
    width: Math.min(6, Math.max(1.5, 1 + 5 * norm)),
    scale: Math.max(0.5, 1.4 * norm),
  };
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `node --check ui-assets/live.js && node tools/ui-smoke/live-topology.mjs`
Expected: every `unit:` line PASS, `ALL PASS`, exit 0. (`/assets/live.js` is served because the dev asset server serves the whole `ui-assets/` dir from disk; confirm with `curl -sf "$SW_UI/assets/live.js" | head -1`.)

- [ ] **Step 5: Biome + commit**

```bash
npx @biomejs/biome check ui-assets/live.js tools/ui-smoke/live-topology.mjs
git add ui-assets/live.js tools/ui-smoke/live-topology.mjs
git commit -m "Add the live-topology pure helpers and their smoke test

formatScaled (the dashboard's W→kW→MW ladder, shared from here on),
liveLabelLine (node second line: power, reactive or SoC), and
edgeFlow (physical-flow chevron attributes with dead band and
parent-count sharing). Unit-tested in-browser via a Playwright
smoke script that later tasks extend with e2e assertions.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 2: Dashboard delegates its format ladder to `live.js`

**Files:**
- Modify: `ui-assets/dashboard.js` (the module-level `fmt` function, ~line 15-30)

**Interfaces:**
- Consumes: `formatScaled(value, unit)` from Task 1.
- Produces: unchanged `fmt(quantity, unit, value)` behavior — this is a pure dedup.

- [ ] **Step 1: Replace the ladder body with delegation**

In `ui-assets/dashboard.js`, add to the imports:

```js
import { formatScaled } from "./live.js";
```

and change `fmt`'s power branch from the inline ladder to:

```js
function fmt(quantity, unit, value) {
  if (value == null || !Number.isFinite(value)) return "—";
  if (quantity === "Power" || quantity === "ReactivePower" || unit === "W" || unit === "var") {
    return formatScaled(value, unit);
  }
  // Voltage, frequency, percentage etc. — fixed unit, modest precision.
  return `${value.toFixed(2)} ${unit}`;
}
```

(The three inline ladder lines are deleted; `fmtRowPower` already aliases `fmt` and needs no change.)

- [ ] **Step 2: Verify identical output in the browser**

Run: `node --check ui-assets/dashboard.js && npx @biomejs/biome check ui-assets/dashboard.js`, then reload `$SW_UI` in the smoke browser or run:

```bash
node - <<'EOF'
import { chromium } from "playwright";
const b = await chromium.launch({ args: ["--no-sandbox"] });
const p = await (await b.newContext()).newPage();
await p.goto(process.env.SW_UI, { waitUntil: "networkidle" });
await p.click(".mglist-card:not(.mglist-new)");
await new Promise(r => setTimeout(r, 3000));
const v = await p.evaluate(() => document.querySelector('[data-stream="pv_power"]').textContent);
console.log("pv tile:", v);
if (!/(W|kW|MW)$/.test(v.trim())) process.exit(1);
await b.close();
EOF
```

Expected: the PV tile still renders a `... kW` value.

- [ ] **Step 3: Commit**

```bash
git add ui-assets/dashboard.js
git commit -m "Share the power-format ladder with live.js

The dashboard's fmt() now delegates its W→kW→MW branch to
formatScaled so the topology labels and every dashboard readout
scale identically, with one implementation.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 3: Live node labels — state map, WS wiring, batched flush

**Files:**
- Modify: `ui-assets/topology.js`
- Modify: `ui-assets/repl.js` (the `ev.kind === "sample"` block, ~line 394)
- Modify: `ui-assets/routing.js` (the topology subview-enter branch in `applyMode`, ~line 210)
- Modify: `tools/ui-smoke/live-topology.mjs` (append e2e section)

**Interfaces:**
- Consumes: `liveLabelLine` from Task 1.
- Produces:
  - `topology.applySample(ev)` — `ev` is the WS sample `{id, metric, value}`; repl.js calls it.
  - `topology.flushLive()` — applies pending live updates immediately; routing.js calls it on topology subview enter.
  - Module-internal: `liveValues: Map<number, {p,q,soc}>`, `liveDirty: Set<number>`, `liveEnabled: boolean` (always `true` until Task 5 wires the toggle), `maxAbsBoundW: number`.

- [ ] **Step 1: Extend the smoke script with failing e2e label assertions**

Append to `tools/ui-smoke/live-topology.mjs`, before the `check("no page errors", ...)` line:

```js
// ── e2e: live labels on the canvas ────────────────────────────────
await page.click(".mglist-card:not(.mglist-new)");
await new Promise((r) => setTimeout(r, 1000));
await page.click('#mg-subtoggle .mode-btn[data-subview="topology"]');
await new Promise((r) => setTimeout(r, 3500)); // > one 1 Hz flush
const labels = await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  return topology.debugLiveLabels();
});
check("e2e: some node has a kW/W line", labels.some((l) => /\n-?\d+(\.\d+)? (W|kW|MW)/.test(l)), JSON.stringify(labels));
check("e2e: battery node shows SoC", labels.some((l) => /SoC \d+%/.test(l)), JSON.stringify(labels));
```

- [ ] **Step 2: Run to verify it fails**

Run: `node tools/ui-smoke/live-topology.mjs`
Expected: the two `e2e:` checks FAIL (`debugLiveLabels` is not a function → evaluate throws → script fails; that's fine, it proves the surface doesn't exist yet).

- [ ] **Step 3: Implement in topology.js**

Inside the `topology` IIFE, next to the other module state (`let manualArrangement = ...`):

```js
  // ── live overlay state ─────────────────────────────────────────
  // Per-component last-known live values, fed by applySample from
  // the WS sample stream; ids marked dirty are flushed to the
  // canvas in ONE nodesDS/edgesDS update per second.
  const liveValues = new Map(); // id -> { p, q, soc }
  const liveDirty = new Set();
  let liveEnabled = true; // Task 5 wires the persisted toggle
  let liveFlushTimer = null;
  // Largest |active power bound| seen — the magnitude reference for
  // edge flow scaling. Reset on topology refresh.
  let maxAbsBoundW = 0;

  function liveEntry(id) {
    let e = liveValues.get(id);
    if (!e) {
      e = { p: null, q: null, soc: null };
      liveValues.set(id, e);
    }
    return e;
  }

  function topologyVisible() {
    return (
      document.body.dataset.mode === "microgrids" &&
      document.body.dataset.mgView === "selected" &&
      document.body.dataset.subview === "topology"
    );
  }

  function armLiveFlush() {
    if (liveFlushTimer !== null) return;
    liveFlushTimer = setTimeout(() => {
      liveFlushTimer = null;
      flushLive();
    }, 1000);
  }

  // One batched canvas update for every dirty component. Parked
  // while the subview is hidden (subview enter calls flushLive()
  // directly to catch up).
  function flushLive() {
    if (!liveEnabled || !nodesDS || liveDirty.size === 0) return;
    if (!topologyVisible()) return;
    const nodeUpdates = [];
    let linesChanged = false;
    for (const id of liveDirty) {
      const c = componentById.get(id);
      if (!c) continue;
      const line = liveLabelLine({ category: c.category, ...liveEntry(id) });
      const label = line == null ? shortLabel(c.name) : `${shortLabel(c.name)}\n${line}`;
      const prev = nodesDS.get(id);
      if (prev && prev.label !== label) {
        if ((prev.label.includes("\n")) !== (label.includes("\n"))) linesChanged = true;
        nodeUpdates.push({ id, label });
      }
    }
    liveDirty.clear();
    if (nodeUpdates.length) nodesDS.update(nodeUpdates);
    // A node gaining/losing its second line changes its height —
    // re-measure so the tidy layout keeps its spacing.
    if (linesChanged && !manualArrangement) pendingMeasuredRelayout = true;
  }
```

In the public API object (next to `applySample`-style methods like `resetNotify`):

```js
    /// Live-overlay feed: one WS sample. Cheap — records the value,
    /// marks the id dirty, and arms the 1 Hz flush.
    applySample(ev) {
      if (!liveEnabled) return;
      const e = liveEntry(ev.id);
      if (ev.metric === "active_power_w") e.p = ev.value;
      else if (ev.metric === "reactive_power_var") e.q = ev.value;
      else if (ev.metric === "soc_pct") e.soc = ev.value;
      else if (
        ev.metric === "active_power_lower_bound_w" ||
        ev.metric === "active_power_upper_bound_w"
      ) {
        maxAbsBoundW = Math.max(maxAbsBoundW, Math.abs(ev.value));
        return; // bounds feed the scale reference only
      } else {
        return;
      }
      liveDirty.add(ev.id);
      armLiveFlush();
    },
    /// Apply pending live updates now — subview enter calls this so
    /// a hidden tab's accumulated samples land immediately.
    flushLive,
    /// Smoke-test hook: every node label currently on the canvas.
    debugLiveLabels() {
      return nodesDS ? nodesDS.get().map((n) => n.label) : [];
    },
```

In `apply()` (the topology-refresh entry), after `buildVisData` has repopulated `componentById`, prune and reset:

```js
    // Live overlay: forget components that left the topology, mark
    // every survivor dirty so the next flush rebuilds its label
    // (names/categories may have changed), and reset the flow
    // scale reference (rated bounds may have changed too).
    for (const id of [...liveValues.keys()]) {
      if (!componentById.has(id)) liveValues.delete(id);
      else liveDirty.add(id);
    }
    maxAbsBoundW = 0;
    if (liveDirty.size) armLiveFlush();
```

Add the import at the top of topology.js:

```js
import { edgeFlow, liveLabelLine } from "./live.js";
```

(`edgeFlow` is used in Task 4; importing both now avoids touching the line twice.)

- [ ] **Step 4: Wire the WS feed (repl.js) and subview catch-up (routing.js)**

`ui-assets/repl.js`, in the `ev.kind === "sample"` block after `gridFrequency.applySample(ev)`:

```js
        topology.applySample(ev);
```

with the import added to repl.js's import block:

```js
import { topology } from "./topology.js";
```

`ui-assets/routing.js`, in `applyMode`'s topology-enter branch (the one that calls `requestAnimationFrame(() => topology.fit())`):

```js
    topology.flushLive();
```

- [ ] **Step 5: Run the smoke script to verify it passes**

Run: `node --check ui-assets/topology.js && node tools/ui-smoke/live-topology.mjs`
Expected: unit checks and both `e2e:` label checks PASS, no page errors.

- [ ] **Step 6: Biome + commit**

```bash
npx @biomejs/biome check ui-assets/topology.js ui-assets/repl.js ui-assets/routing.js tools/ui-smoke/live-topology.mjs
git add ui-assets/topology.js ui-assets/repl.js ui-assets/routing.js tools/ui-smoke/live-topology.mjs
git commit -m "Show live power / reactive / SoC on topology nodes

Nodes gain a second label line fed from the existing 1 Hz WS sample
broadcast: an id-to-values map marks dirty ids and one batched
nodesDS.update per second rewrites only changed labels. The flush
parks while the subview is hidden (subview enter catches up), a
topology refresh prunes departed components, and a node gaining or
losing its line re-arms the measured relayout so spacing stays
right.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 4: Edge flow chevrons

**Files:**
- Modify: `ui-assets/topology.js` (extend `flushLive`; edges are `{ id: "${parent}-${child}", from, to, arrows: "to" }` from `buildVisData`)
- Modify: `tools/ui-smoke/live-topology.mjs` (append chevron assertions)

**Interfaces:**
- Consumes: `edgeFlow(childPowerW, parentCount, siteMaxRatedW)` from Task 1 (already imported in Task 3); `liveValues`, `maxAbsBoundW` from Task 3.
- Produces: edge DataSet entries gain `width` and `arrows: { to: {...}, middle: {...} }`; smoke hook `topology.debugLiveEdges()`.

- [ ] **Step 1: Extend the smoke script with failing chevron assertions**

Append before the `check("no page errors", ...)` line:

```js
// ── e2e: flow chevrons ────────────────────────────────────────────
const edges = await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  return topology.debugLiveEdges();
});
const withChevron = edges.filter((e) => e.middleEnabled);
check("e2e: some edge has a flow chevron", withChevron.length > 0, JSON.stringify(edges));
// berlin demo: PV generates (negative) → its edge chevron points
// toward the parent (negative scaleFactor).
check("e2e: an exporting edge points at the parent", withChevron.some((e) => e.scaleFactor < 0), JSON.stringify(withChevron));
check("e2e: chevron widths clamped", withChevron.every((e) => e.width >= 1 && e.width <= 6), JSON.stringify(withChevron));
```

- [ ] **Step 2: Run to verify it fails**

Run: `node tools/ui-smoke/live-topology.mjs`
Expected: chevron checks FAIL (`debugLiveEdges` missing).

- [ ] **Step 3: Implement edge updates in `flushLive`**

In topology.js, extend `flushLive()` — after the node-updates loop, before `liveDirty.clear()` build edge updates; the parent-count map comes from the current edge set:

```js
    // Edge flow: recompute every edge that touches a dirty child.
    // Parent counts come from the live edge set (parallel paths
    // split the child's flow, matching the meter aggregation rule).
    const edgeUpdates = [];
    if (edgesDS) {
      const parentCount = new Map();
      for (const e of edgesDS.get()) {
        parentCount.set(e.to, (parentCount.get(e.to) || 0) + 1);
      }
      for (const e of edgesDS.get()) {
        if (!liveDirty.has(e.to)) continue;
        const child = liveValues.get(e.to);
        const flow = edgeFlow(child ? child.p : null, parentCount.get(e.to) || 1, maxAbsBoundW);
        edgeUpdates.push({
          id: e.id,
          width: flow.chevron ? flow.width : 1.5,
          arrows: {
            to: { enabled: true, scaleFactor: 0.7 },
            middle: flow.chevron
              ? {
                  enabled: true,
                  type: "arrow",
                  // Negative flips the chevron toward the parent —
                  // physical flow for export/generation.
                  scaleFactor: (flow.towardParent ? -1 : 1) * flow.scale,
                }
              : { enabled: false },
          },
          color: flow.chevron ? { color: "#79b8ff", inherit: false } : { color: "#6b7280", inherit: false },
        });
      }
    }
```

and after the existing `if (nodeUpdates.length) nodesDS.update(nodeUpdates);`:

```js
    if (edgeUpdates.length) edgesDS.update(edgeUpdates);
```

Add the smoke hook to the public API next to `debugLiveLabels`:

```js
    debugLiveEdges() {
      if (!edgesDS) return [];
      return edgesDS.get().map((e) => ({
        id: e.id,
        width: e.width ?? 1.5,
        middleEnabled: Boolean(e.arrows && e.arrows.middle && e.arrows.middle.enabled),
        scaleFactor: e.arrows && e.arrows.middle && e.arrows.middle.enabled ? e.arrows.middle.scaleFactor : 0,
      }));
    },
```

Note: `arrows: "to"` (the string form set in `buildVisData`) and the object form coexist — the update replaces the whole `arrows` value with the object form, keeping the structural end arrowhead enabled. Hidden (dashed) edges keep `dashes: true` since updates merge by field.

- [ ] **Step 4: Run the smoke script to verify it passes**

Run: `node tools/ui-smoke/live-topology.mjs`
Expected: all unit + label + chevron checks PASS. Also eyeball one screenshot:

```js
// optional visual: await page.screenshot({ path: "live-topology.png" })
```

- [ ] **Step 5: Biome + commit**

```bash
npx @biomejs/biome check ui-assets/topology.js tools/ui-smoke/live-topology.mjs
git add ui-assets/topology.js tools/ui-smoke/live-topology.mjs
git commit -m "Draw physical power flow as mid-edge chevrons

Each edge touching a dirty child gets a middle arrow whose
direction follows the physical flow (export points at the parent,
via a negative scaleFactor) and whose size/width scale with
sqrt(|flow| / max-rated-bound); below the dead band the chevron
disappears so dead legs read as dead. The structural end arrowhead
is untouched, so wiring and flow stay separate visual channels.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 5: The `live` toggle — pill, persistence, revert, help text

**Files:**
- Modify: `ui-assets/index.html` (`#topology-controls` strip, ~line 274; help dialog list item about the topology canvas)
- Modify: `ui-assets/app.js` (`setupCanvasControls`, ~line 110)
- Modify: `ui-assets/topology.js` (public `setLive(on)`; `liveEnabled` init from localStorage)
- Modify: `tools/ui-smoke/live-topology.mjs` (toggle assertions)

**Interfaces:**
- Consumes: everything from Tasks 3-4.
- Produces: `topology.setLive(on: boolean)`; `topology.liveOn() -> boolean`; localStorage `switchyard-topology-live` (`"0"` = off, anything else/absent = on).

- [ ] **Step 1: Extend the smoke script with failing toggle assertions**

Append before the `check("no page errors", ...)` line:

```js
// ── e2e: live toggle ──────────────────────────────────────────────
await page.click("#topology-controls .live-btn");
await new Promise((r) => setTimeout(r, 500));
const off = await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  return { labels: topology.debugLiveLabels(), edges: topology.debugLiveEdges(), on: topology.liveOn() };
});
check("e2e: toggle off clears label lines", off.labels.every((l) => !l.includes("\n")), JSON.stringify(off.labels));
check("e2e: toggle off clears chevrons", off.edges.every((e) => !e.middleEnabled));
check("e2e: liveOn() reports off", off.on === false);
await page.reload({ waitUntil: "networkidle" });
await page.click(".mglist-card:not(.mglist-new)").catch(() => {});
await new Promise((r) => setTimeout(r, 1500));
const persisted = await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  return topology.liveOn();
});
check("e2e: off state survives reload", persisted === false);
await page.evaluate(() => localStorage.removeItem("switchyard-topology-live"));
```

- [ ] **Step 2: Run to verify it fails**

Run: `node tools/ui-smoke/live-topology.mjs`
Expected: toggle checks FAIL (no `.live-btn`, click times out → script fails).

- [ ] **Step 3: Implement**

`ui-assets/index.html`, inside `#topology-controls` after the snap button:

```html
        <span class="ctl-label">values</span>
        <button type="button" class="pill live-btn active"
                title="Show live power / SoC on nodes and flow chevrons on edges">live</button>
```

Help dialog: extend the Topology bullet that describes the canvas (the one mentioning the ＋ Add palette) with:

```html
          <li><strong>Live values</strong> — nodes show current power (and reactive / SoC), edges a chevron in the direction power actually flows, sized by magnitude. The <em>live</em> pill (top right) toggles it.</li>
```

`ui-assets/topology.js` — init from storage and the public setter. Change the Task 3 declaration to:

```js
  let liveEnabled = localStorage.getItem("switchyard-topology-live") !== "0";
```

Public API additions:

```js
    /// The live-overlay toggle. Off reverts every label to its
    /// structural one-liner and strips the chevrons in one bulk
    /// update, then re-measures the (now shorter) nodes.
    setLive(on) {
      liveEnabled = Boolean(on);
      if (liveEnabled) {
        localStorage.removeItem("switchyard-topology-live");
        for (const id of liveValues.keys()) liveDirty.add(id);
        flushLive();
      } else {
        localStorage.setItem("switchyard-topology-live", "0");
        liveDirty.clear();
        if (nodesDS) {
          nodesDS.update(
            nodesDS.get().map((n) => {
              const c = componentById.get(n.id);
              return { id: n.id, label: c ? shortLabel(c.name) : n.label.split("\n")[0] };
            }),
          );
        }
        if (edgesDS) {
          edgesDS.update(
            edgesDS.get().map((e) => ({
              id: e.id,
              width: 1.5,
              arrows: { to: { enabled: true, scaleFactor: 0.7 }, middle: { enabled: false } },
            })),
          );
        }
      }
      if (!manualArrangement) {
        pendingMeasuredRelayout = true;
        if (network) network.redraw();
      }
    },
    liveOn() {
      return liveEnabled;
    },
```

`ui-assets/app.js`, in `setupCanvasControls`'s click handler after the snap branch:

```js
    const liveBtn = ev.target.closest(".live-btn");
    if (liveBtn && canvas.setLive) {
      liveBtn.classList.toggle("active");
      canvas.setLive(liveBtn.classList.contains("active"));
    }
```

And in `app.js` init (near `setupCanvasControls` calls), sync the pill to the persisted state:

```js
  const liveBtn = document.querySelector("#topology-controls .live-btn");
  if (liveBtn) liveBtn.classList.toggle("active", topology.liveOn());
```

(The formulas strip has no `.live-btn`, so the shared handler is a no-op there; `canvas.setLive` guard keeps the formulas canvas safe even if one is added by mistake.)

- [ ] **Step 4: Run the smoke script to verify everything passes**

Run: `node tools/ui-smoke/live-topology.mjs`
Expected: every unit and e2e check PASS, `ALL PASS`, exit 0.

- [ ] **Step 5: Biome + commit**

```bash
npx @biomejs/biome check ui-assets/app.js ui-assets/topology.js tools/ui-smoke/live-topology.mjs
git add ui-assets/index.html ui-assets/app.js ui-assets/topology.js tools/ui-smoke/live-topology.mjs
git commit -m "Add the persisted live toggle for the topology overlay

A live pill beside the layout picker (default on, sticky via
localStorage) turns the value lines and flow chevrons off in one
bulk revert — the canvas without them is exactly the pre-overlay
canvas. Toggling re-measures node sizes so the layout spacing is
right in both modes.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 6: Full verification pass

**Files:**
- No new files; fixes go into the task that owns them (as fixups).

- [ ] **Step 1: Full smoke run against a fresh server**

Restart the test server (commands in Global Constraints) so the e2e runs against a clean state, then:

```bash
node tools/ui-smoke/live-topology.mjs
```

Expected: `ALL PASS`.

- [ ] **Step 2: Whole-repo checks**

```bash
S=/tmp/claude-1000/-vagrant/b49b8d6e-6a43-4841-85d2-96946765d29c/scratchpad
export PROTOC=$S/protoc/bin/protoc
npx @biomejs/biome check ui-assets/*.js tools/ui-smoke/*.mjs
cargo test 2>&1 | tee $S/live-topo-tests.log | grep -c "test result: ok"   # expect 10
git status --short    # clean, bar untracked scratch
```

Expected: biome clean, 10/10 suites (nothing Rust changed — this is the no-server-change constraint check), tree clean.

- [ ] **Step 3: Visual confirmation screenshot**

Take one screenshot with live on at 1600×950 (berlin demo, topology subview) and read it: labels legible, chevrons on the powered legs pointing the right way (PV edge chevron toward its meter), no overlap regressions.

- [ ] **Step 4: Done**

Report completion; pr-prep runs as its own step when the user asks.
