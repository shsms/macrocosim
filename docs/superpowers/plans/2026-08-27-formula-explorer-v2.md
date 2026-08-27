# Formula Explorer v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move switchyard to stock `frequenz-microgrid-component-graph` 0.6.2 (no explain fork) and fold the Formulas subview into the Topology screen as a floating formula panel, with parsing/highlighting rebuilt client-side from the formula string.

**Architecture:** A new DOM-free `formula-ast.js` parses rendered formula strings and renders HTML with sign-tracking and hoverable spans. `side-panel.js` generalizes from one floating panel slot to N concurrent draggable panels inside a `#panel-dock`. A new `formula-panel.js` hosts the explorer as one such panel over the live Topology canvas; the Formulas subview, its second canvas, and `explain.js` are deleted. The server endpoint slims to returning the formula string only.

**Tech Stack:** Rust (axum), vanilla ES modules, vis-network, biome.

**Spec:** `docs/superpowers/specs/2026-08-27-formula-explorer-v2-design.md`

## Global Constraints

- Subagent models: sonnet minimum for task work, opus for the final integration review (user preference; never haiku).
- `git add` files explicitly by name — never `-A`/`.`/`-u`/`-a`; never add `.nfs*` files.
- Lint gate for any `ui-assets/` change: `npx @biomejs/biome check ui-assets` (must spell out `@biomejs/biome`; config in `biome.json`).
- Tee every test run to a scratch file and grep the file, e.g. `cargo test 2>&1 | tee /tmp/claude-1000/-vagrant/d9ffc5a7-5e1d-4700-94e1-d5c9f875c63a/scratchpad/t.log`.
- Each task leaves the app building and working: `cargo build` green, biome clean, no dangling imports.
- Dependency target: `frequenz-microgrid-component-graph = "0.6.2"` from crates.io, no git, no features (Task 4).
- Panel names (= chrome button ids where a toggle exists): `node`, `formula-tree` (dashboard per-stream tree), `defaults-btn`, `scenario-report-btn`, `formula-btn` (the explorer). This refines the spec's `formula` label to the existing button-id convention.
- Commit messages: repo style is a short imperative summary line (see `git log`), plus the Claude trailers used earlier on this branch.

---

### Task 1: Shared parser/renderer module `formula-ast.js`

**Files:**
- Create: `ui-assets/formula-ast.js`
- Create: `tools/formula-ast-test.mjs`
- Modify: `ui-assets/formulas.js` (delete its private `parseFormula`/`formulaToHtml`, import the module)

**Interfaces:**
- Consumes: nothing (module must stay DOM-free so plain `node` can import it).
- Produces: `parseFormula(src) -> ast`, `formulaToHtml(ast) -> html string`, `formulaToText(ast) -> string` (exact reconstruction of the input for drift checks). AST node kinds: `{kind:"op", op:"+"|"-"|"*"|"/", left, right}`, `{kind:"neg", inner}`, `{kind:"paren", inner}`, `{kind:"ref", id}`, `{kind:"num", value}`, `{kind:"none"}`, `{kind:"call", name, args}`, `{kind:"ident", name}`, `{kind:"unknown", text}`.

- [ ] **Step 1: Write the failing test**

Create `tools/formula-ast-test.mjs` (run with plain `node`; no test framework exists in this repo — this file is the JS test seed the todo's B1 harness will later absorb):

```js
// Round-trip and rendering tests for ui-assets/formula-ast.js.
// Run: node tools/formula-ast-test.mjs   (exits non-zero on failure)
import {
  formulaToHtml,
  formulaToText,
  parseFormula,
} from "../ui-assets/formula-ast.js";

let failures = 0;
function check(name, cond, detail) {
  if (!cond) {
    failures++;
    console.error(`FAIL ${name}${detail ? `: ${detail}` : ""}`);
  }
}

// 1. Round-trip: parse + formulaToText must reproduce the crate's
// rendering byte-for-byte. Cases mirror the 0.6.2 Expr::render tests.
const ROUND_TRIP = [
  "#1",
  "0.0",
  "-0.0",
  "#10 + #11 + #12 + #13",
  "#11 - #10",
  "#11 + #12 - #10",
  "#11 - #12 - #10",
  "-(#10 + #11 + #12)",
  "-#10",
  "None",
  "COALESCE(#1002, #1001, 0.0)",
  "MAX(#2 - COALESCE(#1002, #1001, 0.0), 0.0)",
  "MIN(#1, #2)",
  "COALESCE(#8, #9) + COALESCE(#12, #13)",
  "#2 - (#3 - #4)",
];
for (const src of ROUND_TRIP) {
  const out = formulaToText(parseFormula(src));
  check(`round-trip ${src}`, out === src, `got ${out}`);
}

// 2. Subtracted tinting: every ref that is taken OUT of the
// measurement sits inside a .formula-subtracted wrapper. DOM-free
// scan: walk the tags, tracking how many formula-subtracted spans
// are open; a ref seen at depth > 0 records positive (subtracted),
// at depth 0 negative (added).
function subtractedIds(html) {
  const ids = [];
  const tokens = html.split(/(<[^>]+>)/);
  let d = 0;
  const stack = [];
  for (const t of tokens) {
    if (t.startsWith("<span")) {
      stack.push(t.includes("formula-subtracted"));
      if (t.includes("formula-subtracted")) d++;
      const m = /data-id="(\d+)"/.exec(t);
      if (m) ids.push(d > 0 ? Number(m[1]) : -Number(m[1])); // negative = added
    } else if (t === "</span>") {
      if (stack.pop()) d--;
    }
  }
  return ids;
}
{
  const html = formulaToHtml(parseFormula("#11 + #12 - #10"));
  const ids = subtractedIds(html);
  check("sub tint: #10 red", ids.includes(10), JSON.stringify(ids));
  check("sub tint: #11 not red", ids.includes(-11), JSON.stringify(ids));
  check("sub tint: #12 not red", ids.includes(-12), JSON.stringify(ids));
}
{
  const html = formulaToHtml(parseFormula("-(#10 + #11)"));
  const ids = subtractedIds(html);
  check("neg tint: #10 red", ids.includes(10), JSON.stringify(ids));
  check("neg tint: #11 red", ids.includes(11), JSON.stringify(ids));
}
{
  const html = formulaToHtml(parseFormula("MAX(#2 - COALESCE(#1002, 0.0), 0.0)"));
  check("ref link", html.includes('data-id="1002"'), html);
  check("call span", html.includes('class="formula-call"'), html);
  check(
    "hover spans",
    (html.match(/class="formula-node/g) || []).length >= 3,
    html,
  );
}
{
  const html = formulaToHtml(parseFormula("None"));
  check("None renders as value", html.includes("formula-num"), html);
}

if (failures) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log("formula-ast: all tests passed");
```

- [ ] **Step 2: Run test to verify it fails**

Run: `node tools/formula-ast-test.mjs`
Expected: FAIL — `Cannot find module '.../ui-assets/formula-ast.js'`

- [ ] **Step 3: Write the module**

Create `ui-assets/formula-ast.js`. Start from the parser at `ui-assets/formulas.js:20-94` (copy it — it moves here) and make these changes:

```js
// Shared parser + renderer for graph-crate-rendered formula strings
// like `MAX(#2 - COALESCE(#1002, #1001, 0.0), 0.0)`. Used by the
// dashboard's per-stream formula tree and the formula explorer
// panel. DOM-free on purpose: tools/formula-ast-test.mjs imports it
// under plain node, so it must not touch document/window (that is
// also why it carries its own escapeHtml instead of app.js's).
const escapeHtml = (s) =>
  String(s).replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c],
  );
