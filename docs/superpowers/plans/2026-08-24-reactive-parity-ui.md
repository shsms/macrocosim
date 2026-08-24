# Reactive Parity — Sub-project 3 (UI + loopback) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface reactive power in the UI at parity with active power — hover-card Q envelope bar, dashboard Q (per-inverter and a grid-Q/site-PF tile fed by a new loopback `ReactivePower` forwarder), inspector knobs — plus the formula-convergence E2E test deferred from SP1.

**Architecture:** The loopback gains a `ReactivePower`-typed grid-formula forwarder beside the monomorphic `Power` one, publishing a `grid_reactive_power` aggregate stream (deliberately absent from energy accumulation). The dashboard's generic tile machinery paints it from markup alone; site PF is derived client-side from the two grid snapshots. The hover card routes its already-computed reactive section through `envelopeBar` (which gains a unit parameter). Inspector knobs POST the SP1/SP2 defuns through the existing eval path.

**Tech Stack:** Rust (tokio, frequenz-microgrid 0.6), vanilla ES-module JS in `ui-assets/`, Playwright smoke script.

**Spec:** `docs/superpowers/specs/2026-08-24-reactive-parity-design.md` — the "UI" decision bullet (lines ~191-204), Sub-project 3 section, and Testing bullets ("Formula convergence", "UI smoke").

## Global Constraints

