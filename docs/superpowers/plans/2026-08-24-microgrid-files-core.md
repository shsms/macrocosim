# Microgrid Files — Core (Sub-project 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One managed Lisp file per microgrid (`microgrids/<id>.lisp`) written by switchyard and evaluated on load, plus `enterprise.lisp` for process-wide state — replacing the overrides journal, the persist gate, and the in-memory `loaded_files` replay.

**Architecture:** A new `src/lisp/microgrid_file.rs` owns the file format (marker parsing, block rendering, atomic writes, id rewriting). Components gain `make_fn()` / `constructor_kwargs()` so a live site renders back to `%make-*` forms. The registry entry gains `source` / `managed` / `unsaved`; `make-microgrid` errors on id collisions unless the same file is being re-loaded. Structural evals regenerate the owning file; reload/watch/undo/snapshots all become per-file/per-microgrid.

**Tech Stack:** Rust (tokio, axum, tulisp, tulisp_fmt, notify, parking_lot), vanilla-JS UI in `ui-assets/`.

**Spec:** `docs/superpowers/specs/2026-08-21-microgrid-files-design.md` (read it first; conflicts resolve against it, except where a Ruling below refines it).

## Global Constraints

- Commit style: imperative subject, body explains *why*; trailer exactly `Signed-off-by: Sahas Subramanian <sahas.subramanian@proton.me>`; NO `Co-Authored-By` or AI-attribution trailers.
- Stage files explicitly by name (`git add path …`); never `git add -A`/`-u`/`.`; never add `.nfs*` files.
- Gate per task: `cargo clippy --lib --tests -- -D warnings && cargo fmt --check && cargo test --lib`. JS changes must satisfy `npx biome check ui-assets` (config in `biome.json`).
- Never mention, cite, or copy from any repository outside this checkout in code, comments, commit messages, or docs.
- Comments in easy English; say what a thing is, not why the change was made.
- Markers (exact bytes, used everywhere):
  - begin: `;;; switchyard:generated — rewritten by switchyard, do not edit`
  - end: `;;; switchyard:end`
  - Detection matches on the *prefix* `;;; switchyard:generated` / `;;; switchyard:end` so hand-edited trailing text on a marker line does not unmanage a file.