```

(If `app.js`'s `escapeHtml` differs from the above, copy the body from `app.js` verbatim instead — behavior must match.)

In `atom()`, after the existing `#`-ref branch and the number-literal
branch, and before the ident branch, add unary negation (the number
regex is tried first so `-0.0` stays one signed number token,
matching the crate's rendering of negative constants):

```js
    if (src[i] === "-") {
      i++;
      return { kind: "neg", inner: atom() };
    }
```

In the ident branch, before returning `{kind: "ident"}`, add the
`None` literal (0.6.2's `Expr::None` renders as the bare word):

```js
      if (ident === "None") return { kind: "none" };
```

Export `parseFormula`, then add the two renderers:

```js
// Renders the AST back to text, byte-for-byte identical to the input
// string (the parser keeps paren nodes, so no re-grouping logic is
// needed). refreshFormula uses this to detect grammar drift between
// the crate's renderer and this parser.
export function formulaToText(node) {
  switch (node.kind) {
    case "ref":
      return `#${node.id}`;
    case "num":
      return renderNumber(node.value);
    case "none":
      return "None";
    case "ident":
      return node.name;
    case "neg":
      return `-${formulaToText(node.inner)}`;
    case "paren":
      return `(${formulaToText(node.inner)})`;
    case "op":
      return `${formulaToText(node.left)} ${node.op} ${formulaToText(node.right)}`;
    case "call":
      return `${node.name}(${node.args.map(formulaToText).join(", ")})`;
    default:
      return node.text || "";
  }
}

// Renders a number like the Rust side: whole numbers get one decimal
// place ("0.0"), everything else prints plainly. -0 keeps its sign.
function renderNumber(value) {
  if (Object.is(value, -0)) return "-0.0";
  return Number.isInteger(value) ? value.toFixed(1) : String(value);
}