- Commit messages: imperative subject, short why-body in easy English; trailer EXACTLY `Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>`; NO Co-Authored-By or AI-attribution trailers.
- Stage files explicitly by name; never stage `.nfs*` files or the untracked scratch files under `examples/`.
- Rust gate per task: `cargo test --lib`, `cargo clippy --lib --tests -- -D warnings`, `cargo fmt --check` — all clean.
- JS gate per task touching `ui-assets/`: `npx @biomejs/biome check ui-assets` (NOT `npx biome`, which is a wrong no-op package).
- All commands FOREGROUND.
- Edge chevrons stay P/DC-based; no varh accumulation for Q anywhere; no gRPC route for capability mutation (spec "Not added, on purpose").
- Sign convention: +Q inductive/lagging; PF qualifier "leading" when P and Q have opposite signs (matches `hovercard.js` `powerFactor` and the smoke tests' pinned expectations).

---

### Task 1: Loopback `ReactivePower` grid forwarder

**Files:**
- Modify: `src/ui/loopback.rs`

**Interfaces:**
- Consumes: upstream crate `frequenz_microgrid` — `metric::AcPowerReactive` (metric.rs:62 of the vendored 0.6.0), generic `LogicalMeterHandle::grid::<M>()`, `quantity::ReactivePower` with `.as_volt_amperes_reactive()`.
- Produces: aggregate stream `"grid_reactive_power"` published with quantity `"ReactivePower"`, unit `"var"` via the same `publish_scalar` path the `Power` forwarders use. Task 2's E2E test and Task 3's dashboard tile depend on this exact stream name.

- [ ] **Step 1: Extend imports.** In `src/ui/loopback.rs` (imports at lines 13-16), change `quantity::Power` to `quantity::{Power, ReactivePower}`.

- [ ] **Step 2: Add the reactive forwarder pair.** Beside `subscribe_power_forwarder` / `publish_power` (lines ~363-415), add `subscribe_reactive_forwarder` / `publish_reactive`, mirroring them line for line with three deltas: `Formula<Power>` → `Formula<ReactivePower>` / `Sample<Power>` → `Sample<ReactivePower>`; `p.as_watts()` → `q.as_volt_amperes_reactive()`; and NO `energy_stream_for` / `accumulate_energy` block — instead a comment:

```rust
// No energy hook on purpose: reactive energy (varh) accumulation is
// out of scope; energy_stream_for never maps this stream.
```

The publish call becomes `publish_scalar(stream, "ReactivePower", "var", value, ts_ms, site, state);`.

- [ ] **Step 3: Subscribe the grid Q formula.** In `subscribe_power_forwarders` (lines ~237-286), after the `metered` loop and before the battery-pool block, add:

```rust
if let Some(h) = subscribe_reactive_forwarder(
    "grid_reactive_power",
    lm.grid::<metric::AcPowerReactive>(),
    site,
    state.clone(),
)
.await
{
    handles.push(h);
}
```

Only the grid formula — no consumer/producer/pv reactive streams (spec: one site tile).

- [ ] **Step 4: Pin the no-varh rule.** In `loopback.rs`'s `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn grid_reactive_power_has_no_energy_stream() {
    assert_eq!(energy_stream_for("grid_reactive_power"), None);
}
```

- [ ] **Step 5: Gate.** `cargo test --lib`, `cargo clippy --lib --tests -- -D warnings`, `cargo fmt --check` — all clean.

- [ ] **Step 6: Commit** (stage `src/ui/loopback.rs` by name): `Forward the grid reactive-power formula over the loopback` — body: why the twin is a parallel pair (keeps the Power path monomorphic) and why the energy hook is deliberately absent.

---

### Task 2: Formula-convergence E2E test (deferred obligation from SP1)

**Files:**
- Modify: `tests/ui_http.rs`

**Interfaces:**
- Consumes: `tests/common/mod.rs` `TestServer::start(config_body)` (spawns physics + gRPC + UI server + `ui::spawn_microgrid_loopback`, exposes `ui_url`); Task 1's `"grid_reactive_power"` stream; the EV charger's `reactive_power_var: Some(0.0)` telemetry (`src/sim/ev_charger.rs:193-218` → `proto_conv.rs:258-268` emits an `AcPowerReactive` sample).
- Produces: nothing later tasks consume; discharges the SP1 final-review ruling ("formula-convergence E2E deferred to SP3; mechanism — EV zero-Q sample — pinned now").

- [ ] **Step 1: Write the test.** In `tests/ui_http.rs`, following the harness pattern of `site_import_creates_microgrid_with_working_formulas` (lines ~176-239): start a `TestServer` whose config contains a grid meter, a battery + battery inverter, AND an EV charger (copy component forms from an existing test config in the file/harness; the EV charger is the point — the grid-Q formula spans it, and only its explicit `Some(0.0)` Q sample lets the aggregation converge). Then poll `GET {ui_url}/api/mg/{id}/microgrid/latest` in a bounded retry loop (~20 s, 200 ms interval — mirror the polling style used elsewhere in the file) until the JSON contains an entry with `"stream": "grid_reactive_power"`; assert its `value` is present and finite and its `quantity`/`unit` are `"ReactivePower"`/`"var"`. Finally assert no `"grid_reactive_energy"` stream ever appeared in the same snapshot.

Test name: `grid_reactive_formula_converges_over_a_site_with_an_ev_charger`.

- [ ] **Step 2: Run it.** `cargo test --test ui_http grid_reactive_formula_converges -- --nocapture` — PASS. Mutation-check the mechanism once: the test must be observed to time out (or hang past several poll rounds) if the EV charger's telemetry Q were absent — verify by temporarily changing `ev_charger.rs` to `reactive_power_var: None`, watching the poll fail to converge, then reverting (do NOT commit the mutation).

- [ ] **Step 3: Gate.** `cargo test --lib` + the new test + clippy + fmt clean.

- [ ] **Step 4: Commit** (stage `tests/ui_http.rs`): `Prove the grid reactive formula converges with an EV charger` — body: the EV's telemetry zero is load-bearing (absent Q reads as unknown upstream and the aggregation never resolves); this was deferred from the core sub-project because it needs the loopback stream. (Corrected during execution: the SP3 mutation-check disproved this — see the spec's 2026-08-24 correction; the test stands as end-to-end stream coverage.)

---

### Task 3: Dashboard — inverter-tier Q, grid-Q/site-PF tile, scenario-report rows

**Files:**
- Modify: `ui-assets/dashboard.js`, `ui-assets/index.html`, `ui-assets/dialogs.js`, `ui-assets/style.css` (only if the tier3 row grid needs a column)

**Interfaces:**
- Consumes: per-component WS metric `reactive_power_var` (already published by SP1; frontend today only reads it in `topology.js`); Task 1's `grid_reactive_power` microgrid_sample stream (the generic `dashboardTiles` paint/spark/backfill path is stream-name-agnostic — driven purely by `data-stream` markup).
- Produces: DOM hooks Task 6's smoke checks assert on — `[data-stream="grid_reactive_power"]` value + spark elements and a `#site-pf` span.

