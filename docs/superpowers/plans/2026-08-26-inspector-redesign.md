# Component Inspector Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the floating component inspector live read-back (knob values, active setpoints with TTL, P/Q envelopes) on a redesigned sectioned panel, and split the triple-serving side panel into a shell with tenants.

**Architecture:** Server side, a new `GET /api/component` snapshot endpoint plus a `SiteEvent::KnobChanged` broadcast emitted from the knob-setter chokepoints, built on small read accessors added to `DynamicScalar`, `Meter`, `SolarInverter`, `TimeoutTracker`, and `PowerAxis`. Client side, a `side-panel.js` shell owns the floating card lifecycle with per-tenant teardown; `inspect.js` becomes the redesigned component-inspector tenant (Graph/Simulation segmented chips, envelope bars, edit-in-place knobs, folded-by-default charts).

**Tech Stack:** Rust (axum, tokio broadcast, tulisp), vanilla ES-module JS, uPlot, biome.

**Spec:** `docs/superpowers/specs/2026-08-25-inspector-redesign-design.md`

## Global Constraints

- Commit style: plain imperative sentence, no prefix (repo convention, e.g. "Give Config::watch a readiness signal for deterministic tests"). Every commit message ends with the two trailer lines the session mandates (`Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` and the `Claude-Session:` line).
- `git add` ONLY explicit file paths — never `-A`, `.`, `-u`, or `commit -a`. Never add `.nfs*` files.
- Rust gate per task: `cargo test` (workspace) must pass before each commit.
- JS gate per task: `npx @biomejs/biome check ui-assets/<touched files>` — gate only the files the task touches (`index.html`/`style.css` have known pre-existing failures per todo.org; do not gate on them, do not worsen them).
- Manual app check where a task says so: `cargo run --bin switchyard examples/berlin-demo.lisp`, then open the printed UI URL in a browser.
- UI visual vocabulary is fixed by the mocks (artifact "Switchyard Inspector") and `ui-assets/style.css` tokens: IBM Plex Sans/Mono, `--bg #1c2128`, `--bg-elev #262b34`, `--border #353a45`, `--muted #7d848e`, `--accent #79b8ff`, `--good #6fbf73`, `--bad #e58275`, `--standby #c4ad55`. No emoji, no new colors.
- Knob token vocabulary (used by the WS event, the snapshot payload, and the client): `meter-power`, `meter-reactive-power`, `meter-power-factor`, `solar-sunlight`, `reactive-pf-limit`, `reactive-apparent-va`.
- Capture test output per the user's convention: `cargo test 2>&1 | tee /tmp/claude-1000/-vagrant/7b03e942-e705-41cf-a259-d03eb2d9fd06/scratchpad/test-out.txt` and grep the file, so failure names survive.

---

### Task 1: DynamicScalar remembers its printed source

**Files:**
- Modify: `src/sim/dynamic_scalar.rs`
- Test: `src/sim/dynamic_scalar.rs` (existing `#[cfg(test)] mod tests`, lines 154+)

**Interfaces:**
- Consumes: nothing new.
- Produces: `DynamicScalar::source_text(&self) -> Option<String>` — the Lisp source of an expression-driven scalar, printed once at construction; `None` for constants. Task 2 and Task 6 rely on it.

Rationale: the stored `TulispObject` must only be touched under the interpreter lock, but the snapshot handler runs on an axum thread. Printing once at construction (`TulispObject` implements `Display`; same pattern as `src/lisp/overrides.rs:296`) sidesteps that entirely.

- [ ] **Step 1: Write the failing test** (append to the existing tests module in `dynamic_scalar.rs`)

```rust
#[test]
fn constant_has_no_source_text() {
    let s = DynamicScalar::constant(42.0);
    assert_eq!(s.source_text(), None);
}

#[test]
fn from_lisp_captures_printed_source() {
    let mut ctx = TulispContext::new();
    let obj = ctx.eval_string("'(lambda () 5)").unwrap();
    // from_lisp receives the evaluated object; mirror however the
    // neighbouring tests in this module build one.
    let s = DynamicScalar::from_lisp(&mut ctx, obj).unwrap();
    let text = s.source_text().expect("dynamic scalar has source text");
    assert!(text.contains("lambda"), "got: {text}");
}
```