// Render the AST as nested HTML:
// - every compound sub-expression wraps in a .formula-node span so a
//   hover handler can highlight exactly the part under the cursor;
// - every #N ref is a .formula-ref span carrying data-id;
// - right operands of binary `-` and negated subtrees wrap in
//   .formula-subtracted: they are what gets taken OUT of the
//   measurement (readers resolve nesting with closest(), matching
//   the old explain.js semantics);
// - calls with long arg lists break onto their own lines.
export function formulaToHtml(node) {
  const wrap = (inner) => `<span class="formula-node">${inner}</span>`;
  const isAtom = (n) => n.kind === "ref" || n.kind === "num";
  function rec(n) {
    switch (n.kind) {
      case "ref":
        return `<span class="formula-ref formula-node" data-id="${n.id}" title="select component ${n.id}">#${n.id}</span>`;
      case "num":
        return `<span class="formula-num">${renderNumber(n.value)}</span>`;
      case "none":
        return `<span class="formula-num">None</span>`;
      case "ident":
        return `<span class="formula-ident">${escapeHtml(n.name)}</span>`;
      case "paren":
        return wrap(`(${rec(n.inner)})`);
      case "neg":
        return wrap(
          `<span class="formula-op">-</span><span class="formula-subtracted">${rec(n.inner)}</span>`,
        );
      case "op": {
        const right =
          n.op === "-"
            ? `<span class="formula-subtracted">${rec(n.right)}</span>`
            : rec(n.right);
        return wrap(
          `${rec(n.left)} <span class="formula-op">${n.op}</span> ${right}`,
        );
      }
      case "call": {
        const head = `<span class="formula-call">${escapeHtml(n.name)}</span>`;
        const args = n.args.map(rec);
        if (n.args.length <= 2 && n.args.every(isAtom)) {
          return wrap(`${head}(${args.join(", ")})`);
        }
        const indented = args
          .map((a) => `<div class="formula-arg">${a},</div>`)
          .join("");
        return wrap(`${head}(${indented})`);
      }
      default:
        return `<span class="formula-raw">${escapeHtml(n.text || "")}</span>`;
    }
  }
  return rec(node);
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `node tools/formula-ast-test.mjs`
Expected: `formula-ast: all tests passed`

- [ ] **Step 5: Switch `formulas.js` to the shared module**

In `ui-assets/formulas.js`: delete the local `parseFormula` (lines 13-94) and `formulaToHtml` (lines 96-132); add `import { formulaToHtml, parseFormula } from "./formula-ast.js";`; in `renderFormulaPanel` keep `formulaToHtml(parseFormula(src))` as-is. Update the file's header comment: the parser now lives in `formula-ast.js`. Note the rendering of the dashboard tree changes slightly (compound expressions gain `.formula-node` wrappers, subtracted terms tint red, long call args gain trailing commas) — that is intended convergence, not a bug.

- [ ] **Step 6: Verify**

Run: `npx @biomejs/biome check ui-assets` and `cargo build 2>&1 | tee <scratchpad>/build.log` (build proves nothing imports the deleted symbols; the UI is served from static files).
Expected: both clean.

- [ ] **Step 7: Commit**

```bash
git add ui-assets/formula-ast.js tools/formula-ast-test.mjs ui-assets/formulas.js
git commit -m "Extract the formula parser into a shared formula-ast module"
```

---

### Task 2: Multi-panel shell

**Files:**
- Modify: `ui-assets/side-panel.js` (full rewrite, keeps its exports' names where possible)
- Modify: `ui-assets/index.html:328-340` (wrap `#inspector` in `#panel-dock`, add classes)
- Modify: `ui-assets/style.css:375-403` (dashboard dock), `:1305-1360` (float rules)
- Modify: `ui-assets/app.js` (close/Esc call sites, delete `setupInspectorDrag`)
- Modify: `ui-assets/dialogs.js:116-135, 151, 250` (toggle + render into `contentEl`)
- Modify: `ui-assets/formulas.js:139-166` (rename tenant to `formula-tree`, render into `contentEl`)
- Modify: `ui-assets/routing.js:230` (`closeAllPanels`)

**Interfaces:**
- Consumes: `inspectEl`, `inspectorEl` from `app.js` (unchanged exports).
- Produces (from `side-panel.js`): `openPanel(name, render, teardown?)` where `render(contentEl)` receives the panel's own content element; `closePanel(name)` (argument now required); `closeAllPanels()`; `closeTopPanel()` → boolean (closed something); `isPanelOpen(name)` → boolean. `currentPanel()` is deleted. The node inspector's panel keeps DOM ids `#inspector`/`#inspect`, so `inspect.js` needs no changes.

- [ ] **Step 1: index.html — dock container and classes**

Replace lines 328-340 (`<!-- Floating right … -->` through `</aside>`) with:

```html
      <!-- Floating panels dock. Panels (inspector, formula explorer,
           Defaults, Report, dashboard formula tree) stack here on the
           right; each is independently closable and draggable. On the
           Dashboard subview the dock becomes a docked grid column. -->
      <div id="panel-dock">
        <aside id="inspector" class="float-panel" aria-label="inspector">
          <div id="inspector-drag" class="panel-drag" title="Drag to move"><span class="drag-grip"></span></div>
          <button id="inspector-close" class="float-close" type="button" title="Close (Esc)">×</button>
          <div id="inspect" class="panel-content">
            <p class="hint">Click a node to inspect. Right-click for the context menu.</p>
          </div>
        </aside>
      </div>
```

- [ ] **Step 2: side-panel.js — rewrite**

Replace the whole file body (keep the header comment's spirit, updated for multiple panels):

```js
// The floating panel shell. Each named panel — the node inspector
// ("node"), the formula explorer ("formula-btn"), the Defaults editor,
// the live Scenario report, the dashboard formula tree — is its own
// concurrently-openable, draggable card inside #panel-dock. Re-opening
// an open panel just re-renders it (running its teardown first);
// closing runs teardown and hides the card. The shell never knows
// what's inside a panel; each tenant supplies its own teardown since
// only it knows what live resources (charts, timers) it owns.

// name → { el, contentEl, teardown }
const panels = new Map();
// Open panels, oldest first — Esc closes the newest.
const openStack = [];

const POS_KEY_PREFIX = "sw-panel-pos-";

function ensurePanel(name) {
  let p = panels.get(name);
  if (p) return p;
  let el;
  let contentEl;
  if (name === "node") {
    // The inspector's markup is static in index.html so inspect.js's
    // getElementById world keeps working untouched.
    el = document.getElementById("inspector");
    contentEl = document.getElementById("inspect");
  } else {
    el = document.createElement("aside");
    el.className = "float-panel";
    el.id = `panel-${name}`;
    el.innerHTML = `
      <div class="panel-drag" title="Drag to move"><span class="drag-grip"></span></div>
      <button class="float-close" type="button" title="Close">×</button>
      <div class="panel-content"></div>`;
    document.getElementById("panel-dock").appendChild(el);
    contentEl = el.querySelector(".panel-content");
    el.querySelector(".float-close").addEventListener("click", () => closePanel(name));
  }
  wireDrag(el, name);
  p = { el, contentEl, teardown: null };
  panels.set(name, p);
  return p;
}

// Drag-to-move via the grab strip; the offset is a transform on the
// panel, persisted per panel so it sticks across sessions. Inert in
// the dashboard subview, where the dock is a grid column (the CSS
// there forces transform: none and hides the strips).
function wireDrag(el, name) {
  const strip = el.querySelector(".panel-drag");
  let { dx, dy } = loadPos(name);
  if (dx || dy) el.style.transform = `translate(${dx}px, ${dy}px)`;
  strip.addEventListener("pointerdown", (e) => {
    if (document.body.dataset.subview === "dashboard") return;
    e.preventDefault();
    strip.setPointerCapture(e.pointerId);
    const startX = e.clientX - dx;
    const startY = e.clientY - dy;
    const rect = el.getBoundingClientRect();
    const baseLeft = rect.left - dx;
    const baseTop = rect.top - dy;
    const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
    const move = (ev) => {
      dx = clamp(
        ev.clientX - startX,
        -(baseLeft + rect.width - 80),
        window.innerWidth - baseLeft - 80,
      );
      dy = clamp(ev.clientY - startY, -baseTop, window.innerHeight - baseTop - 40);
      el.style.transform = `translate(${dx}px, ${dy}px)`;
    };
    const stop = () => {
      strip.removeEventListener("pointermove", move);
      savePos(name, dx, dy);
    };
    strip.addEventListener("pointermove", move);
    strip.addEventListener("pointerup", stop, { once: true });
    strip.addEventListener("pointercancel", stop, { once: true });
  });
}

function loadPos(name) {
  try {
    return JSON.parse(localStorage.getItem(POS_KEY_PREFIX + name)) ?? { dx: 0, dy: 0 };
  } catch (_) {
    return { dx: 0, dy: 0 };
  }
}
function savePos(name, dx, dy) {
  try {
    localStorage.setItem(POS_KEY_PREFIX + name, JSON.stringify({ dx, dy }));
  } catch (_) {
    // Storage unavailable — the position just doesn't stick.
  }
}

// The matching chrome toggle lights up while its panel is open, so
// its state tracks the actual panel instead of a private flag.
function syncButton(name, open) {
  document
    .getElementById(name)
    ?.classList.toggle("primary", open && name.endsWith("-btn"));
}

function syncBodyClass() {
  document.body.classList.toggle("panel-open", openStack.length > 0);
}

// Open (or re-render) the panel `name`. `render(contentEl)` fills the
// panel's own content element; `teardown()` runs when this panel
// re-renders or closes — each tenant cleans up ONLY its own resources.
export function openPanel(name, render, teardown = null) {
  const p = ensurePanel(name);
  p.teardown?.();
  p.teardown = teardown;
  if (!openStack.includes(name)) openStack.push(name);
  p.el.classList.add("open");
  syncBodyClass();
  syncButton(name, true);
  render(p.contentEl);
}

// Close one panel: run its teardown, reset its content, hide its card.
export function closePanel(name) {
  const p = panels.get(name);
  if (!p || !openStack.includes(name)) return;
  const teardown = p.teardown;
  p.teardown = null;
  teardown?.();
  p.contentEl.innerHTML =
    name === "node"
      ? '<p class="hint">Click a node to inspect. Right-click for the context menu.</p>'
      : "";
  p.el.classList.remove("open");
  openStack.splice(openStack.indexOf(name), 1);
  syncBodyClass();
  syncButton(name, false);
}

export function closeAllPanels() {
  for (const name of [...openStack].reverse()) closePanel(name);
}

// Esc: close the most recently opened panel. Returns whether one was.
export function closeTopPanel() {
  const top = openStack[openStack.length - 1];
  if (top == null) return false;
  closePanel(top);
  return true;
}

export function isPanelOpen(name) {
  return openStack.includes(name);
}
```

Note: `side-panel.js` no longer imports anything from `app.js` (the old `inspectEl`/`inspectorEl` imports go — this also removes an import cycle).

- [ ] **Step 3: style.css — dock + generic panel classes**

Replace the dashboard block (lines 375-403) with:

```css
/* On the Dashboard subview the panel dock keeps the old docked
   column: tiles reflow beside it instead of hiding under it, and
   there is no click-position gesture there for a resize to break.
   The canvas subviews get the floating overlay instead — see
   #panel-dock below. */
body.panel-open[data-subview="dashboard"] main {
  grid-template-columns: 1fr 420px;
  grid-template-areas:
    "mgheader        mgheader"
    "topology        paneldock"
    "drawer-splitter drawer-splitter"
    "repl            repl";
}
body.panel-open[data-subview="dashboard"] #panel-dock {
  grid-area: paneldock;
  justify-self: stretch;
  align-self: stretch;
  max-height: none;
  margin: 10px;
  overflow-y: auto;
  pointer-events: auto;
}
/* Docked panels stack full-width in the column; undo any drag offset. */
body[data-subview="dashboard"] .float-panel {
  transform: none !important;
  width: auto;
  max-height: none;
}
body[data-subview="dashboard"] .panel-drag { display: none; }
```

Replace the `#inspector` float block (lines 1305-1341, through the `#inspect` rule) with:

```css
/* The panel dock overlays the right side of the content cell instead
   of taking a grid column of its own: opening or closing a panel must
   never resize the canvas — a double-click's second click would land
   on a re-fitted, shifted graph. The dock itself is click-through
   (pointer-events: none); each panel card re-enables hits. The top
   margin keeps the floating canvas controls clickable above it. */
#panel-dock {
  grid-area: topology;
  justify-self: end;
  align-self: start;
  display: flex;
  flex-direction: column;
  gap: 10px;
  max-height: calc(100% - 58px);
  max-width: calc(100% - 20px);
  margin: 48px 10px 10px;
  z-index: 7;
  position: relative;
  pointer-events: none;
}
.float-panel {
  display: none;
  width: 420px;
  max-width: 100%;
  min-height: 0;
  pointer-events: auto;
  background: var(--bg-elev);
  border: 1px solid var(--border);
  border-radius: 8px;
  box-shadow: 0 6px 24px rgba(0, 0, 0, 0.35);
  overflow: hidden;
}
.float-panel.open { display: flex; flex-direction: column; }
.panel-content {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
  /* slim top padding — the drag strip above already clears the
     floating × */
  padding: 0.4rem 1rem 1rem;
}
```

Then sweep the rest of `style.css`: rename every remaining `#inspector-drag` selector to `.panel-drag`, every `body.inspector-open` to `body.panel-open`, and any remaining bare `#inspector` layout selector to `.float-panel` (keep `#inspect`-scoped rules — that element still exists with both id and class). `grep -n "inspector-open\|inspector-drag" ui-assets/style.css` must come back empty afterwards.

- [ ] **Step 4: app.js call sites**

- `setupFloatingPanels()` (line ~137): `#inspector-close` click → `closePanel("node"); topology.select([]);`. Delete the `setupInspectorDrag()` call and the whole `setupInspectorDrag` function (the shell owns drag now).
- Line ~404: `topology.setSelectionHandler(showComponent, () => closePanel("node"));`
- Both Escape branches (lines ~423-431 and ~463-467): replace `closePanel()` with `closeTopPanel()`. Keep the `topology.select([])` in the topology branch. (The `data-subview === "formulas"` sub-branch stays until Task 3 removes it.)
- Update the import from `./side-panel.js` to `{ closePanel, closeTopPanel }` plus whatever else app.js already pulls.

- [ ] **Step 5: dialogs.js and formulas.js re-host**

`dialogs.js` `makeSidePanelToggle` (lines 116-128) becomes:

```js
function makeSidePanelToggle(btnId, render, teardown = null) {
  const btn = document.getElementById(btnId);
  btn.addEventListener("click", () => {
    if (isPanelOpen(btnId)) {
      closePanel(btnId);
      return;
    }
    openPanel(btnId, render, teardown);
  });
}
```

with imports updated to `{ closePanel, isPanelOpen, openPanel }`. `renderDefaults` and `renderScenarioReport` change signature to `(contentEl)` and write `contentEl.innerHTML` instead of `inspectEl.innerHTML` (lines 151 and 250; drop the now-unused `inspectEl` import if nothing else in the file uses it — check the other `inspectEl` references first).

`formulas.js`: `openFormulaPanel(stream)` → `openPanel("formula-tree", (contentEl) => renderFormulaPanel(contentEl, stream))`; `renderFormulaPanel(contentEl, stream)` writes `contentEl.innerHTML` and queries `contentEl.querySelector(".formula-tree")`; drop the `inspectEl` import.

`routing.js:230`: `closePanel()` → `closeAllPanels()` (import updated).

`inspect.js`: no code change (its `openPanel("node", () => renderNode(d, gen), …)` render callback simply ignores the new `contentEl` argument, and `inspectEl` still is that element). Update only the two comments that say tenants "take turns" (lines ~552, ~861) to say the panel closes/re-renders.

- [ ] **Step 6: Verify**

Run: `npx @biomejs/biome check ui-assets && cargo build 2>&1 | tee <scratchpad>/build.log`
Expected: clean. Then a manual smoke via the running app if a session is available (not blocking): open a node inspector AND the Defaults panel — both cards visible, both draggable, Esc closes Defaults first then the inspector; dashboard subview docks them stacked.

- [ ] **Step 7: Commit**

```bash
git add ui-assets/side-panel.js ui-assets/index.html ui-assets/style.css ui-assets/app.js ui-assets/dialogs.js ui-assets/formulas.js ui-assets/routing.js ui-assets/inspect.js
git commit -m "Generalize the side panel into concurrent floating panels"
```

---

### Task 3: Formula explorer panel; remove the Formulas subview

**Files:**
- Create: `ui-assets/formula-panel.js`
- Delete: `ui-assets/explain.js`
- Modify: `ui-assets/index.html` (add `#formula-btn`; delete Formulas tab, `#formulas` block, `#formula-tip`; reword help)
- Modify: `ui-assets/app.js`, `ui-assets/routing.js`, `ui-assets/splitter.js`, `ui-assets/editor.js:260`, `ui-assets/style.css`

**Interfaces:**
- Consumes: `parseFormula`/`formulaToHtml`/`formulaToText` from `formula-ast.js` (Task 1); `openPanel`/`closePanel`/`isPanelOpen` from `side-panel.js` (Task 2); `topology` from `topology.js` (`selectedIds()`, `highlight(ids, subtractedIds)`, `unhighlight()`); `jumpToTopology`, `escapeHtml`, `mgPath`, `notify` from `app.js`.
- Produces (from `formula-panel.js`): `setupFormulaToggle()` (wires `#formula-btn`), `formulaSelectionChanged()` (canvas selection hook), `refreshFormula()` (topology-change hook). The old endpoint's `formula` field is the only response field consumed — this task works unchanged against both the fork server (today) and the 0.6.2 server (Task 4).

- [ ] **Step 1: index.html — button in, subview out**

- Line 24-25 area: add `<button id="formula-btn" class="hdr-btn">Formulas</button>` next to the Defaults button.
- Delete line 71 (`<button class="mode-btn" data-subview="formulas">Formulas</button>`).
- Delete the whole `#formulas` block (lines 86-176, `<div id="formulas">` through `</section>` closing `#why-drawer`).
- Delete line 364 (`<div id="formula-tip" hidden></div>`).
- Help text: delete the Formulas-subview bullet list items (lines 617-623) and replace with one item under the Topology section: `<li>The <em>Formulas</em> button (top right) opens the formula explorer as a floating panel: pick a metric, hover a part of the formula to highlight its components on the canvas (subtracted parts show red), click a <code>#N</code> to jump to that component. The <em>Engine options</em> card changes how the formula is generated; all options are view-local.</li>`. Update the inspector bullet (line 627): panels are plural now (`floating panels on the right — the inspector, the formula explorer, Defaults, the Report — each closes with its <em>×</em> or <kbd>Esc</kbd> (newest first)`). Keep lines 587/595 (dashboard tiles → formula tree) as-is.

- [ ] **Step 2: Write formula-panel.js**

```js
// The formula explorer panel: the graph crate's generated formulas,
// rendered live over the Topology canvas. Fetches
// /api/mg/{id}/formula for the active metric and renders the parsed
// formula with subtracted terms tinted red; hovering a sub-expression
// highlights its components on the canvas, clicking a #N jumps to it.
// Structural editing stays live underneath — app.js re-fetches on
// topology WS events while the panel is open, and the "limit to the
// selected components" toggle re-fetches on selection changes.

import { escapeHtml, jumpToTopology, mgPath, notify } from "./app.js";
import { formulaToHtml, formulaToText, parseFormula } from "./formula-ast.js";
import { readSelectedMg } from "./routing.js";
import { closePanel, isPanelOpen, openPanel } from "./side-panel.js";
import { topology } from "./topology.js";

// Metrics that accept an id set (the "limit to selection" toggle)
// vs. the ones that need exactly one id.
const GROUP_ID_METRICS = new Set([
  "battery",
  "pv",
  "chp",
  "wind_turbine",
  "ev_charger",
  "steam_boiler",
  "battery_ac_coalesce",
  "pv_ac_coalesce",
]);
const SINGLE_ID_METRICS = new Set(["component", "component_ac_coalesce"]);

// Engine-option checkboxes → query params on the formula request.
const ENGINE_OPTIONS = [
  ["cfg-prefer-meters", "prefer_meters"],
  ["cfg-phantom", "phantom_loads"],
  ["cfg-no-fallback", "no_fallback"],
  ["cfg-allow-unconnected", "allow_unconnected"],
  ["cfg-allow-validation-failures", "allow_validation_failures"],
  ["cfg-unspecified-inverters", "allow_unspecified_inverters"],
];

let metric = "grid";
let formulaText = "";
let requestSeq = 0;
let lastSelectionKey = "";
let hovered = null;

const PANEL = "formula-btn";
const useSelectionEl = () => document.getElementById("use-selection");

const MARKUP = `
  <section class="card">
    <h2>Metric</h2>
    <div id="metric-buttons">
      <div class="field">
        <span class="field-label">power</span>
        <button type="button" class="pill metric-btn active" data-metric="grid">grid</button>
        <button type="button" class="pill metric-btn" data-metric="consumer">consumer</button>
        <button type="button" class="pill metric-btn" data-metric="producer">producer</button>
        <button type="button" class="pill metric-btn" data-metric="battery">battery</button>
        <button type="button" class="pill metric-btn" data-metric="pv">PV</button>
        <button type="button" class="pill metric-btn" data-metric="chp">CHP</button>
        <button type="button" class="pill metric-btn" data-metric="wind_turbine">wind</button>
        <button type="button" class="pill metric-btn" data-metric="ev_charger">EV</button>
        <button type="button" class="pill metric-btn" data-metric="steam_boiler">boiler</button>
      </div>
      <div class="field">
        <span class="field-label">voltage / freq</span>
        <button type="button" class="pill metric-btn" data-metric="grid_coalesce">grid</button>
        <button type="button" class="pill metric-btn" data-metric="battery_ac_coalesce">battery</button>
        <button type="button" class="pill metric-btn" data-metric="pv_ac_coalesce">PV</button>
      </div>
      <div class="field">
        <span class="field-label">one component</span>
        <button type="button" class="pill metric-btn" data-metric="component">reading</button>
        <button type="button" class="pill metric-btn" data-metric="component_ac_coalesce">voltage / freq</button>
      </div>
    </div>
    <label class="check"><input type="checkbox" id="use-selection" />
      limit to the selected components</label>
  </section>
  <details class="card" id="config-panel">
    <summary><h2>Engine options</h2><span id="config-count" class="muted"></span></summary>
    <div class="field">
      <span class="field-label">formulas</span>
      <label class="check"><input type="checkbox" id="cfg-prefer-meters" />
        prefer meters over component readings</label>
      <label class="check"><input type="checkbox" id="cfg-phantom" />
        include phantom loads in consumer formula</label>
      <label class="check"><input type="checkbox" id="cfg-no-fallback" />
        disable fallback components</label>
    </div>
    <div class="field">
      <span class="field-label">graph building</span>
      <label class="check"><input type="checkbox" id="cfg-allow-unconnected" />
        allow unconnected components</label>
      <label class="check"><input type="checkbox" id="cfg-allow-validation-failures" />
        allow component validation failures</label>
      <label class="check"><input type="checkbox" id="cfg-unspecified-inverters" />
        treat unspecified inverters as battery inverters</label>
    </div>
  </details>
  <section class="card">
    <h2>Formula</h2>
    <p id="formula-error" class="graph-error" hidden></p>
    <pre id="formula-view" class="formula-tree"></pre>
    <div class="field">
      <button type="button" class="tool-btn" id="copy-formula"
        title="Copy the plain formula string">copy</button>
    </div>
    <p class="hint">Hover a part of the formula to highlight its
      components on the canvas — subtracted parts show red. Click a
      <code>#N</code> to jump to that component.</p>
  </section>`;

export function setupFormulaToggle() {
  document.getElementById("formula-btn").addEventListener("click", () => {
    if (isPanelOpen(PANEL)) {
      closePanel(PANEL);
      return;
    }
    openPanel(PANEL, renderPanel, teardown);
  });
}

function teardown() {
  clearCrossHighlight();
  hovered = null;
  formulaText = "";
  lastSelectionKey = "";
}

function renderPanel(contentEl) {
  contentEl.innerHTML = MARKUP;
  wire(contentEl);
  refreshFormula();
}

function wire(contentEl) {
  // Metric picker.
  const bar = contentEl.querySelector("#metric-buttons");
  bar.addEventListener("click", (ev) => {
    const btn = ev.target.closest(".metric-btn");
    if (!btn) return;
    metric = btn.dataset.metric;
    for (const b of bar.querySelectorAll(".metric-btn")) {
      b.classList.toggle("active", b === btn);
    }
    refreshFormula();
  });
  useSelectionEl().addEventListener("change", refreshFormula);
  // Engine options are client-side request parameters, so a change
  // only needs a re-fetch — nothing to store on the server.
  for (const box of contentEl.querySelectorAll("#config-panel input[type=checkbox]")) {
    box.addEventListener("change", () => {
      updateConfigCount();
      refreshFormula();
    });
  }
  contentEl.querySelector("#copy-formula").addEventListener("click", async () => {
    if (!formulaText) {
      notify("No formula to copy.");
      return;
    }
    try {
      await navigator.clipboard.writeText(formulaText);
      notify("Copied the formula.", "success");
    } catch (e) {
      notify(`Copy failed: ${e.message}`);
    }
  });
  // Hover a sub-expression → cross-highlight its components on the
  // canvas; the ids come straight from the rendered DOM.
  const view = contentEl.querySelector("#formula-view");
  view.addEventListener("mouseover", (ev) => {
    const span = ev.target.closest(".formula-node");
    if (span === hovered) return;
    hovered?.classList.remove("expr-hl");
    hovered = span;
    if (!span) {
      clearCrossHighlight();
      return;
    }
    span.classList.add("expr-hl");
    crossHighlight(idsIn(span));
  });
  view.addEventListener("mouseleave", () => {
    hovered?.classList.remove("expr-hl");
    hovered = null;
    clearCrossHighlight();
  });
  view.addEventListener("click", (ev) => {
    const ref = ev.target.closest(".formula-ref");
    if (ref) jumpToTopology(Number(ref.dataset.id));
  });
}

function updateConfigCount() {
  const boxes = document.querySelectorAll("#config-panel input[type=checkbox]");
  const on = [...boxes].filter((box) => box.checked).length;
  document.getElementById("config-count").textContent = on ? `${on} on` : "";
}

function idsIn(span) {
  const refs = span.matches(".formula-ref")
    ? [span]
    : [...span.querySelectorAll(".formula-ref")];
  return refs.map((r) => Number(r.dataset.id));
}

// Highlight `ids` on the canvas: subtracted occurrences red, added
// ones blue — per span, read from the .formula-subtracted wrappers
// the renderer emits. A canvas node turns red only when every
// covered occurrence of it is subtracted.
function crossHighlight(ids) {
  const set = new Set(ids);
  const subIds = new Set();
  const addIds = new Set();
  for (const ref of document.querySelectorAll("#formula-view .formula-ref")) {
    const id = Number(ref.dataset.id);
    const covered = set.has(id);
    const inSub = covered && ref.closest(".formula-subtracted") != null;
    ref.classList.toggle("hl", covered);
    ref.classList.toggle("hl-sub", inSub);
    if (covered) (inSub ? subIds : addIds).add(id);
  }
  const subtracted = [...subIds].filter((id) => !addIds.has(id));
  topology.highlight(ids, subtracted);
}

function clearCrossHighlight() {
  topology.unhighlight();
  for (const ref of document.querySelectorAll("#formula-view .formula-ref")) {
    ref.classList.remove("hl", "hl-sub");
  }
}

function showFormulaError(message) {
  const errorEl = document.getElementById("formula-error");
  errorEl.textContent = message;
  errorEl.hidden = false;
  document.getElementById("formula-view").innerHTML = "";
  formulaText = "";
}

// Canvas selection hook, called from app.js's selection handler. With
// the "limit to selection" toggle on (or a single-component metric),
// the formula follows the selection live.
export function formulaSelectionChanged() {
  if (!isPanelOpen(PANEL)) return;
  const key = topology.selectedIds().join(",");
  if (key === lastSelectionKey) return;
  lastSelectionKey = key;
  if (
    SINGLE_ID_METRICS.has(metric) ||
    (GROUP_ID_METRICS.has(metric) && useSelectionEl().checked)
  ) {
    refreshFormula();
  }
}

export async function refreshFormula() {
  if (!isPanelOpen(PANEL)) return;
  // A debounced call can land after the user left the microgrid.
  if (readSelectedMg() == null) return;

  const params = new URLSearchParams({ metric });
  const selection = topology.selectedIds();
  if (
    SINGLE_ID_METRICS.has(metric) ||
    (GROUP_ID_METRICS.has(metric) && useSelectionEl().checked && selection.length)
  ) {
    params.set("ids", selection.join(","));
  }
  for (const [id, param] of ENGINE_OPTIONS) {
    if (document.getElementById(id).checked) params.set(param, "true");
  }

  const seq = ++requestSeq;
  let data;
  try {
    const res = await fetch(`${mgPath("formula")}?${params}`);
    data = await res.json();
  } catch (e) {
    if (seq !== requestSeq) return; // a newer request superseded this one
    showFormulaError(`Formula request failed: ${e.message}`);
    return;
  }
  if (seq !== requestSeq) return;
  if (!data.ok) {
    showFormulaError(data.error);
    return;
  }
  document.getElementById("formula-error").hidden = true;
  formulaText = data.formula;
  const ast = parseFormula(data.formula);
  document.getElementById("formula-view").innerHTML = formulaToHtml(ast);
  // The parsed AST must render back to the exact server string; if
  // the grammars drift, surface it loudly during development.
  const roundTrip = formulaToText(ast);
  if (roundTrip !== data.formula) {
    console.error("formula grammar drift:", { server: data.formula, client: roundTrip });
  }
}
```

Note for the implementer: `escapeHtml` ends up unused in the final file above — drop it from the import (biome will flag it).

- [ ] **Step 3: app.js rewiring**

- Line 27: delete `import { formulaCanvas, refreshFormula, setupExplainPanel } from "./explain.js";` → `import { formulaSelectionChanged, refreshFormula, setupFormulaToggle } from "./formula-panel.js";`
- Line ~404: `topology.setSelectionHandler((d) => { showComponent(d); formulaSelectionChanged(); }, () => { closePanel("node"); formulaSelectionChanged(); });`
- Delete line ~409 `setupCanvasControls("formulas-controls", formulaCanvas());` and replace the `setupExplainPanel();` call with `setupFormulaToggle();`.
- Escape branch lines ~426-431: delete the `data-subview === "formulas"` sub-branch entirely (keep `closeTopPanel()`).
- WS refresh (lines ~487-494): condition becomes `if (isPanelOpen("formula-btn"))` (import `isPanelOpen` from `side-panel.js`); keep the 300 ms debounce and the comment, updated: the formula depends on the topology, so re-derive while the panel is open.
- Delete `setupFormulaDrawerSplitter()` from `init()` and its import.

- [ ] **Step 4: routing.js, splitter.js, editor.js**

- `routing.js`: delete line 17 (`import { formulaCanvas, refreshFormula } from "./explain.js";`). Line 107 `VALID_SUBVIEWS`: drop `"formulas"`. Line 160 hash regex: keep `formulas` in the alternation but map it, right after the exec: `if (m && m[2] === "formulas") m[2] = "topology";` (old bookmarks land on Topology instead of a dead route — adapt to the actual variable shape at that line). Delete line 232 (`formulaCanvas().resetNotify();`), the whole `subview === "formulas"` block (lines ~250-261), and the `formulaCanvas().apply(data)` block (lines ~410-411, keep the `topology` half). Fix the comment at ~392.
- `splitter.js`: delete `setupFormulaDrawerSplitter` (lines ~61-82) and its export.
- `editor.js:260`: reword the comment (the menu items are no longer shared with a Formulas canvas).
- Delete `ui-assets/explain.js` (`git rm ui-assets/explain.js`).

- [ ] **Step 5: style.css purge**

Delete the rule blocks for: `#formulas` (grid layout + `grid-template-rows`), `#formula-canvas-pane`, `#formula-topology`, `#formula-side`, `#why-drawer`, `#why-tree`, `#why-detail`, `.fsplitter`, `#formula-tip`, and any `.exp-*` classes used only by the Why drawer/tooltip (`grep -n "exp-" ui-assets/style.css` and check each against remaining JS). KEEP: `.formula-tree`, `.formula-ref`, `.formula-node`, `.formula-subtracted`, `.formula-op`, `.formula-num`, `.formula-call`, `.formula-arg`, `.formula-ident`, `.formula-raw`, `.expr-hl`, `.hl`, `.hl-sub`, `.metric-btn`, `#metric-buttons`, `#config-panel`, `.graph-error` — all still used by `formula-panel.js`/`formulas.js`. Verify with `grep -on 'class="[^"]*"' ui-assets/formula-panel.js` against the kept selectors.

- [ ] **Step 6: Verify**

Run:
```bash
grep -rn "explain\|formulaCanvas\|why-drawer\|why-tree\|formula-topology\|formulas-controls\|data-subview=\"formulas\"" ui-assets/*.js ui-assets/index.html
npx @biomejs/biome check ui-assets
cargo build 2>&1 | tee <scratchpad>/build.log
```
Expected: the grep returns nothing (except, at most, prose comments that are accurate); biome + build clean.

- [ ] **Step 7: Commit**

```bash
git add ui-assets/formula-panel.js ui-assets/index.html ui-assets/app.js ui-assets/routing.js ui-assets/splitter.js ui-assets/editor.js ui-assets/style.css
git rm ui-assets/explain.js
git commit -m "Replace the Formulas subview with a formula explorer panel"
```

---

### Task 4: Graph crate 0.6.2; slim the formula endpoint

**Files:**
- Modify: `Cargo.toml:29`
- Modify: `src/ui/handlers/formula.rs`
- Modify: `src/ui/tests.rs:895-990` (explained-formula section)

**Interfaces:**
- Consumes: 0.6.2's `Graph::*_formula(...) -> Result<Formula, Error>` (same names/argument shapes as the `_explained` twins, minus the suffix); `Formula: Display`.
- Produces: `GET /api/mg/{id}/formula` success payload is now exactly `{"ok": true, "metric": <string>, "formula": <string>}`. Error payloads unchanged.

- [ ] **Step 1: Update the failing tests first**

In `src/ui/tests.rs`: rename `formula_endpoint_returns_ast_and_explanation` to `formula_endpoint_returns_formula`; inside it, delete any assertions on `parsed["ast"]`, `parsed["explanation"]`, `parsed["commented"]`, and add:

```rust
    assert!(parsed.get("ast").is_none());
    assert!(parsed.get("explanation").is_none());
    assert!(parsed.get("commented").is_none());
```

(keep the existing `parsed["formula"]` assertion). Update the doc comment at line 895. Leave the other four formula tests untouched — they only assert `formula`/error kinds.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test formula_endpoint 2>&1 | tee <scratchpad>/t4a.log`
Expected: `formula_endpoint_returns_formula` FAILS (the fork server still returns `ast`).

- [ ] **Step 3: Switch the dependency**

`Cargo.toml:29` becomes:

```toml
frequenz-microgrid-component-graph = "0.6.2"
```

Run `cargo update -p frequenz-microgrid-component-graph 2>&1 | tee <scratchpad>/t4b.log` if the lockfile doesn't converge on plain `cargo build`; the lockfile must end with exactly one `frequenz-microgrid-component-graph` entry, version 0.6.2 (the `frequenz-microgrid` crate's `^0.6.0` requirement unifies onto it).

- [ ] **Step 4: Slim the handler**

In `src/ui/handlers/formula.rs`:
- Import line 15: `use frequenz_microgrid_component_graph::{ComponentGraphConfig, ErrorKind, Formula};`
- The metric match (lines 102-130): every `*_formula_explained(` → `*_formula(`; the binding becomes `let formula: Result<Formula, _> = match query.metric.as_str() { ... }`.
- The success arm (lines 131-142) becomes:

```rust
    Ok(match formula {
        Ok(formula) => Json(json!({
            "ok": true,
            "metric": query.metric,
            "formula": formula.to_string(),
        })),
        Err(e) => Json(json!({ "ok": false, "error": e.to_string(), "kind": kind_of(&e) })),
    })
```

- Update the module doc comment (lines 1-8 and 45-46): the endpoint returns the rendered formula string; parsing/highlighting live client-side in `formula-ast.js`.

- [ ] **Step 5: Run the full suite**

Run: `cargo test 2>&1 | tee <scratchpad>/t4c.log` then `grep -E "FAILED|test result" <scratchpad>/t4c.log`
Expected: all green. If a test fails on a changed formula STRING (not on missing fields), that is the fork→0.6.2 fallback-emission drift the spec accepts: update the assertion to the new string only after reading the test's topology and confirming the new formula is sensible for it — do not loosen assertions blindly.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/ui/handlers/formula.rs src/ui/tests.rs
git commit -m "Move to stock component-graph 0.6.2, drop the explain payload"
```

---

### Task 5: Integration verification sweep

**Files:**
- Create: `<scratchpad>/boot-smoke.mjs` (throwaway — NOT committed)
- Modify: `todo.org` (progress note)

- [ ] **Step 1: JS boot smoke**

curl-200 is not a boot test: the ES-module graph must actually load, or TDZ/import-cycle errors ship. Write `<scratchpad>/boot-smoke.mjs`:

```js
// Boot smoke: import the whole ES-module graph under a DOM shim.
// TDZ / import-cycle / missing-export errors throw here instead of
// in the user's browser. Throwaway — lives in the scratchpad.
const stub = () =>
  new Proxy(function () {}, {
    get(_t, p) {
      if (p === Symbol.toPrimitive || p === "toString") return () => "";
      if (p === "then") return undefined; // not a thenable
      return stub();
    },
    apply: () => stub(),
    construct: () => stub(),
  });
globalThis.document = stub();
globalThis.window = globalThis;
globalThis.navigator = stub();
globalThis.localStorage = stub();
globalThis.location = { hash: "", pathname: "/", search: "" };
globalThis.WebSocket = stub();
globalThis.requestAnimationFrame = () => 0;
globalThis.addEventListener = () => {};
globalThis.getComputedStyle = stub();
globalThis.CSS = stub();
globalThis.ResizeObserver = stub();
globalThis.fetch = () => new Promise(() => {});
await import("/vagrant/switchyard/ui-assets/app.js");
console.log("boot smoke: module graph loaded");
```

Run: `node <scratchpad>/boot-smoke.mjs`
Expected: `boot smoke: module graph loaded`. If the shim itself is what breaks (a module needs a global the stub list lacks), extend the shim, not the app code; if the app code throws a ReferenceError/TypeError about its own bindings, that is a real bug — fix it.

- [ ] **Step 2: Full gate**

Run:
```bash
cargo test 2>&1 | tee <scratchpad>/final-test.log && grep "test result" <scratchpad>/final-test.log
npx @biomejs/biome check ui-assets
node tools/formula-ast-test.mjs
grep -rn "explained\|ExplainedFormula\|commented" src/ --include="*.rs"
```
Expected: tests green, biome clean, parser tests green, grep only hits prose/comments that are still accurate (fix any that aren't).

- [ ] **Step 3: Live click-through (if a browser session is available)**

Launch the app (see the `run` skill / project docs). On a sample microgrid: open Formulas panel → grid metric renders; hover a COALESCE arm → canvas highlights, subtracted refs red; click `#N` → jumps + selects, inspector opens beside the formula panel (both visible); toggle "limit to selection" with two batteries selected on the `battery` metric → formula narrows and follows selection; delete a meter → formula updates within ~300 ms; dashboard subview → panels dock stacked; Esc closes newest panel first. If no browser is available, record this list as pending manual QA in the todo.org note instead.

- [ ] **Step 4: todo.org + commit**

Add a progress note to the "Formula explorer as an overlay on the topology canvas" entry (use the org-tasks helper, not hand-editing): implemented on branch `formula-explorer-v2` per this plan + spec; also note the 0.6.2 upgrade and that the separate "explain" fork is gone. Then:

```bash
git add todo.org
git commit -m "Note the formula explorer v2 progress in todo.org"
```

---

## Self-Review (done at plan-writing time)

- Spec coverage: dependency+endpoint → Task 4; parser incl. neg/None → Task 1; multi-panel shell incl. `formula-tree` rename, drag persistence, dashboard dock → Task 2; overlay panel incl. live-follow toggle, WS re-fetch, cross-highlight, copy, error kinds, subview removal → Task 3; version-unification/drift acceptance → Task 4 Step 5; testing section → Tasks 1/4/5.
- Deviation from spec, intentional: panel key is `formula-btn` (button-id convention) rather than `formula`; noted in Global Constraints.
- Ordering keeps every commit working: the overlay (Task 3) consumes only the `formula` field, which both the fork server and the 0.6.2 server provide, so the endpoint switch can land last.