- [ ] **Step 1: Inverter tier tracks Q.** In `ui-assets/dashboard.js` `batteryPairs`:
  - Add `"reactive_power_var"` to `TRACKED_INVERTER` (line ~318).
  - In `applySample` (lines ~478-494) add `else if (ev.metric === "reactive_power_var") inv.reactive = ev.value;`.
  - In `seedInverter` (line ~391) fetch `"reactive_power_var"` as a fourth metric and store `inv.reactive`.
  - In `render`'s inverter cell (line ~364-370), after the envelope bar line add:

```js
<span class="tier3-reactive muted">${inv.reactive == null ? "—" : formatScaled(inv.reactive, "var")}</span>
```

  If `.tier3-row` uses a fixed `grid-template-columns` in `ui-assets/style.css`, widen it by one column for `.tier3-reactive`; if it's flex/auto, no CSS change.

- [ ] **Step 2: Grid-Q tile markup.** In `ui-assets/index.html`, immediately after the Grid power tile (lines 201-206), add:

```html
<section class="dash-tile">
  <h2>Grid reactive power</h2>
  <div class="dash-value muted" data-stream="grid_reactive_power">—</div>
  <svg class="dash-spark" data-stream="grid_reactive_power" viewBox="0 0 100 30" preserveAspectRatio="none"></svg>
  <div class="dash-meta muted"><span id="site-pf">site PF —</span> · <code>grid_formula</code></div>
</section>
```

The generic `dashboardTiles` machinery (paint at dashboard.js:145-152, `fmt` already handles `ReactivePower`/`var` at :18-19, backfill at :228-263) picks this up with no per-stream code.

- [ ] **Step 3: Derived site PF.** In `dashboardTiles`, keep the existing per-stream `snap` store; in `applySample` (line ~179-183), after `paint(...)`, when `ev.stream` is `"grid_power"` or `"grid_reactive_power"`, call a new `updateSitePf()`:

```js
function updateSitePf() {
  const el = document.getElementById("site-pf");
  if (!el) return;
  const p = snaps.get("grid_power")?.value;
  const q = snaps.get("grid_reactive_power")?.value;
  if (!Number.isFinite(p) || !Number.isFinite(q) || (p === 0 && q === 0)) {
    el.textContent = "site PF —";
    return;
  }
  const pf = Math.abs(p) / Math.hypot(p, q);
  // Qualifier convention matches the hover card: opposite signs = leading,
  // same signs = lagging, no qualifier when PF rounds to unity.
  const tag = pf >= 0.995 ? "" : p * q < 0 ? " leading" : " lagging";
  el.textContent = `site PF ${pf.toFixed(2)}${tag}`;
}
```

(Adapt the exact snapshot-store access to what `dashboardTiles` actually keeps — the paint path already holds the latest value per stream; reuse that, do not add a second store. Call `updateSitePf()` from `backfill` too so the meta line seeds on mode entry.)

- [ ] **Step 4: Scenario report card rows.** In `ui-assets/dialogs.js` `renderScenarioCard` (lines 184-215), after the `main-meter peak` row add two `<dt>/<dd>` pairs:

```js
<dt>main-meter Q peak</dt><dd>${fmt(r.peak_main_meter_var, "VAr")}</dd>
<dt>site PF at Q peak</dt><dd>${r.site_pf_at_peak_var == null ? "—" : r.site_pf_at_peak_var.toFixed(2)}</dd>
```

(`fmt` renders `— ` for null and `x.xx kVAr` otherwise; PF is a ratio, so it bypasses `fmt`.) The fields come from SP2's `ScenarioReport` (`peak_main_meter_var`, `site_pf_at_peak_var`).

