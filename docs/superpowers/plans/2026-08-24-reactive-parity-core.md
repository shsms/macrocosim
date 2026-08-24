# Reactive Parity — Core (Sub-project 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One `PowerAxis` control path used by both P and Q, reactive bounds augmentable over the gRPC API with gateway parity, capability-hull config bounds, an honest P-only story for EV charger/grid, and the battery's fictional Q axis removed.

**Architecture:** A new `src/sim/axis.rs` owns delay + slew + optional PF/kVA caps + a TTL `VecBounds` augmentation queue per axis. The two inverters and the EV charger rebuild their control paths on it (`ReactivePath` dissolves; `reactive.rs` keeps only the caps math + a new hull). Telemetry Q bounds widen to `VecBounds`; the augment RPC and both setpoint gateways gain the reactive arm; the battery stops consuming Q.

**Tech Stack:** Rust (tokio, tonic/prost, parking_lot, chrono), existing sim primitives (`CommandDelay`, `Ramp`, `ComponentBounds`, `VecBounds`).

**Spec:** `docs/superpowers/specs/2026-08-24-reactive-parity-design.md` — read it first; its Decisions bind every task. Sub-projects 2 (meter drivers/readout) and 3 (UI) are NOT in this plan.

## Global Constraints

- Commit style: imperative subject, why-body, trailer exactly `Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>`; NO other trailers. Stage files by name; never `.nfs*` files; never `git add -A`/`.`.
- Gate per task: `cargo clippy --all-targets -- -D warnings && cargo fmt --check && cargo test --lib`; run the full `cargo test` before the final commit of each task.
- Comments in easy English, describing what things are — never why the change was made.
- Behavior preservation is the default: the existing inverter/EV/reactive tests are the specification. The ONLY intended behavior changes are the ones a task names explicitly; anything else that flips a test is a bug in the conversion.
- Units in `SetpointError::OutOfBounds`: `"W"` for active, `"VAr"` for reactive (existing convention).
- Sign conventions unchanged: production negative, consumption positive; +Q inductive.

## Pre-existing facts the tasks rely on

