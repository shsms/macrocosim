# Metrics Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Dashboard subview with a floating metrics panel on the Topology canvas — three foldable cards with combined uPlot charts, chip toggles carrying live values, per-source PF from two new reactive streams — and delete the Dashboard.

**Architecture:** A DOM-free `metrics-store.js` inherits dashboard.js's 900-slot ring store and grows subscriber notification + windowed series reads; `metrics-panel.js` renders the panel (shell tenant `metrics-btn`) with uPlot charts. The server adds `pv_reactive_power` / `battery_reactive_power` forwarders. Then the Dashboard subview, `dashboard.js`, and `formulas.js` are demolished and routing/e2e updated.

**Tech Stack:** Rust/axum + frequenz-microgrid 0.6 (server), vanilla ES modules + vendored uPlot (client), node tools tests, biome.

**Spec:** `docs/superpowers/specs/2026-08-27-metrics-panel-design.md` — read it first; it argues every decision below.

## Global Constraints

- Commit style: short imperative subject, body prose, then exactly `Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>`. NO Co-Authored-By / AI trailers.
- `git add` files explicitly by name — never `-A`, `.`, `-u`, or `commit -a`. Never add `.nfs*` files.
- Tee test output to a scratch file and grep the file, never a bare pipe (e.g. `cargo test 2>&1 | tee /tmp/claude-1000/-vagrant/*/scratchpad/t.log`).
- Rust gates: `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `cargo test`.
- JS gates: `npx @biomejs/biome check ui-assets` (tolerated baseline: index.html a11y warnings, topology.js:270, ~26 style.css warnings — no NEW warnings), `node tools/formula-ast-test.mjs`, `node tools/boot-smoke.mjs`, plus the new `node tools/metrics-store-test.mjs` from Task 2 on.
- Biome formatting: 2-space indent, double quotes, trailing commas — run `npx @biomejs/biome check --write ui-assets/<file>` before committing JS.
- Comment style: explain constraints and why, matching each file's existing density. No change-narration comments.
- Never push, never open PRs.
- A commit message body line must never START with `#` (rebase strips it) — keep `#N` refs mid-line.

---

### Task 1: Server — per-source reactive streams

**Files:**
- Modify: `src/ui/loopback.rs` (~line 255–266, the grid-reactive block inside `subscribe_power_forwarders`)
- Test: `tests/ui_http.rs` (new test + two assertions in `grid_reactive_formula_converges_over_a_site_with_an_ev_charger`)

**Interfaces:**
- Produces: `microgrid_sample` streams `pv_reactive_power` and `battery_reactive_power` (quantity `"ReactivePower"`, unit `"var"`), riding `/api/mg/{id}/microgrid/latest`, `/api/mg/{id}/microgrid/history`, and the WS — Task 3's client reads them by these exact names.

- [ ] **Step 1: Write the failing e2e test**

Append to `tests/ui_http.rs` (model: the existing `grid_reactive_formula_converges_over_a_site_with_an_ev_charger` at line ~260; reuse its `json` helper and poll shape):

```rust
/// The metrics panel's Reactive card charts per-source Q: grid, PV,
/// battery — the same logical-meter formulas as the power streams,
/// metric AcPowerReactive. This proves both new streams end to end
/// over a site that has both categories, and that neither grows a
/// varh companion (reactive-energy integration stays out of scope,
/// as with grid Q).
#[tokio::test(flavor = "multi_thread")]
async fn per_source_reactive_streams_converge() {
    let topology = r#"
(%make-grid-connection-point :id 1
    :successors
    (list (%make-meter :id 2
                        :successors
                        (list (%make-battery-inverter :id 3
                                                        :successors
                                                        (list (%make-battery :id 4)))))
          (%make-meter :id 5
                       :successors
                       (list (%make-solar-inverter :id 6)))))
"#;
    let s = TestServer::start(topology).await;
    let client = reqwest::Client::new();

    let mgs = json(&client, format!("{}/api/microgrids", s.ui_url)).await;
    let id = mgs
        .as_array()
        .expect("microgrids array")
        .first()
        .expect("one microgrid")["id"]
        .as_u64()
        .expect("microgrid id");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let snapshot = loop {
        let body = json(
            &client,
            format!("{}/api/mg/{id}/microgrid/latest", s.ui_url),
        )
        .await;
        let converged = ["pv_reactive_power", "battery_reactive_power"]
            .iter()
            .all(|s| body[*s]["value"].as_f64().is_some_and(|v| v.is_finite()));
        if converged || tokio::time::Instant::now() >= deadline {
            break body;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    for stream in ["pv_reactive_power", "battery_reactive_power"] {
        let entry = &snapshot[stream];
        entry["value"]
            .as_f64()
            .filter(|v| v.is_finite())
            .unwrap_or_else(|| panic!("{stream} never converged: {snapshot}"));
        assert_eq!(entry["quantity"], "ReactivePower", "{stream}: {snapshot}");
        assert_eq!(entry["unit"], "var", "{stream}: {snapshot}");
    }
    assert!(
        snapshot.get("pv_reactive_energy").is_none()
            && snapshot.get("battery_reactive_energy").is_none(),
        "unexpected varh stream: {snapshot}"
    );
}
```

And in the existing `grid_reactive_formula_converges_over_a_site_with_an_ev_charger`, after the varh assertion, add (its topology has no PV, so the PV formula errors at forwarder-build time and the stream is never spawned — the key can never appear):

