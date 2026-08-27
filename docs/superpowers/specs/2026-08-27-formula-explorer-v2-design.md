# Formula explorer v2 — design

Date: 2026-08-27
Status: approved (design review in chat)
Todo entry: "Formula explorer as an overlay on the topology canvas"
(todo.org); also retires the `explain` fork dependency.

## Context and scope

Two coupled changes:

1. **Graph crate to crates.io 0.6.2.** The `explain` feature is not
   being merged upstream, so switchyard leaves the
   `shsms/frequenz-microgrid-component-graph-rs` fork (0.6.1 + explain
   + serde) for stock `frequenz-microgrid-component-graph = "0.6.2"`.
   0.6.2 has no `ExplainedFormula`, no `Formula::ast()`, no serde
   feature: the `*_formula()` generators return a `Formula` whose only
   public output is its rendered string
   (e.g. `MAX(#2 - COALESCE(#1002, #1001, 0.0), 0.0)`).
2. **Formulas subview folds into Topology** as a formula overlay
   panel, per the todo entry — one canvas, not two.

Accepted losses (explicit decision): the Why drawer, per-span
explanation tooltips, and the `commented` (reason-annotated) copy
variant. Kept, rebuilt client-side from the formula string alone:
formula pretty-printing, subtracted-terms-in-red, per-subexpression
hover → canvas cross-highlight, click-`#N`-to-select.

Out of scope: dashboard space rethink (own todo entry), site-level
controls, any re-adding of explanation-like content (static operator
tooltips etc. — YAGNI until asked for).

## 1. Dependency + endpoint

`Cargo.toml`: `frequenz-microgrid-component-graph = "0.6.2"` from
crates.io, no git rev, no features. The lockfile's duplicate
0.6.0/0.6.1 entries collapse to one 0.6.2.

`src/ui/handlers/formula.rs`:

- Every `*_formula_explained()` call becomes its `*_formula()` twin;
  the metric table, id parsing, single-id rule, engine-options
  handling (`ComponentGraphConfig` builder — all six options exist
  unchanged in 0.6.2), `spawn_blocking`, and typed error kinds
  (`kind_of`) stay as they are.
- Success payload slims to `{ok: true, metric, formula}` where
  `formula` is `Formula`'s `Display` output. The `commented`, `ast`,
  and `explanation` fields disappear.

Server tests (`src/ui/tests.rs` explained-formula section) update to
assert on the formula string + error kinds only.

Two knock-on effects of leaving the fork, both accepted:

- **Version unification.** `frequenz-microgrid` (the loopback client
  whose logical meter feeds `/microgrid/formulas` and the dashboard)
  requires component-graph `^0.6.0`; today it resolves to a separate
  crates.io 0.6.0 copy alongside the fork. After the switch both
  unify to one 0.6.2, so the dashboard's formulas and the explorer's
  come from the same engine version — a consistency gain.
- **Formula drift.** The fork's base and 0.6.2 differ beyond explain
  removal (fallback emission was reworked — e.g.
  `reached_below`/`outside_feeds` → `reaches_any_below`), so some
  topologies will render different formula strings after the upgrade.
  Existing tests assert loosely (`contains("#3")` etc.), so expected
  churn is small; visible changes in example sites are the upgrade
  working as intended, not regressions.

Related work note: the unimplemented `formula-engine-versions` spec
(branch of the same name, design-only) presumes the Formulas subview
and the fork engine labeled `current` with explain. It needs revision
before implementation; nothing here blocks on it.

## 2. One parser module: `ui-assets/formula-ast.js`

`parseFormula` moves out of `formulas.js` into a new `formula-ast.js`,
together with a renderer that subsumes both existing renderers
(`formulas.js formulaToHtml`, `explain.js astToHtml`). Grammar is
unchanged: numbers, `#N` refs, `+ - * /`, parens, calls
(`COALESCE`/`MIN`/`MAX`), identifiers.

The renderer produces, from the parsed AST:

- **Sign tracking**: right operands of binary `-` (and unary
  negation) render inside `.formula-subtracted` wrappers, nesting
  through parens exactly as `astToHtml` does today — a double
  subtraction un-tints.
