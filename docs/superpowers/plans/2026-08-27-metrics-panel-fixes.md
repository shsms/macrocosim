# Metrics Panel Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the spec addendum: `grid_frequency` as a real loopback stream (main-meter workaround deleted everywhere, scenario metrics re-keyed onto the grid formula streams), a Charts-only GCP inspector, and independent floating panels with header-safe drag.

**Architecture:** Server first (frequency forwarder; scenario journal fed from the loopback publish path with renamed report fields), then client demolition of the `gridFrequency` synthesis + the GCP inspector card, then the side-panel float/drag rework, then smoke + full gate.

**Tech Stack:** Rust/axum + frequenz-microgrid 0.6.0 (no version bump — the Frequency sender arm already exists in the locked version), vanilla ES modules + vendored uPlot, node tools tests, biome, Playwright.

**Spec:** `docs/superpowers/specs/2026-08-27-metrics-panel-design.md` — the **Addendum** section is the authority for this plan.

## Global Constraints

- Commit style: short imperative subject, prose body, then exactly `Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>`. NO Co-Authored-By / AI trailers. A body line must never START with `#`.
- `git add` files explicitly by name — never `-A`, `.`, `-u`, or `commit -a`. Never add `.nfs*` files (close the holder instead).
- Tee test output to a scratch file (`SCRATCH=/tmp/claude-1000/-vagrant/d9ffc5a7-5e1d-4700-94e1-d5c9f875c63a/scratchpad`) and grep the file.
- Rust gates: `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `cargo test`.
- JS gates: `npx @biomejs/biome check ui-assets tools` (no NEW warnings vs baseline; format with `--write`: 2-space indent, double quotes, trailing commas), `node tools/boot-smoke.mjs`, `node tools/formula-ast-test.mjs`, `node tools/metrics-store-test.mjs`.
- Comments explain constraints/why at each file's density; no change-narration.
- Never push.

---

### Task 1: Server — `grid_frequency` via the Frequency formula; `main_meter_id` off the payload

**Files:**
- Modify: `src/ui/loopback.rs` (new forwarder + publisher; replace the stale frequency comment block at ~267–276)
- Modify: `src/ui/handlers/topology.rs` (drop `main_meter_id` field, ~lines 37 and 114)
- Modify: `src/ui/tests.rs` (~line 362: drop the `main_meter_id` assertion)
- Test: `tests/ui_http.rs` (new e2e)

**Interfaces:**
- Produces: `microgrid_sample` stream `grid_frequency` (quantity `"Frequency"`, unit `"Hz"`) on latest/history/WS — Task 3's client reads it generically. The topology payload no longer carries `main_meter_id`.
- Leaves alone (Task 2's job): `MicrogridSite::main_meter_id()` and every scenario-journal use.

- [ ] **Step 1: Write the failing e2e test**

Append to `tests/ui_http.rs` — a multi-feeder site (two meters under the grid), exactly the shape the old main-meter path went dead on:

```rust
/// grid_frequency streams via the logical meter's Frequency formula
/// (a COALESCE over the PCC's meters) — usable since
/// frequenz-microgrid 0.6.0 wired the Frequency sender arm. The
/// topology here hangs TWO meters under the grid: the shape where
/// the old main-meter workaround returned None and the frequency
/// stream silently died. The topology payload also no longer
/// carries the retired main_meter_id flag.
#[tokio::test(flavor = "multi_thread")]
async fn grid_frequency_streams_on_a_multi_feeder_site() {
    let topology = r#"
(%make-grid-connection-point :id 1
    :successors
    (list (%make-meter :id 2
                        :successors
                        (list (%make-solar-inverter :id 3)))
          (%make-meter :id 4
                       :successors
                       (list (%make-battery-inverter :id 5
                                                       :successors
                                                       (list (%make-battery :id 6)))))))
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

    let topo = json(&client, format!("{}/api/mg/{id}/topology", s.ui_url)).await;
    assert!(
        topo.get("main_meter_id").is_none(),
        "main_meter_id should be retired from the payload: {topo}"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let snapshot = loop {
        let body = json(
            &client,
            format!("{}/api/mg/{id}/microgrid/latest", s.ui_url),
        )
        .await;
        let converged = body["grid_frequency"]["value"]
            .as_f64()
            .is_some_and(|v| v.is_finite());
        if converged || tokio::time::Instant::now() >= deadline {
            break body;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    let entry = &snapshot["grid_frequency"];
    let hz = entry["value"]
        .as_f64()
        .unwrap_or_else(|| panic!("grid_frequency never converged: {snapshot}"));
    // The OU frequency model mean-reverts around 50 Hz; anything in a
    // generous grid band proves real data, not a fabricated zero.
    assert!((45.0..=55.0).contains(&hz), "implausible Hz {hz}: {snapshot}");
    assert_eq!(entry["quantity"], "Frequency", "{snapshot}");
    assert_eq!(entry["unit"], "Hz", "{snapshot}");
    assert!(
        snapshot.get("grid_frequency_energy").is_none(),
        "unexpected energy companion: {snapshot}"
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

`cargo test --test ui_http grid_frequency_streams 2>&1 | tee "$SCRATCH/f1.log"; grep -E "test result|panicked" "$SCRATCH/f1.log"` — expected: FAIL (`main_meter_id` still present, then `grid_frequency never converged`).

- [ ] **Step 3: Implement**

`src/ui/loopback.rs`:
- Add `Frequency` to the `quantity::{…}` import (line ~15).
- Replace the stale comment block (~267–276, "Grid frequency via `lm.grid::<metric::AcFrequency>()` would be the natural way … Until that lands, frequency stays on the per-component path") with the real call, placed with the other lm-based forwarders (before the BatteryPool block):

```rust
    // Site frequency via the grid Frequency formula — a COALESCE over
    // the meters feeding the PCC, so any site shape works (the old
    // main-meter workaround required exactly one grid → one meter).
    // Wired since frequenz-microgrid 0.6.0 added the Frequency
    // sender arm.
    if let Some(h) = subscribe_frequency_forwarder(
        "grid_frequency",
        lm.grid::<metric::AcFrequency>(),
        site,
        state.clone(),
    )
    .await
    {
        handles.push(h);
    }
```

- Add the forwarder + publisher, mirroring `subscribe_reactive_forwarder` / `publish_reactive` exactly (same skip/log/lag handling, same doc style):

```rust
/// Subscribe to the Frequency-valued grid formula and spawn a
/// forwarder that pushes each `Sample<Frequency>` onto the
/// MicrogridSite event bus as a `MicrogridSample { stream,
/// quantity: "Frequency", unit: "Hz", .. }`. Mirrors the reactive
/// forwarder; no energy hook — energy_stream_for never maps it.
async fn subscribe_frequency_forwarder(
    stream: &'static str,
    formula: Result<frequenz_microgrid::Formula<Frequency>, frequenz_microgrid::Error>,
    site: &MicrogridSite,
    state: SharedMicrogrid,
) -> Option<JoinHandle<()>> {
    /* body copied from subscribe_reactive_forwarder with
       publish_frequency in place of publish_reactive */
}

fn publish_frequency(
    stream: &'static str,
    sample: Sample<Frequency>,
    site: &MicrogridSite,
    state: &SharedMicrogrid,
) {
    let value = sample.value().map(|q| q.as_hertz());
    let ts_ms = /* same ts extraction as publish_reactive */;
    publish_scalar(stream, "Frequency", "Hz", value, ts_ms, site, state);
}
```

If the duplication across power/reactive/frequency forwarders is trivially foldable into one generic helper without fighting the type-level differences, fold it; if it needs type gymnastics, keep the third copy — the existing two copies chose duplication deliberately (see the comment on `subscribe_reactive_forwarder`).

`src/ui/handlers/topology.rs`: delete the `main_meter_id` struct field (~37) and its `site.main_meter_id()` population (~114), plus any doc comment on the field. `src/ui/tests.rs` ~362: delete the `assert_eq!(v["main_meter_id"], 2)` line (and surrounding comment if it only serves it).

- [ ] **Step 4: Verify**

`cargo test --test ui_http 2>&1 | tee "$SCRATCH/f1b.log"; grep "test result" "$SCRATCH/f1b.log"` then the full gate (`clippy -D warnings`, `fmt --check`, `cargo test` teed). `src/ui/tests.rs` runs under `cargo test --lib`.

- [ ] **Step 5: Commit**

```bash
git add src/ui/loopback.rs src/ui/handlers/topology.rs src/ui/tests.rs tests/ui_http.rs
git commit -m "Stream grid frequency from the loopback" -m "frequenz-microgrid 0.6.0 carries the Frequency sender arm the old
comment said was missing, so grid_frequency becomes a real stream via
the grid Frequency formula and works on any site shape. The
main_meter_id flag leaves the topology payload; the sim-side method
stays until the scenario reporter re-keys in the next commit.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 2: Server — scenario metrics re-key onto the grid formula streams

**Files:**
- Modify: `src/sim/scenario.rs` (journal: delete `record_sample` + `record_main_meter_pq`; add grid-sample methods; rename fields; rewrite the unit tests at ~540–570)
- Modify: `src/sim/microgrid_site/history.rs` (~140–160: drop the `record_sample` loop's main-meter feed, the `main_meter_pq` pass, and the `main_id` derivation)
- Modify: `src/sim/microgrid_site/mod.rs` (delete `main_meter_id()` ~339 and its tests ~1893–1904; add the loopback-facing hook)
- Modify: `src/sim/microgrid_site/scenarios.rs` (report renames; drop `main_meter_id` field ~63 and its population ~369)
- Modify: `src/ui/loopback.rs` (`publish_scalar` feeds the journal for the two grid streams)
- Modify: `ui-assets/panels.js` (~713), `src/bin/swctl.rs` (~1367, ~1402)
- Test: `tests/scenario.rs` (rename reads ~97–169), `tests/ui_http.rs` (~114: report `main_meter_id` assertion)

**Interfaces:**
- Produces: report JSON fields `peak_grid_w`, `peak_grid_var`, `grid_window_averages` (shape of each entry unchanged), `site_pf_at_peak_var` (name kept); `main_meter_id` gone from the report. New site hook `MicrogridSite::record_grid_sample(kind, value, ts)` (exact name/signature is the implementer's; loopback + journal must agree).
- Consumes: Task 1's loopback state (`publish_scalar` signature unchanged).

- [ ] **Step 1: Red — flip the tests first**

- `tests/scenario.rs`: change every `r["peak_main_meter_w"]` read (~97, 112, 128, 169) to `r["peak_grid_w"]`.
- `tests/ui_http.rs` ~114: replace `assert_eq!(report["main_meter_id"], 2)` with `assert!(report.get("main_meter_id").is_none(), "{report}")` and, beside the existing peak read in that test (if any), use the new names.
- `src/sim/scenario.rs` unit tests (~540–570): rewrite against the new journal surface, preserving what they prove — peak tracking, window-average bucketing/eviction, not-running short-circuit, and PF-pairing:

```rust
    #[test]
    fn grid_peak_and_windows_track_grid_power_samples() {
        let mut j = ScenarioJournal::default();
        j.start("s", ts(0));
        j.record_grid_power(3000.0, ts(100));
        j.record_grid_power(9000.0, ts(300));
        j.record_grid_power(4000.0, ts(800));
        assert_eq!(j.peak_grid_active_w(), 9000.0);
    }

    #[test]
    fn grid_pq_pairs_last_p_with_peak_q() {
        let mut j = ScenarioJournal::default();
        j.start("s", ts(0));
        j.record_grid_power(8000.0, ts(100));
        j.record_grid_reactive(6000.0, ts(101));
        j.record_grid_power(2000.0, ts(200));
        j.record_grid_reactive(-1000.0, ts(201)); // |Q| smaller: not a new peak
        assert_eq!(j.peak_grid_pq(), Some((8000.0, 6000.0)));
    }

    #[test]
    fn grid_samples_ignored_outside_the_running_window() {
        let mut j = ScenarioJournal::default();
        j.record_grid_power(9000.0, ts(100));
        assert_eq!(j.peak_grid_active_w(), 0.0);
    }
```

(Adapt to the file's existing `ts()` helper, journal constructor, and start/stop API — read the surrounding tests first; keep their conventions.)

- [ ] **Step 2: Run to verify red**

`cargo test --lib sim::scenario 2>&1 | tee "$SCRATCH/f2.log"; grep -E "test result|error" "$SCRATCH/f2.log"` — compile errors for the missing methods count as red.

- [ ] **Step 3: Implement**

`src/sim/scenario.rs`:
- Rename `peak_main_meter_active_w` → `peak_grid_active_w`, `peak_main_meter_pq` → `peak_grid_pq` (fields, getters, reset sites), updating doc comments (they describe the grid formula stream now, resampled at ~1 Hz by the loopback).
- Delete `record_sample` (its whole body was main-meter work) and `record_main_meter_pq`.
- Add, with the not-running short-circuit both old methods had:

```rust
    /// Hand one loopback `grid_power` sample (the LM grid formula's
    /// site import/export) to the reporter: peak-so-far, the
    /// 15-minute window averages, and the P side of the PF-at-peak
    /// pair. ~1 Hz resampled — coarser than the raw telemetry the
    /// retired main-meter path read, accepted in the spec addendum.
    pub fn record_grid_power(&mut self, value: f32, now: DateTime<Utc>) {
        if !self.is_running() {
            return;
        }
        let v = value as f64;
        if v > self.peak_grid_active_w {
            self.peak_grid_active_w = v;
        }
        let window_start = (now.timestamp() / WINDOW_AVG_LENGTH_S) * WINDOW_AVG_LENGTH_S;
        let entry = self.window_avgs.entry(window_start).or_insert((0.0, 0));
        entry.0 += v;
        entry.1 += 1;
        while self.window_avgs.len() > WINDOW_AVG_CAPACITY {
            self.window_avgs.pop_first();
        }
        self.last_grid_p = Some(v);
    }

    /// The Q twin: kept only while the |Q|-peak-so-far updates, so the
    /// stored pair is the last-seen P alongside the largest |Q| — the
    /// same last-P pairing the UI's PF chips use. Q samples arriving
    /// before any P sample can't form a pair and are skipped.
    pub fn record_grid_reactive(&mut self, value: f32, _now: DateTime<Utc>) {
        if !self.is_running() {
            return;
        }
        let Some(p) = self.last_grid_p else { return };
        let q = value as f64;
        let is_new_peak = match self.peak_grid_pq {
            Some((_, prev_q)) => q.abs() > prev_q.abs(),
            None => true,
        };
        if is_new_peak {
            self.peak_grid_pq = Some((p, q));
        }
    }
```

  plus the `last_grid_p: Option<f64>` field (reset in `start`).

`src/sim/microgrid_site/history.rs`: delete the `journal.record_sample(…)` loop, the `main_meter_pq` block, and the `main_id` computation feeding them (keep `record_battery_sample` / `record_pv_sample` / `advance_sample_cursor` untouched). Delete whatever now-unused code computed the main-meter (p, q) snapshot pair.

`src/sim/microgrid_site/mod.rs`: delete `main_meter_id()` (~339, including its doc) and its two tests (~1893–1904). Add the hook the loopback calls (site-side so the `scenario` lock stays private):

```rust
    /// Feed one loopback grid-formula sample to the scenario journal.
    /// Called from the UI loopback's publish path — the reporter's
    /// site metrics read the same microgrid-rs streams the metrics
    /// panel shows, not a raw meter.
    pub fn record_grid_power_sample(&self, value: f32, ts: DateTime<Utc>) {
        self.inner.scenario.write().record_grid_power(value, ts);
    }
    pub fn record_grid_reactive_sample(&self, value: f32, ts: DateTime<Utc>) {
        self.inner.scenario.write().record_grid_reactive(value, ts);
    }
```

`src/ui/loopback.rs` `publish_scalar` — after the latest/history writes, feed the journal (convert `ts_ms`; skip unparseable timestamps):

```rust
    // The scenario reporter's site metrics ride the same two grid
    // formula streams the panel charts — fed here so the journal
    // needs no main-meter special case.
    if let Some(v) = value {
        if stream == "grid_power" || stream == "grid_reactive_power" {
            if let Some(ts) = chrono::DateTime::from_timestamp_millis(ts_ms) {
                if stream == "grid_power" {
                    site.record_grid_power_sample(v, ts);
                } else {
                    site.record_grid_reactive_sample(v, ts);
                }
            }
        }
    }
```

`src/sim/microgrid_site/scenarios.rs`: rename the struct fields (`peak_main_meter_w` → `peak_grid_w`, `peak_main_meter_var` → `peak_grid_var`, `main_meter_window_averages` → `grid_window_averages`), delete the `main_meter_id` field + its population, update the assembly at ~316–369 and every doc comment. `site_pf_at_peak_var` keeps its name and derivation.

`ui-assets/panels.js` ~713: `report.peak_main_meter_w` → `report.peak_grid_w`; grep panels.js for `main_meter_window_averages`/`peak_main_meter_var`/`main_meter_id` and rename/remove those reads too. `src/bin/swctl.rs` ~1367/~1402: same renames (grep the whole file for `main_meter`).

- [ ] **Step 4: Verify**

Full gate: `cargo clippy --all-targets -- -D warnings && cargo fmt --check && cargo test 2>&1 | tee "$SCRATCH/f2b.log"; grep "test result" "$SCRATCH/f2b.log"`. `tests/scenario.rs` exercises live scenarios — if a peak assertion now races the 1 Hz stream cadence, extend that test's existing wait/poll pattern rather than weakening the assertion. Then JS gates + `grep -rn "main_meter" src/ ui-assets/ tools/ tests/` — every survivor must be justified in the report (expected: none).

- [ ] **Step 5: Commit**

```bash
git add src/sim/scenario.rs src/sim/microgrid_site/history.rs src/sim/microgrid_site/mod.rs src/sim/microgrid_site/scenarios.rs src/ui/loopback.rs src/bin/swctl.rs ui-assets/panels.js tests/scenario.rs tests/ui_http.rs
git commit -m "Re-key scenario site metrics onto the grid formula streams" -m "The journal's peak, PF-at-peak, and window-average metrics now
consume the loopback's grid_power / grid_reactive_power samples via a
site hook, so the main-meter concept dies entirely - method, payload
flag, and report field. Report fields rename to grid terms and the
readers follow. Accepted: 1 Hz resampled peaks and a short blind
window during loopback rebuilds, per the spec addendum.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 3: Client — delete the gridFrequency synthesis; GCP inspector Charts-only

**Files:**
- Modify: `ui-assets/chrome.js` (delete the `gridFrequency` module + header-comment rewrite)
- Modify: `ui-assets/routing.js` (drop import + `applyTopology` call)
- Modify: `ui-assets/repl.js` (drop import + `applySample` call)
- Modify: `ui-assets/metrics-panel.js` (drop import + `gridFrequency.backfill()` call)
- Modify: `ui-assets/metrics-store.js` (delete `resetStream` — its only caller was the feeder)
- Modify: `tools/metrics-store-test.mjs` (drop the `resetStream` assertions; keep the rest of that block coherent)
- Modify: `ui-assets/inspect.js` (grid category: Charts card only, charting the site stream)

**Interfaces:**
- Consumes: Task 1's `grid_frequency` stream (arrives as a plain `microgrid_sample`, so `metricsStore.applySample` in repl.js already ingests it with zero special-casing; `metricsStore.backfill()` now restores it from `/microgrid/history` like every other stream — the old 600 s cap note dies with the feeder).
- Produces: nothing new for later tasks; Task 5 asserts the GCP card set.

- [ ] **Step 1: Demolition**

- `chrome.js`: delete the whole `gridFrequency` IIFE and the frequency paragraphs of the module header (keep clock + pulse bar). Drop the now-unused `metricsStore` import if nothing else in the file uses it.
- `routing.js`: drop `gridFrequency` from the chrome.js import; delete `gridFrequency.applyTopology(data)` in `refreshTopology`.
- `repl.js`: drop the import; delete `gridFrequency.applySample(ev)` from the `kind === "sample"` dispatch.
- `metrics-panel.js`: drop the import; delete `gridFrequency.backfill()` in `render()` (the store backfill alone now covers frequency).
- `metrics-store.js`: delete `resetStream` and its doc comment. `tools/metrics-store-test.mjs`: remove the resetStream asserts; the surrounding ring tests must still pass unchanged.

- [ ] **Step 2: GCP inspector**

In `inspect.js`, branch on `d.category === "grid"` where the card markup is assembled (~line 368's template): the grid renders ONLY a Charts card (open by default — it is the only card), no Component/Power/Setpoints cards and no setpoints fetch/log wiring for grid selections. The Connections fold may stay (it is topology, not telemetry — keep it; it costs nothing and answers "what hangs off the PCC").

The grid Charts card charts the site `grid_frequency` stream from the metrics store — the sim's Grid publishes no per-component telemetry, which is why the old per-component fetch drew nothing:

```js
import { metricsStore } from "./metrics-store.js";

// The GCP has no per-component telemetry (the sim's Grid publishes
// none by design) — its one chart is the site grid_frequency stream,
// the same source the metrics panel's Frequency card reads.
async function buildGridFrequencyChart(container) {
  const slot = document.createElement("div");
  slot.className = "chart";
  container.appendChild(slot);
  await metricsStore.backfill();
  const series = () => {
    const { xs, ys } = metricsStore.series("grid_frequency", 600);
    return [xs, ys];
  };
  const opts = {
    width: slot.clientWidth || 280,
    height: 140,
    title: "Frequency (site) (Hz)",
    cursor: { drag: { x: false, y: false } },
    legend: { show: false },
    scales: { x: { time: true } },
    axes: [
      { stroke: "#7d848e", grid: { stroke: "#353a45", width: 0.5 } },
      { stroke: "#7d848e", grid: { stroke: "#353a45", width: 0.5 }, size: 60 },
    ],
    series: [{}, { stroke: "#79b8ff", width: 1.5, points: { show: false }, spanGaps: false }],
  };
  const plot = new uPlot(opts, series(), slot);
  let queued = false;
  const unsub = metricsStore.subscribe(() => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(() => {
      queued = false;
      plot.setData(series());
    });
  });
  return () => {
    unsub();
    plot.destroy();
  };
}
```

Wire the returned teardown into the inspector's existing per-selection teardown path (the generation-guarded chart lifecycle around `liveCharts`/`clear()` — read it and match: the teardown must run on selection change AND panel close, exactly once). The store's subscriber list survives panel-open state, and repl.js feeds `applySample` unconditionally, so the chart grows live even with the metrics panel closed.

- [ ] **Step 3: Verify**

JS gates (boot-smoke especially — the import graph changed) + `grep -rn "gridFrequency\|resetStream\|main_meter" ui-assets tools` → zero hits. Live check against a running server: select the GCP → only Charts (+ Connections) card, frequency chart draws with data and grows; select a battery → the full card set is unchanged; metrics panel Frequency card shows data on a **multi-feeder** site (run `examples/grid-diamond-silent-leg.lisp` — the previously-dead shape).

- [ ] **Step 4: Commit**

```bash
git add ui-assets/chrome.js ui-assets/routing.js ui-assets/repl.js ui-assets/metrics-panel.js ui-assets/metrics-store.js ui-assets/inspect.js tools/metrics-store-test.mjs
git commit -m "Read frequency from the stream and slim the GCP inspector" -m "The client-side gridFrequency synthesis dies - grid_frequency now
arrives as a plain microgrid_sample stream on any site shape. The
grid connection point's inspector renders only a Charts card whose
one chart is that site stream, since the sim's Grid deliberately
publishes no per-component telemetry.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 4: Client — independent floating panels + header-safe drag

**Files:**
- Modify: `ui-assets/side-panel.js` (cascade placement, drag clamp, open-time position sanitizing)
- Modify: `ui-assets/style.css` (dock → positioning context; panels absolute; per-panel scroll)

**Interfaces:**
- Consumes: the existing tenant API (`openPanel`/`closePanel`/`openStack`, `sw-panel-pos-<name>` storage). No API changes — every tenant keeps working untouched.

**Requirements (behavior, verified live in Step 2):**

1. **Independence:** with any two (or three) panels open, opening/closing/re-rendering one never changes another's height or width. Implementation direction: `#panel-dock` stops being a flex column and becomes a positioning context covering the canvas area (`pointer-events: none`; panels re-enable `pointer-events: auto`); each `.float-panel` is `position: absolute`, anchored top-right, with `max-height` bounded by the dock (viewport minus chrome) and `overflow-y: auto` on `.panel-content` so a tall panel scrolls internally instead of growing past the viewport.
2. **Cascade:** a panel opening without a stored position takes a small stagger offset from the panels already open (e.g. `top = 12 + 32·k` where k is its position in the open order) so simultaneous panels never open perfectly stacked. Stored drag positions still apply on top as today.
3. **Drag clamp:** during drag, the grab strip cannot go under the fixed header — clamp `dy` so the strip's top stays at or below the header's bottom edge (measure the header at drag start, don't hardcode 48) — nor off the other three viewport edges (keep the existing 80 px horizontal margins).
4. **Open-time sanitize:** `openPanel` measures the panel after render; if the strip would sit above the header's bottom, below the viewport, or horizontally off-screen, clamp the stored `(dx, dy)` back into view and persist the corrected value. A user who dragged a panel into an unreachable spot gets it back on the next toggle.
5. The inspector's static `#inspector` panel participates identically (it is just another `.float-panel`).
6. **Height resize (user request, added mid-run; model revised after user acceptance testing):** the gripper sets a persisted **max-height cap** (`sw-panel-size-<name>`), while the panel's height is otherwise always content-hugging: `height: auto` under `max-height: min(cap, dock)`. Consequences the user asked for by name: a panel can never be stretched past its content (on gesture end the manual height converts to the cap and the panel snaps to content size); expanding/collapsing a card auto-grows/shrinks the panel up to the cap and dock bounds; shrinking below content scrolls `.panel-content`. The conversion happens after the resize gesture ends (not per-frame, which would fight the native drag); the cap is restored on open and sanitized against the viewport.
7. **Drag floor is the dock's top edge (revised):** clamping against the `<header>` proved insufficient — panels could still be dragged under the pulse bar ("setpoints/5s") and the microgrid header below it. The dock's own top edge sits below all chrome by construction; the drag clamp and the open-time sanitize both floor the strip at the dock's top, measured live.

Keep the diff minimal — this is a layout change plus two clamps, not a shell rewrite. Update the side-panel.js header comment and the style.css dock comment (they describe the column dock).

- [ ] **Step 1: Implement** per the requirements above.

- [ ] **Step 2: Verify live** (Playwright or manual against a running server), asserting at least:
  - open metrics panel → note `getBoundingClientRect().height`; open formula panel → metrics panel height unchanged (±2 px) and formula panel offset by the cascade;
  - open inspector too → all three independent;
  - drag the metrics panel upward hard → strip stays below the header;
  - write a poisoned position (`localStorage["sw-panel-pos-metrics-btn"] = '{"dx":0,"dy":-999}'`), close + reopen → strip visible below the header and the stored value now sanitized;
  - close/reopen panels repeatedly → no drift, positions persist.

Also the JS gates (boot smoke, biome — no new warnings).

- [ ] **Step 3: Commit**

```bash
git add ui-assets/side-panel.js ui-assets/style.css
git commit -m "Float the panels independently and keep the drag strip reachable" -m "Open panels no longer share the dock column's height - each is an
absolute float with its own viewport-bounded scroll and a cascade
offset on open. The drag clamp keeps the grab strip below the header,
and every open sanitizes a stored position that would leave the strip
unreachable.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 5: Smoke + full gate

**Files:**
- Modify: `tools/ui-smoke/live-topology.mjs`

- [ ] **Step 1: Extend the smoke script** (existing conventions: `check`, `waitFor`, unguarded waits):
  - metrics panel: assert the Frequency fold-summary (`[data-summary="frequency"]`) shows an `Hz` value (not `—`) within a bounded wait;
  - GCP inspector: click the grid node → assert the inspector shows a Charts card with a `canvas`, and NO `#card-component` / `#card-power` / setpoints section;
  - panel independence: with the metrics panel open, record its height; open the formula panel; assert the metrics panel's height is unchanged (±2 px);
  - drag sanitize: poison `sw-panel-pos-metrics-btn` with `dy:-999` via `page.evaluate`, toggle the panel closed and open, assert the strip's `getBoundingClientRect().top` is at or below the header's bottom;
  - negative: `fetch("/api/mg/<id>/topology")` in-page → payload has no `main_meter_id` key.

- [ ] **Step 2: Full gate**, everything teed: the smoke script (expect the one pre-existing reactive-knob FAIL and nothing else), `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `cargo test`, biome (no new), boot-smoke, formula-ast, metrics-store tests.

- [ ] **Step 3: Commit**

```bash
git add tools/ui-smoke/live-topology.mjs
git commit -m "Cover the frequency stream, GCP card, and panel floats in the smoke" -m "Asserts the Frequency card carries data, the grid inspector slims to
its Charts card, open panels no longer resize each other, a poisoned
panel position self-heals on open, and main_meter_id is gone from the
topology payload.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

### Task 6: Inverter Power card — stop the Q envelope flicker

**Root cause (investigated live + by elimination):** the inspector's Q
bar lo/hi are fed by WS `reactive_power_*_bound_var` samples (which
can never be null — only recorded `Some` values are emitted), but
`applySnapshot` overwrites them with `snap.envelope.reactive`, which
is null for any component whose CHILDREN expose no Q bounds —
`reactive_setpoint_envelope` short-circuits on `child_env?` before
consulting the component's OWN `reactive_bounds()`. Every accepted
setpoint triggers a snapshot re-fetch, so under a 1 Hz control loop
the Q graduation (`.env-ends`) empties and refills once a second,
resizing the panel. The graduation VALUES moving with P is correct
physics (Q capability derives from P under a PF/apparent cap) and
stays.

**Files:**
- Modify: `src/ui/handlers/component.rs` (~217–219: envelope derivation falls back to own bounds)
- Modify: `ui-assets/inspect.js` (`applySnapshot` ~787–796: preserve live bounds when the snapshot has none)
- Modify: `ui-assets/style.css` (`.env-ends` row keeps its height when empty)
- Test: `src/ui/tests.rs` (component-snapshot envelope assertion)

- [ ] **Step 1: Failing test** — in `src/ui/tests.rs`, find the existing `/api/component` snapshot test (grep `envelope`); add/extend: for a battery inverter configured with a reactive capability (e.g. `:reactive-apparent-va`), `envelope.reactive` is non-null and brackets 0 (lo < 0 < hi). Run it red.

- [ ] **Step 2: Server fix** — in `component.rs`, the envelope population becomes "the effective window the UI should draw": setpoint envelope when one exists, else the component's own bounds:

```rust
        envelope: Envelope {
            active: envelope_tuple(
                site.active_setpoint_envelope(id)
                    .or_else(|| site.get(id).and_then(|c| c.effective_active_bounds())),
            ),
            reactive: envelope_tuple(
                site.reactive_setpoint_envelope(id)
                    .or_else(|| site.get(id).and_then(|c| c.reactive_bounds())),
            ),
        },
```

(Match the real accessor names — `effective_active_bounds` / `reactive_bounds` per `src/sim/microgrid_site/mod.rs:761/825`; if `site.get` isn't reachable from the handler, add a small site helper instead of widening visibility. The setpoint GATES themselves are untouched — this only changes what the snapshot reports for drawing.)

- [ ] **Step 3: Client hardening** — in `applySnapshot`, a null snapshot envelope must not clobber stream-fed bounds (same preservation the code already applies to `liveVal`):

```js
  const [rLo, rHi] = snap.envelope?.reactive ?? [null, null];
  liveState.axes.reactive.lo = rLo ?? prevAxes?.reactive.lo ?? null;
  liveState.axes.reactive.hi = rHi ?? prevAxes?.reactive.hi ?? null;
```

  (and the matching two lines on the active axis), updating the now-stale "envelope.reactive is null for every real topology" comment. In `style.css`, give `.env-ends` a `min-height` of one line so an empty ends row no longer changes the card's height (check the rule's current shape first; it may need `1em`/line-height units to match).

- [ ] **Step 4: Verify** — the new test green; full Rust gate; JS gates; live check on a site with an inverter under a 1 Hz setpoint loop (drive one with `(set-active-power …)` from the REPL or a scenario): the Q graduation holds steady while its values track P, and the panel height stays constant across seconds.

- [ ] **Step 5: Commit**

```bash
git add src/ui/handlers/component.rs src/ui/tests.rs ui-assets/inspect.js ui-assets/style.css
git commit -m "Keep the inspector's Q envelope steady under setpoint refreshes" -m "The component snapshot now reports the component's own bounds when
no child exposes any, so the accepted-setpoint re-fetch no longer
clobbers the stream-fed Q window with null - which emptied and
refilled the graduation once a second under a control loop and
resized the panel. The snapshot-null preservation and a fixed-height
ends row guard the remaining transients; the values still tracking P
is the capability physics, unchanged.

Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>"
```

---

## Self-review notes (already applied)

- Spec-addendum coverage: A → Task 1+3, B → Task 2, C → Task 3, D+E → Task 4, F → no task (documentation only); Task 5 pins A/C/D/E behavior.
- Task 1 leaves `main_meter_id()` alive for one commit (history.rs still calls it) — Task 2 deletes producer and consumers together, so each commit builds standalone.
- Type consistency: `record_grid_power`/`record_grid_reactive` (journal) vs `record_grid_power_sample`/`record_grid_reactive_sample` (site hook) vs the `publish_scalar` feed all match; `peak_grid_active_w()`/`peak_grid_pq()` getters match the scenarios.rs assembly renames.
- The metrics-store node test loses only the `resetStream` asserts; the ring/window tests stand.