- [ ] **Step 5: Gate.** `npx @biomejs/biome check ui-assets` clean; `cargo test --lib` still green (no Rust change expected).

- [ ] **Step 6: Commit** (stage the touched `ui-assets/` files by name): `Show reactive power on the dashboard` — body: tier Q span, the tile rides the generic data-stream machinery, site PF derived client-side from the two grid snapshots, report card gains the SP2 Q stats.

---

### Task 4: Hover card Q envelope bar

**Files:**
- Modify: `ui-assets/hovercard.js`

**Interfaces:**
- Consumes: `hoverCardModel`'s existing `reactive` section (`hovercard.js:81-91` — `{label, text, color, lo, hi, value}` from `live.q`/`live.qLo`/`live.qHi`).
- Produces: rendered `.hc-bar` + `.hc-bar-ends` for the reactive row — Task 6's smoke asserts on it.

- [ ] **Step 1: Unit parameter.** Change `envelopeBar(section)` (lines 112-121) to `envelopeBar(section, unit = "W")` and replace both hardcoded `"W"` endpoint formats at line ~118 with `formatScaled(section.lo, unit)` / `formatScaled(section.hi, unit)`.

- [ ] **Step 2: Route the reactive section through it.** In the render template (line ~154-155), the active/dc calls stay `${envelopeBar(m.power)}${envelopeBar(m.dc)}`; replace the bespoke reactive `hc-row` (line ~155) with `${envelopeBar(m.reactive, "VAr")}` — `envelopeBar` already returns `""` for a null section and renders the plain row without a bar when `lo`/`hi` are absent, so behavior for Q-less components is unchanged.

- [ ] **Step 3: Gate.** `npx @biomejs/biome check ui-assets` clean.

- [ ] **Step 4: Commit** (stage `ui-assets/hovercard.js`): `Draw the reactive envelope bar on the hover card` — body: the model always computed lo/hi/value for Q and the renderer discarded them; the bar helper hardcoded W and would have mislabeled VArs.

---

### Task 5: Inspector knobs

**Files:**
- Modify: `ui-assets/inspect.js`

**Interfaces:**
- Consumes: eval POST path `evalQuoted(expr)` → `POST /api/mg/{id}/eval` with a raw s-expression body (`inspect.js:311-338`); defuns `set-reactive-power` (pre-existing inverter setpoint), `set-meter-reactive-power` and `set-meter-power-factor ID PF &optional LEADING` (SP2).
- Produces: knob inputs with `data-defun` values Task 6's smoke asserts on.

- [ ] **Step 1: Extend the knob table.** In `KNOBS_BY_CATEGORY` (`inspect.js:154-162`):

```js
meter: [
  { label: "power (W or expr)", defun: "set-meter-power", dynamic: true },
  { label: "reactive power (VAr or expr)", defun: "set-meter-reactive-power", dynamic: true },
  { label: "power factor (0–1]", defun: "set-meter-power-factor", flag: "leading" },
],
inverter: [
  { label: "reactive power (VAr)", defun: "set-reactive-power" },
  { label: "reactive PF limit", defun: "set-reactive-pf-limit" },
  { label: "reactive apparent (VA)", defun: "set-reactive-apparent-va" },
],
```

- [ ] **Step 2: Render the flag checkbox.** In the knob render block (`inspect.js:216-230`), when `k.flag` is set, append inside the `<dd>` after the input:

```js
<label class="knob-flag"><input type="checkbox" class="knob-flag-input" /> ${escapeHtml(k.flag)}</label>
```

- [ ] **Step 3: POST with the flag.** In the change handler (`inspect.js:264-271`), read a sibling `.knob-flag-input` if present and append ` t` when checked:

```js
const flag = e.target.closest("dd")?.querySelector(".knob-flag-input");
evalQuoted(`(${e.target.dataset.defun} ${d.id} ${v}${flag?.checked ? " t" : ""})`);
```

The checkbox alone never submits — it qualifies the next value entered (comment this in the code).