(Adapt the construction boilerplate to match the existing tests in this module — they already build a `TulispContext` and objects; copy their setup rather than inventing new helpers.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p switchyard dynamic_scalar 2>&1 | tee <scratchpad>/t1.txt`
Expected: FAIL — `source_text` not found.

- [ ] **Step 3: Implement**

In `DynamicScalar`, add a field and getter, and populate it in every expression-taking constructor (`from_eval`, `from_funcall`, `from_lisp` — whichever of these store a `Source`):

```rust
pub struct DynamicScalar {
    cached: AtomicU32,
    source: Option<Source>,
    /// Printed Lisp form of `source`, captured at construction so
    /// read-back never touches the TulispObject off the interpreter
    /// lock. None for constants.
    source_text: Option<String>,
}

// in each constructor that stores a source object `obj`:
source_text: Some(obj.to_string()),
// in constant():
source_text: None,

pub fn source_text(&self) -> Option<String> {
    self.source_text.clone()
}
```

`set(v)` (the constant-override path) must also clear `source_text` if it clears `source`; keep the two fields in lockstep everywhere `source` is assigned.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p switchyard dynamic_scalar`
Expected: PASS (all tests in the module).

- [ ] **Step 5: Commit**

```bash
git add src/sim/dynamic_scalar.rs
git commit -m "Capture a DynamicScalar's printed Lisp source at construction"
```

---

### Task 2: Knob read-back accessors on components

**Files:**
- Modify: `src/sim/component.rs` (trait + reading types), `src/sim/meter.rs`, `src/sim/inverter/solar_inverter.rs`
- Test: `src/lisp/defuns/load_drivers.rs` (existing `#[cfg(test)]` module, lines 164+)

**Interfaces:**
- Consumes: Task 1's `DynamicScalar::source_text()`.
- Produces (all in `src/sim/component.rs`, used by Task 6):

```rust
#[derive(Clone, Debug)]
pub struct ScalarReading { pub value: f32, pub expr: Option<String> }

#[derive(Clone, Debug)]
pub enum ReactiveReading {
    Var(ScalarReading),
    PowerFactor { pf: f32, leading: bool },
}

// on trait SimulatedComponent, defaulted so only meters/solar override:
fn meter_power_reading(&self) -> Option<ScalarReading> { None }
fn meter_reactive_reading(&self) -> Option<ReactiveReading> { None }
fn sunlight_reading(&self) -> Option<ScalarReading> { None }
// (reactive pf-limit / apparent-va read-back already exists:
//  SimulatedComponent::reactive_capability() -> Option<ReactiveCapability>,
//  component.rs:574 — do NOT duplicate it.)
```

- [ ] **Step 1: Write the failing tests** (in `load_drivers.rs` tests, using the existing `crate::lisp::test_support::config_with` helper the sibling tests use)

```rust
#[test]
fn meter_power_reading_round_trips_constant_and_expr() {
    let (cfg, _dir) = config_with("(%make-meter :id 7)");
    cfg.eval("(set-meter-power 7 1500)").unwrap();
    let site = cfg.site();
    let c = site.get(7).unwrap();
    let r = c.meter_power_reading().expect("reading");
    assert_eq!(r.value, 1500.0);
    assert_eq!(r.expr, None);

    cfg.eval("(set-meter-power 7 (lambda () 25))").unwrap();
    cfg.refresh_once();
    let r = site.get(7).unwrap().meter_power_reading().expect("reading");
    assert_eq!(r.value, 25.0);
    assert!(r.expr.as_deref().unwrap_or("").contains("lambda"));
}

#[test]
fn meter_power_factor_reading_reports_pf_and_leading() {
    let (cfg, _dir) = config_with("(%make-meter :id 7)");
    cfg.eval("(set-meter-power 7 1000)").unwrap();
    cfg.eval("(set-meter-power-factor 7 0.9 t)").unwrap();
    let site = cfg.site();
    match site.get(7).unwrap().meter_reactive_reading() {
        Some(ReactiveReading::PowerFactor { pf, leading }) => {
            assert!((pf - 0.9).abs() < 1e-6);
            assert!(leading);
        }
        other => panic!("expected PowerFactor, got {other:?}"),
    }
}

#[test]
fn sunlight_reading_reads_back_percentage() {
    let (cfg, _dir) = config_with("(%make-solar-inverter :id 4)");
    cfg.eval("(set-solar-sunlight 4 63)").unwrap();
    cfg.refresh_once();
    let site = cfg.site();
    let r = site.get(4).unwrap().sunlight_reading().expect("reading");
    assert_eq!(r.value, 63.0);
}
```

(Match the exact make-form spellings and eval/refresh helpers used by the neighbouring tests in `load_drivers.rs` — e.g. if they spell it `(make-solar :id 4 ...)` or call `cfg.refresh_once()` differently, follow them.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p switchyard load_drivers 2>&1 | tee <scratchpad>/t2.txt`
Expected: FAIL — methods not found.

- [ ] **Step 3: Implement**

`component.rs`: add the two types and the three defaulted trait methods (block above).

`meter.rs` (fields `power_source: RwLock<Option<DynamicScalar>>` line 52, `reactive_source: RwLock<Option<ReactiveSource>>` line 57):

```rust
fn meter_power_reading(&self) -> Option<ScalarReading> {
    self.power_source.read().unwrap().as_ref().map(|s| ScalarReading {
        value: s.get(),
        expr: s.source_text(),
    })
}

fn meter_reactive_reading(&self) -> Option<ReactiveReading> {
    self.reactive_source.read().unwrap().as_ref().map(|r| match r {
        ReactiveSource::Var(s) => ReactiveReading::Var(ScalarReading {
            value: s.get(),
            expr: s.source_text(),
        }),
        ReactiveSource::PowerFactor { pf, leading } => {
            ReactiveReading::PowerFactor { pf: *pf, leading: *leading }
        }
    })
}
```

`solar_inverter.rs` (field `sunlight_source: RwLock<DynamicScalar>` line 74):

```rust
fn sunlight_reading(&self) -> Option<ScalarReading> {
    let s = self.sunlight_source.read().unwrap();
    Some(ScalarReading { value: s.get(), expr: s.source_text() })
}
```

(Use the lock idiom the surrounding methods use — if the repo's `RwLock` is parking_lot, drop the `.unwrap()`.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p switchyard load_drivers`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/sim/component.rs src/sim/meter.rs src/sim/inverter/solar_inverter.rs src/lisp/defuns/load_drivers.rs
git commit -m "Add knob read-back accessors for meter and solar sources"
```

---

### Task 3: TimeoutTracker exposes remaining TTL

**Files:**
- Modify: `src/timeout_tracker.rs`
- Test: `src/timeout_tracker.rs` (existing tests module, or add one following the file's style)

**Interfaces:**
- Consumes: nothing new.
- Produces: `TimeoutTracker::remaining(&self, id: u64, axis: SetpointAxis) -> Option<Duration>` — time left before the (id, axis) setpoint expires; `None` when untracked or already past. Task 6 relies on it.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn remaining_reports_time_left_and_none_when_absent() {
    let t = TimeoutTracker::default();
    t.add(1, SetpointAxis::Active, Duration::from_secs(60));
    let left = t.remaining(1, SetpointAxis::Active).expect("tracked");
    assert!(left <= Duration::from_secs(60) && left > Duration::from_secs(58));
    assert_eq!(t.remaining(1, SetpointAxis::Reactive), None);
    assert_eq!(t.remaining(2, SetpointAxis::Active), None);
}
```

(If `TimeoutTracker` has no `Default`, construct it the way `add`'s existing tests do.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p switchyard timeout_tracker`
Expected: FAIL — `remaining` not found.

- [ ] **Step 3: Implement** (the map is `HashMap<(u64, SetpointAxis), Instant>`, line 47)

```rust
/// Time left before the (id, axis) setpoint expires. None when the
/// pair isn't tracked or the deadline already passed (the sweep will
/// remove it shortly).
pub fn remaining(&self, id: u64, axis: SetpointAxis) -> Option<Duration> {
    let map = self.deadlines.lock().unwrap();
    map.get(&(id, axis))
        .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
}
```

(Adapt the field name / lock idiom to the actual struct; use the same interior access `add` uses.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p switchyard timeout_tracker`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/timeout_tracker.rs
git commit -m "Expose remaining setpoint TTL from the timeout tracker"
```

---

### Task 4: Augmentation-active read-back

**Files:**
- Modify: `src/sim/axis.rs`, `src/sim/bounds.rs` (only if the count accessor is missing), `src/sim/component.rs`, plus the axis-owning components' trait impls (`src/sim/inverter/solar_inverter.rs`, `src/sim/inverter/battery_inverter.rs`, and any other component that owns `PowerAxis` fields — find them with `grep -rn ": PowerAxis" src/sim`)
- Test: `src/sim/axis.rs` tests module (PowerAxis unit tests live at lines 327+)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `PowerAxis::augmented(&self, now: DateTime<Utc>) -> bool` — true while at least one live (unexpired) augmentation narrows this axis.
  - `SimulatedComponent::augmentation_active(&self, axis: SetpointAxis, now: DateTime<Utc>) -> bool` (trait method, default `false`), overridden by axis-owning components to delegate to the matching `PowerAxis`.

- [ ] **Step 1: Write the failing test** (beside the existing PowerAxis tests, mirroring how `augment` is called there — e.g. the tests around bounds/augment already construct an axis and a `ComponentBounds`)

```rust
#[test]
fn augmented_reflects_live_augmentations_and_expiry() {
    let axis = /* construct exactly like the neighbouring augment test */;
    let now = Utc::now();
    assert!(!axis.augmented(now));
    axis.augment(now, /* bounds as in the neighbouring test */, Duration::from_secs(60));
    assert!(axis.augmented(now));
    assert!(!axis.augmented(now + chrono::Duration::seconds(120)));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p switchyard axis 2>&1 | tee <scratchpad>/t4.txt`
Expected: FAIL — `augmented` not found.

- [ ] **Step 3: Implement**

In `axis.rs` (`augs: Mutex<ComponentBounds>`, line ~64):

```rust
/// True while at least one unexpired augmentation is narrowing this
/// axis — the inspector's "augmented" badge.
pub fn augmented(&self, now: DateTime<Utc>) -> bool {
    self.augs.lock().unwrap().live_augmentation_count(now) > 0
}
```

In `bounds.rs`: if `ComponentBounds` has no way to count live augmentations, add `pub fn live_augmentation_count(&self, now: DateTime<Utc>) -> usize` using the SAME expiry predicate its envelope computation already applies to its augmentation queue (do not invent a second notion of "expired" — reuse or extract the existing one).

In `component.rs`, the trait default; in each axis-owning component:

```rust
fn augmentation_active(&self, axis: SetpointAxis, now: DateTime<Utc>) -> bool {
    match axis {
        SetpointAxis::Active => self.active.augmented(now),
        SetpointAxis::Reactive => self.reactive.augmented(now),
    }
}
```

(Field names per component — solar/battery inverters have `reactive: PowerAxis`; check each component's active-axis field name in its file. A component with only one axis returns `false` for the other.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p switchyard axis && cargo test -p switchyard`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/sim/axis.rs src/sim/bounds.rs src/sim/component.rs src/sim/inverter/solar_inverter.rs src/sim/inverter/battery_inverter.rs
git commit -m "Expose whether live augmentations narrow a power axis"
```

(Adjust the file list to whatever components actually gained the override.)

---

### Task 5: KnobChanged broadcast from the setter chokepoints

**Files:**
- Modify: `src/sim/events.rs`, `src/sim/microgrid_site/mod.rs`, `src/lisp/defuns/load_drivers.rs`, `src/lisp/defuns/reactive.rs`, `src/ui/handlers/control.rs`
- Test: `src/lisp/defuns/load_drivers.rs` and `src/lisp/defuns/reactive.rs` tests modules

**Interfaces:**
- Consumes: Task 1's `source_text()` (via the values the defuns already hold).
- Produces:
  - New `SiteEvent` variant (serde derives handle the wire shape; NO existing match needs editing — both existing matches have catch-alls):

```rust
/// A runtime knob changed — REPL, scenario, typed control API, or the
/// web UI; all writes funnel through the defuns or the control
/// handlers, and both emit this. The inspector refreshes its
/// edit-in-place inputs from it.
KnobChanged {
    id: u64,
    ts_ms: i64,
    /// One of: "meter-power" / "meter-reactive-power" /
    /// "meter-power-factor" / "solar-sunlight" /
    /// "reactive-pf-limit" / "reactive-apparent-va".
    knob: &'static str,
    /// New value; None when the knob was cleared (pf-limit /
    /// apparent-va accept clearing).
    value: Option<f32>,
    /// Printed Lisp source when the write installed an expression.
    expr: Option<String>,
    /// meter-power-factor only.
    leading: Option<bool>,
},
```

  - `MicrogridSite::note_knob_changed(&self, id: u64, knob: &'static str, value: Option<f32>, expr: Option<String>, leading: Option<bool>)` — stamps `ts_ms` and fire-and-forgets on the bus (same send-and-swallow pattern as `bump_version`, mod.rs:409-416).

- [ ] **Step 1: Write the failing tests**

In `load_drivers.rs` tests (subscribe before eval; the site exposes `subscribe_events()`, mod.rs:418):

```rust
#[test]
fn set_meter_power_broadcasts_knob_changed() {
    let (cfg, _dir) = config_with("(%make-meter :id 7)");
    let mut rx = cfg.site().subscribe_events();
    cfg.eval("(set-meter-power 7 1500)").unwrap();
    let mut seen = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        seen.push(ev);
    }
    assert!(
        seen.iter().any(|ev| matches!(
            ev,
            SiteEvent::KnobChanged { id: 7, knob: "meter-power", value: Some(v), expr: None, .. }
                if (*v - 1500.0).abs() < 1e-6
        )),
        "no matching KnobChanged on the bus; saw: {seen:?}"
    );
}
```

In `reactive.rs` tests: same drain-and-assert for `(set-reactive-pf-limit 4 0.95)` → `knob: "reactive-pf-limit", value: Some(0.95)` and `(set-reactive-pf-limit 4 0)` → `value: None`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p switchyard knob_changed 2>&1 | tee <scratchpad>/t5.txt` (name the tests so this filter catches them)
Expected: FAIL — variant/method not found.

- [ ] **Step 3: Implement**

1. `events.rs`: add the variant (block above).
2. `microgrid_site/mod.rs`, next to `bump_version`:

```rust
pub fn note_knob_changed(
    &self,
    id: u64,
    knob: &'static str,
    value: Option<f32>,
    expr: Option<String>,
    leading: Option<bool>,
) {
    let _ = self.inner.events.send(SiteEvent::KnobChanged {
        id,
        ts_ms: chrono::Utc::now().timestamp_millis(),
        knob,
        value,
        expr,
        leading,
    });
}
```

(Use the timestamp helper the file already uses for `ts_ms` elsewhere — grep `timestamp_millis` in the file and copy that idiom.)

3. Emit after each successful set in the defuns (they hold the site handle `w` and the id; emit ONLY on the success path, after the underlying setter returned true/Ok):
   - `load_drivers.rs` `set-meter-power` (lines 24-49): constant → `w.note_knob_changed(id, "meter-power", Some(p), None, None)`; expression → `w.note_knob_changed(id, "meter-power", Some(resolved_now), Some(printed), None)` where `printed` is the same `obj.to_string()` used for `source_text` (print before handing the object to `DynamicScalar::from_lisp`, or read it back via the reading accessor).
   - Same pattern for `set-meter-reactive-power`, `set-meter-power-factor` (value = pf, `leading: Some(leading)`), `set-solar-sunlight`.
   - `reactive.rs` `set-reactive-pf-limit` / `set-reactive-apparent-va`: `value: pf_or_va` (`None` on clear), `expr: None`.
4. `control.rs` typed drive handlers (the second door, lines 37-286): after each successful `set_active_power_override` / `set_reactive_power_override` / `set_power_factor` / `set_sunlight_pct`, call the matching `site.note_knob_changed(...)` with the same tokens.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p switchyard 2>&1 | tee <scratchpad>/t5b.txt`
Expected: PASS, including the untouched suites (the new variant must not break serde round-trips or the WS pump — no existing match is exhaustive, so failures here mean something else).

- [ ] **Step 5: Commit**

```bash
git add src/sim/events.rs src/sim/microgrid_site/mod.rs src/lisp/defuns/load_drivers.rs src/lisp/defuns/reactive.rs src/ui/handlers/control.rs
git commit -m "Broadcast KnobChanged from every runtime-knob setter path"
```

---

### Task 6: The /api/component snapshot endpoint

**Files:**
- Create: `src/ui/handlers/component.rs`
- Modify: `src/ui/handlers/mod.rs` (module + re-export), `src/ui/mod.rs` (routes)
- Test: `src/ui/tests.rs`

**Interfaces:**
- Consumes: Tasks 1-4 accessors; `resolve_site(config, mg_id)` (`src/ui/handlers/mod.rs:30`); `MicrogridSite::{active_setpoint_envelope, reactive_setpoint_envelope}` (mod.rs:719/783); `setpoints_window` (mod.rs:1289); `TimeoutTracker::remaining` — reach the tracker the same way the timeout sweep does (grep `start_timeout_loop` in `src/` for where Config holds it; pass what the handler needs via the existing `State(Config)`).
- Produces: `GET /api/component?id=N` (legacy) and `GET /api/mg/{mg_id}/component?id=N` returning:

```json
{
  "id": 12,
  "knobs": [
    {"knob": "solar-sunlight", "value": 87.0, "expr": null, "leading": null},
    {"knob": "reactive-pf-limit", "value": 0.95, "expr": null, "leading": null}
  ],
  "setpoints": [
    {"axis": "active", "value": -5000.0, "remaining_ms": 42000}
  ],
  "augmented": {"active": true, "reactive": false},
  "envelope": {"active": [-8400.0, 0.0], "reactive": [-3600.0, 3600.0]}
}
```

Response types (all `#[derive(Serialize)]`, in `component.rs`):

```rust
pub(in crate::ui) struct ComponentStateResponse {
    id: u64,
    knobs: Vec<KnobState>,
    setpoints: Vec<ActiveSetpoint>,
    augmented: AxisFlags,
    envelope: Envelope,
}
struct KnobState { knob: &'static str, value: Option<f32>, expr: Option<String>, leading: Option<bool> }
struct ActiveSetpoint { axis: &'static str, value: f32, remaining_ms: Option<u64> }
struct AxisFlags { active: bool, reactive: bool }
struct Envelope { active: Option<(f32, f32)>, reactive: Option<(f32, f32)> }
```

Assembly rules:
- **knobs**: from `meter_power_reading` / `meter_reactive_reading` (Var → `meter-reactive-power`; PowerFactor → `meter-power-factor` with `leading`) / `sunlight_reading` / `reactive_capability()` (pf_limit → `reactive-pf-limit`, apparent_va → `reactive-apparent-va`; emit each entry with `value: None` when unset so the client still renders the input). Only include knob entries the component's category actually has — mirror the client's `KNOBS_BY_CATEGORY` + solar rule.
- **setpoints**: last ACCEPTED event per axis from `setpoints_window(id, <10 min>)` (`SetpointEvent.kind` distinguishes axes; skip augment kinds), paired with `remaining_ms` from `TimeoutTracker::remaining`. Omit an axis with no accepted event; `remaining_ms: null` for a persistent (untracked) setpoint.
- **envelope**: `active_setpoint_envelope(id)` / `reactive_setpoint_envelope(id)`, first segment's lower to last segment's upper.
- **augmented**: `augmentation_active(axis, Utc::now())`.
- 404 (StatusCode::NOT_FOUND with a plain-string body, like `resolve_site`'s error) for an id the site doesn't have; `resolve_site` already 404s unknown microgrids.

- [ ] **Step 1: Write the failing tests** (in `src/ui/tests.rs`, using the existing `config_with` + `call`/`get` helpers, lines 17-75)

```rust
#[tokio::test]
async fn component_snapshot_reads_meter_knobs_and_envelope() {
    let cfg = config_with("(%make-meter :id 7)");
    cfg.eval("(set-meter-power 7 1500)").unwrap();
    cfg.eval("(set-meter-power-factor 7 0.9 t)").unwrap();
    let (status, body) = call(&cfg, get("/api/component?id=7")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let knobs = v["knobs"].as_array().unwrap();
    let power = knobs.iter().find(|k| k["knob"] == "meter-power").unwrap();
    assert_eq!(power["value"], 1500.0);
    let pf = knobs.iter().find(|k| k["knob"] == "meter-power-factor").unwrap();
    assert_eq!(pf["leading"], true);
}

#[tokio::test]
async fn component_snapshot_prints_expression_sources() {
    let cfg = config_with("(%make-meter :id 7)");
    cfg.eval("(set-meter-power 7 (lambda () 25))").unwrap();
    cfg.refresh_once();
    let (_s, body) = call(&cfg, get("/api/component?id=7")).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let power = v["knobs"].as_array().unwrap().iter()
        .find(|k| k["knob"] == "meter-power").unwrap().clone();
    assert!(power["expr"].as_str().unwrap().contains("lambda"));
}

#[tokio::test]
async fn component_snapshot_404s_unknown_ids() {
    let cfg = config_with("(%make-meter :id 7)");
    let (status, _b) = call(&cfg, get("/api/component?id=99")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _b) = call(&cfg, get("/api/mg/9999/component?id=7")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
```

Plus one inverter-shaped test: build a solar inverter, `(set-solar-sunlight 4 63)` + `(set-reactive-pf-limit 4 0.95)`, assert both knob entries and `envelope.reactive` non-null. Plus one setpoint test: plant a setpoint the way `setpoints_resolve_per_microgrid_and_legacy_first_site` (tests.rs:1678) does — or eval `(set-active-power ...)` with a lifetime — and assert `setpoints[0].remaining_ms` is `> 0` and `<= lifetime`.

(Whether `call`/`config_with` are async or the tests use `#[test]` + a runtime: copy the exact shape of the neighbouring handler tests.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p switchyard component_snapshot 2>&1 | tee <scratchpad>/t6.txt`
Expected: FAIL — route unknown (404 where 200 expected) or compile error.

- [ ] **Step 3: Implement**

`component.rs`: `component(State(config), Query(q))` using `config.legacy_site()`, and `component_for_mg(State(config), Path(mg_id), Query(q))` using `resolve_site` — both delegating to one `fn component_state(site, tracker_handle, id) -> Result<ComponentStateResponse, (StatusCode, String)>` per the assembly rules. Query struct: `#[derive(Deserialize)] struct ComponentQuery { id: u64 }` (mirror `history.rs`'s query struct style).

`handlers/mod.rs`: `mod component;` + re-export like the siblings.

`ui/mod.rs`: register beside the history/setpoints pairs:

```rust
.route("/api/component", get(component))            // near mod.rs:94
.route("/api/mg/{mg_id}/component", get(component_for_mg))  // near mod.rs:132
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p switchyard 2>&1 | tee <scratchpad>/t6b.txt`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/ui/handlers/component.rs src/ui/handlers/mod.rs src/ui/mod.rs src/ui/tests.rs
git commit -m "Serve a component read-back snapshot at /api/component"
```

---

### Task 7: Extract evalQuoted into a neutral module

**Files:**
- Create: `ui-assets/eval.js`
- Modify: `ui-assets/inspect.js`, `ui-assets/dialogs.js`, `ui-assets/explain.js`, `ui-assets/editor.js`, `ui-assets/topology.js`, `ui-assets/index.html` (only if it preloads modules by name)

**Interfaces:**
- Consumes: `mgPath` from `./routing.js`, `notify` from `./app.js` (as today).
- Produces: `eval.js` exporting `evalQuoted(expr, label = expr)` and `jsToLispString(s)` — MOVED VERBATIM from `inspect.js` (lines 326-365), not rewritten. `inspect.js` re-exports them (`export { evalQuoted, jsToLispString } from "./eval.js";`) so this task can land without touching importer semantics, then each importer is pointed at `./eval.js` directly.

- [ ] **Step 1: Move the two functions** into `ui-assets/eval.js` with their comment blocks intact; add the two imports they need at the top.

- [ ] **Step 2: Update importers** — in `dialogs.js`, `explain.js`, `editor.js`, `topology.js`, change `from "./inspect.js"` to `from "./eval.js"` for these two names only (other names stay on inspect.js). Drop the temporary re-export from `inspect.js` once all four are moved.

- [ ] **Step 3: Gate**

Run: `npx @biomejs/biome check ui-assets/eval.js ui-assets/inspect.js ui-assets/dialogs.js ui-assets/explain.js ui-assets/editor.js ui-assets/topology.js`
Expected: clean (same warnings as before at worst).

- [ ] **Step 4: Manual smoke** — `cargo run --bin switchyard examples/berlin-demo.lisp`, open the UI, select a node, rename it, disconnect/reconnect nothing (just confirm the rename toast/refresh works — it exercises `evalQuoted` through the new module).

- [ ] **Step 5: Commit**

```bash
git add ui-assets/eval.js ui-assets/inspect.js ui-assets/dialogs.js ui-assets/explain.js ui-assets/editor.js ui-assets/topology.js
git commit -m "Move evalQuoted and jsToLispString into their own module"
```

---

### Task 8: The side-panel shell with per-tenant teardown

**Files:**
- Create: `ui-assets/side-panel.js`
- Modify: `ui-assets/app.js` (openInspector/clearSide move out), `ui-assets/inspect.js`, `ui-assets/dialogs.js`, `ui-assets/formulas.js`, `ui-assets/routing.js`

**Interfaces:**
- Consumes: `inspectEl`, `inspectorEl` from `./app.js`.
- Produces (`side-panel.js`):

```js
// Open the floating panel showing `name` ("node" / "formula" /
// "defaults-btn" / "scenario-report-btn"). `render()` fills
// inspectEl; `teardown()` runs when this tenant is replaced or the
// panel closes — each tenant cleans up ONLY its own resources.
export function openPanel(name, render, teardown = null)
export function closePanel()   // runs current teardown, resets hint, hides card
export function currentPanel() // -> name | null
```

Behavior contract (verbatim from today's `openInspector`/`clearSide`, app.js:73-79 + inspect.js:526-539):
- `openPanel` runs the PREVIOUS tenant's teardown first (content swap), sets `body.inspector-open`, `inspectorEl.dataset.panel = name`, syncs the `#defaults-btn`/`#scenario-report-btn` `.primary` highlight, then calls `render()`.
- `closePanel` runs the current teardown, restores the "Click a node to inspect…" hint, removes the body class, deletes `dataset.panel`, clears both button highlights.
- Teardown must be idempotent (closePanel after openPanel-swap must not double-run the old tenant's).

Migration:
- `inspect.js showComponent` → `openPanel("node", renderFn, () => liveCharts.clear())` (chart teardown moves OUT of the shared path; the scenario-report timer no longer lives in inspect.js at all).
- `dialogs.js makeSidePanelToggle` (lines 121-134) → `openPanel(btnId, render, teardown)`; the scenario report passes `() => clearInterval(timer)` and `startScenarioReportLoop`/the module-private timer handle in inspect.js (lines 545-549) is DELETED — the timer becomes local to dialogs.js.
- `formulas.js` (line ~151) → `openPanel("formula", renderFn)` (no teardown).
- Every `clearSide()` caller (`app.js:148,372,399,435`, `routing.js:229`) → `closePanel()`. Keep a `clearSide` alias export only if the diff would otherwise touch nothing but names — prefer the rename.
- `refitCharts` (splitter.js:50,131, routing.js:245) stays exported from inspect.js unchanged.

- [ ] **Step 1: Write `side-panel.js`** per the contract; move the button-highlight loop and hint string in verbatim.
- [ ] **Step 2: Migrate the tenants and callers** as listed.
- [ ] **Step 3: Gate**: `npx @biomejs/biome check ui-assets/side-panel.js ui-assets/app.js ui-assets/inspect.js ui-assets/dialogs.js ui-assets/formulas.js ui-assets/routing.js`
- [ ] **Step 4: Manual check** (run the app): select node → panel; open Defaults → content swaps, Defaults button lights; open Report → poll starts; select a node again → report poll STOPS (watch the network tab: no more 2s scenario-report fetches — this is the per-tenant-teardown proof); Esc closes; formula tile click opens formula panel.
- [ ] **Step 5: Commit**

```bash
git add ui-assets/side-panel.js ui-assets/app.js ui-assets/inspect.js ui-assets/dialogs.js ui-assets/formulas.js ui-assets/routing.js
git commit -m "Split the floating panel into a shell with per-tenant teardown"
```

---

### Task 9: Inspector restructure — sections, chips, folded rows

**Files:**
- Modify: `ui-assets/inspect.js` (renderInspect + showComponent), `ui-assets/style.css`

**Interfaces:**
- Consumes: shell from Task 8; existing `selectField` is DELETED (replaced by chips); `OPERATIONAL_MODES`, `ACCEPTS_SETPOINTS`, `knobsFor` stay.
- Produces: the static structure of the mocks (artifact "Switchyard Inspector"): header (name input, id, category chip in `--cat-*` color, health chip, augmented badge slot), **Graph** (mode chips), **Simulation** (health/telemetry/commands chips), **Power** (envelope-bar placeholders + knob inputs — live data arrives in Task 10), **Charts** (folded row, unfold renders today's charts), **Recent setpoints** (unchanged), **Connections** (collapsed footer, expands to today's parent/child lists). Helper produced for Task 10: `renderSegRow(knobKey, current, options, semantics)` returning chip-row HTML, and a delegated click handler that evals the same setters the old selects did (`set-component-operational-mode` / `set-component-health` / `set-component-telemetry-mode` / `set-component-command-mode`, inspect.js:267-278).

Key specifics:
- Chip CSS (new classes in style.css, mirroring the mock exactly): `.seg` row = flex wrap gap 4px; chip = `padding:2px 8px; border:1px solid var(--border); border-radius:4px; background:var(--bg); color:var(--muted); font:12px var(--font-mono); cursor:pointer`. Active states: `.seg.on` accent-tinted (`background:rgba(121,184,255,.16); color:var(--accent)`), `.seg.on-good`, `.seg.on-warn`, `.seg.on-bad` with the good/standby/bad tints. Semantic mapping: health ok→on-good, error→on-bad, standby→on-warn; telemetry/commands normal→on, anything else→on-warn; mode always→on.
- Disabled rows keep today's semantics: when `provides_telemetry === false` / `accepts_control === false`, render the row's chips with a `disabled` style (muted, no pointer) and the same hover `title` reason (inspect.js:222-224).
- Folded rows: Charts and Connections render as the mock's one-line rows (`sect` header left, mono summary + chevron right). Clicking toggles an `open` class; Charts unfold calls the existing chart-building code (currently inline in `showComponent`, lines 388-434 — extract it to `async function buildCharts(d, container)` and call it on FIRST unfold only); fold state persists via `localStorage` key `sw-inspector-charts-open` (wrap reads/writes in try/catch, default folded). Connections expand shows today's parent/child `<ul>`s with disconnect buttons (markup from renderInspect lines 196-206 + 251-255, unchanged including the `structureEditable()` lock).
- `showComponent` no longer fetches history up front — that moves behind the unfold. `renderSetpoints` still runs on open.
- Header chips reuse the `.chip` treatment from the mock (tint = `--cat-<category>` for the category chip; `.hc-chip`-style health chip).

- [ ] **Step 1: Restructure `renderInspect`** into the section order above, building chip rows with `renderSegRow` and wiring one delegated click listener on the panel (`[data-knob][data-value]` chips → `evalQuoted` with the mapped defun).
- [ ] **Step 2: Extract `buildCharts`** and gate it behind the Charts fold row; wire the Connections fold row.
- [ ] **Step 3: Add the CSS** (chips, fold rows, header chips, envelope-bar classes `.env-bar/.env-live/.env-sp/.env-ends` copied from the hovercard's `.hc-bar` family with the mock's sizes).
- [ ] **Step 4: Gate**: `npx @biomejs/biome check ui-assets/inspect.js`
- [ ] **Step 5: Manual check** (run the app): select meter → Graph/Simulation/Power/Charts-folded/Setpoints/Connections-folded render; click a health chip → health changes (WS refresh re-renders with the chip moved); charts unfold → charts appear and live-update; fold → refold persists across reselection; commands row absent on the meter, present on an inverter.
- [ ] **Step 6: Commit**

```bash
git add ui-assets/inspect.js ui-assets/style.css
git commit -m "Restructure the component inspector into sectioned chips and folds"
```

---

### Task 10: Live read-back — snapshot fetch, edit-in-place knobs, envelope bars, TTL

**Files:**
- Modify: `ui-assets/inspect.js`, `ui-assets/repl.js` (WS hub), `ui-assets/style.css` (only leftovers)

**Interfaces:**
- Consumes: Task 6's `/api/component` (via `mgPath("component")`), Task 5's `knob_changed` WS frames, Task 9's structure.
- Produces: exported `inspectorLive` object from `inspect.js` with `applyKnob(ev)`, `applySample(ev)`, `applySetpoint(ev)` — the WS hub in `repl.js` calls them:
  - in the `"sample"` branch (repl.js:395-402): add `inspectorLive.applySample(ev);`
  - in the `"setpoint"` branch (repl.js:407-410): add `inspectorLive.applySetpoint(ev);`
  - new branch: `else if (ev.kind === "knob_changed") { inspectorLive.applyKnob(ev); }` — and add `"knob_changed"` to the `perMg` kind list (repl.js:389-391).

Behavior:
- `showComponent` fetches `${mgPath("component")}?id=${d.id}` alongside `renderSetpoints` (same generation-guard discipline, inspect.js:372-386). On success: pre-fill each knob input (`data-defun` matched to the knob token; expression knobs get the `expr` text + an `expr` chip; PF's leading checkbox set from `leading`), paint envelope bars (bar geometry: `left% = (value - lo) / (hi - lo) * 100`, clamped — same formula as hovercard.js:112-121), render the setpoint row (`▾ value · TTL Ns`, ticking down via a 1s interval owned by the node tenant's teardown), set the augmented badge. On failure: knobs render blank write-only with a `.hint` "read-back unavailable: <msg>" line — everything else stays (spec's degrade rule).
- Edit-in-place: inputs no longer clear on submit. `focus` sets `data-editing`; `applyKnob`/snapshot refresh skip an input with `data-editing`; Enter commits via `evalQuoted` (existing handler, minus the `e.target.value = ""` clear); Esc or blur-without-change restores the last live value (kept on `data-live`). On a REJECTED eval (`evalQuoted` resolves `{ok:false}`), restore `data-live` into the input (toast already fires inside evalQuoted).
- `applySample`: for the four bound metrics (`active_power_lower_bound_w`, `active_power_upper_bound_w`, `reactive_power_lower_bound_var`, `reactive_power_upper_bound_var` — token spellings from topology.js:1478-1481) and live `active_power_w`/`reactive_power_var`, update the bars for the currently-shown id only.
- `applySetpoint`: on an accepted `active_power`/`reactive_power` event, reset that axis's setpoint row and restart the TTL countdown (`ev.value`; TTL unknown from the event → re-fetch the snapshot for that one field, or show "TTL —" until the next snapshot; pick the re-fetch). On an accepted `augment_bounds`/`augment_reactive_bounds`, re-fetch the snapshot (envelope + augmented flag changed).
- The 1s TTL interval and the WS-applied state are torn down in the node tenant's teardown (Task 8's hook) — extend it: `() => { liveCharts.clear(); stopTtlTimer(); }`.
- WS reconnect needs NO new code: `openWebSocket`'s `onopen` already calls `onTopologyChanged(0)` on reconnect (repl.js:356-364), and the topology refresh re-renders the open node panel, which re-runs the snapshot fetch. Verify this in the manual check rather than adding a second refresh path.
- A dynamic knob whose snapshot carries `expr: null` (shouldn't happen — `source_text` is captured at construction — but the spec's degrade rule covers it): render the literal placeholder text `(expression)` in the input; typing a replacement expression over it still commits normally.

- [ ] **Step 1: Implement the snapshot fetch + prefill + bars + TTL row** in `showComponent`/`renderInspect`.
- [ ] **Step 2: Implement `inspectorLive`** and wire the three hub call sites in `repl.js`.
- [ ] **Step 3: Implement edit-in-place semantics** (focus freeze, Enter/Esc, reject-restore).
- [ ] **Step 4: Gate**: `npx @biomejs/biome check ui-assets/inspect.js ui-assets/repl.js`
- [ ] **Step 5: Manual check** (run the app):
  1. Select a meter; knob inputs show current values. In the REPL run `(set-meter-power 7 2000)` → the input updates WITHOUT reselecting (KnobChanged path).
  2. `(set-meter-power 7 (lambda () 25))` → input shows the lambda text + expr chip.
  3. Focus the input, run another REPL set → input does NOT move; Esc → reverts to the newest live value.
  4. Type an out-of-range PF (e.g. `1.5` into power factor) → toast + input snaps back.
  5. Drive a setpoint (`swctl` or `(set-active-power ... :lifetime-ms 60000)`) on an inverter → setpoint row appears with a ticking TTL; bar shows the marker.
  6. Kill and restart the server mid-view → on WS reconnect the panel still works (reselect refreshes).
- [ ] **Step 6: Commit**

```bash
git add ui-assets/inspect.js ui-assets/repl.js ui-assets/style.css
git commit -m "Wire live knob, envelope, and setpoint read-back into the inspector"
```

---

### Task 11: Final gate and bookkeeping

**Files:**
- Modify: `todo.org` (via the org-tasks helper, not hand-editing)

- [ ] **Step 1: Full gate**: `cargo test 2>&1 | tee <scratchpad>/final.txt` — grep the file for `FAILED|panicked`; `npx @biomejs/biome check` on every ui-assets file touched in Tasks 7-10.
- [ ] **Step 2: Manual sweep** (run the app): one pass over the Task 8/9/10 manual checks' happy paths; plus the meter AND solar-inverter panels against the mocks.
- [ ] **Step 3: Todo bookkeeping**: append a note to the "Inspector redesign for component selection" entry recording what landed and that browser-test coverage is parked on B1 (already noted there). Leave the entry INPROGRESS until the user closes it.
- [ ] **Step 4: Commit** any bookkeeping (`git add todo.org`) only if the user has said to commit todo.org changes; otherwise leave dirty and say so.