```rust
    // No PV in this site: the pv reactive formula fails at build and
    // its forwarder is skipped, so the stream never appears.
    assert!(
        snapshot.get("pv_reactive_power").is_none(),
        "unexpected pv_reactive_power stream: {snapshot}"
    );
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test ui_http per_source_reactive 2>&1 | tee "$SCRATCH/t1.log"; grep -E "test result|panicked" "$SCRATCH/t1.log"` (set `SCRATCH=/tmp/claude-1000/-vagrant/d9ffc5a7-5e1d-4700-94e1-d5c9f875c63a/scratchpad`)
Expected: FAIL — `pv_reactive_power never converged` (the streams don't exist yet).

- [ ] **Step 3: Add the two forwarders**

In `src/ui/loopback.rs subscribe_power_forwarders`, directly after the existing `grid_reactive_power` block (`if let Some(h) = subscribe_reactive_forwarder(...) { handles.push(h); }`) and before the frequency comment, replace the stale "Only the grid formula" comment above the grid block and add:

```rust
    // Per-source Q for the metrics panel's Reactive card: the same
    // logical-meter formulas as the power streams, metric
    // AcPowerReactive. energy_stream_for maps none of them — varh
    // accumulation stays out of scope, as with grid Q.
    for (stream, formula) in [
        (
            "pv_reactive_power",
            lm.pv::<metric::AcPowerReactive>(None),
        ),
        (
            "battery_reactive_power",
            lm.battery::<metric::AcPowerReactive>(None),
        ),
    ] {
        if let Some(h) = subscribe_reactive_forwarder(stream, formula, site, state.clone()).await {
            handles.push(h);
        }
    }
```

The old comment `// Only the grid formula — no consumer/producer/pv reactive streams (spec: one site tile).` is now false — replace it with `// Site Q at the connection point.` above the grid block. Check `lm.battery`'s exact signature in `~/.cargo/registry/src/*/frequenz-microgrid-0.6.0/src/logical_meter/logical_meter_handle.rs:87` — it takes `Option<Vec<u64>>` like `pv`; if `cargo check` disagrees, match what the handle declares.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test ui_http reactive 2>&1 | tee "$SCRATCH/t1b.log"; grep -E "test result" "$SCRATCH/t1b.log"`
Expected: both reactive tests PASS. Then the full gate: `cargo clippy --all-targets -- -D warnings && cargo fmt --check && cargo test 2>&1 | tee "$SCRATCH/t1c.log"; tail -5 "$SCRATCH/t1c.log"`.

- [ ] **Step 5: Commit**

```bash
git add src/ui/loopback.rs tests/ui_http.rs
git commit -m "Stream per-source reactive power from the loopback" -m "The metrics panel charts grid, PV, and battery reactive power as
separate series. Reuse the reactive forwarder with the pv and battery
logical-meter formulas; no varh companions, matching grid Q.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 2: `metrics-store.js` — DOM-free sample store + PF helpers

**Files:**
- Create: `ui-assets/metrics-store.js`
- Create: `tools/metrics-store-test.mjs`
- Modify: `.github/workflows/ci.yml` (ui-lint job: add `node tools/metrics-store-test.mjs` beside the other node gates)
- Modify: `AGENTS.md` (the JS gate list: add the new test command)

**Interfaces:**
- Consumes: `mgPath` from `./app.js` (same import dashboard.js uses today), `isPanelOpen` from `./side-panel.js`.
- Produces (Task 3 + 4 rely on these exact names):
  - `metricsStore.applySample(ev)` — ev is a `microgrid_sample` WS frame `{stream, quantity, unit, ts_ms, value}`.
  - `metricsStore.backfill()` — async; refills rings from `/microgrid/history`, reseeds latest, notifies.
  - `metricsStore.startAutoReseed(periodMs = 5000)` / `stopAutoReseed()` — 5 s latest-reseed poll + visibilitychange, self-gated on `isPanelOpen("metrics-btn")`.
  - `metricsStore.resetStream(stream)` — clears one ring (gridFrequency's re-backfill hook).
  - `metricsStore.latest(stream)` → `{value, quantity, unit}` or `null`.
  - `metricsStore.series(stream, windowS)` → `{xs: number[], ys: (number|null)[]}` — last `windowS` ring slots, xs in epoch seconds synthesized at 1 Hz ending now, NaN slots as null.
  - `metricsStore.subscribe(cb)` → unsubscribe function; cb fires (no args) after any sample/backfill/reseed lands.
  - `fmtValue(quantity, unit, value)` → display string (dashboard.js's `fmt`, moved).
  - `pfValue(p, q)` → number|null; `pfText(p, q)` → `"PF 0.98 lag"` / `"PF 1.00"` / `"PF —"`.
- Constructor note: the module is DOM-free except `document.addEventListener("visibilitychange", …)` inside startAutoReseed and the `fetch` calls — the node test only imports the pure parts (see test's import shim).

- [ ] **Step 1: Write the failing node test**

`tools/metrics-store-test.mjs` (model: `tools/formula-ast-test.mjs` — plain asserts, `process.exit(1)` on failure). The store module reaches `fetch`/`document` only inside functions, so importing it under node needs the same minimal shim trick boot-smoke uses; keep it lighter — stub globals before the dynamic import:

```js
// Unit tests for the metrics store's pure surface: ring push/read
// windowing, PF derivation, and display formatting. DOM/fetch shims
// are inert — the tested paths never call them.
import assert from "node:assert/strict";

globalThis.document = {
  addEventListener() {},
  removeEventListener() {},
  hidden: false,
};
globalThis.localStorage = { getItem: () => null, setItem() {} };
globalThis.window = globalThis;

const { metricsStore, fmtValue, pfValue, pfText } = await import(
  new URL("../ui-assets/metrics-store.js", import.meta.url)
);

// ── pf helpers ──────────────────────────────────────────────────
assert.equal(pfValue(null, 5), null);
assert.equal(pfValue(0, 0), null);
assert.ok(Math.abs(pfValue(100, 0) - 1) < 1e-9);
assert.ok(Math.abs(pfValue(30, 40) - 0.6) < 1e-9);
// Same signs → lagging, opposite → leading, unity (>= 0.995) drops
// the qualifier — the site-PF rule the old dashboard tile used.
assert.equal(pfText(30, 40), "PF 0.60 lag");
assert.equal(pfText(30, -40), "PF 0.60 lead");
assert.equal(pfText(100, 1), "PF 1.00");
assert.equal(pfText(null, 40), "PF —");

// ── fmtValue ────────────────────────────────────────────────────
assert.equal(fmtValue("Power", "W", 1_234_000), "1.23 MW");
assert.equal(fmtValue("ReactivePower", "var", -1234), "-1.23 kVAr");
assert.equal(fmtValue("Frequency", "Hz", 50.0171), "50.02 Hz");
assert.equal(fmtValue("Power", "W", null), "—");

// ── ring + series windowing ─────────────────────────────────────
const t0 = 1_700_000_000_000;
for (let i = 0; i < 10; i++) {
  metricsStore.applySample({
    stream: "grid_power",
    quantity: "Power",
    unit: "W",
    ts_ms: t0 + i * 1000,
    value: i,
  });
}
assert.deepEqual(metricsStore.latest("grid_power"), {
  value: 9,
  quantity: "Power",
  unit: "W",
});
const s5 = metricsStore.series("grid_power", 5);
assert.equal(s5.xs.length, 5);
assert.equal(s5.ys.length, 5);
// Newest-last, values 5..9; older slots fell outside the window.
assert.deepEqual(s5.ys, [5, 6, 7, 8, 9]);
// xs are 1 Hz apart, monotonically increasing.
for (let i = 1; i < s5.xs.length; i++) {
  assert.ok(Math.abs(s5.xs[i] - s5.xs[i - 1] - 1) < 1e-9);
}
// A null-value sample lands as a null gap.
metricsStore.applySample({
  stream: "grid_power",
  quantity: "Power",
  unit: "W",
  ts_ms: t0 + 10_000,
  value: null,
});
const s3 = metricsStore.series("grid_power", 3);
assert.deepEqual(s3.ys, [8, 9, null]);
// A window wider than the ring's fill pads the front with nulls.
const wide = metricsStore.series("grid_power", 20);
assert.equal(wide.ys.length, 20);
assert.equal(wide.ys[0], null);
// resetStream empties the ring but keeps no stale latest ghost.
metricsStore.resetStream("grid_power");
assert.deepEqual(metricsStore.series("grid_power", 5).ys, [null, null, null, null, null]);

// ── subscribe ───────────────────────────────────────────────────
let fired = 0;
const un = metricsStore.subscribe(() => {
  fired++;
});
metricsStore.applySample({
  stream: "pv_power",
  quantity: "Power",
  unit: "W",
  ts_ms: t0,
  value: 1,
});
assert.equal(fired, 1);
un();
metricsStore.applySample({
  stream: "pv_power",
  quantity: "Power",
  unit: "W",
  ts_ms: t0 + 1000,
  value: 2,
});
assert.equal(fired, 1);

console.log("metrics-store-test: all assertions passed");
```

- [ ] **Step 2: Run it to verify it fails**

Run: `node tools/metrics-store-test.mjs`
Expected: FAIL — cannot find `../ui-assets/metrics-store.js`.

- [ ] **Step 3: Write the store**

`ui-assets/metrics-store.js`. This is dashboard.js's IIFE store (lines 62–317) reshaped: DOM painting removed, `latest` map + subscriber list added. Port the code, don't rewrite it — comments included, updated where the tile references die:

```js
// The metrics panel's sample store: one 900-slot × 1 Hz ring per
// microgrid_sample stream (15 min — the server history cap), plus
// the latest sample per stream and a subscriber list the panel
// renderer hangs off. DOM-free: the renderer (metrics-panel.js)
// subscribes and reads; nothing here touches elements.

import { mgPath } from "./app.js";
import { isPanelOpen } from "./side-panel.js";

const PANEL = "metrics-btn";
const SPARK_LEN = 900;

// Power auto-scale: W → kW → MW etc. via the same ladder the rest of
// the app reads in (live.js formatScaled, inlined here to keep this
// module import-light for the node unit tests). The wire unit for
// reactive power is SI "var"; every readout spells it "VAr".
export function fmtValue(quantity, unit, value) {
  if (value == null || !Number.isFinite(value)) return "—";
  const shown = unit === "var" ? "VAr" : unit;
  if (quantity === "Power" || quantity === "ReactivePower" || unit === "W" || unit === "var") {
    const a = Math.abs(value);
    if (a >= 1e6) return `${(value / 1e6).toFixed(2)} M${shown}`;
    if (a >= 1e3) return `${(value / 1e3).toFixed(2)} k${shown}`;
    return `${value.toFixed(1)} ${shown}`;
  }
  return `${value.toFixed(2)} ${shown}`;
}

// Power factor from matching P and Q samples: |P| / hypot(P, Q).
// null when either side is missing or both are zero (PF undefined).
export function pfValue(p, q) {
  if (!Number.isFinite(p) || !Number.isFinite(q) || (p === 0 && q === 0)) return null;
  return Math.abs(p) / Math.hypot(p, q);
}

// Chip readout. Sign convention as the old site-PF tile: opposite
// signs on P and Q read as leading, same as lagging, and the
// qualifier drops once PF rounds to unity (>= 0.995) so a clean
// reading doesn't flicker between the two on noise.
export function pfText(p, q) {
  const pf = pfValue(p, q);
  if (pf == null) return "PF —";
  const tag = pf >= 0.995 ? "" : p * q < 0 ? " lead" : " lag";
  return `PF ${pf.toFixed(2)}${tag}`;
}

export const metricsStore = (() => {
  const sparkBuf = new Map(); // stream -> { values: Float32Array, cursor }
  const latestMap = new Map(); // stream -> { value, quantity, unit }
  const listeners = new Set();
  let reseedTimer = null;
  let reseedVisHandler = null;

  function buf(stream) {
    let b = sparkBuf.get(stream);
    if (!b) {
      b = { values: new Float32Array(SPARK_LEN).fill(NaN), cursor: 0 };
      sparkBuf.set(stream, b);
    }
    return b;
  }
  function notify() {
    for (const cb of listeners) cb();
  }

  return {
    subscribe(cb) {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    latest(stream) {
      return latestMap.get(stream) ?? null;
    },
    // Last `windowS` slots, oldest first, xs synthesized at 1 Hz
    // ending now — the ring is positional (one slot per second), so
    // per-slot timestamps are honest without storing them. NaN slots
    // (no sample) come back as null, which uPlot renders as a gap.
    series(stream, windowS) {
      const b = buf(stream);
      const n = Math.min(windowS, SPARK_LEN);
      const now = Date.now() / 1000;
      const xs = new Array(n);
      const ys = new Array(n);
      for (let i = 0; i < n; i++) {
        const v = b.values[(b.cursor - n + i + SPARK_LEN * 2) % SPARK_LEN];
        xs[i] = now - (n - 1 - i);
        ys[i] = Number.isNaN(v) ? null : v;
      }
      return { xs, ys };
    },
    applySample(ev) {
      const b = buf(ev.stream);
      b.values[b.cursor] = ev.value == null ? NaN : ev.value;
      b.cursor = (b.cursor + 1) % SPARK_LEN;
      latestMap.set(ev.stream, {
        value: ev.value ?? null,
        quantity: ev.quantity,
        unit: ev.unit,
      });
      notify();
    },
    // Clear one stream's ring (the grid-frequency feeder re-backfills
    // from outside the /microgrid/history map; without the reset each
    // panel re-open would append the same history again). The latest
    // entry stays — the value is still the latest known.
    resetStream(stream) {
      const b = buf(stream);
      b.values.fill(NaN);
      b.cursor = 0;
    },
    async backfill() {
      // Past 15 min per stream, server-side, so charts show the
      // trend immediately on panel open instead of growing from
      // empty. Best-effort: a 503 mid-rebuild leaves the old rings;
      // WS frames fill forward from here.
      try {
        const hres = await fetch(mgPath("microgrid/history"));
        if (hres.ok) {
          const hmap = await hres.json();
          for (const [stream, samples] of Object.entries(hmap)) {
            const b = buf(stream);
            b.values.fill(NaN);
            const slice = samples.slice(-SPARK_LEN);
            const start = SPARK_LEN - slice.length;
            for (let i = 0; i < slice.length; i++) {
              const v = slice[i]?.value;
              b.values[start + i] = v == null ? NaN : v;
            }
            b.cursor = 0;
          }
        }
      } catch (_) {
        // Loopback not up yet — the reseed below may still land.
      }
      await this.reseedLatest();
      notify();
    },
    // Value-only refresh from the server's cached latest sample —
    // the WS Sample stream drops frames on lag and a backgrounded
    // tab throttles its receiver, so chips could otherwise freeze on
    // a stale number. No ring push: the ring stays aligned to the
    // WS/backfill sample flow.
    async reseedLatest() {
      try {
        const res = await fetch(mgPath("microgrid/latest"));
        if (!res.ok) return;
        const map = await res.json();
        for (const [stream, snap] of Object.entries(map)) {
          latestMap.set(stream, {
            value: snap.value ?? null,
            quantity: snap.quantity,
            unit: snap.unit,
          });
        }
        notify();
      } catch (_) {
        // Best-effort.
      }
    },
    // Safety net against dropped WS frames while the panel is open:
    // slow-poll the latest snapshot, and refresh immediately when
    // the tab returns to the foreground. Idempotent — a second call
    // replaces the timer instead of stacking one.
    startAutoReseed(periodMs = 5000) {
      this.stopAutoReseed();
      reseedTimer = setInterval(() => {
        if (isPanelOpen(PANEL)) this.reseedLatest();
      }, periodMs);
      reseedVisHandler = () => {
        if (!document.hidden && isPanelOpen(PANEL)) this.reseedLatest();
      };
      document.addEventListener("visibilitychange", reseedVisHandler);
    },
    stopAutoReseed() {
      if (reseedTimer !== null) {
        clearInterval(reseedTimer);
        reseedTimer = null;
      }
      if (reseedVisHandler !== null) {
        document.removeEventListener("visibilitychange", reseedVisHandler);
        reseedVisHandler = null;
      }
    },
  };
})();
```

Import-cycle note: `app.js` exports `mgPath`?? — no: `mgPath` is exported from `routing.js` and re-exported through `app.js` (dashboard.js imports it from `./app.js` today; formulas.js from `./app.js` too). Import it exactly as dashboard.js does: `import { mgPath } from "./app.js";` — if boot-smoke flags a cycle (app.js will import metrics-store via the panel), switch to `import { mgPath } from "./routing.js";` which routing exports directly (check `grep -n "export" ui-assets/routing.js` — `mgPath` lives there). Prefer the routing.js import from the start: the store must be importable by the node test with minimal graph.

**Correction to the code above:** use `import { mgPath } from "./routing.js";` (verify with `grep -n "export function mgPath\|export const mgPath" ui-assets/routing.js ui-assets/app.js` and import from whichever file DEFINES it).

The node test imports the module while `fetch` etc. are unused — but the top-level imports (`routing.js`, `side-panel.js`) pull their module graphs. If that graph breaks under node, re-export the pure helpers so the test can import them without the store: keep `fmtValue`/`pfValue`/`pfText` free of any dependence on the imports, and if `await import` of the full module fails under the shims, extend the shims minimally (boot-smoke.mjs shows the Proxy-DOM trick) rather than splitting the module.

- [ ] **Step 4: Run the test to verify it passes**

Run: `node tools/metrics-store-test.mjs`
Expected: `metrics-store-test: all assertions passed`. Also `npx @biomejs/biome check ui-assets tools/metrics-store-test.mjs` — no new warnings.

- [ ] **Step 5: Wire the CI gate + AGENTS.md**

`.github/workflows/ci.yml`, ui-lint job: add `- run: node tools/metrics-store-test.mjs` directly after the `node tools/formula-ast-test.mjs` step. `AGENTS.md`: the sentence listing the node gates gains the same command.

- [ ] **Step 6: Commit**

```bash
git add ui-assets/metrics-store.js tools/metrics-store-test.mjs .github/workflows/ci.yml AGENTS.md
git commit -m "Add the metrics-panel sample store" -m "dashboard.js's ring store, reshaped DOM-free: 900-slot rings,
latest-sample map, subscriber notification, and windowed series
reads for uPlot, plus per-source PF helpers. Node-tested in
tools/metrics-store-test.mjs and wired into the CI ui-lint job.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 3: `metrics-panel.js` — the panel, both pills in the controls bar

**Files:**
- Create: `ui-assets/metrics-panel.js`
- Modify: `ui-assets/index.html` (controls bar: `panels` group; header: remove `#formula-btn`)
- Modify: `ui-assets/style.css` (panel cards/chips/chart rules)
- Modify: `ui-assets/app.js` (import + wire the toggle, store reseed at init, topology-backfill hook)
- Modify: `ui-assets/repl.js` (`microgrid_sample` → also `metricsStore.applySample`)
- Modify: `ui-assets/chrome.js` (gridFrequency: also feed `metricsStore`; backfill also on panel open)

**Interfaces:**
- Consumes: `metricsStore`, `fmtValue`, `pfValue`, `pfText` from `./metrics-store.js`; `makeSidePanelToggle`, `isPanelOpen` from `./side-panel.js`; global `uPlot` (vendored, already loaded for inspect.js — check how index.html loads it: `grep -n uplot ui-assets/index.html`, same script tag serves both).
- Produces: `setupMetricsPanel()` (app.js calls it once at init); panel tenant name `"metrics-btn"`; localStorage keys `sw-metrics-card-<key>`, `sw-metrics-series-<stream>`, `sw-metrics-pf`, `sw-metrics-window`.
- During this task the Dashboard still exists and still works — repl.js/chrome.js double-feed both stores; Task 4 removes the dashboard side.

- [ ] **Step 1: index.html — the `panels` group + header cleanup**

In the `#topology-controls` div, after the `values` button, add:

```html
        <span class="ctl-label">panels</span>
        <button type="button" id="formula-btn" class="pill"
                title="Formula explorer: how each derived metric is computed from the topology">formulas</button>
        <button type="button" id="metrics-btn" class="pill"
                title="Derived site metrics: power, reactive power / PF, frequency — live charts">metrics</button>
```

Remove the old header button line `<button id="formula-btn" class="hdr-btn">Formulas</button>`. The id moves, so `makeSidePanelToggle("formula-btn", …)` and every `isPanelOpen("formula-btn")` gate keep working untouched. Check `side-panel.js syncButton`: it toggles class `primary` on the button — `.pill.primary` has no styling. Add to style.css (near the `.pill.active` rules if any, else with the canvas-controls styles):

```css
/* A lit panels-group pill: its panel is open. Same accent treatment
   as the layout picker's active state. */
#topology-controls .pill.primary {
  border-color: var(--accent);
  color: var(--accent);
}
```

- [ ] **Step 2: Write `ui-assets/metrics-panel.js`**

```js
// The metrics panel: three foldable cards — Power, Reactive power,
// Frequency — each one combined uPlot chart over the loopback's
// aggregate streams plus a chip row that is legend, live readout,
// and series toggle in one. Replaces the Dashboard subview's tiles;
// the store (metrics-store.js) owns the data, this module owns the
// DOM. Series colors follow the category palette so chart lines
// mean what the canvas already means.

import { gridFrequency } from "./chrome.js";
import { fmtValue, metricsStore, pfText, pfValue } from "./metrics-store.js";
import { isPanelOpen, makeSidePanelToggle } from "./side-panel.js";

const PANEL = "metrics-btn";
const WINDOWS = [
  { key: "1m", secs: 60 },
  { key: "5m", secs: 300 },
  { key: "10m", secs: 600 },
];

const CARDS = [
  {
    key: "power",
    title: "Power",
    headline: "grid_power",
    defaultOpen: true,
    series: [
      { stream: "grid_power", label: "grid", color: "--cat-grid" },
      {
        stream: "battery_pool_power",
        label: "battery",
        color: "--cat-battery",
        band: ["battery_pool_bounds_lower", "battery_pool_bounds_upper"],
      },
      { stream: "pv_power", label: "pv", color: "--cat-inverter-solar" },
      { stream: "consumer_power", label: "consumer", color: "--flow-import" },
      { stream: "producer_power", label: "producer", color: "--flow-export" },
    ],
  },
  {
    key: "reactive",
    title: "Reactive power",
    headline: "grid_reactive_power",
    defaultOpen: false,
    pfOverlay: true,
    series: [
      { stream: "grid_reactive_power", label: "grid", color: "--cat-grid", p: "grid_power" },
      { stream: "pv_reactive_power", label: "pv", color: "--cat-inverter-solar", p: "pv_power" },
      {
        stream: "battery_reactive_power",
        label: "battery",
        color: "--cat-battery",
        p: "battery_pool_power",
      },
    ],
  },
  {
    key: "frequency",
    title: "Frequency",
    headline: "grid_frequency",
    defaultOpen: false,
    series: [{ stream: "grid_frequency", label: "frequency", color: "--accent" }],
  },
];

// ── persisted knobs (same try/catch discipline as the inspector) ──
function loadKey(key, fallback) {
  try {
    const v = localStorage.getItem(key);
    return v == null ? fallback : v;
  } catch (_) {
    return fallback;
  }
}
function saveKey(key, value) {
  try {
    localStorage.setItem(key, value);
  } catch (_) {
    // Storage unavailable — the choice just doesn't stick.
  }
}
const cardOpen = (c) => loadKey(`sw-metrics-card-${c.key}`, c.defaultOpen ? "1" : "0") === "1";
const seriesOn = (s) => loadKey(`sw-metrics-series-${s.stream}`, "1") === "1";
const pfOn = () => loadKey("sw-metrics-pf", "0") === "1";
const windowSecs = () => {
  const k = loadKey("sw-metrics-window", "5m");
  return (WINDOWS.find((w) => w.key === k) ?? WINDOWS[1]).secs;
};

const cssColor = (v) => getComputedStyle(document.documentElement).getPropertyValue(v).trim();

// One live uPlot per unfolded card; rebuilt (not re-dataed) on any
// config change — window, series toggle, PF overlay, unfold — and
// destroyed on fold/close. plots: card key → { plot, streams }.
let plots = new Map();
let unsubscribe = null;
let repaintQueued = false;

function destroyPlots() {
  for (const { plot } of plots.values()) plot.destroy();
  plots = new Map();
}

// Scale choice across every visible series of a card so they share
// one y-axis honestly (the inspector's chooseScale, multi-series).
function chooseDiv(values) {
  const max = Math.max(0, ...values.map((v) => Math.abs(v)));
  if (max >= 1e6) return { div: 1e6, prefix: "M" };
  if (max >= 1e3) return { div: 1e3, prefix: "k" };
  return { div: 1, prefix: "" };
}

function buildChart(card, slot) {
  const active = card.series.filter(seriesOn);
  if (!active.length) {
    slot.innerHTML = '<p class="hint">all series toggled off</p>';
    return;
  }
  // Scale + shape decisions are made once per build; assemble() then
  // regenerates the full data array — active series, envelope-band
  // series, PF series — in exactly the built shape, so the repaint
  // path can setData() without ever guessing whether shapes match.
  const secs = windowSecs();
  const all = [];
  for (const s of active) {
    for (const v of metricsStore.series(s.stream, secs).ys) if (v != null) all.push(v);
  }
  const unit = metricsStore.latest(card.headline)?.unit ?? "";
  const shown = unit === "var" ? "VAr" : unit;
  const isPower = unit === "W" || unit === "var";
  const { div, prefix } = isPower ? chooseDiv(all) : { div: 1, prefix: "" };
  const scaled = (stream) =>
    metricsStore.series(stream, secs).ys.map((v) => (v == null ? null : v / div));
  // Battery envelope: lower/upper bounds as invisible series with a
  // translucent band between them, behind the battery trace. Only
  // included when the bounds streams carry data at build time.
  const bandCfg = active.find((s) => s.band);
  const hasBand =
    bandCfg && bandCfg.band.every((b) => metricsStore.series(b, secs).ys.some((v) => v != null));
  const withPf = card.pfOverlay && pfOn();
  const assemble = () => {
    const data = [metricsStore.series(active[0].stream, secs).xs];
    for (const s of active) data.push(scaled(s.stream));
    if (hasBand) {
      data.push(scaled(bandCfg.band[1]), scaled(bandCfg.band[0]));
    }
    if (withPf) {
      for (const s of active) {
        const p = metricsStore.series(s.p, secs).ys;
        const q = metricsStore.series(s.stream, secs).ys;
        data.push(q.map((qv, i) => pfValue(p[i], qv == null ? null : qv)));
      }
    }
    return data;
  };
  const series = [
    {},
    ...active.map((s) => ({
      stroke: cssColor(s.color),
      width: 1.5,
      points: { show: false },
      spanGaps: false,
    })),
  ];
  const bands = [];
  if (hasBand) {
    series.push(
      { stroke: "transparent", points: { show: false } },
      { stroke: "transparent", points: { show: false } },
    );
    // data indices: 0 = xs, 1..active.length = traces, then hi, lo.
    bands.push({
      series: [active.length + 1, active.length + 2],
      fill: "rgba(241, 149, 91, 0.10)",
    });
  }
  // PF overlay: dashed per-source PF on a right-hand 0.85–1.02
  // scale, derived at draw time from the matching P and Q rings.
  const axes = [
    { stroke: "#7d848e", grid: { stroke: "#353a45", width: 0.5 } },
    {
      stroke: "#7d848e",
      grid: { stroke: "#353a45", width: 0.5 },
      size: 56,
      label: prefix || shown ? `${prefix}${shown}` : "",
      labelSize: 12,
    },
  ];
  if (withPf) {
    for (const s of active) {
      series.push({
        stroke: cssColor(s.color),
        width: 1,
        dash: [3, 3],
        points: { show: false },
        scale: "pf",
      });
    }
    axes.push({
      scale: "pf",
      side: 1,
      stroke: "#7d848e",
      grid: { show: false },
      size: 44,
    });
  }
  const opts = {
    width: slot.clientWidth || 380,
    height: 150,
    cursor: { drag: { x: false, y: false } },
    legend: { show: false },
    scales: { x: { time: true }, pf: { range: [0.85, 1.02] } },
    axes,
    series,
    bands,
  };
  plots.set(card.key, { plot: new uPlot(opts, assemble(), slot), assemble });
}

// Chips + fold summaries repaint on every store notify (rAF-
// coalesced); charts re-data on the same tick.
function repaint(contentEl) {
  for (const card of CARDS) {
    const summary = contentEl.querySelector(`[data-summary="${card.key}"]`);
    if (summary) {
      const head = metricsStore.latest(card.headline);
      let text = head ? fmtValue(head.quantity, head.unit, head.value) : "—";
      if (card.key === "reactive") {
        const p = metricsStore.latest("grid_power")?.value;
        text += ` · ${pfText(p, head?.value ?? null)}`;
      }
      if (card.key === "power") text = `grid ${text}`;
      summary.textContent = text;
    }
    for (const s of card.series) {
      const v = contentEl.querySelector(`[data-chip-value="${s.stream}"]`);
      if (!v) continue;
      const snap = metricsStore.latest(s.stream);
      v.textContent = snap ? fmtValue(snap.quantity, snap.unit, snap.value) : "—";
      if (s.p) {
        const pf = contentEl.querySelector(`[data-chip-pf="${s.stream}"]`);
        if (pf) pf.textContent = pfText(metricsStore.latest(s.p)?.value, snap?.value ?? null);
      }
    }
  }
  // Charts re-data in place: assemble() regenerates the exact data
  // shape each plot was built with, so no shape checks are needed.
  // Scale (kW vs MW) and band presence refresh on the next rebuild
  // (window / toggle / unfold), not per tick.
  for (const { plot, assemble } of plots.values()) plot.setData(assemble());
}

function scheduleRepaint(contentEl) {
  if (repaintQueued) return;
  repaintQueued = true;
  requestAnimationFrame(() => {
    repaintQueued = false;
    if (isPanelOpen(PANEL)) repaint(contentEl);
  });
}

function rebuildCard(key) {
  const entry = plots.get(key);
  const card = CARDS.find((c) => c.key === key);
  const slot = document.querySelector(`#panel-metrics-btn [data-chart="${key}"]`);
  if (entry) {
    entry.plot.destroy();
    plots.delete(key);
  }
  if (card && slot && cardOpen(card)) {
    slot.innerHTML = "";
    buildChart(card, slot);
  }
}

function chipHtml(s) {
  return `
    <button type="button" class="mchip${seriesOn(s) ? "" : " off"}" data-chip="${s.stream}"
            style="--chip-color: var(${s.color})">
      <span class="mchip-dot"></span>
      <span class="mchip-name">${s.label}</span>
      <span class="mchip-value" data-chip-value="${s.stream}">—</span>
      ${s.p ? `<span class="mchip-pf" data-chip-pf="${s.stream}">PF —</span>` : ""}
    </button>`;
}

function cardHtml(card) {
  const open = cardOpen(card);
  const pfChip = card.pfOverlay
    ? `<button type="button" class="mchip mchip-pf-toggle${pfOn() ? "" : " off"}" data-pf-toggle>
         <span class="mchip-name">PF overlay</span>
       </button>`
    : "";
  return `
    <section class="mcard fold${open ? " open" : ""}" data-card="${card.key}">
      <h3 class="fold-toggle" data-fold-toggle>${card.title}
        <span class="fold-summary"><span data-summary="${card.key}">—</span><span class="fold-chevron">▾</span></span>
      </h3>
      <div class="fold-body">
        <div class="mchart" data-chart="${card.key}"></div>
        <div class="mchips">${card.series.map(chipHtml).join("")}${pfChip}</div>
      </div>
    </section>`;
}

function render(contentEl) {
  const winKey = loadKey("sw-metrics-window", "5m");
  contentEl.innerHTML = `
    <div class="metrics-panel">
      <div class="metrics-head">
        <h2>Metrics</h2>
        <span class="ctl-label">window</span>
        ${WINDOWS.map(
          (w) =>
            `<button type="button" class="pill win-pill${w.key === winKey ? " active" : ""}" data-window="${w.key}">${w.key}</button>`,
        ).join("")}
      </div>
      ${CARDS.map(cardHtml).join("")}
    </div>`;

  contentEl.querySelector(".metrics-panel").addEventListener("click", (ev) => {
    const win = ev.target.closest("[data-window]");
    if (win) {
      saveKey("sw-metrics-window", win.dataset.window);
      for (const b of contentEl.querySelectorAll("[data-window]")) {
        b.classList.toggle("active", b === win);
      }
      for (const c of CARDS) rebuildCard(c.key);
      return;
    }
    const pfToggle = ev.target.closest("[data-pf-toggle]");
    if (pfToggle) {
      saveKey("sw-metrics-pf", pfOn() ? "0" : "1");
      pfToggle.classList.toggle("off", !pfOn());
      rebuildCard("reactive");
      return;
    }
    const chip = ev.target.closest("[data-chip]");
    if (chip) {
      const stream = chip.dataset.chip;
      const card = CARDS.find((c) => c.series.some((s) => s.stream === stream));
      saveKey(`sw-metrics-series-${stream}`, seriesOn({ stream }) ? "0" : "1");
      chip.classList.toggle("off", !seriesOn({ stream }));
      if (card) rebuildCard(card.key);
      return;
    }
    const foldToggle = ev.target.closest("[data-fold-toggle]");
    if (foldToggle) {
      const cardEl = foldToggle.closest("[data-card]");
      const card = CARDS.find((c) => c.key === cardEl.dataset.card);
      const nowOpen = !cardEl.classList.contains("open");
      cardEl.classList.toggle("open", nowOpen);
      saveKey(`sw-metrics-card-${card.key}`, nowOpen ? "1" : "0");
      rebuildCard(card.key);
    }
  });

  for (const card of CARDS) rebuildCard(card.key);
  unsubscribe = metricsStore.subscribe(() => scheduleRepaint(contentEl));
  metricsStore.backfill().then(() => {
    if (!isPanelOpen(PANEL)) return;
    for (const c of CARDS) rebuildCard(c.key);
    repaint(contentEl);
  });
  gridFrequency.backfill();
}

function teardown() {
  unsubscribe?.();
  unsubscribe = null;
  destroyPlots();
}

// Re-backfill on topology mutations while open — app.js's debounced
// topology-backfill hook calls this; closed, the next open backfills
// anyway.
export function metricsTopologyRefresh() {
  if (isPanelOpen(PANEL)) metricsStore.backfill();
}

export function setupMetricsPanel() {
  makeSidePanelToggle(PANEL, render, teardown);
  metricsStore.startAutoReseed();
}
```

- [ ] **Step 3: style.css — panel rules**

Add after the existing `.formula-panel` block (grep `formula-panel` for the spot). The fold/`.fold-toggle`/`.fold-body`/`.fold-summary`/`.fold-chevron` classes are the inspector's — check they are defined un-scoped (`grep -n "\.fold" ui-assets/style.css`); if scoped under `#inspector`/`.insp-card`, copy the minimal fold rules into the `.mcard` block instead of widening inspector selectors:

```css
/* ── Metrics panel ─────────────────────────────────────────────
   Three foldable unit-family cards; chips double as legend, live
   readout, and series toggle. Series colors ride the category
   palette via --chip-color so chart, chip, and canvas agree. */
.metrics-panel .metrics-head {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 2px 0 10px;
}
.metrics-panel .metrics-head h2 {
  font-size: 13px;
  margin: 0 auto 0 0;
}
.metrics-panel .win-pill {
  padding: 2px 9px;
  font-size: 11px;
}
.mcard {
  border: 1px solid var(--border);
  border-radius: 6px;
  margin-bottom: 10px;
  background: var(--bg);
}
.mcard .fold-body {
  padding: 2px 10px 10px;
}
.mchart {
  min-height: 20px;
}
.mchips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: 8px;
}
.mchip {
  display: flex;
  align-items: baseline;
  gap: 7px;
  background: var(--pill-surface);
  border: 1px solid var(--pill-border);
  border-radius: 999px;
  padding: 3px 11px;
  cursor: pointer;
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--fg);
}
.mchip .mchip-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  align-self: center;
  background: var(--chip-color);
}
.mchip .mchip-name {
  color: var(--muted);
}
.mchip .mchip-value {
  font-variant-numeric: tabular-nums;
}
.mchip .mchip-pf {
  color: var(--muted);
  font-size: 11px;
}
/* Off = hollow dot on a flat ground; the value stays fully
   readable (mockup decision — a dimmed chip hid the number). */
.mchip.off {
  background: transparent;
}
.mchip.off .mchip-dot {
  background: transparent;
  box-shadow: inset 0 0 0 1.5px var(--chip-color);
}
.mchip.off .mchip-name {
  text-decoration: line-through;
  text-decoration-color: var(--flow-dim);
}
```

Also: the metrics panel needs a width — the generated `.float-panel` cards size to content. Check how `#panel-formula-btn` gets its width (`grep -n "float-panel\|panel-formula\|panel-content" ui-assets/style.css`) and give `#panel-metrics-btn` the same treatment at ~430px:

```css
#panel-metrics-btn {
  width: 430px;
}
```

- [ ] **Step 4: Wire app.js / repl.js / chrome.js (double-feed; Dashboard stays alive)**

- `app.js`: add `import { metricsTopologyRefresh, setupMetricsPanel } from "./metrics-panel.js";`. In `init()` (near `setupFormulaTileClicks()`), add `setupMetricsPanel();`. In `armTopologyBackfill`'s timeout callback, after `dashboardTiles.backfill();` add `metricsTopologyRefresh();`.
- `repl.js`: add `import { metricsStore } from "./metrics-store.js";`. In the WS dispatch, `else if (ev.kind === "microgrid_sample") { dashboardTiles.applySample(ev); metricsStore.applySample(ev); }`.
- `chrome.js`: add `import { metricsStore } from "./metrics-store.js";`. In `gridFrequency`: `backfill()` also does `metricsStore.resetStream("grid_frequency")` before the loop and pushes each sample via `metricsStore.applySample({ stream: "grid_frequency", quantity: j.quantity, unit: j.unit, ts_ms, value })` beside the `dashboardTiles.applySample` call; `applySample(ev)` likewise pushes to both.

- [ ] **Step 5: Verify**

```
npx @biomejs/biome check ui-assets tools 2>&1 | tee "$SCRATCH/t3-biome.log" | tail -5
node tools/boot-smoke.mjs && node tools/metrics-store-test.mjs && node tools/formula-ast-test.mjs
cargo build 2>&1 | tail -3
```
Expected: no new biome warnings; boot smoke green (the import graph grew metrics-store/metrics-panel). Then live check: launch the app (`cargo run` + an example site, or the Playwright helper from the formula-explorer branch), open Topology, click the `metrics` pill → panel opens, Power chart draws with data after ~2 s, chips show values, window pills re-range, series chips toggle, PF overlay adds the dashed lines + right axis, fold/unfold persists across close/reopen. The Dashboard subview must still work unchanged (double-feed).

- [ ] **Step 6: Commit**

```bash
git add ui-assets/metrics-panel.js ui-assets/index.html ui-assets/style.css ui-assets/app.js ui-assets/repl.js ui-assets/chrome.js
git commit -m "Add the floating metrics panel" -m "Three foldable unit-family cards - power, reactive power with a PF
overlay, frequency - each a combined uPlot chart over the aggregate
streams, with chips as legend, live readout, and series toggle in
one. Both panel toggles now live in the canvas-controls bar's new
panels group. The Dashboard subview still stands; demolition follows
separately.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 4: Demolish the Dashboard subview

**Files:**
- Delete: `ui-assets/dashboard.js`, `ui-assets/formulas.js`
- Modify: `ui-assets/index.html` (drop `#dashboard` block + Dashboard nav button; help-text prose)
- Modify: `ui-assets/routing.js` (subview set/defaults/redirect/applyMode/refreshTopology/imports)
- Modify: `ui-assets/app.js` (drop dashboardTiles + setupFormulaTileClicks wiring)
- Modify: `ui-assets/repl.js` (drop dashboard imports + row-module dispatch)
- Modify: `ui-assets/chrome.js` (gridFrequency: metricsStore only)
- Modify: `ui-assets/side-panel.js` (dashboard drag special case + comment prose)
- Modify: `ui-assets/style.css` (all `.dash-*`, tier-row, dashboard dock/display rules)
- Modify: `AGENTS.md` (module map + dashboard prose)

**Interfaces:**
- Consumes: everything Task 3 produced. After this task `grep -rn "dashboard" ui-assets/*.js` must return only prose-free hits (ideally none beyond `swctl`-unrelated comments that still make sense).

- [ ] **Step 1: routing.js**

- Imports: drop the `./dashboard.js` import block entirely; drop `dashboardTiles`/row modules from the file.
- `VALID_SUBVIEWS = new Set(["topology", "dispatches"])`.
- `readSubview()` fallback → `"topology"`.
- `parseHash`: the two `subview: "dashboard"` defaults → `"topology"`; regex keeps all four names — `/^#microgrids\/(\d+)(?:\/(dashboard|topology|formulas|dispatches))?$/` — and the redirect line becomes `if (m && (m[2] === "formulas" || m[2] === "dashboard")) m[2] = "topology";` with the comment extended: dashboard was retired for the metrics panel; old bookmarks land on Topology.
- `subview: m[2] || "dashboard"` → `|| "topology"`.
- `applyMode`: delete the whole `subview === "dashboard"` block (backfill + reseeds). `gridFrequency.backfill()` moved to panel-open in Task 3.
- `refreshTopology`: delete the `seed` line + four row `refresh` calls; keep `gridFrequency.applyTopology(data)` and the pulse-bar lines. Update the comment above (`// Keep the row modules' shape current…` dies).
- The `jumpToTopology` comment naming "dashboard tier rows, the dashboard formula tree's chips" → "the formula explorer's #N refs and the metrics panel".

- [ ] **Step 2: index.html**

- Delete the whole `<div id="dashboard" hidden>…</div>` block (lines ~108–175).
- Delete `<button class="mode-btn" data-subview="dashboard">Dashboard</button>` from `#mg-subtoggle`.
- Grep the help dialog for `Dashboard`/`dashboard` (`grep -n -i dashboard ui-assets/index.html`) and rewrite each hit: tiles/subview references become metrics-panel references ("the metrics pill (top-right of the canvas) opens live site metrics — power, reactive power / PF, frequency").
- The `#panel-dock` comment block naming "the dashboard subview" updates: panels always float.

- [ ] **Step 3: app.js, repl.js, chrome.js**

- `app.js`: drop `import { dashboardTiles } from "./dashboard.js";` and `import { setupFormulaTileClicks } from "./formulas.js";`; drop the `setupFormulaTileClicks()` call; in `armTopologyBackfill` drop `dashboardTiles.backfill();` (keep `metricsTopologyRefresh()`); drop `dashboardTiles.startAutoReseed();` (the store's reseed started in `setupMetricsPanel`). Update the comments that narrate the dashboard reseed.
- `repl.js`: drop the `./dashboard.js` import block; `kind === "sample"` loses the four row-module calls (keep `gridFrequency`, `topology`, `inspectorLive`, `liveCharts`); `kind === "microgrid_sample"` becomes `metricsStore.applySample(ev)` only. The comment mentioning "the dashboard doesn't paint with samples from a neighbour" reworded to the metrics panel.
- `chrome.js`: drop the `dashboardTiles` import and its two call sites — `metricsStore` only. Module header comment "grid-frequency tile feeder" → "grid-frequency stream feeder (metrics panel)". The comment "fetch the most recent sample on dashboard entry" → "on panel open".

- [ ] **Step 4: side-panel.js + style.css**

- `side-panel.js`: delete the `if (document.body.dataset.subview === "dashboard") return;` line in `wireDrag` and shrink its comment (drag is always live now); header comment "the dashboard formula tree" → drop from the tenant list.
- `style.css`: delete the dashboard dock rules (`body.panel-open[data-subview="dashboard"] main`, `…#panel-dock`, `body[data-subview="dashboard"] .float-panel`, `…panel-drag`), every `#dashboard` / `.dash-*` / tier-row rule (lines ~408–450 display matrix entries mentioning `#dashboard`, ~647–790 dash tiles/sections/tier template, `.dash-tile-interactive` ~943), and the `body.compact .dash-envelope` line. The display matrix keeps `#topology`/`#dispatches` behavior — rewrite the matrix comment for two subviews.

- [ ] **Step 5: Delete the modules + AGENTS.md**

```bash
git rm ui-assets/dashboard.js ui-assets/formulas.js
```
(If an `.nfs*` file appears in ui-assets afterwards, a process still holds the deleted file — find and close it, e.g. a mime session via `close_session`; never add or `git rm` the `.nfs*` file.)

`AGENTS.md`: the module-map sentence listing `dashboard.js` and the site-PF/"every dashboard row tier" prose (lines ~56, ~69) — rewrite for `metrics-store.js`/`metrics-panel.js` and drop the row-tier claim.

- [ ] **Step 6: Verify**

```
node tools/boot-smoke.mjs && node tools/metrics-store-test.mjs && node tools/formula-ast-test.mjs
npx @biomejs/biome check ui-assets 2>&1 | tail -5
grep -rn -i "dashboard" ui-assets/*.js ui-assets/index.html ui-assets/style.css
cargo test 2>&1 | tee "$SCRATCH/t4.log"; grep "test result" "$SCRATCH/t4.log"
```
Expected: boot smoke green (the deleted modules left no dangling imports); the grep returns nothing (or only deliberate prose — justify each survivor); all Rust suites green (server untouched, but `src/ui/tests.rs`/`tests/ui_http.rs` may assert on served HTML — fix any that reference the Dashboard subview). Live check: old URL `#microgrids/<id>/dashboard` lands on Topology; the subtoggle shows Topology/Dispatches; Esc, panel drag, formula explorer, inspector all behave; localStorage from a pre-change session (subview `dashboard`) resolves to Topology.

- [ ] **Step 7: Commit**

```bash
git add -u ui-assets AGENTS.md
```
NO — `-u` is banned. Add explicitly:
```bash
git add ui-assets/index.html ui-assets/routing.js ui-assets/app.js ui-assets/repl.js ui-assets/chrome.js ui-assets/side-panel.js ui-assets/style.css AGENTS.md
git rm --cached --ignore-unmatch ui-assets/dashboard.js ui-assets/formulas.js
git commit -m "Drop the Dashboard subview" -m "The metrics panel owns the tiles' job and the topology canvas plus
inspector own per-component detail, so the subview, its tier rows,
and the formula-tree doorway go. Old dashboard bookmarks and stored
subviews land on Topology; panels are pure floats everywhere now.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```
(`git rm` in Step 5 already staged the deletions; the `--cached --ignore-unmatch` line is a no-op safety if Step 5 was done differently.)

---

### Task 5: e2e smoke rework + full gate

**Files:**
- Modify: `tools/ui-smoke/live-topology.mjs` (dashboard sections → metrics panel)

**Interfaces:**
- Consumes: the running app from Tasks 1–4; the script's existing helpers (read the file first — it has its own launch/assert conventions).

- [ ] **Step 1: Rework the live smoke script**

Read `tools/ui-smoke/live-topology.mjs` in full. Its dashboard coverage (the `import("/assets/dashboard.js")` block ~line 208 and the Dashboard-subview click-through ~line 555) is dead. Replace with metrics-panel coverage in the same style as the file's formula-panel section:

- Delete the `dashboard.js` import block (it unit-checked `sitePfText` in-page; that logic now has node tests in `tools/metrics-store-test.mjs` — in-page re-check is redundant).
- Replace the `#mg-subtoggle` dashboard click-through with: click `#metrics-btn` → assert `#panel-metrics-btn.open` exists; wait ~3 s; assert at least one `.mchip .mchip-value` shows a value other than `—`; assert the Power card contains a `canvas` (uPlot mounted); click a series chip → assert it gains `.off`; click it again → assert `.off` clears; click `#metrics-btn` again → assert the panel closed. Negative control: assert `document.querySelector("#dashboard") === null` and the subtoggle has no `data-subview="dashboard"` button.

- [ ] **Step 2: Run the reworked script + the whole gate**

```
node tools/ui-smoke/live-topology.mjs 2>&1 | tee "$SCRATCH/t5-smoke.log" | tail -15
cargo clippy --all-targets -- -D warnings && cargo fmt --check
cargo test 2>&1 | tee "$SCRATCH/t5.log"; grep "test result" "$SCRATCH/t5.log"
npx @biomejs/biome check ui-assets tools 2>&1 | tail -5
node tools/boot-smoke.mjs && node tools/formula-ast-test.mjs && node tools/metrics-store-test.mjs
```
Expected: everything green. (The smoke script needs the app runnable locally — it launches or attaches per its own header comments; follow them.)

- [ ] **Step 3: Commit**

```bash
git add tools/ui-smoke/live-topology.mjs
git commit -m "Point the live smoke at the metrics panel" -m "The dashboard click-through and in-page sitePfText checks are dead;
cover the metrics pill, chip toggles, chart mount, and the removed
subview instead.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

## Self-review notes (already applied)

- Spec coverage: server streams (T1), store+PF (T2), panel/cards/chips/PF-overlay/window/persistence + pills move (T3), demolition/routing/side-panel/AGENTS (T4), e2e (T5). The spec's "fold-summary values update live" is repaint()'s summary writes; "battery envelope band" is T3's bands block; "no varh" is T1's negative assertions.
- The `formula-btn` header→pill move rides T3 (the panels group is created there); its `hdr-btn`→`pill` class change keeps `syncButton`'s `primary` toggle working via the new `.pill.primary` rule.
- dashboard.js's `fmt` semantics live on as `fmtValue` (T2 tests pin MW/kVAr/Hz cases); `sitePfText`'s convention lives on as `pfText` (T2 tests pin lead/lag/unity).
- Type consistency: `metricsStore.series → {xs, ys}` consumed by T3's buildChart/repaint; `metricsTopologyRefresh`/`setupMetricsPanel` consumed by T3-step-4 app.js wiring and kept in T4.