- [ ] **Step 4: Gate.** `npx @biomejs/biome check ui-assets` clean.

- [ ] **Step 5: Commit** (stage `ui-assets/inspect.js`): `Add reactive setpoint and meter reactive knobs to the inspector` — body: the eval path already existed; the PF knob's leading flag rides the defun's optional argument.

---

### Task 6: UI smoke additions + docs

**Files:**
- Modify: `tools/ui-smoke/live-topology.mjs`, `AGENTS.md`, `todo.org` (only if a matching open entry exists)

**Interfaces:**
- Consumes: Tasks 3-5's DOM hooks; the smoke harness (`check`/`waitFor`, `hoverNodeCard` at line ~482); a live server started for the run.

- [ ] **Step 1: Add the smoke checks** to `tools/ui-smoke/live-topology.mjs`, following the existing `check`/`waitFor` style:
  - **Hover Q bar:** extend the `hoverNodeCard(1001)` e2e block — assert the card DOM contains at least two `.hc-bar` elements (active + reactive) and that a `.hc-bar-ends` span's text matches `/VAr/`:

```js
const hcBars = await page.evaluate(() => ({
  bars: document.querySelectorAll(".hover-card .hc-bar").length,
  ends: [...document.querySelectorAll(".hover-card .hc-bar-ends")].map((e) => e.textContent),
}));
check("e2e: hover card draws the reactive envelope bar", hcBars.bars >= 2 && hcBars.ends.some((t) => /VAr/.test(t)), JSON.stringify(hcBars));
```

  - **Dashboard tile:** in/near the dashboard-mode section, `waitFor` the tile value to paint and the PF meta to derive:

```js
const tile = await waitFor(async () => {
  const t = await page.evaluate(() => document.querySelector('.dash-value[data-stream="grid_reactive_power"]')?.textContent);
  return t && t !== "—" ? t : null;
});
check("e2e: grid reactive tile paints a var value", /VAr|var/.test(tile), tile);
const pfMeta = await page.evaluate(() => document.getElementById("site-pf")?.textContent);
check("e2e: site PF derives from the two grid streams", /site PF \d\.\d\d( (lagging|leading))?/.test(pfMeta ?? ""), pfMeta ?? "null");
```

  - **Knobs:** select a meter node (reuse however the existing script selects/inspects nodes; a click on the node's pill then reading the inspector pane), then:

```js
const knobDefuns = await page.evaluate(() => [...document.querySelectorAll(".knob-input")].map((i) => i.dataset.defun));
check("e2e: meter reactive knobs present", knobDefuns.includes("set-meter-reactive-power") && knobDefuns.includes("set-meter-power-factor"), JSON.stringify(knobDefuns));
```

    and one round-trip: set the reactive knob input to `500`, dispatch a `change` event, then `waitFor` a direct eval probe from Node — `POST {BASE}/api/mg/{id}/eval` body `(component-reactive-power ID)` — to return ≈500.

- [ ] **Step 2: Run the smoke.** Build and start a server exactly the way the repo's docs/AGENTS.md prescribe for smoke runs (a config with a battery inverter + meter, e.g. the berlin demo used by the existing e2e block), then `SW_UI=http://127.0.0.1:PORT node tools/ui-smoke/live-topology.mjs` — ALL checks PASS (pre-existing ones included). Stop the server afterwards.

- [ ] **Step 3: Docs sweep.** Update `AGENTS.md`'s UI bullets to mention the Q envelope bar, the grid-reactive tile + derived site PF, and the new knobs. Grep `todo.org` for open reactive-UI entries (e.g. hover bar / dashboard Q); mark any that exist DONE with a short resolution note in the file's established style — do not invent entries.

- [ ] **Step 4: Gate.** `npx @biomejs/biome check ui-assets` (script lives outside ui-assets, so also keep the .mjs consistent with its own style), `cargo test --lib`, clippy, fmt — clean.

- [ ] **Step 5: Commit** (stage the touched files by name): `Smoke-check the reactive UI surfaces` — body: what the three checks pin and where the docs moved.
