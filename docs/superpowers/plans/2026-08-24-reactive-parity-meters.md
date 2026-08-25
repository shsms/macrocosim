# Reactive Parity — Meters & Readout (Sub-project 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Meters carry reactive loads (VAr sources and power-factor derivation), Q becomes scriptable and recordable — drivers, scenario wrappers, query defuns, a reactive-bounds CSV, report stats — and the Python client reaches parity.

**Architecture:** `Meter` gains a reactive source slot (enum: `Var(DynamicScalar)` | `PowerFactor { pf, leading }`) beside `power_source`, with construction kwargs, trait doors, defuns, and the drive HTTP op extended to match; the #537 aggregation fix makes a P-overridden meter report its own Q source or its children's sum instead of zero. Readout mirrors the active side surface by surface.

**Tech Stack:** Rust, tulisp DSL, the existing scenario/CSV/report machinery, Python client (`python/src/switchyard/`).

**Spec:** `docs/superpowers/specs/2026-08-24-reactive-parity-design.md` (Decisions "Meters carry reactive loads", "Trait + DSL surface for meter Q", "Meter Q aggregation fix", "Readout", plus the PF conventions). Sub-project 1 is merged into this branch; sub-project 3 (UI/loopback) is NOT in this plan.

## Global Constraints

- Commit style: imperative subject, why-body, trailer exactly `Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>`; NO other trailers. Stage by name; never `.nfs*`; never `git add -A`/`.`.
- Gate per task: `cargo clippy --all-targets -- -D warnings && cargo fmt --check && cargo test --lib`; full `cargo test` before each task's final commit. Python task: the repo's Python checks if any exist (look for noxfile/pytest config under `python/`; if none, compile-check via `python3 -m compileall python/src`).
- Sign conventions: production negative, consumption positive; +Q inductive/lagging. The PF driver derives `Q = |P| · tan(acos(pf))`, negated when `leading`; `pf` is true cos φ, valid in `(0, 1]` (0 rejected — division-free but meaningless; 1 → Q = 0 allowed). `:reactive-pf-limit` remains the ratio k = |Q|/|P| — different meaning, documented. (Superseded 2026-08-25: `derive_pf_q` now uses **signed** P — `Q = P·tan(acos(pf))`, still negated when leading — so an exporting meter's lagging Q stays on the export's sign and the UI's sign-pair labeling agrees; the `|P|` wording here and in the three sites below records what this plan specified at the time.)
- Constructed-vs-poked freeze: runtime drivers never change what `constructor_kwargs` renders (mirror `constructed_power`).
- Floats render via `lisp_float32`; `:leading t` renders like `:hidden t`; new kwargs join the managed-file round-trip test.
- Comments in easy English, what things are.

## Pre-existing facts (current HEAD of `reactive-parity`)

- `Meter` (src/sim/meter.rs): fields `power_source: RwLock<Option<DynamicScalar>>`, `constructed_power: Option<f32>` (set in `new` via `filter(|s| !s.is_dynamic()).map(|s| s.get())`); `sum_children(site, value_fn)` shared walk (`:74-84`); `aggregate_reactive` returns 0.0 when `power_source` is set (`:93-101` — the #537 bug); `set_fixed_power` (`:107-109`); trait impls incl. `has_unrenderable_source` (`:198-205`) and `constructor_kwargs` (`:207-225`). `DynamicScalar` has `constant(v)`, `is_dynamic()`, `get()`, `refresh(ctx)`, `from_lisp(&TulispObject, fallback) -> Option<DynamicScalar>`.
- `MeterArgs` (src/lisp/make.rs:53-70): kwargs id/name/interval/power/successors/hidden/stream-jitter-pct/modes; `%make-meter` defun at `make.rs:265-302`.
- Drivers (src/lisp/defuns/load_drivers.rs): `set-meter-power` number/lambda/symbol dispatch (`:21-46`) — the pattern to copy; trait doors `set_active_power_override`/`set_active_power_source`/`takes_active_power_override` (src/sim/component.rs).
- Drive HTTP op (src/ui/handlers/control.rs): `DriveRequest { power_w, sunlight_pct, soc_pct }` (`:39-43`), gating via `takes_*` (`:169-186`), application (`:209-215`).
- Scenario wrappers (sim/scenarios.lisp): `drive-meter`/`drive-solar` build `:kind` plists (`:190-199`); `scenario--drive` dispatches on `:kind` (`:247-253`, `eq 'drive-solar` → `set-solar-sunlight`, else `set-meter-power`); `scenario--run` installs them (`:274-290`).
- Queries (src/lisp/defuns/queries.rs): `component-active-power` (`:49-58`), `component-bound-lower/upper` via `bound_edge` over `effective_active_bounds` (`:60-70`, helper at `:20-45`).
- CSV (src/sim/scenario_csv.rs): telemetry header incl. `reactive_power_var` already; `BOUNDS_CSV_HEADER = "ts_iso,lower_w,upper_w,bands\n"`; the bounds row writer collapses outer-hull (first-band lower / last-band upper) with a `|`-joined `bands` column (`:120-140`). `scenario_open_csv` (src/sim/microgrid_site/scenarios.rs:145-165) opens setpoints+bounds sinks for components with `effective_active_bounds().is_some()`; the sampler writes rows in src/sim/microgrid_site/history.rs (snapshot loop `:60-90`, `bounds_sinks` write further down; main-meter sampling near `:126-130` via `journal.record_sample(..., main_id, now)`).
- Report (src/sim/microgrid_site/scenarios.rs): `ScenarioReport` struct `:46-73` (`peak_main_meter_w`, built at `:296-350` from a journal accumulator `g` with `peak_main_meter_active_w()` and `window_avgs()`).
- Python (python/src/switchyard/build.py): `meter(...)` builder kwargs (`:394-420`, maps to the lisp kwargs); `class Meter.power -> DrivenSignal[Power]` (`:203-224`) with `read` via live telemetry, `set_` via the `"drive"` op `{"power_w": ...}`, `cue=(set-meter-power ...)`, `check_ref=(id, "active-power")`. Inverter builder already exposes `reactive_*` cap kwargs (`:436-462`).
- Managed-file round-trip test: `render_block_round_trips_through_a_fresh_config` (src/lisp/microgrid_file.rs).

---

### Task 1: the meter reactive source

**Files:**
- Modify: `src/sim/meter.rs`, `src/sim/component.rs` (trait doors), `src/lisp/make.rs` (`MeterArgs` + `%make-meter`)
- Test: `meter.rs` + `make.rs` test modules; extend the round-trip test fixture in `src/lisp/microgrid_file.rs`

**Interfaces:**
- Consumes: `DynamicScalar`, `lisp_float32`, existing meter fields.
- Produces (later tasks call these exactly):

```rust
// meter.rs
pub enum ReactiveSource {
    /// VArs directly: constant, lambda, or symbol.
    Var(DynamicScalar),
    /// Derive Q from this meter's live P: |P|·tan(acos(pf)), negated
    /// when leading. `pf` is true cos φ in (0, 1].
    PowerFactor { pf: f32, leading: bool },
}
// New Meter fields:
//   reactive_source: RwLock<Option<ReactiveSource>>,
//   constructed_reactive: Option<ConstructedReactive>,   // persistence freeze
// where:
enum ConstructedReactive { Var(f32), PowerFactor { pf: f32, leading: bool } }
// Meter::new gains one parameter:
pub fn new(id, interval, power_source: Option<DynamicScalar>,
           reactive_source: Option<ReactiveSource>,
           stream_jitter_pct, hidden) -> Self
// (constructed_reactive derived inside new: Var(s) if !s.is_dynamic() → Var(s.get());
//  PowerFactor passes through as constructed; dynamic Var → None.)
pub fn set_fixed_reactive_power(&self, vars: f32);            // → Var(constant)
pub fn set_power_factor_source(&self, pf: f32, leading: bool); // → PowerFactor

// component.rs trait additions (defaults shown):
fn set_reactive_power_override(&self, _vars: f32) -> bool { false }   // Meter: true
fn takes_reactive_power_override(&self) -> bool { false }             // Meter: true
fn set_reactive_power_source(&self, _scalar: DynamicScalar) {}        // Meter impl
fn set_power_factor(&self, _pf: f32, _leading: bool) -> bool { false } // Meter: true
```

**Behavior (binding):**
- `aggregate_reactive` becomes: own `reactive_source` if `Some` (`Var` → `scalar.get()`; `PowerFactor` → `derive_pf_q(self.aggregate_active(site), pf, leading)`), else `sum_children(site, aggregate_reactive_var)` — the P override no longer zeroes Q (todo #537). Add a small free fn `fn derive_pf_q(p: f32, pf: f32, leading: bool) -> f32` = `p.abs() * (pf.clamp(f32::MIN_POSITIVE, 1.0).acos()).tan() * if leading { -1.0 } else { 1.0 }` — clamp only as a NaN guard; construction validates the range.
- `refresh_inputs` refreshes the `Var` scalar too (PF needs no refresh).
- `has_unrenderable_source` ORs in: `constructed_reactive.is_none() && matches!(*reactive_source.read(), Some(ReactiveSource::Var(_)))` (a PF source installed at runtime is also unrenderable → cover it: `constructed_reactive.is_none() && reactive_source.read().is_some()`).
- `constructor_kwargs` appends, after `:power`: `ConstructedReactive::Var(v)` (finite) → `(":reactive-power", lisp_float32(v))`; `PowerFactor { pf, leading }` → `(":power-factor", lisp_float32(pf))` + `(":leading", "t")` when leading.
- `MeterArgs` gains `reactive_power<":reactive-power">: Option<LispValue>`, `power_factor<":power-factor">: Option<f64>`, `leading: Option<bool>`. `%make-meter` validation: `:reactive-power` together with `:power-factor` → `Error::invalid_argument("make-meter: :reactive-power and :power-factor are mutually exclusive")`; `:leading` without `:power-factor` → error; `:power-factor` outside `(0.0, 1.0]` → error naming the range. Build `ReactiveSource` accordingly (`Var` via `DynamicScalar::from_lisp(v, 0.0)` mirroring `:power`).
- `set_fixed_power`'s doc paragraph and the struct doc updated to mention the reactive slot.

- [ ] **Step 1: Failing tests** (write these, adapting helper shapes from each file's existing tests):

```rust
// meter.rs
#[test]
fn reactive_var_source_and_children_sum() {
    // Meter with :power override AND a Var reactive source reports the
    // Var value (not 0 — the #537 fix); one with a P override and NO
    // reactive source sums its children's Q.
}
#[test]
fn power_factor_source_tracks_live_p() {
    // PF 0.8 lagging on a meter with :power 8000 → Q ≈ 8000·tan(acos(0.8)) ≈ 6000;
    // leading → −6000; changing the P override moves Q on the next read.
}
#[test]
fn constructed_reactive_freezes_for_rendering() {
    // Built with :reactive-power 500 → kwargs contain it; a runtime
    // set_fixed_reactive_power(999) leaves kwargs unchanged.
    // Built plain + runtime PF source → has_unrenderable_source() true.
}
// make.rs
#[test]
fn meter_reactive_kwargs_validate() {
    // :reactive-power + :power-factor together → error;
    // :leading without :power-factor → error;
    // :power-factor 0 / 1.5 → error; :power-factor 1.0 → ok, Q = 0.
}
```

Also extend the round-trip fixture in `microgrid_file.rs` with `(%make-meter :id 10 :power 2000.0 :reactive-power 500.0)` and `(%make-meter :id 11 :power 2000.0 :power-factor 0.9 :leading t)` — byte-stability must hold.

- [ ] **Step 2: RED** (compile failures count for the new-param `Meter::new`; chase every constructor call site — grep `Meter::new`).
- [ ] **Step 3: Implement; green.**
- [ ] **Step 4: Full gate + full `cargo test`.**
- [ ] **Step 5: Commit** — `Give meters a reactive power source`.

---

### Task 2: drivers — defuns, scenario wrappers, the drive op

**Files:**
- Modify: `src/lisp/defuns/load_drivers.rs`, `sim/scenarios.lisp`, `src/ui/handlers/control.rs`
- Test: `load_drivers.rs` + `src/lisp/defuns/scenarios.rs` test modules; `src/ui/tests.rs` for the drive op

**Interfaces:**
- Consumes: Task 1's trait doors.
- Produces:
  - `(set-meter-reactive-power ID VALUE)` — number → `set_reactive_power_override`; lambda/symbol → `set_reactive_power_source` via `DynamicScalar::from_lisp(v, 0.0)`; mirrors `set-meter-power`'s dispatch and lenient-bool convention exactly (`load_drivers.rs:21-46` is the template — copy its shape, error wording pattern included).
  - `(set-meter-power-factor ID PF &optional LEADING)` — validates `0.0 < PF <= 1.0` (error naming the range), calls `set_power_factor(pf, leading)`; unknown id errors; non-meter is the lenient no-op this file's convention uses.
  - Scenario wrappers in `sim/scenarios.lisp` beside `drive-meter` (`:190-199`):

```lisp
(defun drive-meter-reactive (id source)
  "Drive section: feed meter ID reactive VArs from SOURCE (a constant,
a symbol, or a dynamic source like `timeline`). Compiles to
`set-meter-reactive-power`."
  (list :kind 'drive-meter-reactive :target id :source source))

(defun drive-meter-pf (id pf &optional leading)
  "Drive section: hold meter ID at power factor PF (cos phi, 0..1],
LEADING non-nil for capacitive. Compiles to `set-meter-power-factor`."
  (list :kind 'drive-meter-pf :target id :pf pf :leading leading))
```

  - `scenario--drive` (`:247-253`) grows the two arms (dispatch on `:kind`: `'drive-meter-reactive` → `set-meter-reactive-power`, `'drive-meter-pf` → `set-meter-power-factor` with `:pf`/`:leading`), keeping the existing else-is-`set-meter-power` fallback last.
  - `DriveRequest` (control.rs:39-43) gains `reactive_var: Option<f64>`, `power_factor: Option<f64>`, `leading: Option<bool>`; gating mirrors the existing pattern (`takes_reactive_power_override` for both new drives; `leading` without `power_factor` → the invalid-request error shape the file uses; `power_factor` range-checked (0,1]); application calls the trait doors. The "at least one field" check (`:193-195` area) includes the new fields.

- [ ] **Step 1: Failing tests**

```rust
// load_drivers.rs — mirror the existing lambda/symbol tests:
#[test] fn set_meter_reactive_power_accepts_number_lambda_symbol() { /* Var const + lambda via refresh_once + symbol re-deref; read via aggregate_reactive_var */ }
#[test] fn set_meter_power_factor_derives_from_live_p() { /* PF 0.8 on driven meter; leading flips sign; PF 1.5 errors */ }
// scenarios.rs — mirror the drive-meter plist test (:697-710):
#[test] fn reactive_drive_wrappers_tag_their_kind() { /* plist-get :kind / :target / :pf */ }
// ui/tests.rs — drive op:
#[tokio::test] async fn drive_op_accepts_reactive_var_and_power_factor() { /* POST drive {reactive_var}; {power_factor, leading}; a non-meter 4xx; pf out of range 4xx */ }
```

- [ ] **Step 2: RED; Step 3: implement; Step 4: full gate + full `cargo test`; Step 5: Commit** — `Drive meter reactive power from lisp, scenarios and the drive op`.

---

### Task 3: readout — queries, reactive-bounds CSV, report stats

**Files:**
- Modify: `src/lisp/defuns/queries.rs`, `src/sim/scenario_csv.rs`, `src/sim/microgrid_site/scenarios.rs`, `src/sim/microgrid_site/history.rs`
- Test: `queries.rs` tests; `tests/scenario.rs` or the microgrid_site test modules for CSV/report

**Interfaces:**
- Consumes: `reactive_bounds() -> Option<VecBounds>` (SP1), the journal accumulator `g` (find its type via `peak_main_meter_active_w`, scenarios.rs:320).
- Produces:
  - `(component-reactive-power ID)` → `aggregate_reactive_var` (mirror `component-active-power`, `queries.rs:49-58`).
  - `(component-reactive-bound-lower ID)` / `(component-reactive-bound-upper ID)` — mirror `bound_edge` (`queries.rs:20-45`) over `reactive_bounds()` first band; component without a Q axis → the same not-found-style error shape `bound_edge` uses for open bounds, message naming "no reactive envelope".
  - Reactive-bounds CSV: `CsvSink::open_reactive_bounds(dir, id)` writing `<id>-reactive-bounds.csv`, header `"ts_iso,lower_var,upper_var,bands\n"`, row writer identical in shape to the active one (outer hull + `|`-joined bands, `scenario_csv.rs:120-140`). `scenario_open_csv` opens it for components with `reactive_bounds().is_some()` (a fourth `CsvSinks` map + field, mirroring `scenario_bounds_csv` end to end — grep its uses: open, write in the history sampler loop, reset/close). The sampler writes from `c.reactive_bounds()` next to where it writes active bounds.
  - Report: `ScenarioReport` gains `pub peak_main_meter_var: f64` and `pub site_pf_at_peak_var: Option<f64>`. Tracking: in the history sampler's main-meter path, when the snapshot belongs to `main_id`, feed the journal a paired sample (add `record_main_meter_pq(p: f32, q: f32)` on the journal accumulator; it keeps `peak_var` by max |Q| and stores the paired P at that instant). At report build (`scenarios.rs:296-350`): `site_pf_at_peak_var = (peak pair) → |P| / (P² + Q²).sqrt()` (None before any sample or when both are 0). Serialize-only struct — additive fields are safe (swctl parses `serde_json::Value`).

- [ ] **Step 1: Failing tests**

```rust
// queries.rs
#[test] fn reactive_queries_mirror_active() { /* component-reactive-power on an inverter with an armed Q; bound queries return the caps band edges; a battery id errors on the bound query */ }
// scenario/CSV (place beside the existing bounds-CSV coverage — grep "-bounds.csv" in tests):
#[test] fn reactive_bounds_csv_records_the_live_q_envelope() { /* open recording on a site with an inverter; tick; the <id>-reactive-bounds.csv exists with header + a row whose edges match the caps band */ }
#[test] fn report_carries_peak_q_and_pf_at_peak() { /* drive main meter P and Q (set-meter-power + set-meter-reactive-power on the main meter), sample, report: peak_main_meter_var ≈ driven Q; site_pf_at_peak_var ≈ |P|/sqrt(P²+Q²) */ }
```

- [ ] **Step 2: RED; Step 3: implement; Step 4: full gate + full `cargo test`; Step 5: Commit** — `Read and record reactive power like active power`.

---

### Task 4: Python client parity

**Files:**
- Modify: `python/src/switchyard/build.py`
- Test: whatever harness exists under `python/` (check for pytest/nox; else `python3 -m compileall python/src` + a doc-example sanity block if the module has one)

**Interfaces:**
- Consumes: Task 1's kwargs, Task 2's drive-op fields and defuns.
- Produces:
  - `meter(...)` builder gains `reactive_power: Power | RawLisp | None = None`, `power_factor: float | None = None`, `leading: bool | None = None`, mapped into `args` exactly like the existing kwargs (`:394-420` — the args dict keys become the lisp kwarg names; follow how `power`/`hidden` map; docstring sentence each, including mutual exclusion which the server enforces).
  - `class Meter` gains, mirroring `power` (`:203-224`):

```python
@property
def reactive_power(self) -> DrivenSignal[ReactivePower]:
    """The meter's reactive power — read the telemetry, set the VAr load."""
    # read: live reactive power for the component (find the live-client
    # accessor next to active_power; if none exists for reactive, add it
    # following the same shape).
    # set_: control op "drive" with {"reactive_var": value...}
    # cue: (set-meter-reactive-power {id} {value})
    # check_ref: (self.component_id, "reactive-power")
```

  Use the `ReactivePower`/VAr quantity type the package already has (grep `ReactivePower|VoltAmpereReactive` under `python/src/switchyard/`; the inverter cap kwargs at `:436-462` show the existing conventions — if no quantity type exists, follow whatever `reactive_apparent_va` uses). A `power_factor` setter is NOT required (spec: kwargs + the reactive DrivenSignal twin); if a one-line `SettingSignal` fits the file's idiom, it may be added, otherwise skip.

- [ ] **Step 1: Write the change; verify with the Python check available; run one honest usage check** (e.g. a small `python3 -c` constructing `meter(reactive_power=..., power_factor=...)` and asserting the rendered lisp kwargs — find how builders render args to lisp and assert the two new kwargs appear).
- [ ] **Step 2: Full Rust gate untouched-check** (`cargo test --lib` once).
- [ ] **Step 3: Commit** — `Bring the Python meter builder to reactive parity`.

---

### Task 5: docs + todo

**Files:**
- Modify: `scenarios/README.md` (driver table gains `set-meter-reactive-power`, `set-meter-power-factor`, and the two `drive-*` wrappers; CSV section documents `<id>-reactive-bounds.csv`), `AGENTS.md` (meter description gains the reactive source; the k-vs-cosφ sentence already exists — verify it still reads correctly next to the now-real PF driver), `todo.org` via the org-tasks helper (mark #537 DONE with a one-line resolution: meters report their own Q source or children's sum; P overrides no longer zero Q)
- Test: full gate once

- [ ] **Step 1: Sweep + edit; Step 2: gate; Step 3: Commit** — `Document the meter reactive drivers`.

---

## Execution notes

- Task order is dependency order (1 → 2 → 3 → 4 → 5); Task 4 depends on 1+2 only.
- `Meter::new` gains a parameter in Task 1 — every constructor call site in tests updates mechanically (`None` for no reactive source).

## Self-review (done at plan-writing time)

- Spec coverage (SP2 scope): reactive source enum + kwargs + validation + persistence + freeze + unrenderable warning (T1), trait doors + defuns + scenario wrappers + drive op (T2), aggregation fix #537 (T1), query defuns + reactive-bounds CSV + report Q stats (T3), Python parity (T4), docs + #537 bookkeeping (T5). Not in scope: varh (spec: out), UI knobs (SP3), loopback (SP3).
- PF conventions match the spec exactly (cos φ in (0,1], `:leading t`, Q = |P|·tan(acos(pf)), +Q lagging default).
- Type consistency: `ReactiveSource`, `ConstructedReactive`, `set_fixed_reactive_power`, `set_power_factor_source`, trait doors `set_reactive_power_override/source`, `takes_reactive_power_override`, `set_power_factor(pf, leading)`, defun names, `record_main_meter_pq`, report fields `peak_main_meter_var`/`site_pf_at_peak_var` — used consistently across tasks.