- `CommandDelay` (`src/sim/ramp.rs:34-128`): `new(Duration)`, `set_target(f32)`, `poll(now) -> Option<f32>` (stamps + promotes; re-returns the armed value every call), `reset()`, `armed()`.
- `Ramp` (`ramp.rs:136-206`): `new(rate, initial)`, `set_target` (NaN-rejecting), `snap_to`, `actual()`, `target()`, `advance(dt) -> f32`.
- `ComponentBounds` (`src/sim/bounds.rs:212-320`): `rated(lo, hi)`, `add_augmentation(ts, VecBounds, lifetime)`, `drop_expired(now)`, `effective_at(now) -> VecBounds`, `effective()`, `validate_active_setpoint(w)` (0 always allowed), `clamp`.
- `VecBounds` (`bounds.rs:17-138`): `single`, `new`, `contains`, `clamp` (nearest-edge, identity on empty), `intersect`, `sum_single`; `Bounds { lower: Option<f32>, upper: Option<f32> }`.
- `ReactiveCapability` (`src/sim/reactive.rs:34-92`): `pf_limit`/`apparent_va` `Option<f32>` pub fields; `q_bounds_at(p) -> (f32, f32)` with the no-caps fallback `|Q| ≤ |P|`; `microsim_default()` = PF 0.35. `ReactivePath` (`reactive.rs:112-208`) is what dissolves.
- BatteryInverter tick/setpoints: `src/sim/inverter/battery_inverter.rs:115-256` (health gate resets+trips, own-bounds clamp of armed P, children push `set_dc_active_reactive(p_share, q_share)`, `measured_w` = Σ p_share·`dc_accept_ratio`, no-healthy-children zeroes published P and Q). Solar: `solar_inverter.rs:160-250` (health gate snaps P to 0 but keeps the armed curtailment and only `override_published(0.0)` on Q — todo #998; unarmed target = sunlight floor `min_avail_w()` = `rated_lower_w·sun%/100`; armed target = `max(armed, avail)` then envelope-clamped; per-axis reset parks active at `min_avail_w()`). EV: `ev_charger.rs:154-246` (SoC-derate ∩ rated ∩ aug tracking envelope, validation WITHOUT the derate, empty-intersection park 0, unarmed target untouched).
- Gateway: `MicrogridSite::aggregate_child_bounds` + `active_setpoint_envelope` (`src/sim/microgrid_site/mod.rs:660-715`); consumed by `server.rs` `do_set_power` (`:264-273`, Active-only today) and `setpoints.rs` (`:100-124` active arm; the reactive arm documents "no gateway" at `:74-76`).
- Augment RPC: `server.rs:664-735` (`Metric::AcPowerActive` hard gate at `:672-677`; `validate_augmentation` is metric-agnostic; the routed call `component.augment_active_bounds(now, proposed, lifetime)`).
- Config bounds: `proto_conv.rs:128-157` (reactive sampled from live `reactive_bounds()`, fallback fake `±p_max`; Battery advertises `DcPower`).
- Telemetry: `component.rs:203-208` (`reactive_power_bounds: Option<(f32, f32)>`); streaming `proto_conv.rs:249-260`; WS scalar names `reactive_power_lower_bound_var`/`reactive_power_upper_bound_var` from `src/sim/history.rs:65-66`; `Telemetry::value_for` reactive-bounds arms at `component.rs:246-247`.
- Battery Q: `battery.rs` `pending_q` accumulation (`:261-265`), signed-apparent fold (`:214`), `dc_current_a` (`:225-229`), doc contracts `:59-66` and `:237-244`. `set_dc_active_reactive` trait default `component.rs:508-510`; sole real caller `battery_inverter.rs:194`.
- Spec appendix carries the rest of the survey (file:line) — consult it before hunting.

---

### Task 1: `PowerAxis` + the capability hull

**Files:**
- Create: `src/sim/axis.rs`
- Modify: `src/sim/mod.rs` (add `pub mod axis;` beside `pub mod ramp;`), `src/sim/reactive.rs` (add `hull`; nothing removed yet)
- Test: inline `#[cfg(test)]` in `axis.rs` and `reactive.rs`

**Interfaces:**
- Consumes: `CommandDelay`, `Ramp`, `ComponentBounds`, `VecBounds`, `ReactiveCapability`, `SetpointError`.
- Produces (exact, later tasks depend on these):

```rust
pub struct AxisConfig {
    /// Static rated bounds. `None` for a Q axis (its static shape is the caps).
    pub rated: Option<(f32, f32)>,
    /// PF/kVA caps. `Some` for Q axes, `None` for P axes.
    pub caps: Option<ReactiveCapability>,
    pub command_delay: Duration,
    /// Slew rate per second; `f32::INFINITY` disables ramping.
    pub ramp_rate_per_s: f32,
    /// "W" or "VAr" — carried into `SetpointError::OutOfBounds`.
    pub unit: &'static str,
}

pub enum IdleTarget {
    /// Leave the ramp target alone when no command is armed.
    Hold,
    /// Track this value when no command is armed (clamped into the
    /// tracking envelope; an empty envelope parks at 0).
    Value(f32),
}

pub struct StepCtx<'a> {
    /// The OTHER axis's live value (P for a Q axis). Ignored when `caps` is None.
    pub other_axis: f32,
    /// Extra per-tick envelope from the component (EV SoC derate,
    /// solar sunlight floor). Intersected into the tracking envelope
    /// only — never into validation.
    pub dynamic: Option<&'a VecBounds>,
    pub idle: IdleTarget,
}

impl PowerAxis {
    pub fn new(cfg: AxisConfig) -> Self;
    /// rated ∩ caps@other ∩ live augmentations (NO dynamic hook).
    pub fn validation_envelope_at(&self, now: DateTime<Utc>, other_axis: f32) -> VecBounds;
    /// validation envelope ∩ the dynamic hook.
    pub fn tracking_envelope_at(&self, now: DateTime<Utc>, other_axis: f32,
                                dynamic: Option<&VecBounds>) -> VecBounds;
    /// NaN → OutOfBounds; 0 always accepted (park rule); else must be
    /// inside the validation envelope. On Ok, enqueues into the delay.
    pub fn accept(&self, value: f32, now: DateTime<Utc>, other_axis: f32)
        -> Result<(), SetpointError>;
    /// drop_expired → promote (poll) → re-clamp the retained armed
    /// value into the tracking envelope (empty envelope → park 0) or
    /// apply the idle rule → slew → publish → return the new actual.
    pub fn step(&self, now: DateTime<Utc>, dt: Duration, ctx: StepCtx<'_>) -> f32;
    pub fn augment(&self, ts: DateTime<Utc>, bounds: VecBounds, lifetime: Duration);
    /// delay.reset + ramp.set_target(park). Does not touch `published`.
    pub fn reset(&self, park: f32);
    /// delay.reset + ramp.snap_to(0) + publish 0 — the health-trip snap.
    pub fn trip(&self);
    pub fn override_published(&self, v: f32);
    pub fn published(&self) -> f32;
    pub fn actual(&self) -> f32;          // ramp.actual()
    pub fn armed(&self) -> Option<f32>;   // delay.armed()
    /// Static side only: rated ∩ augmentations (telemetry's
    /// effective_active_bounds shape). Empty VecBounds when rated is None
    /// and no augmentations are live.
    pub fn effective_static(&self) -> VecBounds;
    pub fn effective_static_at(&self, now: DateTime<Utc>) -> VecBounds;
    pub fn set_pf_limit(&self, pf: Option<f32>);      // no-op + debug log when caps is None
    pub fn set_apparent_va(&self, va: Option<f32>);   // ditto
    pub fn capability(&self) -> Option<ReactiveCapability>;
}
```

- Produces on `ReactiveCapability`: `pub fn hull(&self, p_rated_abs: f32) -> (f32, f32)` — the widest Q over all P in `[0, p_rated_abs]`:
  - `(None, None)` → `(-p_rated_abs, p_rated_abs)` (the `|Q| ≤ |P|` fallback cone at rated P)
  - `(Some(k), None)` → `±k·p_rated_abs`
  - `(None, Some(s))` → `±s`
  - `(Some(k), Some(s))`: the curves `k·P` and `√(s²−P²)` cross at `P* = s/√(1+k²)`; if `P* ≤ p_rated_abs` the hull is `±k·s/√(1+k²)`, else `±min(k·p_rated_abs, √(max(s²−p_rated_abs², 0)))`.

**Implementation notes (binding):**
- Internal fields: `caps: Mutex<Option<ReactiveCapability>>`, `bounds: Mutex<ComponentBounds>` (when `rated` is None, construct via a new `ComponentBounds::unbounded()` — add it in bounds.rs: `rated: VecBounds::default()` i.e. empty, so `effective_at` = augmentations-only intersected into nothing → augmentations alone; check `intersect` on empty-lhs: `VecBounds::default().intersect(&x)` yields empty — WRONG for this use. Instead implement `effective_at` composition inside `PowerAxis`: keep `rated: Option<VecBounds>` in the axis and fold `Some` via intersect, `None` via clone of the augmentation product. Concretely: axis stores `augs: Mutex<ComponentBounds>` built as `ComponentBounds::rated(f32::NEG_INFINITY, f32::INFINITY)`? NO — infinities leak into proto bounds. The clean shape: axis owns `rated: Option<(f32, f32)>` + `augs: Mutex<AugQueue>` where `AugQueue` is `ComponentBounds` constructed with a rated band ONLY when `rated` is `Some`; when `None`, use `ComponentBounds::rated`'s sibling constructor you add: `ComponentBounds::augmentations_only()` whose `effective_at` returns the intersection of live augmentations alone (empty `VecBounds` when none are live — meaning "no static constraint", and the caller treats empty-as-unconstrained ∩). Add exactly that constructor + a unit test in bounds.rs; `effective_at` for it: start from the first live augmentation's bounds and intersect the rest; no live augmentations → empty.)
- Envelope composition rule (write one private fn used by both envelope methods): start from `Option<VecBounds>` accumulator = None; fold in, in order: static effective (skip when it is empty AND rated was None), caps band (`VecBounds::single(q_bounds_at(other))` when caps is Some), dynamic. Folding: None + x → Some(x); Some(a) + x → Some(a.intersect(&x)). Final None → unconstrained: represent as `VecBounds::single(f32::NEG_INFINITY, f32::INFINITY)`? Never reached in practice (P axes always have rated; Q axes always have caps) — return empty `VecBounds` and document that `accept` treats a fully-unconstrained axis as accept-anything-finite, `step` as clamp-free. Pin with a unit test.
- `accept` mirrors `validate_active_setpoint`'s park rule (`bounds.rs:302-315`): non-finite → `OutOfBounds`; `value == 0.0` → Ok; envelope non-empty and `!contains` → `OutOfBounds { value, unit: cfg.unit, envelope }`.
- `step`'s promote arm: `if let Some(armed) = self.delay.poll(now) { if env.0.is_empty() { ramp.set_target(0.0) } else { ramp.set_target(env.clamp(armed)) } }` — the armed request is retained by `CommandDelay` itself (poll re-returns it), which is what makes tighten→follow / re-widen→restore work with no extra state. Idle arm: `IdleTarget::Value(v)` → same empty→0/clamp treatment; `Hold` → nothing.

- [ ] **Step 1: Write failing unit tests** in `axis.rs` (they fail to compile until the type exists). Cover, at minimum, with real assertions:

```rust
// P-flavored axis: rated bounds + augmentation TTL + re-clamp.
#[test]
fn armed_target_follows_a_tightening_envelope_and_restores() {
    let ax = PowerAxis::new(AxisConfig { rated: Some((-10_000.0, 10_000.0)),
        caps: None, command_delay: Duration::ZERO,
        ramp_rate_per_s: f32::INFINITY, unit: "W" });
    let t0 = Utc::now();
    ax.accept(8_000.0, t0, 0.0).unwrap();
    let ctx = |d: Option<&VecBounds>| StepCtx { other_axis: 0.0, dynamic: d, idle: IdleTarget::Hold };
    assert_eq!(ax.step(t0, Duration::from_secs(1), ctx(None)), 8_000.0);
    // Tighten via a 2 s augmentation → follows; expire → restores.
    ax.augment(t0, VecBounds::single(-3_000.0, 3_000.0), Duration::from_secs(2));
    assert_eq!(ax.step(t0 + chrono::Duration::seconds(1), Duration::from_secs(1), ctx(None)), 3_000.0);
    assert_eq!(ax.step(t0 + chrono::Duration::seconds(3), Duration::from_secs(1), ctx(None)), 8_000.0);
}

#[test]
fn empty_tracking_envelope_parks_at_zero_but_idle_hold_is_untouched() {
    let ax = PowerAxis::new(AxisConfig { rated: Some((0.0, 22_000.0)), caps: None,
        command_delay: Duration::ZERO, ramp_rate_per_s: f32::INFINITY, unit: "W" });
    let t0 = Utc::now();
    // Armed target vs a dynamic envelope that doesn't intersect it → park 0.
    ax.accept(10_000.0, t0, 0.0).unwrap();
    let derate = VecBounds::single(30_000.0, 40_000.0); // disjoint from rated
    assert_eq!(ax.step(t0, Duration::from_secs(1),
        StepCtx { other_axis: 0.0, dynamic: Some(&derate), idle: IdleTarget::Hold }), 0.0);
    // Unarmed + zero-excluding augmentation: Hold means 0 stays 0.
    let ax2 = PowerAxis::new(AxisConfig { rated: Some((0.0, 22_000.0)), caps: None,
        command_delay: Duration::ZERO, ramp_rate_per_s: f32::INFINITY, unit: "W" });
    ax2.augment(t0, VecBounds::single(5_000.0, 22_000.0), Duration::from_secs(60));
    assert_eq!(ax2.step(t0, Duration::from_secs(1),
        StepCtx { other_axis: 0.0, dynamic: None, idle: IdleTarget::Hold }), 0.0);
}

#[test]
fn q_axis_validates_against_caps_and_augmentations_but_not_dynamic() {
    let ax = PowerAxis::new(AxisConfig { rated: None,
        caps: Some(ReactiveCapability { pf_limit: None, apparent_va: Some(5_000.0) }),
        command_delay: Duration::ZERO, ramp_rate_per_s: f32::INFINITY, unit: "VAr" });
    let t0 = Utc::now();
    // At P=3000, kVA circle allows |Q| ≤ 4000.
    assert!(ax.accept(4_500.0, t0, 3_000.0).is_err());
    ax.accept(3_500.0, t0, 3_000.0).unwrap();
    // A TTL augmentation narrows the Q envelope — the spec's failing RPC case.
    ax.augment(t0, VecBounds::single(-1_000.0, 1_000.0), Duration::from_secs(60));
    assert!(ax.accept(3_500.0, t0 + chrono::Duration::seconds(1), 3_000.0).is_err());
    let err = ax.accept(f32::NAN, t0, 3_000.0).unwrap_err();
    assert!(matches!(err, SetpointError::OutOfBounds { unit: "VAr", .. }));
    // 0 always accepted.
    ax.accept(0.0, t0, 3_000.0).unwrap();
}

#[test]
fn idle_value_tracks_and_clamps() {
    // Solar-shaped: unarmed axis tracks the provided idle value,
    // clamped by an augmentation cap.
    let ax = PowerAxis::new(AxisConfig { rated: Some((-30_000.0, 0.0)), caps: None,
        command_delay: Duration::ZERO, ramp_rate_per_s: f32::INFINITY, unit: "W" });
    let t0 = Utc::now();
    assert_eq!(ax.step(t0, Duration::from_secs(1),
        StepCtx { other_axis: 0.0, dynamic: None, idle: IdleTarget::Value(-6_000.0) }), -6_000.0);
    ax.augment(t0, VecBounds::single(-2_000.0, 0.0), Duration::from_secs(60));
    assert_eq!(ax.step(t0 + chrono::Duration::seconds(1), Duration::from_secs(1),
        StepCtx { other_axis: 0.0, dynamic: None, idle: IdleTarget::Value(-6_000.0) }), -2_000.0);
}

#[test]
fn trip_snaps_and_reset_parks() {
    let ax = PowerAxis::new(AxisConfig { rated: Some((-10_000.0, 10_000.0)), caps: None,
        command_delay: Duration::ZERO, ramp_rate_per_s: 1_000.0, unit: "W" });
    let t0 = Utc::now();
    ax.accept(5_000.0, t0, 0.0).unwrap();
    ax.step(t0, Duration::from_secs(2),
        StepCtx { other_axis: 0.0, dynamic: None, idle: IdleTarget::Hold }); // → 2000
    ax.trip();
    assert_eq!(ax.actual(), 0.0);
    assert_eq!(ax.published(), 0.0);
    assert_eq!(ax.armed(), None);
    // reset(park) re-targets without snapping.
    ax.accept(5_000.0, t0, 0.0).unwrap();
    ax.step(t0, Duration::from_secs(1),
        StepCtx { other_axis: 0.0, dynamic: None, idle: IdleTarget::Hold }); // → 1000
    ax.reset(0.0);
    let v = ax.step(t0, Duration::from_secs(1) / 2,
        StepCtx { other_axis: 0.0, dynamic: None, idle: IdleTarget::Hold });
    assert!((v - 500.0).abs() < 1.0, "ramps toward the park value, no snap: {v}");
}
```

And in `reactive.rs`, hull tests:

```rust
#[test]
fn hull_shapes() {
    let pf = ReactiveCapability { pf_limit: Some(0.5), apparent_va: None };
    assert_eq!(pf.hull(10_000.0), (-5_000.0, 5_000.0));
    let kva = ReactiveCapability { pf_limit: None, apparent_va: Some(4_000.0) };
    assert_eq!(kva.hull(10_000.0), (-4_000.0, 4_000.0));
    let neither = ReactiveCapability { pf_limit: None, apparent_va: None };
    assert_eq!(neither.hull(10_000.0), (-10_000.0, 10_000.0));
    // Both: k=1, s=5000 → cross at P*=s/√2≈3536 ≤ rated → hull ±k·s/√2.
    let both = ReactiveCapability { pf_limit: Some(1.0), apparent_va: Some(5_000.0) };
    let (lo, hi) = both.hull(10_000.0);
    assert!((hi - 5_000.0 / 2f32.sqrt()).abs() < 1.0 && (lo + hi).abs() < 1e-3);
    // Both, rated below the crossing: k=1, s=5000, rated 2000 → ±min(2000, √(s²−4M))=±2000.
    let (lo2, hi2) = both.hull(2_000.0);
    assert!((hi2 - 2_000.0).abs() < 1.0 && (lo2 + hi2).abs() < 1e-3);
}
```

- [ ] **Step 2: Run** `cargo test --lib axis` and `cargo test --lib reactive::tests::hull` → FAIL (types missing).
- [ ] **Step 3: Implement** `axis.rs` + `ComponentBounds::augmentations_only()` (+ its bounds.rs unit test) + `ReactiveCapability::hull` per the notes above.
- [ ] **Step 4: Run to green**, then the task gate.
- [ ] **Step 5: Commit** — `git add src/sim/axis.rs src/sim/mod.rs src/sim/reactive.rs src/sim/bounds.rs`; message: `Add a per-axis control path shared by P and Q`.

---

### Task 2: rebuild the two inverters on `PowerAxis`; dissolve `ReactivePath`

**Files:**
- Modify: `src/sim/inverter/battery_inverter.rs`, `src/sim/inverter/solar_inverter.rs`, `src/sim/inverter/mod.rs` (the shared `inverter_telemetry` signature keeps its `(f32, f32)` reactive-bounds param for now — Task 5 widens it), `src/sim/reactive.rs` (delete `ReactivePath` + its tests; keep `ReactiveCapability` + hull; move any still-relevant `ReactivePath` test as an axis or inverter test)
- Test: existing inverter test modules are the specification; add the two new ones below

**Interfaces:**
- Consumes: Task 1's `PowerAxis`/`AxisConfig`/`StepCtx`/`IdleTarget`.
- Produces: `BatteryInverter { active: PowerAxis, reactive: PowerAxis, measured_w: Mutex<f32>, … }`, `SolarInverter { active: PowerAxis, reactive: PowerAxis, … }` — field names `active`/`reactive` exactly (Tasks 4–6 reference them).

**Conversion contract (battery inverter):**
- `new`: `active = PowerAxis::new(AxisConfig { rated: Some((cfg.rated_lower_w, cfg.rated_upper_w)), caps: None, command_delay: cfg.command_delay, ramp_rate_per_s: cfg.ramp_rate_w_per_s, unit: "W" })`; `reactive = PowerAxis::new(AxisConfig { rated: None, caps: Some(cfg.reactive), command_delay: cfg.reactive_command_delay, ramp_rate_per_s: cfg.reactive_ramp_rate_var_per_s, unit: "VAr" })`.
- tick health gate: `active.trip(); *measured_w = 0.0; reactive.trip(); return;` (unchanged semantics — today's `delay.reset + ramp.snap_to(0)` IS `trip()` minus the publish; `measured_w` is the published P here, still zeroed explicitly).
- tick body: `let commanded_p = self.active.step(now, dt, StepCtx { other_axis: 0.0, dynamic: None, idle: IdleTarget::Hold });` then `let commanded_q = self.reactive.step(now, dt, StepCtx { other_axis: p_live, dynamic: None, idle: IdleTarget::Hold });` (where `p_live = *measured_w.lock()` read before, exactly as today) — then the children push exactly as today (Task 4 changes the push itself).
- `set_active_setpoint` → `self.active.accept(power_w, Utc::now(), 0.0)` — CAUTION: `accept` needs `now` for `effective_at`; the old path used `validate_active_setpoint` (wall-clock `effective()`). Keep wall-clock behavior: give `PowerAxis::accept` the signature from Task 1 (it takes `now`); call it with `Utc::now()` here, preserving today's semantics under the stepped clock (validation always used wall time — see `bounds.rs:288-290`).
- `set_reactive_setpoint` → `self.reactive.accept(vars, Utc::now(), *self.measured_w.lock())`.
- `reset_setpoint` / `reset_setpoint_axis`: `active.reset(0.0)` / `reactive.reset(0.0); reactive.override_published(0.0)` (the Q reset today zeroes published immediately — preserve; pinned by `axis_reset_leaves_the_other_axis_running`).
- `augment_active_bounds` → `self.active.augment(ts, bounds, lifetime)`.
- Telemetry/bounds reads: `effective_active_bounds` → `Some(self.active.effective_static())`; `reactive_bounds` → `Some(self.reactive… )` — still the tuple shape this task: expose it via `self.reactive.capability()`-based `q_bounds_at(p)`? NO — the envelope must now include Q augmentations. Add to `PowerAxis`: this task extends nothing; instead compute the tuple as the first band of `self.reactive.tracking_envelope_at(Utc::now(), p, None)` (empty → `(0.0, 0.0)`). Task 5 replaces the tuple with the full `VecBounds`.
- `set_reactive_pf_limit`/`set_reactive_apparent_va` → the axis cap setters.

**Conversion contract (solar):** same construction; tick health gate becomes `self.active-snap`: keep EXACT current behavior — `active` must snap actual to 0 while KEEPING the armed curtailment (`ramp.snap_to(0)` without `delay.reset`): add nothing to the axis for this — express it as `self.active.override-free` path: the axis exposes `actual`/ramp only internally… **Binding resolution:** give `PowerAxis` one more method in this task, `pub fn snap_output(&self, v: f32)` (= `ramp.snap_to(v)`, delay untouched, published untouched), used ONLY by solar's health gate: `self.active.snap_output(0.0); self.reactive.trip(); return;` — this IS the todo #998 fix (Q axis full trip; P curtailment kept), covered by the new test below. Healthy tick: `let avail = self.min_avail_w(); let p = self.active.step(now, dt, StepCtx { other_axis: 0.0, dynamic: Some(&VecBounds::single(avail, f32::INFINITY)), dynamic — WAIT` — the sunlight floor must clamp the armed target UP to avail (production is negative; `max(armed, avail)`), and `VecBounds` with `f32::INFINITY` edges leaks infinities into proto if this envelope ever escapes. It never escapes (tracking only, telemetry uses `effective_static`), but keep it finite anyway: `VecBounds::single(avail, 0.0)` — solar P is never positive (rated upper 0.0), so `[avail, 0]` is the correct sun-limited band. Idle: `IdleTarget::Value(avail)`. Per-axis reset: `active.reset(self.min_avail_w())`, `reactive.reset(0.0); reactive.override_published(0.0)`.
- NOTE the one intended solar behavior change (pin it): an augmentation demanding MORE production than sunlight allows (aug band entirely below `[avail, 0]`) now yields an empty tracking envelope → park 0, where today it produced at the aug edge beyond available sun. The new behavior is the spec's park rule and is physically saner; add the test below.

- [ ] **Step 1: New failing tests** (write first; they fail against `ReactivePath`-based code only after the conversion compiles — treat the conversion as the RED→GREEN unit):

```rust
// solar_inverter.rs — the #998 fix, pinned:
#[test]
fn health_trip_trips_q_but_keeps_the_armed_curtailment() {
    // build a solar inverter, arm P curtailment + Q setpoint, tick once;
    // set health error via the site, tick → P actual 0, Q published 0,
    // Q armed cleared; restore health, tick → P resumes toward the ARMED
    // curtailment (not full sun), Q stays 0 until re-dispatched.
}
#[test]
fn augmentation_beyond_available_sun_parks_at_zero() {
    // sunlight 10% of rated → avail = -3000; augment [-8000, -6000] (60 s):
    // tracking envelope is empty → actual parks at 0 (was: produced -6000).
}
```

(Write them as real code following the file's existing test helpers — `run_with_ctx`-style site setup is in the file's test module; copy its pattern.)

- [ ] **Step 2: Convert both inverters** per the contracts; delete `ReactivePath`; migrate its 8 tests: `accept_then_step_drives_to_target`, `rejects_out_of_envelope`, `re_clamps_at_promotion`, `re_clamps_on_p_drift_after_settle`, `override_published_wins`, `runtime_caps` become `PowerAxis` tests in `axis.rs` (same assertions, constructed with a Q-flavored `AxisConfig`); `pf_limit_scales_with_p`, `kva_limit_is_circle`, `both_intersect` stay in `reactive.rs` (they test `q_bounds_at`).
- [ ] **Step 3: Run** the full inverter + axis + reactive suites → green; every pre-existing test unchanged except mechanical constructor renames (list any test whose ASSERTIONS you had to touch in the report — there should be none beyond the two new ones).
- [ ] **Step 4: Task gate + full `cargo test`.**
- [ ] **Step 5: Commit** — `Rebuild the inverters on the shared power axis`.

---

### Task 3: EV charger on `PowerAxis` + honest P-only telemetry

**Files:**
- Modify: `src/sim/ev_charger.rs`, and (after the survey check below) any other P-only AC component whose telemetry emits active samples but no reactive
- Test: `ev_charger.rs` test module; `src/proto_conv.rs` tests

**Interfaces:**
- Consumes: Task 1 axis; EV keeps `state: Mutex<EvState>` for SoC.
- Produces: `EvCharger { active: PowerAxis, … }`; EV telemetry gains `reactive_power_var: Some(0.0)`.

**Conversion contract:** `active = PowerAxis::new(AxisConfig { rated: Some((cfg.rated_lower_w, cfg.rated_upper_w)), caps: None, command_delay: cfg.command_delay, ramp_rate_per_s: cfg.ramp_rate_w_per_s, unit: "W" })`. Tick: compute `(soc_lo, soc_hi)` exactly as today (`ev_charger.rs:164-170`), then `let derate = VecBounds::single(soc_lo, soc_hi); let p = self.active.step(now, dt, StepCtx { other_axis: 0.0, dynamic: Some(&derate), idle: IdleTarget::Hold });` then the SoC integration unchanged. `set_active_setpoint` → `self.active.accept(power_w, Utc::now(), 0.0)` (validation excludes the derate — that is exactly the axis's validation/tracking split). `effective_active_bounds` → `Some(VecBounds::single(s.effective_lower_w, s.effective_upper_w).intersect(&self.active.effective_static()))` (unchanged shape). `reset_setpoint` → `active.trip()`? NO — today's EV reset is `delay.reset + ramp.snap_to(0.0)` (`ev_charger.rs:248-251`) which IS `trip()` minus published (EV has no published slot; `aggregate_power_w` reads `actual`): use `self.active.trip()` and note `published` is unused by EV.
- Telemetry: add `reactive_power_var: Some(0.0)` to the `Telemetry { … }` literal (`ev_charger.rs:217-230`). Then check which OTHER components emit `AcPowerActive` samples without a reactive sample in `proto_conv.rs:216-260`'s streaming path (grid, markers, meters): meters DO emit Q (aggregation); grid/markers emit whatever their `Telemetry` carries — verify with a focused look and add `Some(0.0)` reactive to any component whose telemetry carries `active_power_w: Some(_)` but `reactive_power_var: None` (the spec's "any P-only AC component" rule). Record which you changed.
- Existing EV tests (`park` rules, augmentation clamping, SoC derate) must pass unchanged — they specify the conversion.

- [ ] **Step 1: Failing test** — `ev_telemetry_advertises_zero_reactive` (telemetry `reactive_power_var == Some(0.0)`; and via `telemetry_to_proto`, an `AcPowerReactive` sample with value 0 is emitted for the EV).
- [ ] **Step 2: Convert; run the EV suite green; existing assertions untouched.**
- [ ] **Step 3: Task gate + full `cargo test`.**
- [ ] **Step 4: Commit** — `Move the EV charger onto the power axis and publish its zero Q`.

---

### Task 4: drop the battery's Q axis; the push becomes P-only

**Files:**
- Modify: `src/sim/battery.rs` (remove `pending_q`, `reactive_var`, the signed-apparent fold at `:214`, rewrite `dc_current_a` `:225-229` and the doc contracts `:59-66` + `:237-244`; `aggregate_reactive_var` impl removed → trait default 0.0), `src/sim/component.rs` (delete `set_dc_active_reactive` `:508-510`; keep `set_dc_power` + `dc_accept_ratio`), `src/sim/inverter/battery_inverter.rs` (push loop calls `child.set_dc_power(p_share)`; `q_share` disappears; the comment block updates)
- Test: battery + battery_inverter test modules

**Behavior pins (write the failing tests first):**

```rust
// battery.rs
#[test]
fn dc_power_is_pure_active_even_when_the_inverter_runs_q() {
    // battery under an inverter with an armed Q setpoint: tick the pair;
    // battery telemetry dc_power_w equals the accepted P share exactly
    // (no apparent-power inflation), dc_current_a == dc_power_w / voltage.
}
// battery_inverter.rs — the health gate keeps zeroing Q (spec: only the push is removed):
#[test]
fn no_healthy_children_still_zeroes_published_q() {
    // existing no_healthy_children_means_zero_published keeps its reactive
    // assertions — verify it still passes UNCHANGED; if the conversion in
    // Task 2 moved it, it lives on the axis-published value now.
}
```

- Scenario Wh integrals: no test pins the inflated value (audited); note in the commit body that charge/discharge Wh totals become smaller when Q≠0.
- [ ] **Step 1: RED** (the battery test fails against the signed-apparent fold), **Step 2: implement**, **Step 3: suites green**, **Step 4: task gate + full `cargo test`**, **Step 5: Commit** — `Terminate reactive power at the inverter, not the battery`.

---

### Task 5: telemetry Q bounds become `VecBounds`

**Files:**
- Modify: `src/sim/component.rs` (`reactive_power_bounds: Option<VecBounds>` at `:203-208`; `reactive_bounds()` trait method → `Option<VecBounds>` at `:475-477`; `value_for`'s reactive-bounds arms `:246-247` → first band, mirroring the active arms), `src/sim/inverter/mod.rs` (`inverter_telemetry`'s reactive param → `VecBounds`), both inverters (`reactive_bounds` → `Some(self.reactive.tracking_envelope_at(Utc::now(), p, None))`), `src/proto_conv.rs` (`:249-260` stream all bands like the active arm `:240-248`), `src/sim/history.rs` (the reactive-bounds scalar sampling → first band, exactly like the active arms — WS event names `reactive_power_lower_bound_var`/`reactive_power_upper_bound_var` unchanged)
- Test: component/proto_conv/history test modules

- [ ] **Step 1: Failing test** — a component whose `reactive_bounds` carries two bands streams two `Bounds` entries in its `AcPowerReactive` sample, while the WS/history scalar and `value_for` report the FIRST band's edges.
- [ ] **Step 2: Widen; chase every compile error** (the tuple destructures the compiler finds are the complete consumer list; convert each per the spec's collapse rules).
- [ ] **Step 3: suites green; task gate + full `cargo test`** (integration `tests/` may destructure the tuple — fix per the same rules).
- [ ] **Step 4: Commit** — `Widen reactive bounds to multi-band VecBounds`.

---

### Task 6: reactive augmentation over the API + gateway parity

**Files:**
- Modify: `src/sim/component.rs` (add `fn augment_reactive_bounds(&self, ts: DateTime<Utc>, bounds: VecBounds, lifetime: Duration) {}` default no-op, doc noting the ACK-as-no-op parity with the active side), both inverters (`self.reactive.augment(…)`), `src/sim/microgrid_site/mod.rs` (add `aggregate_child_reactive_bounds` + `reactive_setpoint_envelope` mirroring `:660-715` but over `reactive_bounds()`), `src/server.rs` (augment gate `:672-677` accepts `AcPowerReactive` and routes to the reactive trait method, reusing `validate_augmentation` with the component's `reactive_bounds()` as the reference envelope; `do_set_power`'s gateway `:264-273` drops the `matches!(Active)` guard and picks the envelope per axis), `src/lisp/defuns/setpoints.rs` (the reactive arm gains the same gateway check + envelope-CLAMP as the active arm `:100-124`; delete the "no gateway on this axis" doc sentences `:74-76` and the equivalent in the reactive arm's comment)
- Test: server/setpoints test modules; `tests/grpc.rs` if it exercises augment

- [ ] **Step 1: Failing tests**

```rust
// setpoints.rs — Q augmentation round trip through the DSL-visible surface:
#[test]
fn reactive_augmentation_narrows_accepts_and_expires() {
    // inverter with kVA cap 5000 at P=0; set-reactive-power 3000 → ok.
    // augment_reactive_bounds ±1000 for 2 s (call the trait method directly);
    // set-reactive-power 3000 → rejected; set-reactive-power 3000 with CLAMP
    // → applied as 1000; after expiry (effective_at) 3000 accepted again.
}
#[test]
fn reactive_gateway_mirrors_active() {
    // reactive_setpoint_envelope(id): for an inverter whose children expose
    // no reactive bounds → None (gateway falls through to component
    // validation); the set-reactive-power arm calls it — pin via a site
    // where the INVERTER's own envelope rejects (component-level) and the
    // error message matches the active arm's wording pattern.
}
```

For the gRPC side, follow the existing augment test pattern (find it via `grep -rn augment tests/ src/server.rs`); add: `AC_POWER_REACTIVE` accepted end-to-end (response carries `valid_until_time`; the streamed reactive sample's bounds narrow), a still-rejected metric (e.g. `DC_POWER`) keeps the invalid-argument error naming the metric.
- [ ] **Step 2: Implement; run green; task gate + full `cargo test`.**
- [ ] **Step 3: Commit** — `Accept reactive bounds augmentations and gate Q like P`.

---

### Task 7: capability-hull config bounds + honest zeros

**Files:**
- Modify: `src/proto_conv.rs:128-157` — the reactive `MetricConfigBounds` arm becomes: components WITH a Q axis advertise `capability().hull(p_max)` intersected with… nothing else (the hull IS the static advertisement; expose the capability via a new trait method `fn reactive_capability(&self) -> Option<ReactiveCapability>` default `None`, implemented by both inverters as `self.reactive.capability()`); components WITHOUT (`None`) advertise `(0.0, 0.0)`. The `±p_max` fallback and the live `reactive_bounds()` sampling both disappear from this fn.
- Test: proto_conv test module

- [ ] **Step 1: Failing tests** — the hull matrix: PF-only inverter at rated ±30 kW with k=0.35 advertises ±10.5 kVAr even while idle; kVA-only advertises ±S; both advertises the crossing formula's value; a `:reactive-pf-limit 0 :reactive-apparent-va 0` inverter (neither cap) advertises ±p_rated; EV charger and grid advertise exactly `(0.0, 0.0)`; battery still advertises `DcPower` only.
- [ ] **Step 2: Implement; green; task gate + full `cargo test`.**
- [ ] **Step 3: Commit** — `Advertise the reactive capability hull, honestly zero without one`.

---

### Task 8: docs + todo bookkeeping

**Files:**
- Modify: `AGENTS.md` (the inverter/battery coupling paragraph — Q now terminates at the inverter; the reactive envelope description; one sentence on `:reactive-pf-limit` = ratio k vs the meter PF = cos φ arriving in sub-project 2), `scenarios/README.md` (only if it states Q-bounds claims the branch changed — grep `reactive`), `todo.org` via the org-tasks helper (mark #445, #474, #998 DONE with a one-line resolution note each; annotate d5b's entry: own-envelope re-clamp now codified in `PowerAxis`, child-SoC case remains the open question; note the two audit-cleared items #447/#537 stay open for sub-project 2 where applicable — #537 is SP2's aggregation fix, leave it)
- Test: none (docs); run the full gate once

- [ ] **Step 1: Sweep + edit; Step 2: gate; Step 3: Commit** — `Update docs for the shared power axis`.

---

## Execution notes

- Task order is dependency order. Task 2 is the riskiest (behavior-preserving refactor of battle-tested code) — its existing test suite is the contract; treat any assertion change beyond the two named new tests as a defect.
- `inverter_telemetry`'s signature changes twice (Task 2 keeps the tuple, Task 5 widens) — accepted churn, noted here so the per-commit reviewer doesn't flag it blind.

## Self-review (done at plan-writing time)

- Spec coverage (SP1 scope): PowerAxis + envelope semantics + park rules + validation split + idle targets (T1/T2/T3), ReactivePath dissolution (T2), battery Q drop + health-gate retention + dc docs (T4), VecBounds end-to-end + collapse rules (T5), augment RPC + gateway parity + CLAMP (T6), hull + honest zeros + EV telemetry zero (T3/T7), PV #998 Q-scoped trip (T2), docs/todo (T8). SP2/SP3 items deliberately absent.
- The solar sunlight floor rides the `dynamic` envelope (`[avail, 0]`) + `IdleTarget::Value(avail)`; the one behavior change it introduces (aug-beyond-sun parks at 0) is pinned by a named test.
- Type consistency: `accept(value, now, other_axis)`, `step(now, dt, StepCtx)`, `StepCtx { other_axis, dynamic, idle }`, `effective_static()`, `tracking_envelope_at`, `validation_envelope_at`, `capability()`, `snap_output` (added in T2), `reactive_capability()` trait method (T7) — names used consistently across tasks.