- **Hoverable spans**: every subexpression wraps in a `.formula-node`
  span; every `#N` is a `.formula-ref` with `data-id`. Hovering a
  span cross-highlights its component set on the canvas
  (`canvas.highlight(ids, subtractedIds)` — `crossHighlight` already
  reads sign purely off the rendered `.formula-subtracted` DOM, so it
  ports with renames only). Clicking a ref selects on canvas.
- Call-argument line-breaking as today (short calls inline, long ones
  one arg per line).

Two grammar additions the current parser lacks but 0.6.2 can emit
(verified against its `Expr::render`): **unary negation** —
`-(#10 + #11 + #12)` and `-#10` currently fall through to the
`unknown` node — and the bare **`None`** literal (renders as an
identifier today; should render as a value and count as "no
components" for highlighting). Sign tracking treats a negated subtree
like a subtracted one.

Consumers: the dashboard formula panel (`formulas.js`, which keeps its
fetch + panel code and drops its private parser/renderer) and the
formula overlay panel.

## 3. Multi-panel shell

`side-panel.js` generalizes from one slot to N concurrent floating
panels keyed by name (`node`, `formula`, `formula-tree`,
`defaults-btn`, `scenario-report-btn` — the dashboard's per-stream
formula tree renames to `formula-tree` so it and the explorer stay
distinct tenants). Per panel: its own DOM container (the current
`#inspector` markup becomes a template/factory), its own teardown, its
own close button, its own drag position (grab-strip drag as today),
position persisted to localStorage alongside the existing per-card
fold keys (same try/catch storage discipline). `openPanel(name, …)`
opens or re-renders that panel without touching the others;
`closePanel(name)` closes just it. The chrome-button sync
(`syncButtons`) becomes per-panel. The `inspector-open` body class
becomes "any panel open".

The node inspector's internals (cards, folds, live updates, teardown)
are untouched — it is re-hosted, not redesigned. Defaults and
scenario report likewise re-host as-is.

Default placement: every panel stacks in the one right-side dock the
inspector already uses. A left-side spawn was dropped — the canvas's
left edge belongs to the *+ Add* palette and the card it opens — and
per-panel drag with positions persisted to localStorage covers
placement preference anyway. All draggable.

## 4. Formula overlay on Topology

The Formulas subview is removed: its nav tab, the second canvas
(`formula-topology`), its index.html layout-pill/markup block, and
`explain.js`'s canvas plumbing (`formulaCanvas`, its context menu,
`toggleTelemetry` — the Topology canvas menu is a strict superset) all
go. `explain.js` is deleted; what survives moves into a new
`formula-panel.js` (metric buttons, engine-options popover +
`updateConfigCount`, copy button, fetch/refresh/error rendering) and
`formula-ast.js` (rendering), both rewritten against the slimmed
payload.

The formula panel (a shell tenant, coexisting with the inspector):

- **Metric picker** and **engine options** as today; options still
  ride every request.
- **"Limit to selection" toggle**, off by default. While on, the
  formula re-fetches live as canvas selection changes (id-taking
  metrics get the selected ids; the single-component metrics keep
  their exactly-one rule and error text). Clicking around with the
  toggle off never changes the formula.
- **Live under editing**: structural edits stay enabled; the panel
  re-fetches on the existing topology-change WS events, so the
  formula visibly updates as the graph is rewired.
- **Rendered formula** with red subtraction, hover cross-highlight,
  click-to-select; **copy** copies the plain formula string.
- Error states render the typed error kinds as today
  (`component_not_found` vs `invalid_graph` wording).

Selection double-duty is accepted behavior: with both panels open, a
canvas click updates the inspector and (only if the toggle is on) the
formula.

## Testing

- Server: updated formula-endpoint tests (string + error kinds);
  `cargo test` green.
- Client: JS boot smoke — load the ES-module graph under the node DOM
  shim (import-cycle/TDZ regression guard; `explain.js` deletion and
  the new modules change the import graph).
- Parser/renderer: unit-testable pure functions — round-trip a set of
  representative rendered formulas (incl. nested subtraction and long
  COALESCE chains) through parse + render, assert subtracted-span
  placement and ref ids.
- In-browser click-through of the overlay rides the pending B1
  browser-test harness, as with the inspector redesign.
