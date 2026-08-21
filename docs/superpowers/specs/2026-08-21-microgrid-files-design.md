# Microgrid files — design

2026-08-21. One Lisp file per microgrid as the only source of truth,
written by switchyard and readable by people; explicit loads and
unloads; a state directory that can be switched at run time. Replaces
the base-config + overrides-journal scheme.

## Problem

Today a microgrid's definition is spread over a hand-written config,
an append-only `config.<id>.overrides.lisp` journal, an in-memory
list of loaded files, and a persist gate that decides which REPL forms
reach the journal. The pieces disagree:

- Nothing scans `microgrids/` at start-up, and the list of loaded
  files lives only in memory — a microgrid created from the UI is
  gone after a restart.
- The overrides path is computed twice (Rust and Lisp) against
  different anchors (`--state-dir` vs the script's directory); a stub
  booted from another directory comes back registered and empty, with
  no error.
- Auto-assigned component ids are journaled without `:id` and
  re-minted on replay, so ids drift between runs.
- Two microgrids with the same id silently merge; the second wins.
- Snapshots and the overrides dialog act on the lowest-id microgrid,
  not the selected one; process-wide settings (`*-defaults`, the
  enterprise id, frequency) are journaled into one microgrid's file.
- `(set-microgrid-id)` exists only as a test-fixture rewrite.

## Decisions

- **One file per microgrid, two sections.** `microgrids/<id>.lisp`:
  a *generated block* (everything the UI can express: the
  `make-microgrid` form with id, name, gRPC port, TSO, and inside its
  `:topology` every component with an explicit `:id` and all its
  construction arguments, then every connection) followed by a
  *script section* switchyard never touches and always evaluates
  after the structure, inside the microgrid's scope. Markers:

  ```lisp
  ;;; switchyard:generated — rewritten by switchyard, do not edit
  (make-microgrid :id 2201 :name "Berlin demo" :grpc-port 8810 :tso "TN"
    :topology (lambda ()
      (%make-grid-connection-point :id 1 :name "grid-1")
      (%make-meter :id 2 :name "meter-2" :main t)
      …
      (connect 1 2)
      …))
  ;;; switchyard:end
  ;; Anything below is yours. It runs after the structure, in this
  ;; microgrid's scope, on every load.
  (set-meter-power 100 (lambda () …))
  ```
- **Explicit loads only.** A microgrid exists in a running switchyard
  because a file was loaded: on the command line
  (`switchyard [--state-dir DIR] [file …]`), with the UI's *Load*
  (a picker over `DIR/microgrids/*.lisp` plus a free path field), or
  with `(load "path")`. There is no directory scan and no manifest.
- **Ids are explicit and collisions are errors.** `make-microgrid`
  without `:id` still auto-allocates (hand-written scripts), but a
  file switchyard writes always carries the id, and loading a file
  whose id is already registered fails with
  `microgrid 2201 is already loaded (from <path>)` — no merge. The UI
  then offers *Load as <next free id>*, which copies the file to
  `microgrids/<new>.lisp` with the id rewritten in the generated block
  (refused for unmanaged files: "edit the id in the file"). Component
  ids stay enterprise-unique as today; a load that would collide on a
  component id fails the same way and nothing is registered.
- **Managed vs unmanaged files.** A file with the markers is managed:
  the UI's structural edits regenerate its block. A file without them
  (today's examples, any hand-written script) is unmanaged: it loads
  and runs, the canvas is read-only for structure (runtime knobs,
  setpoints and scenarios still work), and the inspector offers
  *Adopt*: switchyard writes the generated block for the live
  microgrid above the file's existing text, which becomes the script
  section. The repo's examples that were edited from the canvas
  (`berlin-demo` and the others carrying `(load-overrides)`) are
  converted to the managed format in this change.
- **Every structural UI edit rewrites the file.** Add / connect /
  disconnect / remove / rename / hidden / subtype / construction
  arguments, and `make-microgrid` attributes (name, port, TSO). After
  the mutation succeeds in memory, the generated block is regenerated
  from the live microgrid and the file is replaced atomically
  (temp + rename), script section copied through byte for byte.
  Runtime pokes (`set-active-power`, `set-meter-power`, modes,
  health flips, scenarios) are never persisted — that is what the
  script section is for.
- **Process-wide state has its own file.** `DIR/enterprise.lisp`
  holds the enterprise id, grid-frequency knobs and the `*-defaults`
  variables; it is evaluated before any microgrid file, and the UI
  Defaults panel regenerates it. Nothing process-wide is ever written
  into a microgrid file.
- **Undo / redo** keep working, per microgrid: the server holds a
  bounded history (20 entries) of generated blocks; undo writes the
  previous block and reloads that microgrid. The overrides dialog
  (delete journal forms) is removed.
- **Reload is per microgrid.** Reload = unload + load the same file,
  so the script section runs again. The file watcher that today
  replays everything watches the loaded files and reloads only the
  microgrid whose file changed (a managed file written by switchyard
  itself does not trigger a reload).
- **Unload.** `(unload-microgrid ID)`, `DELETE /api/mg/{id}`, a swctl
  verb and a UI button stop the microgrid's runtime (gRPC listener
  first, so streaming tasks drop their component Arcs; then physics,
  history sampler, loopback), remove the registry entry, free its
  port and clear `current_microgrid` if it pointed there. Files are
  untouched; load brings it back. A running scenario on that
  microgrid refuses the unload (v1). Its dispatches are dropped with
  `Deleted` events. *Unload and delete file* is a second, explicit
  button that also removes `microgrids/<id>.lisp` (snapshots stay).
- **State directory at run time.** The UI's Settings dialog shows the
  current directory, a field to enter another, and the recent ones.
  Switching (`PUT /api/state-dir`) unloads every microgrid, re-points
  the load path, evaluates the new `enterprise.lisp` (creating the
  directory and an empty file if missing), and refreshes the Load
  picker; nothing loads automatically. The last directory is
  remembered in `$XDG_CONFIG_HOME/switchyard/settings.toml`
  (`last_state_dir`, `recent_state_dirs`); `--state-dir` on the
  command line wins over it. The default is the current working
  directory, as today, with one anchor rule everywhere: `state_dir`
  is the only base for relative paths.
- **Snapshots** are per microgrid: `DIR/snapshots/<id>/<name>.lisp`,
  a copy of the microgrid file. Restore replaces the file and reloads
  that microgrid; restoring into a different id rewrites the id in
  the generated block. Endpoints move under `/api/mg/{id}/snapshots`.
- **Create from the UI** asks for name, id (default: next free ≥
  2200) and gRPC port (default: next free), writes a managed file with
  an empty topology and loads it.
- **Removed:** the overrides journal and path logic (Rust and Lisp),
  `(load-overrides)`, the persist gate and structural fingerprint,
  `loaded_files` replay, the `config.*.overrides.lisp` gitignore,
  the dead id-2200 special case, the `set-microgrid-id` test rewrite
  (tests use `make-microgrid :id`), and the ambient (lowest-id)
  snapshot/overrides endpoints.

## Architecture

### Sub-project 1 — files: format, generator, loader (core)

- `src/lisp/microgrid_file.rs` (new): `parse(text) -> { generated:
  Option<Block>, script: String }` (marker detection); `render(site,
  def) -> String` for the generated block; `write(path, block,
  script)` atomic; `rewrite_id(text, new_id)`.
- `SimulatedComponent::make_form(&self) -> String`: each component
  renders its own `(%make-… :id N :name … <every construction
  argument>)` from the config it was built with (the `%make-*`
  primitives, so `*-defaults` at load time cannot change a saved
  microgrid). Invariant, tested per category: load(render(site))
  reproduces the same components, arguments and connections.
- Registry entry gains `source: Option<PathBuf>` and `managed: bool`;
  `make-microgrid` records the file being evaluated (the loader sets
  a "current file" the way `with_microgrid` sets the scope).
- `make-microgrid` with an already-registered id errors (see
  Decisions); `Config::load_file(path)` wraps `(load …)` and maps the
  error to the UI's *Load as* offer; `Config::load_as(path, new_id)`
  copies + rewrites + loads.
- `Config::persist(id)` regenerates and writes a managed file; every
  structural mutation path (the `make-*` defuns, `connect`,
  `disconnect`, `remove-component`, `rename-component`, microgrid
  attribute setters) calls it when the microgrid is managed and the
  mutation came from the UI/REPL (not from a load in progress).
- `enterprise.lisp`: `Config::persist_enterprise()` regenerates it
  from the live defaults/enterprise state; evaluated in `new_inner`
  after the prelude.
- Undo/redo history per microgrid in `Config`.
- UI: Load picker + free path (replaces *Load script…*), create
  dialog with id/port, *Adopt* in the inspector, read-only structure
  on unmanaged microgrids, overrides dialog removed, snapshots per
  microgrid.
- Examples converted; README/AGENTS updated; tests for the round
  trip, collisions, load-as, adoption, persist-on-edit, and a restart
  test (`Config` built twice on the same state dir, loading the
  written file, must yield identical ids).

### Sub-project 2 — unload

Per U1: a per-microgrid runtime bundle (gRPC shutdown trigger, physics
and sampler `JoinHandle`s, loopback abort) kept by the binary and
filled from one shared spawn function (boot loop and
`microgrid_registered` arm unified); `(unload-microgrid ID)` in the
lib emitting `microgrid_unregistered`; `DELETE /api/mg/{id}` (+
`?delete_file=true`), swctl `unload`, UI buttons on the microgrid card
and in the microgrid header; WS forwarders exit on the closed sender.
Tests: unload frees the port and stops the stream, reload after
unload works, unload with a running scenario is refused.

### Sub-project 3 — state directory switching

`Config::switch_state_dir(path)`: unload all, set `state_dir`, set the
load path, evaluate `enterprise.lisp`, emit `state_dir_changed`;
`GET/PUT /api/state-dir`; settings file; Settings dialog and the Load
picker listing `microgrids/*.lisp` of the current directory. Tests:
switch with loaded microgrids unloads them; the settings file round
trips; `--state-dir` beats the remembered one.

## Error handling

- A managed file whose generated block fails to parse loads nothing
  and reports the parse error with the path; the script section is
  not run.
- Writing a file fails → the in-memory mutation stands, the UI shows
  the error, and the microgrid is flagged *unsaved* until a later
  write succeeds.
- `enterprise.lisp` missing → created empty; unreadable → start-up
  error.
- Load of a file that registers no microgrid → warning (as today for
  an empty registry).

## Testing

- Unit: marker parsing (both sections, missing markers, nested
  markers), `make_form` round trip per category, `rewrite_id`,
  collision errors, persist-on-edit for each mutation defun.
- Integration (`src/ui/tests.rs`): create → edit → new `Config` on the
  same dir loading the file → identical topology and ids; load-as
  copy; adopt; unload/reload; state-dir switch; snapshots per
  microgrid.
- Smoke (Playwright): create with id/port, add components, restart the
  server process, Load from the picker, see the same ids on the
  canvas; unload button; settings dialog.

## Out of scope

- Directory scanning / auto-load, manifests.
- Per-microgrid scenario scope (D7) — unload refuses while a scenario
  runs.
- Multi-user or remote state directories.