- Rendering rules (the round-trip invariant `load(render(site))` ≡ same components/args/edges depends on these):
  - Generated forms use the `%make-*` primitives (never the `make-*` wrappers), so `*-defaults` at load time cannot change a saved microgrid.
  - Floats render via `lisp_float` (forced decimal point). Non-finite numerics are never rendered — a kwarg whose value is `f32::INFINITY`/NaN is omitted.
  - Strings render through `crate::lisp::escape_lisp_string`.
  - Enum symbols render quoted: `:operational-mode 'inactive`.
  - Runtime state is never rendered: no `:health` / `:telemetry-mode` / `:command-mode` in generated blocks (health flips and mode pokes are runtime, per the spec's "runtime pokes never persisted"); `:operational-mode` IS rendered (it is config, mutated only by `set-component-operational-mode`, which bumps the structural version).

## Pre-existing facts the tasks rely on

- `Config` lives in `src/lisp/mod.rs`; boot in `src/lisp/boot.rs`; journal code to remove in `src/lisp/overrides.rs`; `%make-*` in `src/lisp/make.rs`; `make-microgrid` in `src/lisp/defuns/microgrids.rs`; registry types in `src/sim/microgrids.rs`.
- `MicrogridEntry { def: MicrogridDef, site: MicrogridSite }`; `MicrogridDef { id, name, grpc_port, tso }` (`src/sim/microgrids.rs:39-54`).
- Components are stored as `Arc<dyn SimulatedComponent>` (no downcast anywhere); each keeps its construction config: `Grid` public fields (`src/sim/grid.rs:7-13`), `Meter { interval, power_source: RwLock<Option<DynamicScalar>>, hidden, stream_jitter_pct }` (`src/sim/meter.rs:18-34`), `Battery.cfg: BatteryConfig` (`src/sim/battery.rs:53`), `BatteryInverter.cfg` (`battery_inverter.rs:54`), `SolarInverter.cfg` (`solar_inverter.rs:60`), `EvCharger.cfg` (`ev_charger.rs:53`), `Marker { category, stream_jitter_pct }`. The `cfg` fields are private but each component's own `impl SimulatedComponent` block can read them.
- `DynamicScalar` exposes `is_dynamic()` and `get()`; the source expression of a lambda/symbol is NOT recoverable.
- Name overrides live site-side in a private `name_overrides` map (`src/sim/microgrid_site/mod.rs:129`); `display_name()` conflates override and auto-default; `connections()` filters hidden edges and `hidden_connections()` is its complement — no unfiltered accessor exists yet (the raw insertion-ordered vec is `inner.connections`, `mod.rs:83`).
- `site_import.rs` has `lisp_float` (`site_import.rs:182-188`) and the quoted-enum precedent (`mode_kwargs`, `:242`).
- Config defaults (used to decide omit-vs-render): `BatteryConfig::default` = capacity 92000, initial-soc 50, soc 10..90, voltage 800, rated ±30000, protect-margin 10; `BatteryInverterConfig::default` = rated ±30000, command_delay 0, ramp ∞, reactive pf 0.35/apparent None, reactive delay 100ms, reactive ramp 2000; `SolarInverterConfig::default` adds sunlight 100, rated −30000..0; `EvChargerConfig::default` = rated 0..22000, delay 500ms, ramp ∞, capacity 30000. Stream interval default is 1000 ms (`ms_to_duration(a.interval, 1000)`).
- `ReactiveCapability { pf_limit: Option<f32>, apparent_va: Option<f32> }` — `None` means "disabled", and the `%make-*` kwarg convention is `0` = disable, absent = inherit default. So a renderer MUST print `:reactive-pf-limit 0` for `None` (absent would resurrect the 0.35 default).
- Enterprise setter defuns: `set-enterprise-id`, `set-assets-socket-addr`, `set-dispatch-socket-addr`, `set-default-request-lifetime-ms` (`src/lisp/defuns/metadata.rs`), `set-timezone` (`clock.rs`), `set-frequency-model` (`frequency.rs`). Readback: `Config::metadata()`, `Config::tz_name()`. (Frequency-model params have no readback — they are NOT regenerated; users keep them in the enterprise script tail.)
- HTTP routes live in `src/ui/mod.rs::router` (`mod.rs:52-185`); handlers in `src/ui/handlers/`. UI undo today is client-side (`ui-assets/editor.js` `undoMgr`, GET/POST `/api/mg/{id}/overrides/text`). Create today is a `prompt()` in `ui-assets/panels.js:38-50` writing a `(load-overrides)` stub via `write_microgrid_stub` (`handlers/microgrids.rs:256-299`). Snapshots today copy the ambient overrides file (`src/lisp/snapshots.rs`); `swctl` calls `/api/snapshots/*` (`src/bin/swctl.rs:1205-1254`).
- `sim/common.lisp` defines `overrides-path` (:113-119), `load-overrides` (:121-128), `every` (:54-72, records handles in `active-timers`), `cancel-timers`.
- Tests: `src/lisp/test_support.rs::config_with` + `wrap_test_body`/`strip_set_microgrid_id`; same pair duplicated in `src/ui/tests.rs:17-73`; ~59 `(set-microgrid-id N)` fixture strings across 16 files. `tools/ui-smoke/live-topology.mjs` is a hand-run Playwright suite (`SW_UI=… node tools/ui-smoke/live-topology.mjs`).

## Rulings (decisions the spec left open — binding for all tasks)

1. **Managed detection is strict:** a file is managed iff its FIRST non-empty line starts with the begin-marker prefix. A marker appearing anywhere else in the file is a parse error (no half-managed files).
2. **Constructed-vs-poked values:** `Meter` records `constructed_power: Option<f32>` at construction (the constant value, `None` for lambda/symbol/absent); `set-meter-power` pokes never touch it. `SolarInverterConfig` gains `pub sunlight_dynamic: bool` (set by `make.rs` on the lambda/symbol path); the renderer omits `:sunlight%` when true. Battery/EV render `cfg.initial_soc_pct` (construction), never live SoC. Inverter reactive kwargs render from `cfg` (construction), not the live capability.
3. **Reload attribution & collision rule:** `Config` keeps an ambient `loading: Arc<Mutex<Option<LoadingFile>>>` (`{ path: PathBuf, managed: bool }`), set for the duration of `load_file`/`reload_file`. `make-microgrid` on an already-registered id: reuse-in-place (today's reset semantics) iff the existing entry's `source` equals the currently-loading path; otherwise hard error `microgrid {id} is already loaded (from {path or "the REPL"})`. Nested `(load …)` inside a file attributes its microgrids to the outer file.
4. **Per-file timer hygiene:** `every` records `(SOURCE-FILE . TIMER)` pairs (source from a new `(current-source-file)` defun, `nil` outside a load); `(cancel-file-timers FILE)` cancels one file's timers; `reload_file` calls it before re-eval. Bare `run-with-timer` calls stay unscoped (documented limitation — the config DSL uses `every`).
5. **Old stubs don't hard-crash:** `load-overrides` stays in `sim/common.lisp` as a no-op that logs a deprecation warning telling the user to Adopt the microgrid; `overrides-path` is deleted. Journal files are never read again.
6. **enterprise.lisp is itself two-section:** generated block (enterprise id, timezone, request lifetime, both socket addrs, every bound `*-defaults`) + free script tail preserved on rewrite (frequency-model knobs and anything else hand-written live there).
7. **Adopt v1:** refused when the entry's source file registers more than one microgrid ("split the file first"). For a single-mg unmanaged file: new content = generated block + original text with the adopted mg's top-level `(make-microgrid …)` form commented out (line-prefix `;; `) and an explanatory comment; response carries a warning naming components whose power/sunlight sources are dynamic (their lambdas are not captured — re-add via `set-meter-power` in the script section). For `source: None` (REPL-created): Adopt writes a fresh `microgrids/<id>.lisp` (block only) and sets `source`/`managed`.
8. **Cross-microgrid component-id collisions fail the load:** `id_or_next` additionally consults the registry (via a new `SiteRouter::owner_of_component(id) -> Option<u64>`); an explicit `:id` owned by another microgrid errors. `make-microgrid` already removes a fresh entry when its topology lambda errors, which satisfies "nothing is registered".
9. **Persist trigger:** `eval_locked` snapshots every registered mg's `structural_version` before the eval; after a successful eval it calls `persist(id)` for each *managed* mg whose version moved (suppressed while `loading` is set — loads must not rewrite the file they are reading). A changed mg with no file (REPL `make-microgrid`) logs a one-line warning that it will not survive a restart. `persist_enterprise()` fires when the CST shows a `*-defaults` setq or a head symbol in {`set-enterprise-id`, `set-timezone`, `set-default-request-lifetime-ms`, `set-assets-socket-addr`, `set-dispatch-socket-addr`}.
10. **Self-write suppression for the watcher:** every switchyard write records `(path → hash of written bytes)` in `Config`; a notify event whose file hashes to the recorded value is ignored (use `std::hash::DefaultHasher`; no new dependency).
11. **gitignore:** drop `config.*.overrides.lisp`; add `/microgrids/`, `/snapshots/`, `/enterprise.lisp` (local state when running from the checkout).

## File Structure

- Create `src/lisp/microgrid_file.rs` — markers, `parse`, `compose`, `write_atomic`, `rewrite_id`, `render_block`, `render_empty_block`.
- Modify `src/sim/component.rs` (+2 trait methods) and each component file for the impls; `src/sim/microgrid_site/mod.rs` (+`name_override`, `all_connections`); `src/lisp/mod.rs` (move `lisp_float` in, new `Config` fields); `src/sim/microgrids.rs` (entry/view fields, `owner_of_component`); `src/lisp/defuns/microgrids.rs` (collision rule); `src/lisp/boot.rs` (boot via `load_file`, enterprise.lisp, watcher); `src/lisp/overrides.rs` (gate rewrite, later journal removal); `src/lisp/snapshots.rs` (per-mg); `src/ui/handlers/*` + `src/ui/mod.rs` (routes); `src/bin/swctl.rs` (snapshots); `sim/common.lisp`; `ui-assets/{panels,dialogs,editor,inspect}.js`, `index.html`; `examples/berlin-demo.lisp`; test fixtures.

---

### Task 1: `microgrid_file` text primitives

**Files:**
- Create: `src/lisp/microgrid_file.rs`
- Modify: `src/lisp/mod.rs` (add `pub mod microgrid_file;` after `pub mod make;`; move `lisp_float` here from `src/sim/site_import.rs:182-188` as `pub(crate) fn lisp_float(v: f64) -> String`, and update `site_import.rs` to `use crate::lisp::lisp_float;`)
- Test: inline `#[cfg(test)] mod tests` in `microgrid_file.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `pub const GENERATED_BEGIN: &str = ";;; switchyard:generated";`
  - `pub const GENERATED_BEGIN_LINE: &str = ";;; switchyard:generated — rewritten by switchyard, do not edit";`
  - `pub const GENERATED_END: &str = ";;; switchyard:end";`
  - `pub struct ParsedFile { pub generated: Option<String>, pub script: String }`
  - `pub fn parse(text: &str) -> Result<ParsedFile, String>`
  - `pub fn compose(generated_block: &str, script: &str) -> String`
  - `pub const FRESH_SCRIPT_HEADER: &str` (the ownership comment used when creating a new managed file)
  - `pub fn write_atomic(path: &std::path::Path, text: &str) -> std::io::Result<()>`
  - `pub fn rewrite_id(text: &str, new_id: u64) -> Result<String, String>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const MANAGED: &str = "\
;;; switchyard:generated — rewritten by switchyard, do not edit
(make-microgrid :id 2201 :name \"a\" :grpc-port 8810
  :topology
  (lambda ()
    (%make-meter :id 5)))
;;; switchyard:end
;; Anything below is yours.
(setq x 1)
";

    #[test]
    fn parse_splits_managed_file_into_block_and_script() {
        let p = parse(MANAGED).unwrap();
        let block = p.generated.expect("managed");
        assert!(block.contains("(make-microgrid :id 2201"));
        assert!(!block.contains("switchyard:generated"));
        assert!(!block.contains("switchyard:end"));
        assert!(p.script.contains("(setq x 1)"));
        assert!(!p.script.contains("make-microgrid"));
    }

    #[test]
    fn parse_treats_markerless_text_as_unmanaged_script() {
        let p = parse("(make-microgrid :id 1 :topology (lambda () nil))\n").unwrap();
        assert!(p.generated.is_none());
        assert!(p.script.contains("make-microgrid"));
    }

    #[test]
    fn parse_rejects_marker_not_at_top_and_missing_end() {
        assert!(parse("(setq x 1)\n;;; switchyard:generated\nnil\n;;; switchyard:end\n").is_err());
        assert!(parse(";;; switchyard:generated\n(make-microgrid :id 1)\n").is_err());
        // A second begin inside the block is an error too.
        assert!(parse(
            ";;; switchyard:generated\n;;; switchyard:generated\nnil\n;;; switchyard:end\n"
        )
        .is_err());
    }

    #[test]
    fn compose_then_parse_round_trips() {
        let block = "(make-microgrid :id 7 :grpc-port 8807\n  :topology (lambda () nil))";
        let script = ";; mine\n(setq y 2)\n";
        let text = compose(block, script);
        assert!(text.starts_with(GENERATED_BEGIN));
        let p = parse(&text).unwrap();
        assert_eq!(p.generated.as_deref().map(str::trim), Some(block.trim()));
        assert_eq!(p.script, script);
    }

    #[test]
    fn rewrite_id_replaces_only_the_microgrid_id() {
        let out = rewrite_id(MANAGED, 2299).unwrap();
        assert!(out.contains("(make-microgrid :id 2299"));
        assert!(out.contains("(%make-meter :id 5)"), "component ids untouched");
        assert!(out.contains("(setq x 1)"), "script untouched");
        // Unmanaged text is refused.
        assert!(rewrite_id("(make-microgrid :id 1)", 2).is_err());
    }

    #[test]
    fn write_atomic_replaces_content() {
        let dir = std::env::temp_dir().join(format!("sw-mgfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.lisp");
        write_atomic(&path, "one").unwrap();
        write_atomic(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert!(!path.with_extension("lisp.tmp").exists());
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib microgrid_file` → FAIL (module missing).

- [ ] **Step 3: Implement**

`parse`: iterate `text.lines()`. Find the first non-empty line. If it does not start with `GENERATED_BEGIN`: scan every line — if any line starts with `GENERATED_BEGIN` or `GENERATED_END`, return `Err("switchyard marker found but not at the top of the file")`; else return unmanaged (`generated: None`, `script: text.to_string()`). If it does: collect subsequent lines until one starting with `GENERATED_END` (missing → `Err("missing ';;; switchyard:end' marker")`; a second begin line inside → `Err`), join them with `\n` as `generated`; everything after the end-marker line (preserving original bytes — use byte offsets from `char_indices`/`match_indices('\n')` rather than re-joining lines, so CRLF-free files round-trip byte-identically) is `script`.

`compose`: `format!("{GENERATED_BEGIN_LINE}\n{block}\n{GENERATED_END}\n{script}")` where `block` is trimmed of trailing newlines first.

`FRESH_SCRIPT_HEADER`:

```rust
pub const FRESH_SCRIPT_HEADER: &str = "\
;; Anything below is yours. It runs after the structure, in this
;; microgrid's scope, on every load.
";
```

`write_atomic`: `create_dir_all(parent)`, write to `path.with_extension("lisp.tmp")`, `flush`, `fs::rename` (same pattern as `Config::replace_overrides_text_locked`, `src/lisp/overrides.rs:441-451`).

`rewrite_id`: `parse` first (unmanaged → `Err("cannot rewrite the id of an unmanaged file — edit it by hand")`). Run `tulisp_fmt::parse` on the generated block, find the first top-level `CstNode::List` whose first atom is `make-microgrid`, walk its children for the atom `:id`, take the next atom's span, splice `new_id.to_string()` over that span in the block text (`Err` when the form or `:id` is missing). Re-`compose` with the original script.

- [ ] **Step 4: Run** `cargo test --lib microgrid_file` → PASS; run the full gate.
- [ ] **Step 5: Commit** — `git add src/lisp/microgrid_file.rs src/lisp/mod.rs src/sim/site_import.rs`, message: `Add the microgrid-file format primitives`.

---

### Task 2: components render their constructor forms

**Files:**
- Modify: `src/sim/component.rs` (trait), `src/sim/grid.rs`, `src/sim/meter.rs`, `src/sim/battery.rs`, `src/sim/inverter/battery_inverter.rs`, `src/sim/inverter/solar_inverter.rs`, `src/sim/ev_charger.rs`, `src/sim/marker.rs`, `src/sim/microgrid_site/mod.rs`, `src/lisp/make.rs` (sunlight flag)
- Test: each component file's existing `#[cfg(test)]` module; `src/lisp/make.rs` tests

**Interfaces:**
- Consumes: `crate::lisp::lisp_float` (Task 1).
- Produces (on `SimulatedComponent`, both REQUIRED — no default impl, so a future component cannot forget them):
  - `fn make_fn(&self) -> &'static str` — e.g. `"%make-meter"`.
  - `fn constructor_kwargs(&self) -> Vec<(&'static str, String)>` — construction kwargs in lisp syntax, EXCLUDING `:id`, `:name`, `:successors` and mode kwargs (the renderer owns those).
- Produces (on `MicrogridSite`):
  - `pub fn name_override(&self, id: u64) -> Option<String>` — the raw `name_overrides` entry only (never the component's auto default).
  - `pub fn all_connections(&self) -> Vec<(u64, u64)>` — clone of the unfiltered, insertion-ordered `inner.connections`.
- Produces (fields): `Meter.constructed_power: Option<f32>`; `SolarInverterConfig.sunlight_dynamic: bool` (default `false`; NOT a plist kwarg).

- [ ] **Step 1: Write failing tests** (add to each component's test module; representative — write the analogous test in every component file):

```rust
// src/sim/battery.rs tests
#[test]
fn constructor_kwargs_round_trip_battery() {
    let mut cfg = BatteryConfig::default();
    cfg.capacity_wh = 50_000.0;
    cfg.initial_soc_pct = 20.0;
    let b = Battery::new(7, std::time::Duration::from_millis(500), cfg);
    assert_eq!(b.make_fn(), "%make-battery");
    let kw = b.constructor_kwargs();
    let s = kw.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(" ");
    assert!(s.contains(":capacity 50000.0"));
    assert!(s.contains(":initial-soc 20.0"));
    assert!(s.contains(":interval 500"));
    assert!(s.contains(":rated-lower -30000.0"));
}
```

```rust
// src/sim/meter.rs tests
#[test]
fn meter_records_constructed_power_constant_only() {
    // Constant power → kwarg present; poked value must NOT change it.
    let m = Meter::new(1, std::time::Duration::from_secs(1),
        DynamicScalar::from_lisp(&1875.0f64.into(), 0.0), 0.0, false);
    let kw = |m: &Meter| m.constructor_kwargs().iter()
        .map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(" ");
    assert!(kw(&m).contains(":power 1875.0"));
    m.set_active_power_override(9999.0);
    assert!(kw(&m).contains(":power 1875.0"), "pokes are not construction");
    // No power source → no :power kwarg; hidden renders.
    let h = Meter::new(2, std::time::Duration::from_secs(1), None, 0.0, true);
    assert!(!kw(&h).contains(":power"));
    assert!(kw(&h).contains(":hidden t"));
}
```

(Adapt the `Meter::new` / `DynamicScalar` calls to the real signatures in the file; if `DynamicScalar::from_lisp` needs a `TulispObject`, build one via `tulisp::TulispObject::from(1875.0)` or construct the scalar with whatever constant constructor exists — read `src/sim/dynamic_scalar.rs` first.)

```rust
// src/sim/inverter/battery_inverter.rs tests
#[test]
fn constructor_kwargs_pin_reactive_disabled_as_zero() {
    let mut cfg = BatteryInverterConfig::default();
    cfg.reactive.pf_limit = None; // disabled at construction
    let inv = BatteryInverter::new(3, std::time::Duration::from_secs(1), cfg);
    let s = inv.constructor_kwargs().iter()
        .map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(" ");
    assert!(s.contains(":reactive-pf-limit 0"), "None must pin as 0, got {s}");
    assert!(!s.contains(":ramp-rate"), "infinite ramp is omitted");
    assert!(s.contains(":command-delay-ms 0"));
}
```

```rust
// src/sim/microgrid_site/mod.rs tests
#[test]
fn name_override_and_all_connections_are_raw() {
    let site = MicrogridSite::new();
    site.register(crate::sim::Meter::new(1, std::time::Duration::from_secs(1), None, 0.0, false));
    site.register(crate::sim::Meter::new(2, std::time::Duration::from_secs(1), None, 0.0, true));
    site.connect(1, 2);
    assert_eq!(site.name_override(1), None, "auto default is not an override");
    site.rename(1, "main".into());
    assert_eq!(site.name_override(1).as_deref(), Some("main"));
    // Hidden endpoint edges are still listed, in insertion order.
    assert_eq!(site.all_connections(), vec![(1, 2)]);
    assert!(site.connections().is_empty(), "visible-only stays filtered");
}
```

- [ ] **Step 2: Run to verify failures** — `cargo test --lib` → new tests fail to compile (methods missing). Because the trait methods are required, EVERY component must gain impls in this task or the build fails — that is the point.

- [ ] **Step 3: Implement.** Trait doc + methods in `component.rs`:

```rust
/// The `%make-*` primitive that rebuilds this component on load.
fn make_fn(&self) -> &'static str;

/// Construction kwargs as lisp-syntax (key, value) pairs, excluding
/// `:id`, `:name`, `:successors` and runtime-mode kwargs — the
/// microgrid-file renderer supplies those. Values follow the file
/// format rules: floats via `lisp_float`, non-finite values omitted,
/// disabled reactive caps pinned as `0`.
fn constructor_kwargs(&self) -> Vec<(&'static str, String)>;
```

Exact kwargs per component (value expressions; `lf` = `crate::lisp::lisp_float(x as f64)`; push in this order):

- **Grid** (`"%make-grid-connection-point"`): `:rated-fuse-current {n}` when `rated_fuse_current != 0`; `:rated-lower`/`:rated-upper` when `rated_active_bounds` is `Some((l,u))`; `:stream-jitter-pct` when `!= 0.0`.
- **Meter** (`"%make-meter"`): `:interval {ms}` when `interval != 1000ms`; `:power {lf}` when `constructed_power` is `Some` and finite; `:hidden t` when `hidden`; `:stream-jitter-pct` when `!= 0.0`. Set `constructed_power` in `Meter::new` from the passed source: `power_source.as_ref().filter(|s| !s.is_dynamic()).map(|s| s.get())` — no signature change.
- **Battery** (`"%make-battery"`): always `:capacity`, `:initial-soc`, `:soc-lower`, `:soc-upper`, `:voltage`, `:rated-lower`, `:rated-upper`, `:soc-protect-margin` from `cfg`; `:interval` when `!= 1000ms`; `:stream-jitter-pct` when `!= 0.0`.
- **BatteryInverter** (`"%make-battery-inverter"`): always `:rated-lower`, `:rated-upper`, `:command-delay-ms {cfg.command_delay.as_millis()}`; `:ramp-rate` only when finite; `:interval` when `!= 1000ms`; `:stream-jitter-pct` when `!= 0.0`; always `:reactive-pf-limit` (`cfg.reactive.pf_limit.map(lf).unwrap_or_else(|| "0".into())`), `:reactive-apparent-va` (same pattern), `:reactive-command-delay-ms {ms}`; `:reactive-ramp-rate` when finite.
- **SolarInverter** (`"%make-solar-inverter"`): same as BatteryInverter plus `:sunlight% {lf(cfg.sunlight_pct)}` UNLESS `cfg.sunlight_dynamic`. In `src/lisp/make.rs` `%make-solar-inverter`, set `cfg.sunlight_dynamic = true` in the existing `else` branch that builds `dynamic_sunlight` (`make.rs:417-427`). Add `pub sunlight_dynamic: bool` to `SolarInverterConfig` with `false` in its `Default`.
- **EvCharger** (`"%make-ev-charger"`): always `:rated-lower`, `:rated-upper`, `:initial-soc`, `:soc-lower`, `:soc-upper`, `:soc-protect-margin`, `:capacity`, `:command-delay-ms`; `:ramp-rate` when finite; `:interval` when `!= 1000ms`; `:stream-jitter-pct` when `!= 0.0`.
- **Marker**: `make_fn` maps `self.category`: `Chp → "%make-chp"`, `WindTurbine → "%make-wind-turbine"`, `SteamBoiler → "%make-steam-boiler"`, `PowerTransformer → "%make-power-transformer"`, `Breaker → "%make-breaker"` (unreachable others: `unreachable!()` with the category in the message). Kwargs: `:stream-jitter-pct` when `!= 0.0`.

Site accessors: `name_override` reads the `name_overrides` map directly; `all_connections` clones `inner.connections` under its read lock.

- [ ] **Step 4: Run** `cargo test --lib` → PASS; full gate.
- [ ] **Step 5: Commit** — stage the touched files by name; message: `Let components render their constructor forms`.

---

### Task 3: block renderer + round-trip invariant

**Files:**
- Modify: `src/lisp/microgrid_file.rs`
- Test: `src/lisp/microgrid_file.rs` tests (uses `test_support::config_with`)

**Interfaces:**
- Consumes: Task 1 primitives; Task 2 trait methods and site accessors; `MicrogridDef`; `MicrogridSite::{components, runtime_of, operational_mode, display_name}`; `crate::lisp::escape_lisp_string`.
- Produces:
  - `pub fn render_block(def: &crate::sim::microgrids::MicrogridDef, site: &crate::sim::MicrogridSite) -> String`
  - `pub fn render_empty_block(def: &crate::sim::microgrids::MicrogridDef) -> String`

- [ ] **Step 1: Write the failing round-trip test**

```rust
#[test]
fn render_block_round_trips_through_a_fresh_config() {
    use super::super::test_support::config_with;
    let body = r#"
(make-microgrid :id 2205 :name "rt" :grpc-port 8815 :tso "TN"
  :topology
  (lambda ()
    (%make-grid-connection-point :id 1 :rated-fuse-current 100)
    (%make-meter :id 2 :name "main" :power 1500.0 :interval 500)
    (%make-meter :id 3 :hidden t)
    (%make-battery-inverter :id 4 :rated-lower -8000.0 :rated-upper 8000.0
                            :reactive-pf-limit 0)
    (%make-battery :id 5 :capacity 50000.0 :initial-soc 20.0)
    (%make-solar-inverter :id 6 :sunlight% 40.0)
    (%make-ev-charger :id 7)
    (%make-chp :id 8 :name "chp")
    (%make-meter :id 9 :operational-mode 'inactive)
    (connect 1 2) (connect 2 4) (connect 4 5)
    (connect 2 6) (connect 2 7) (connect 2 8) (connect 2 3) (connect 2 9)))
"#;
    let (cfg, _dir) = config_with(body);
    let (def, site) = {
        let reg = cfg.microgrids();
        let r = reg.lock();
        let e = r.get(&2205).unwrap();
        (e.def.clone(), e.site.clone())
    };
    let block = render_block(&def, &site);
    // Evaluate the rendered block in a second, fresh Config.
    let (cfg2, _dir2) = config_with(&block);
    let reg2 = cfg2.microgrids();
    let r2 = reg2.lock();
    let e2 = r2.get(&2205).expect("rendered block re-registers the microgrid");
    assert_eq!(e2.def.name, "rt");
    assert_eq!(e2.def.grpc_port, 8815);
    assert_eq!(e2.def.tso.as_deref(), Some("TN"));
    // Same components, same constructor forms, same names, same edges.
    let sig = |site: &crate::sim::MicrogridSite| {
        let mut v: Vec<String> = site.components().iter()
            .map(|c| format!("{} {} {:?} {:?} {:?}", c.id(), c.make_fn(),
                             c.constructor_kwargs(), site.name_override(c.id()),
                             site.operational_mode(c.id())))
            .collect();
        v.sort();
        (v, site.all_connections())
    };
    assert_eq!(sig(&site), sig(&e2.site));
    // Rendering the reloaded site is byte-stable.
    assert_eq!(block, render_block(&e2.def, &e2.site));
}
```

- [ ] **Step 2: Run** → FAIL (`render_block` missing).
- [ ] **Step 3: Implement.** Output shape (two-space indents; one component/connect per line):

```lisp
(make-microgrid :id 2205 :name "rt" :grpc-port 8815 :tso "TN"
  :topology
  (lambda ()
    (%make-grid-connection-point :id 1 :rated-fuse-current 100)
    ...
    (connect 1 2)
    ...))
```

Head: `:name` always (escaped); `:tso` only when `Some`. Body: components in `site.components()` order — for each: `({make_fn} :id {id}` + `:name "{escape_lisp_string(n)}"` when `site.name_override(id)` is `Some` + the `constructor_kwargs()` pairs + `:operational-mode '{m}` when `site.operational_mode(id) != OperationalMode::Unspecified` + `)`. Then one `(connect a b)` per `site.all_connections()` entry. Empty topology → body `nil` on one line: `:topology\n  (lambda ()\n    nil))`. `render_empty_block(def)` renders exactly that head with the `nil` body (used by the create endpoint before any component exists).

- [ ] **Step 4: Run** `cargo test --lib microgrid_file` → PASS; full gate.
- [ ] **Step 5: Commit** — `git add src/lisp/microgrid_file.rs`; message: `Render a live microgrid back into its generated block`.

---

### Task 4: loader — source tracking, collision errors, load_file / load_as

**Files:**
- Modify: `src/sim/microgrids.rs`, `src/lisp/defuns/microgrids.rs`, `src/lisp/defuns/mod.rs` (thread the new args), `src/lisp/mod.rs` (Config fields), `src/lisp/boot.rs` (boot via `load_file`; new defun `current-source-file`), `src/lisp/make.rs` (`id_or_next` cross-mg check), `src/lisp/overrides.rs` (route pure-load evals)
- Test: `src/lisp/defuns/microgrids.rs`, `src/lisp/boot.rs` test modules

**Interfaces:**
- Consumes: Task 1 `parse`.
- Produces:
  - `MicrogridEntry` gains `pub source: Option<PathBuf>`, `pub managed: bool`, `pub unsaved: bool`; `MicrogridView` gains `pub managed: bool`, `pub source: Option<String>` (display string), `pub unsaved: bool` (fill in `From<&MicrogridEntry>`). Fix every `MicrogridEntry { … }` literal (grep `MicrogridEntry {`; the handlers' `create_core` and tests).
  - `#[derive(Clone)] pub struct LoadingFile { pub path: PathBuf, pub managed: bool }` in `src/sim/microgrids.rs`; `pub type LoadingSlot = Arc<Mutex<Option<LoadingFile>>>;` + `pub fn new_loading_slot() -> LoadingSlot`.
  - `Config.loading: LoadingSlot` field; threaded into `defuns::register_microgrids(…, loading.clone())`.
  - `pub fn with_loading<R>(slot: &LoadingSlot, file: LoadingFile, f: impl FnOnce() -> R) -> R` (Drop-guard restore, mirror `with_microgrid`, `src/sim/microgrids.rs:199-212`).
  - `Config::load_file(&self, path: &Path) -> Result<Vec<u64>, String>` — resolve relative paths against `state_dir`; read + `microgrid_file::parse` (managed?); take the interpreter lock; snapshot registry keys; `with_loading(...)` around `ctx.eval_file(...)`; return the newly-registered ids; bump each new site's version.
  - `Config::load_as(&self, path: &Path, new_id: u64) -> Result<u64, String>` — read, `rewrite_id`, `write_atomic` to `state_dir/microgrids/{new_id}.lisp` (error if that file exists or the id is registered), then `load_file` the copy.
  - `SiteRouter::owner_of_component(&self, id: u64) -> Option<u64>` — scans registry entries' sites via `site.get(id)`.
  - New defun `(current-source-file)` → the loading path as a string, or nil (registered in `defuns`, capturing the slot).

- [ ] **Step 1: Write failing tests**

```rust
// src/lisp/defuns/microgrids.rs tests
#[test]
fn loading_a_second_file_with_a_taken_id_errors() {
    let (cfg, dir) = config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
    let other = dir.join("other.lisp");
    std::fs::write(&other, "(make-microgrid :id 9 :grpc-port 8801 :topology (lambda () nil))").unwrap();
    let err = cfg.load_file(&other).unwrap_err();
    assert!(err.contains("microgrid 9 is already loaded"), "{err}");
    assert!(err.contains("config.lisp"), "error names the owning file: {err}");
}

#[test]
fn reloading_the_same_file_reuses_the_entry_in_place() {
    let (cfg, dir) = config_with("(make-microgrid :id 9 :grpc-port 8800 :topology \
                                  (lambda () (%make-grid-connection-point :id 1)))");
    let live = cfg.microgrids().lock().get(&9).unwrap().site.clone();
    // Re-loading the SAME file must keep reuse-in-place semantics.
    let path = dir.join("config.lisp");
    cfg.load_file(&path).expect("same-file reload allowed");
    assert!(live.get(1).is_some(), "same site, rebuilt in place");
}

#[test]
fn repl_make_microgrid_with_taken_id_errors() {
    let (cfg, _dir) = config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
    let err = cfg.eval("(make-microgrid :id 9 :grpc-port 8801 :topology (lambda () nil))").unwrap_err();
    assert!(err.contains("already loaded"), "{err}");
}

#[test]
fn component_id_collisions_across_microgrids_fail_the_load() {
    let (cfg, dir) = config_with("(make-microgrid :id 9 :grpc-port 8800 :topology \
                                  (lambda () (%make-meter :id 42)))");
    let other = dir.join("other.lisp");
    std::fs::write(&other, "(make-microgrid :id 10 :grpc-port 8801 :topology \
                            (lambda () (%make-meter :id 42)))").unwrap();
    let err = cfg.load_file(&other).unwrap_err();
    assert!(err.contains("42"), "{err}");
    assert!(!cfg.microgrids().lock().contains_key(&10), "nothing registered");
}

#[test]
fn load_as_copies_and_rewrites_the_id() {
    let (cfg, dir) = config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
    let src = dir.join("managed.lisp");
    std::fs::write(&src, crate::lisp::microgrid_file::compose(
        "(make-microgrid :id 9 :name \"m\" :grpc-port 8890\n  :topology\n  (lambda ()\n    nil))", "")).unwrap();
    let id = cfg.load_as(&src, 11).expect("load as free id");
    assert_eq!(id, 11);
    assert!(cfg.microgrids().lock().contains_key(&11));
    assert!(dir.join("microgrids/11.lisp").exists());
    // Unmanaged files are refused.
    let raw = dir.join("raw.lisp");
    std::fs::write(&raw, "(make-microgrid :id 9 :topology (lambda () nil))").unwrap();
    assert!(cfg.load_as(&raw, 12).is_err());
}
```

Note: `config_with` boots through `Config::new`, which after this task loads the script via `load_file` — so the boot script's entries carry `source: Some(config.lisp)`.

- [ ] **Step 2: Run to verify failures.**
- [ ] **Step 3: Implement.**
  - Registry/`LoadingFile`/slot/`with_loading`/`owner_of_component` as specified.
  - `make-microgrid` defun: replace the silent-reuse `existing` arm (`defuns/microgrids.rs:146-159`): look up the existing entry's `source`; compute `same_file = loading.as_ref().map(|l| entry.source.as_deref() == Some(l.path.as_path())).unwrap_or(false)`; if `!same_file`, return `Err(invalid_argument(format!("microgrid {id} is already loaded (from {})", source-display-or-"the REPL")))`; if reuse, refresh `managed` from the loading slot. On the fresh-entry arm, fill `source`/`managed` from the slot (`None`/`false` for REPL). Delete the dead auto-id special case (`defuns/microgrids.rs:137-144` — `next_free_id_in` already skips taken ids); keep `next_free_id_in` itself.
  - `id_or_next` (`src/lisp/make.rs:647-671`): the `%make-*` defuns capture the router already; change `id_or_next(&w, a.id)` to `id_or_next(&r, &w, a.id)` taking `&SharedSiteRouter`, and on explicit ids also check `router.owner_of_component(id)` → error `component id {id} is already registered in microgrid {mg}`.
  - `Config::load_file` / `load_as` on `boot.rs` or a new small `impl Config` block in `microgrid_file.rs`'s sibling — put them in `src/lisp/boot.rs` next to `reload`.
  - Boot: in `new_inner`, replace the `for script in &scripts { ctx.eval_file(...) }` loop by constructing `Self { … }` first (move the struct construction up, before script eval and before the background-loop spawns), then `for script in &scripts { cfg.load_file(Path::new(script)).map_err(...)? }`, then the empty-registry warnings, validation logging, and loop spawns (loops must still spawn only after successful eval). Keep `loaded_files` recording for now: `load_file` calls `self.record_loaded_file(resolved)` so `reload()`/`watch()` keep working until Task 6 replaces them.
  - `eval_locked` (`src/lisp/overrides.rs:110-185`): when `top_level_load_paths(src)` yields only loads (`!has_other_forms`), call `self.load_file` for each path (inside the held lock — factor a `load_file_locked(&self, ctx, path)`) instead of `ctx.eval_string`, and return the last result; keep the mixed-forms warning path as-is for now.
  - Register `(current-source-file)`.
- [ ] **Step 4: Run** `cargo test --lib` (expect fallout in tests that relied on silent same-id merge — fix each to load distinct ids or reuse the same file path); full gate.
- [ ] **Step 5: Commit** — message: `Track each microgrid's source file and reject id collisions`.

---

### Task 5: persist-on-edit + enterprise.lisp (+ managed create/import)

**Files:**
- Modify: `src/lisp/overrides.rs` (gate → persist), `src/lisp/boot.rs` (enterprise eval at boot), `src/lisp/mod.rs` (fields), `src/lisp/microgrid_file.rs` (enterprise render), `src/ui/handlers/microgrids.rs` (create/import write managed files), `sim/common.lisp` (`load-overrides` shim, delete `overrides-path`)
- Test: `src/lisp/overrides.rs`, `src/ui/tests.rs`

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces:
  - `Config::persist(&self, id: u64) -> std::io::Result<()>` — for a managed entry with a source: `render_block`, `parse` the existing file (script preserved; a fresh/missing file gets `FRESH_SCRIPT_HEADER`), `compose`, `write_atomic`, record the self-write hash (Ruling 10; store `written_hashes: Arc<Mutex<HashMap<PathBuf, u64>>>` on `Config` now, the watcher consumes it in Task 6), clear `unsaved`. On error: set `unsaved = true`, `broadcast_config_error`.
  - `Config::persist_enterprise(&self) -> std::io::Result<()>` + `fn render_enterprise_block(&self, ctx: &mut TulispContext) -> String`; `pub fn enterprise_path(&self) -> PathBuf` (= `state_dir/enterprise.lisp`).
  - `fn enterprise_setter_in(src: &str) -> bool` (CST scan for the Ruling 9 head-symbol set; sibling of `contains_defaults_setq`).
  - Boot: after the embedded prelude and before argv scripts, eval `enterprise.lisp` when present; create it (markers + empty tail) when missing; unreadable/eval-error → boot error.
  - Create endpoint (`create_core`): validate name/id/port (see Task 7 for the request shape; this task keeps the current `{name, tso}` body but changes the file): write `compose(render_empty_block(&def), FRESH_SCRIPT_HEADER)` to `state_dir/microgrids/{id}.lisp` via `write_atomic` (path scheme CHANGES from `config.{id}.lisp` to `{id}.lisp`), then `config.load_file(&path)` instead of manual registry insert + `record_loaded_file`. Import keeps its flow (create, then `eval_in_mg(id, forms)`) — the eval now triggers `persist(id)`, regenerating the file with the imported components.

- [ ] **Step 1: Write failing tests**

```rust
// src/lisp/overrides.rs tests — REPLACE eval_appends_each_successful_form_to_override_file
// and persist_gate_keeps_config_drops_pokes with:
#[test]
fn structural_evals_regenerate_the_managed_file() {
    let (cfg, dir) = config_with("(make-microgrid :id 9 :grpc-port 8800 :topology \
                                  (lambda () (%make-grid-connection-point :id 1)))");
    // config.lisp is unmanaged → a structural eval flags, but writes nothing.
    cfg.eval("(rename-component 1 \"a\")").unwrap();
    assert!(!dir.join("microgrids").exists());

    // A managed microgrid: created like the UI does it.
    let def = crate::sim::microgrids::MicrogridDef {
        id: 20, name: "m".into(), grpc_port: 8890, tso: None };
    let path = dir.join("microgrids/20.lisp");
    crate::lisp::microgrid_file::write_atomic(&path, &crate::lisp::microgrid_file::compose(
        &crate::lisp::microgrid_file::render_empty_block(&def),
        crate::lisp::microgrid_file::FRESH_SCRIPT_HEADER)).unwrap();
    cfg.load_file(&path).unwrap();
    cfg.eval_in_mg(20, "(%make-meter :id 100 :power 500.0)").unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("(%make-meter :id 100"), "{text}");
    assert!(text.contains(":power 500.0"), "{text}");
    assert!(text.contains("Anything below is yours"), "script section preserved");
    // A poke does not rewrite the file.
    let before = std::fs::read_to_string(&path).unwrap();
    cfg.eval_in_mg(20, "(set-meter-power 100 4321.0)").unwrap();
    assert_eq!(before, std::fs::read_to_string(&path).unwrap());
}

#[test]
fn defaults_edits_regenerate_enterprise_lisp() {
    let (cfg, dir) = config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
    cfg.eval("(setq battery-defaults '(:capacity 1000.0))").unwrap();
    let text = std::fs::read_to_string(dir.join("enterprise.lisp")).unwrap();
    assert!(text.contains("battery-defaults"), "{text}");
    assert!(text.contains(":capacity 1000.0"), "{text}");
    cfg.eval("(set-enterprise-id 77)").unwrap();
    let text = std::fs::read_to_string(dir.join("enterprise.lisp")).unwrap();
    assert!(text.contains("(set-enterprise-id 77)"), "{text}");
}
```

```rust
// src/ui/tests.rs — THE restart test
#[tokio::test]
async fn ui_created_microgrid_survives_a_restart() {
    let (config, dir) = config_with_dir("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))").await;
    // Create via the endpoint, add components via scoped eval.
    let (st, body) = call(config.clone(),
        post("/api/microgrids/create", r#"{"name":"persist me"}"#)).await;
    assert_eq!(st, StatusCode::OK);
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = created["id"].as_u64().unwrap();
    let (st, _) = call(config.clone(), post(&format!("/api/mg/{id}/eval"),
        "(%make-grid-connection-point :id 300 :successors (list (%make-meter :id 301 :power 250.0)))")).await;
    assert_eq!(st, StatusCode::OK);

    // "Restart": a brand-new Config on the same state dir, loading the file.
    let file = dir.join(format!("microgrids/{id}.lisp"));
    let cfg2 = Config::new_with(&[file.to_string_lossy().into_owned()], Some(dir.clone())).unwrap();
    let reg = cfg2.microgrids();
    let r = reg.lock();
    let e = r.get(&id).expect("microgrid survives the restart");
    assert_eq!(e.def.name, "persist me");
    assert!(e.site.get(300).is_some() && e.site.get(301).is_some(), "identical component ids");
    assert!((e.site.get(301).unwrap().aggregate_power_w(&e.site) - 250.0).abs() < 1e-3);
}
```

(`config_with_dir` = the existing `config_with` extended to also return the temp dir; add it beside `config_with` in `src/ui/tests.rs`.)

- [ ] **Step 2: Run to verify failures.**
- [ ] **Step 3: Implement.**
  - `eval_locked`: snapshot `Vec<(id, structural_version, managed)>` before eval; after a successful eval (and not while `loading` is set), for each mg whose version moved: managed+source → `persist(id)` (ignore the io::Result — persist itself banners/flags); source None → `log::warn!("microgrid {id} changed but is not backed by a file; the edit will not survive a restart")`. Then `persist_enterprise()` when `contains_defaults_setq(src) || enterprise_setter_in(src)`. DELETE `append_to_overrides_file` and the journal-append branch + its banner (keep `persisted_overrides*`/`overrides_text*`/`remove_persisted_overrides*` compiling until Task 7 removes their routes with them).
  - `render_enterprise_block` (order fixed): `(set-enterprise-id N)`, `(set-timezone "…")`, `(set-default-request-lifetime-ms N)`, `(set-assets-socket-addr "…")`, `(set-dispatch-socket-addr "…")`, then per category in `["grid","meter","battery","battery-inverter","solar-inverter","ev-charger","marker"]`: eval `(and (boundp '{var}) {var})`; when non-nil emit `(setq {var}\n      '{value})` with the value pretty-printed through `tulisp_fmt::format_with_width(…, 72)` falling back to the raw `Display` text.
  - Boot: eval enterprise.lisp between prelude and scripts (create-if-missing with `compose("", "")`-style empty block — use `compose` with an empty block line `nil`? No: enterprise's generated block may be empty; render markers with nothing between them and the fresh header as tail).
  - `sim/common.lisp`: delete `overrides-path`; replace `load-overrides` body with a no-op that logs once (use the existing logging defun from `src/lisp/defuns/log.rs` — read it for the name; if none fits, `(message …)`): "load-overrides is gone; this microgrid predates managed files — use Adopt in the UI".
  - `create_core`: as in Interfaces. Delete `write_microgrid_stub`.
- [ ] **Step 4: Run** full gate + `cargo test` (integration tests touching create/import may need fixture updates).
- [ ] **Step 5: Commit** — message: `Persist structural edits into managed microgrid files`.

---

### Task 6: per-file reload, per-file timers, watcher rework

**Files:**
- Modify: `src/lisp/boot.rs` (`reload_file`, `reload`, `watch`), `src/lisp/mod.rs` (drop `loaded_files`), `src/lisp/overrides.rs` (drop load-recording), `sim/common.lisp` (`every` / `cancel-timers` / `cancel-file-timers`)
- Test: `src/lisp/boot.rs`; `tests/hot_reload.rs` must keep passing unchanged

**Interfaces:**
- Consumes: Tasks 4–5.
- Produces:
  - `Config::reload_file(&self, path: &Path) -> Result<Vec<u64>, String>` — under one interpreter lock: `(cancel-file-timers "{path}")`, reset the site of every entry whose `source == path`, then `load_file_locked`. Undo/snapshots/watcher all call this.
  - `Config::reload(&self) -> Result<(), String>` — now: cancel ALL timers, reset every entry's site + the bootstrap site + the id allocator (as today, `boot.rs:563-641`), re-eval `enterprise.lisp`, then `load_file_locked` every distinct registered `source` in first-registration order. `loaded_files` field, `record_loaded_file`, and the load-recording in `eval_locked`/`load_file` are DELETED.
  - `watch()` — watch enterprise.lisp + every registered source + `extra_watches`; re-arm the watch list after each reload (sources can change); on event for enterprise.lisp → re-eval it; for a source → `reload_file`; skip events whose content hash matches `written_hashes` (Ruling 10).
  - Lisp: `active-timers` entries become `(FILE . TIMER)` conses (`every` uses `(cons (current-source-file) handle)`); `cancel-timers` cancels the cdr of every entry; new `(cancel-file-timers FILE)` cancels + drops entries whose car `equal`s FILE. Mirror the list-walking idiom already used in `cancel-timers` / `reset-state` (`sim/common.lisp:36-48`).

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn reload_file_resets_only_that_files_microgrids() {
    let (cfg, dir) = config_with("(make-microgrid :id 9 :grpc-port 8800 :topology \
                                  (lambda () (%make-grid-connection-point :id 1)))");
    let other = dir.join("other.lisp");
    std::fs::write(&other, "(make-microgrid :id 10 :grpc-port 8801 :topology \
                            (lambda () (%make-meter :id 50)))").unwrap();
    cfg.load_file(&other).unwrap();
    // Rename mg 9's component, then reload only other.lisp.
    cfg.eval_in_mg(9, "(rename-component 1 \"kept\")").unwrap();
    cfg.reload_file(&other).unwrap();
    let reg = cfg.microgrids();
    let r = reg.lock();
    assert_eq!(r[&9].site.display_name(1).as_deref(), Some("kept"),
               "mg 9 untouched by mg 10's reload");
    assert!(r[&10].site.get(50).is_some());
}

#[test]
fn reload_file_cancels_that_files_timers_only() {
    let (cfg, dir) = config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
    let other = dir.join("t.lisp");
    // The file arms a counter timer via `every`.
    std::fs::write(&other, "(make-microgrid :id 10 :grpc-port 8801 :topology (lambda () nil))\n\
                            (setq n 0)\n\
                            (every :milliseconds 1 :call (lambda () (setq n (+ n 1))))").unwrap();
    cfg.load_file(&other).unwrap();
    let count = || -> i64 {
        cfg.eval_silent("(length active-timers)").unwrap().parse().unwrap()
    };
    assert_eq!(count(), 1, "one timer after the first load");
    cfg.reload_file(&other).unwrap();
    // The reload cancelled t.lisp's old timer before re-arming: still one
    // entry, not two. A second reload stays at one too.
    assert_eq!(count(), 1, "reload must not double-arm the file's timers");
    cfg.reload_file(&other).unwrap();
    assert_eq!(count(), 1);
}
```

- [ ] **Step 2: Run to verify failures.**
- [ ] **Step 3: Implement** per Interfaces. `reload`'s callers: `tests`, `watch` — no HTTP route calls it directly anymore after Task 7.
- [ ] **Step 4: Run** `cargo test --lib boot && cargo test --test hot_reload`; full gate.
- [ ] **Step 5: Commit** — message: `Reload per file instead of replaying a loaded-file list`.

---

### Task 7: HTTP surface — load / create / adopt / undo / per-mg snapshots; journal endpoints removed

**Files:**
- Modify: `src/ui/handlers/microgrids.rs` (create body, load, load-as, adopt), `src/ui/handlers/snapshots.rs`, `src/lisp/snapshots.rs`, `src/ui/handlers/overrides.rs` (delete), `src/ui/handlers/mod.rs`, `src/ui/mod.rs` (routes), `src/lisp/overrides.rs` (delete journal fns + `PersistedOverride`), `src/lisp/mod.rs` (undo history field, re-exports), `src/bin/swctl.rs`
- Test: `src/ui/tests.rs`

**Interfaces:**
- Consumes: Tasks 1–6.
- Produces (routes; all mutating handlers wrap Config calls in `super::blocking`):
  - `POST /api/load` `{path: String}` → `{loaded: [ids]}` or 409 `{error, collision_id, managed, suggested_id}` (suggested = `next_free_id`); detection: the `load_file` error contains `"is already loaded"` — have `load_file` return a structured error enum `LoadError { Collision { id: u64 }, Other(String) }` instead of string-matching (adjust Task 4's signature: `Result<Vec<u64>, LoadError>`; `impl Display`).
  - `POST /api/load-as` `{path: String, id: u64}` → `{id}`.
  - `POST /api/microgrids/create` body becomes `{name, id?, grpc_port?, tso?}`; response unchanged plus `managed: true`. 409 when id/port taken.
  - `POST /api/mg/{mg_id}/adopt` → `{ok: true, warnings: [String]}` per Ruling 7.
  - `POST /api/mg/{mg_id}/undo` and `/redo` → `{ok, undo_depth, redo_depth}`; `GET /api/mg/{mg_id}/undo` → depths. Backed by `Config.undo: Arc<Mutex<HashMap<u64, UndoHistory>>>` where `struct UndoHistory { undo: VecDeque<String>, redo: Vec<String> }` (cap 20, oldest dropped). `persist(id)` pushes the PREVIOUS generated block (parse of the file before overwrite) onto `undo` and clears `redo`. Undo: pop, `compose` with the file's current script, `write_atomic`, `reload_file`, push displaced block onto `redo` (redo mirrors).
  - Snapshots: `GET /api/mg/{id}/snapshots`, `POST /api/mg/{id}/snapshots/save` `{name}`, `POST /api/mg/{id}/snapshots/load` `{name, as_id?}`. Rewrite `src/lisp/snapshots.rs`: dir `snapshots/{mg_id}/`; save = copy the mg's source file (managed entries only, 409 otherwise); load = `write_atomic` the snapshot text over the source + `reload_file`; `as_id` → `load_as` on the snapshot file. Keep `sanitise_snapshot_path`. DELETE the ambient `/api/snapshots*` routes.
  - DELETE routes + handlers + Config fns: `/api/overrides`, `/api/mg/{id}/overrides`, `/api/persisted/*`, `/api/mg/{id}/persisted/*`, `/api/mg/{id}/overrides/text` (GET+POST); `persisted_overrides{,_for,_from}`, `remove_persisted_overrides{,_for,_locked}`, `overrides_text`, `replace_overrides_text{,_locked}`, `overrides_path{,_for}`, `PersistedOverride` (+ its re-export in `src/lisp/mod.rs`), the whole `src/ui/handlers/overrides.rs`.
  - `swctl` (`src/bin/swctl.rs:1205-1254` + the snapshot subcommand docs at `:173,:285,:290`): snapshots now need a microgrid — reuse whatever `--mg`/microgrid-selection convention the other swctl subcommands use (read the file; follow its existing pattern) and hit the per-mg endpoints.

- [ ] **Step 1: Write failing tests** (in `src/ui/tests.rs`; use the `call`/`get`/`post` helpers):

```rust
#[tokio::test]
async fn load_endpoint_offers_load_as_on_collision() {
    let (config, dir) = config_with_dir("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))").await;
    let text = switchyard::lisp::microgrid_file::compose(
        "(make-microgrid :id 9 :name \"dup\" :grpc-port 8890\n  :topology\n  (lambda ()\n    nil))", "");
    std::fs::write(dir.join("dup.lisp"), &text).unwrap();
    let (st, body) = call(config.clone(), post("/api/load", r#"{"path":"dup.lisp"}"#)).await;
    assert_eq!(st, StatusCode::CONFLICT);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["collision_id"], 9);
    let suggested = v["suggested_id"].as_u64().unwrap();
    let (st, _) = call(config.clone(), post("/api/load-as",
        &format!(r#"{{"path":"dup.lisp","id":{suggested}}}"#))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(config.microgrids().lock().contains_key(&suggested));
}

#[tokio::test]
async fn undo_reverts_the_last_structural_edit() {
    let (config, _dir) = config_with_dir("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))").await;
    let (st, body) = call(config.clone(),
        post("/api/microgrids/create", r#"{"name":"u","id":30}"#)).await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    call(config.clone(), post("/api/mg/30/eval", "(%make-meter :id 500)")).await;
    call(config.clone(), post("/api/mg/30/eval", "(%make-meter :id 501)")).await;
    let (st, _) = call(config.clone(), post("/api/mg/30/undo", "")).await;
    assert_eq!(st, StatusCode::OK);
    let site = config.microgrids().lock().get(&30).unwrap().site.clone();
    assert!(site.get(500).is_some() && site.get(501).is_none(), "one step undone");
    let (st, _) = call(config.clone(), post("/api/mg/30/redo", "")).await;
    assert_eq!(st, StatusCode::OK);
    let site = config.microgrids().lock().get(&30).unwrap().site.clone();
    assert!(site.get(501).is_some(), "redo restores");
}

#[tokio::test]
async fn snapshots_are_per_microgrid() {
    let (config, dir) = config_with_dir("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))").await;
    call(config.clone(), post("/api/microgrids/create", r#"{"name":"s","id":31}"#)).await;
    call(config.clone(), post("/api/mg/31/eval", "(%make-meter :id 600)")).await;
    let (st, _) = call(config.clone(), post("/api/mg/31/snapshots/save", r#"{"name":"one"}"#)).await;
    assert_eq!(st, StatusCode::OK);
    assert!(dir.join("snapshots/31/one.lisp").exists());
    call(config.clone(), post("/api/mg/31/eval", "(remove-component 600)")).await;
    let (st, _) = call(config.clone(), post("/api/mg/31/snapshots/load", r#"{"name":"one"}"#)).await;
    assert_eq!(st, StatusCode::OK);
    let site = config.microgrids().lock().get(&31).unwrap().site.clone();
    assert!(site.get(600).is_some(), "restore brings the meter back");
    // The ambient endpoint is gone.
    let (st, _) = call(config.clone(), get("/api/snapshots")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn adopt_makes_an_unmanaged_single_mg_file_managed() {
    let (config, dir) = config_with_dir("(make-microgrid :id 9 :grpc-port 8800 :topology \
                                         (lambda () (%make-meter :id 700 :power 100.0)))").await;
    let (st, body) = call(config.clone(), post("/api/mg/9/adopt", "")).await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let text = std::fs::read_to_string(dir.join("config.lisp")).unwrap();
    assert!(text.starts_with(";;; switchyard:generated"));
    assert!(text.contains(";; (make-microgrid"), "original form commented out: {text}");
    // Managed now: a structural edit rewrites the file.
    call(config.clone(), post("/api/mg/9/eval", "(%make-meter :id 701)")).await;
    let text = std::fs::read_to_string(dir.join("config.lisp")).unwrap();
    assert!(text.contains("(%make-meter :id 701"));
}
```

- [ ] **Step 2: Run to verify failures.**
- [ ] **Step 3: Implement** per Interfaces. Adopt implementation: entry lookup → multi-mg-source check (count registry entries sharing the source) → `render_block` → script = original text with the mg's top-level `(make-microgrid …)` CST form's lines each prefixed `;; ` plus a preceding line `;; superseded by the generated block above:` → warnings for components where (`constructed_power.is_none() && power_source.is_some()`) — expose that as needed via a small trait addition? NO: compute from the render side — a meter whose kwargs lack `:power` but whose `aggregate_power_w` is nonzero is heuristic; instead have `constructor_kwargs` untouched and build warnings from `SolarInverterConfig.sunlight_dynamic`-style info you can reach: add `fn has_unrenderable_source(&self) -> bool` default `false` to `SimulatedComponent`, `true` for Meter when `constructed_power.is_none()` but a dynamic source exists, and for SolarInverter when `cfg.sunlight_dynamic`.
- [ ] **Step 4: Run** full gate + `cargo test` (integration `tests/ui_http.rs` etc. may reference removed endpoints — update).
- [ ] **Step 5: Commit** — message: `Move load, undo, adopt and snapshots onto managed files`.

---

### Task 8: UI

**Files:**
- Modify: `ui-assets/index.html`, `ui-assets/panels.js`, `ui-assets/dialogs.js`, `ui-assets/editor.js`, `ui-assets/inspect.js`, `ui-assets/app.js` (keyboard wiring unchanged, but verify), `ui-assets/chrome.js` (overrides pill removal), `tools/ui-smoke/live-topology.mjs`
- Test: hand-run smoke (`SW_UI=… node tools/ui-smoke/live-topology.mjs`) — extend it; `biome check ui-assets`

**Interfaces:**
- Consumes: Task 7 endpoints; `MicrogridView.managed/source/unsaved` (Task 4).
- Produces (UI behavior):
  1. **Create dialog** replaces the `prompt()` at `panels.js:38-50`: a `<dialog id="create-mg-dialog">` in `index.html` with fields name (text, required), id (number, pre-filled from a new `GET /api/microgrids` scan client-side: `Math.max(2200, …taken+1)` is WRONG — fetch the server's suggestion instead: extend `POST /api/microgrids/create` 409 handling, and pre-fill by computing the next free id from the already-loaded microgrids list the panel holds), port (number, blank = server default). Submit → `mutate("POST", "/api/microgrids/create", {name, id, grpc_port})`; 409 → inline error in the dialog.
  2. **Load picker** (`showLoadScriptDialog`, `panels.js:107-150`): default listing starts at `microgrids/` when that directory exists (pass `dir=microgrids` on the first `/api/scripts` fetch, falling back to the root listing on 4xx); replace the `loadScript` eval-based body (`panels.js:84-99`) with `POST /api/load {path}`; on 409 render a bar in the dialog: "microgrid {collision_id} is already loaded — Load as {suggested_id}?" whose button posts `/api/load-as {path, id: suggested_id}` (hidden when `managed:false` — show "edit the id in the file" instead).
  3. **Managed badge + read-only structure**: microgrid cards (`renderList` in panels.js) show an `unmanaged` chip when `!m.managed` and an `unsaved` chip when `m.unsaved`. In `inspect.js`, gate every structural affordance (add-component controls, connect/disconnect, remove, rename, hidden toggle, construction-arg editors — locate them by following `evalQuoted` callers) behind the current microgrid's `managed` flag (the microgrids list is already fetched; thread the flag into the state the inspector reads). Runtime controls (setpoints, drive, health, modes, scenarios) stay enabled. Disabled affordances get `title="unmanaged file — structure is read-only (Adopt to edit)"`.
  4. **Adopt button** in the inspector's microgrid header, visible when `!managed`: `mutate("POST", `/api/mg/${id}/adopt`)`, then refresh; show returned `warnings` as a toast/banner.
  5. **Undo/redo** (`editor.js` `undoMgr`, `editor.js:52-145`): delete the client stacks and text fetch/post; `record()` becomes a no-op removal — delete it and its call in `inspect.js:304-305`; `undo()`/`redo()` POST `/api/mg/{id}/undo|redo`. Keep the keyboard wiring in `app.js:395-419` calling `undoMgr.undo()/redo()`.
  6. **Overrides dialog removal**: delete `overrideState`, `showOverridesDialog`, `renderOverridesDialog`, `setupOverridesDialog`, `setupOverridesPill` (`dialogs.js:23-250`) and the pill markup in `index.html`/`chrome.js`; remove the `#pending-dialog` markup.
  7. **Snapshots dialog** (`dialogs.js:159-223`): endpoints become `/api/mg/{currentMgId}/snapshots{,/save,/load}`; disable the dialog with a hint when no microgrid is selected or the current one is unmanaged.
- Produces (smoke): extend `tools/ui-smoke/live-topology.mjs` with checks: create dialog exists (`#create-mg-dialog`), overrides pill is gone, load dialog lists `microgrids/`.

- [ ] **Step 1: Implement** the seven items (JS has no unit-test harness beyond the smoke file; write the smoke checks first where practical).
- [ ] **Step 2: Verify** — `npx biome check ui-assets`; boot `cargo run -- examples/berlin-demo.lisp` (AFTER Task 9 lands berlin-demo may be managed; at this task's point boot any example) and hand-run the smoke file; exercise create → edit → restart → Load from picker manually.
- [ ] **Step 3: Commit** — message: `Rework the UI for managed microgrid files`.

---

### Task 9: fixture sweep, example conversion, docs, gitignore

**Files:**
- Modify: `src/lisp/test_support.rs`, `src/ui/tests.rs` (delete `strip_set_microgrid_id` + `LOAD_OVERRIDES_HELPER` and its uses), every fixture containing `(set-microgrid-id N)` (16 files, ~59 occurrences — `grep -rn "set-microgrid-id" --include="*.rs"`), `examples/berlin-demo.lisp`, `.gitignore`, `AGENTS.md`, `README.md` (if it documents overrides), `docs/e2e-testing.md` (if it references overrides endpoints)
- Test: full `cargo test` (lib + integration)

**Interfaces:**
- Consumes: everything prior.
- Produces: no new API — a clean tree.

- [ ] **Step 1: Fixture sweep.** Delete `strip_set_microgrid_id` from both support files; `wrap_test_body` keeps auto-wrapping but no longer strips. Rewrite each `(set-microgrid-id N)` fixture: when the body relied on the auto-wrap, drop the `(set-microgrid-id N)` form — and when N ≠ 2200 mattered to assertions, wrap explicitly: `(make-microgrid :id N :grpc-port 8800 :topology (lambda () …))`. This is one mechanical batch — do it in a single pass, one commit.
- [ ] **Step 2: Convert `examples/berlin-demo.lisp`** to the managed format: generated block first (every component as `%make-*` with explicit `:id` and args — derive it by booting the current file and calling `render_block` from a throwaway test, then paste), then the script section carrying everything else (scenario definitions, `every` blocks, and one `(set-meter-power ID (lambda …))` per meter that had a lambda `:power`, since lambdas cannot live in the generated block). Delete the `(load-overrides)` call. Verify with the existing `default_config_boots_and_registers_library_scenarios` test (`src/lisp/boot.rs:981-997`) and by comparing `/api/topology` component ids before/after conversion by hand.
- [ ] **Step 3: gitignore + docs.** Apply Ruling 11 to `.gitignore`. Update `AGENTS.md`'s config/overrides description to the managed-file model (two sections, markers, explicit loads, per-mg snapshots/undo; `enterprise.lisp`). Sweep docs for `load-overrides` / `overrides` claims: `grep -rn "load-overrides\|overrides" README.md AGENTS.md docs/ scenarios/ --include="*.md"`.
- [ ] **Step 4: Run** the full gate + `cargo test` (all integration suites).
- [ ] **Step 5: Commit** — up to three commits: the fixture sweep, the example conversion, the docs/gitignore.

---

## Execution notes

- Task order is dependency order; no task may be reordered across Task 4 (the loader flip) without re-checking compilation.
- Tasks 4–7 change `Config`'s surface; expect existing tests to need updates — update them for the new semantics rather than weakening assertions.
- The per-commit build must stay green: each task compiles and passes on its own.

## Self-review (done at plan-writing time)

- Spec coverage: file format + markers (T1), `make_form` equivalent + round-trip (T2–T3), registry source/managed + collision + load-as + component-id collision (T4), persist-on-edit + enterprise.lisp + create/import + `load-overrides` removal (T5), per-mg reload + watcher (T6), undo + snapshots-per-mg + adopt + endpoint removals + swctl (T7), UI (T8), examples/tests/gitignore/docs (T9). Deliberately deferred to sub-projects 2–3: unload, state-dir switching (spec sections marked as such).
- The spec's "generated block … `:topology` lambda containing every component" is met with flat `%make-*` + `connect` lines rather than nested `:successors` — equivalent by the round-trip test, simpler to render.
- Type consistency: `load_file` returns `Result<Vec<u64>, LoadError>` from Task 7 onward; Task 4 may land it as `Result<Vec<u64>, String>` and Task 7 upgrades it — Task 7's step text says so.
