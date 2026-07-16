// The Formulas subview: a read-and-arrange canvas plus the formula +
// explanation UI. Fetches /api/mg/{id}/formula for the active metric
// and renders:
//
// - the formula, built from the server's AST, with every sub-expression
//   as a hoverable span: hovering shows a tooltip with the innermost
//   explanation that covers it (details on demand);
// - the "Why" drawer: a master-detail view — a compact tree of all
//   explanation nodes on the left, and the selected node's full
//   rationale, readable and large, on the right;
// - cross-highlighting between the tree, the formula and the canvas.

import { escapeHtml, notify } from "./app.js";
import { alignMenuItems, showMenuItems } from "./editor.js";
import { evalQuoted } from "./inspect.js";
import { mgPath, readSelectedMg } from "./routing.js";
import { createGraphCanvas } from "./topology.js";

// The Formulas canvas: same snapshot as the Topology canvas, no
// structural editing — the context menu only arranges, and the one
// mutation it offers is the telemetry toggle, which flips the
// selection's operational mode (a config edit, persisted and
// undoable). Created lazily: module bodies in this SPA form import
// cycles, so building the canvas at import time could catch
// topology.js half-initialized.
let canvas = null;
export function formulaCanvas() {
  if (!canvas) {
    canvas = createGraphCanvas("formula-topology", {
      onContextMenu(x, y) {
        const sel = canvas.selectedIds();
        const items = [];
        if (sel.length) {
          items.push({
            label: "Toggle telemetry",
            shortcut: "T",
            action: toggleTelemetry,
          });
        }
        if (sel.length >= 2) items.push(...alignMenuItems(canvas));
        if (!items.length) return;
        showMenuItems(document.getElementById("ctx-menu"), items, x, y);
      },
    });
  }
  return canvas;
}

// The telemetry toggle moves each mode to its twin with the SAME
// control capability and the opposite telemetry — so toggling never
// grants or removes control, only telemetry. Off then on lands on
// the explicit full-capability mode for `unspecified`; every other
// mode round-trips exactly.
const TELEMETRY_OFF = {
  unspecified: "control-only",
  "control-and-telemetry": "control-only",
  "telemetry-only": "inactive",
};
const TELEMETRY_ON = {
  "control-only": "control-and-telemetry",
  inactive: "telemetry-only",
};

// Flip the selection between telemetry and no telemetry by setting
// the components' OPERATIONAL MODE (config, not a runtime poke): a
// component without telemetry has no reading, so formulas route
// around it through the fallbacks — the very thing the explanations
// describe. The edit persists via the overrides gate and is
// undoable like any other config edit.
export async function toggleTelemetry() {
  const ids = formulaCanvas().selectedIds();
  if (!ids.length) {
    notify("Nothing selected.");
    return;
  }
  // Mixed selections turn telemetry OFF first (matching the old
  // behavior): only when nothing streams does the toggle turn on.
  const anyOn = ids.some(
    (id) => formulaCanvas().get(id)?.provides_telemetry,
  );
  const table = anyOn ? TELEMETRY_OFF : TELEMETRY_ON;
  const sets = ids
    .map((id) => {
      const current = formulaCanvas().get(id)?.operational_mode;
      const next = table[current];
      return next ? `(set-component-operational-mode ${id} '${next})` : null;
    })
    .filter(Boolean)
    .join(" ");
  if (!sets) return;
  const data = await evalQuoted(`(progn ${sets})`, "Telemetry toggle failed");
  if (data.ok) {
    notify(
      `Telemetry ${anyOn ? "off" : "on"} for ${ids.length} component${ids.length > 1 ? "s" : ""} (operational mode).`,
      "success",
    );
  }
}

// Metrics that take a set of target component ids from the selection,
// and metrics that need exactly one component.
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

let metric = "grid";
// The current formula, plain and with `//` comments, for the copy buttons.
let formulaText = "";
let commentedText = "";

function useSelectionEl() {
  return document.getElementById("use-selection");
}

// ---------------------------------------------------------------- AST

// Renders a number like the Rust side: whole numbers get one decimal
// place ("0.0"), everything else prints plainly. This matches Rust for
// the small constants the engine emits (only 0.0 today); extreme
// values (1e21, 1e-7) would print differently — the round-trip check
// in refreshFormula reports any such drift.
function renderNumber(value) {
  // JS folds -0 into Number.isInteger + toFixed as "0.0", but Rust
  // prints "-0.0"; keep the sign so the round-trip check holds.
  if (Object.is(value, -0)) return "-0.0";
  return Number.isInteger(value) ? value.toFixed(1) : String(value);
}

// Renders the AST to text exactly like the crate's `Display`, used for
// the tooltip lookup and to cross-check the HTML rendering.
function astToText(node) {
  const join = (params, sep) => params.map(astToText).join(sep);
  const grouped = (n) =>
    (n.op === "add" || n.op === "sub") && n.params.length > 1
      ? `(${astToText(n)})`
      : astToText(n);
  switch (node.op) {
    case "none":
      return "None";
    case "neg":
      return `-${grouped(node.param)}`;
    case "number":
      return renderNumber(node.value);
    case "component":
      return `#${node.component_id}`;
    case "add":
      return join(node.params, " + ");
    case "sub":
      return [astToText(node.params[0])]
        .concat(node.params.slice(1).map(grouped))
        .join(" - ");
    case "coalesce":
      return `COALESCE(${join(node.params, ", ")})`;
    case "min":
      return `MIN(${join(node.params, ", ")})`;
    case "max":
      return `MAX(${join(node.params, ", ")})`;
    default:
      return "?";
  }
}

// Renders the AST as nested HTML. Every sub-expression is wrapped in a
// span carrying its own text form (data-expr), so the tooltip handler
// can find the explanation for exactly the part under the cursor.
// Calls with long arg lists break onto their own lines.
function astToHtml(node) {
  const wrap = (n, inner) =>
    `<span class="formula-node" data-expr="${escapeHtml(astToText(n))}">${inner}</span>`;
  const grouped = (n) =>
    (n.op === "add" || n.op === "sub") && n.params.length > 1
      ? `(${astToHtml(n)})`
      : astToHtml(n);
  const isAtom = (n) => n.op === "component" || n.op === "number";
  const call = (name, params) => {
    const head = `<span class="formula-call">${name}</span>`;
    const args = params.map(astToHtml);
    if (params.length <= 2 && params.every(isAtom)) {
      return `${head}(${args.join(", ")})`;
    }
    const indented = args
      .map((a) => `<div class="formula-arg">${a},</div>`)
      .join("");
    return `${head}(${indented})`;
  };
  switch (node.op) {
    case "none":
      return wrap(node, `<span class="formula-num">None</span>`);
    case "neg":
      return wrap(
        node,
        `<span class="formula-op">-</span><span class="formula-subtracted">${grouped(node.param)}</span>`,
      );
    case "number":
      return wrap(node, `<span class="formula-num">${renderNumber(node.value)}</span>`);
    case "component":
      return `<span class="formula-ref formula-node" data-expr="#${node.component_id}" data-id="${node.component_id}" title="select component ${node.component_id}">#${node.component_id}</span>`;
    case "add":
      return wrap(
        node,
        node.params.map(astToHtml).join(` <span class="formula-op">+</span> `),
      );
    case "sub":
      // The subtracted terms tint red: they are what gets taken OUT
      // of the measurement.
      return wrap(
        node,
        [astToHtml(node.params[0])]
          .concat(
            node.params
              .slice(1)
              .map((n) => `<span class="formula-subtracted">${grouped(n)}</span>`),
          )
          .join(` <span class="formula-op">-</span> `),
      );
    case "coalesce":
      return wrap(node, call("COALESCE", node.params));
    case "min":
      return wrap(node, call("MIN", node.params));
    case "max":
      return wrap(node, call("MAX", node.params));
    default:
      return `<span class="formula-raw">?</span>`;
  }
}

// ---------------------------------------------------- explanation data

// The current explanation tree, flattened by DFS. Each entry:
// { node, parent (index or null), depth }.
let flat = [];
// rendered text → index of the DEEPEST explanation node with that
// rendering — the innermost reason for a formula part.
let byRendered = new Map();
// explanation node → its index in `flat`, for the detail pane's
// child chips.
let indexByNode = new Map();

function flatten(node, parent, depth) {
  const index = flat.length;
  flat.push({ node, parent, depth });
  indexByNode.set(node, index);
  if (node.rendered != null) {
    const existing = byRendered.get(node.rendered);
    if (existing == null || flat[existing].depth <= depth) {
      byRendered.set(node.rendered, index);
    }
  }
  for (const child of node.children) flatten(child, index, depth + 1);
}

// "meter_drill" → "meter drill"; the `metric` kind shows its name.
function kindLabel(kind) {
  if (kind.type === "metric") return `metric: ${kind.metric}`;
  if (kind.type === "meter_drill") {
    return kind.prefers_meters
      ? "fallback ladder (meters first)"
      : "fallback ladder (components first)";
  }
  return kind.type.replaceAll("_", " ");
}

function shorten(text, max = 46) {
  return text.length > max ? `${text.slice(0, max - 3)}…` : text;
}

// ------------------------------------------------------- Why drawer

// Indices of tree nodes whose children are folded away. Deep levels
// start folded, so a big formula's tree is not a wall of rows.
let folded = new Set();

function defaultFolded() {
  folded = new Set();
  flat.forEach(({ node, depth }, i) => {
    if (depth >= 2 && node.children.length) folded.add(i);
  });
}

function rowVisible(i) {
  let parent = flat[i].parent;
  while (parent != null) {
    if (folded.has(parent)) return false;
    parent = flat[parent].parent;
  }
  return true;
}

// Unfolds every ancestor so row `i` can be seen (used when jumping to
// a node from the formula or the detail chips).
function unfoldTo(i) {
  let parent = flat[i].parent;
  while (parent != null) {
    folded.delete(parent);
    parent = flat[parent].parent;
  }
}

// Applies fold state to the rendered rows: visibility and the
// chevrons.
function refreshTreeFolding() {
  for (const row of document.querySelectorAll("#why-tree .why-row")) {
    const i = Number(row.dataset.i);
    row.hidden = !rowVisible(i);
    const toggle = row.querySelector(".why-toggle");
    if (toggle && !toggle.classList.contains("leaf")) {
      toggle.textContent = folded.has(i) ? "▸" : "▾";
    }
  }
}

function whyTreeHtml() {
  return flat
    .map(({ node, depth }, i) => {
      const snippet =
        node.rendered == null
          ? `<span class="exp-silent">(nothing emitted)</span>`
          : `<code class="exp-code">${escapeHtml(shorten(node.rendered))}</code>`;
      const toggle = node.children.length
        ? `<span class="why-toggle" data-toggle="${i}">${folded.has(i) ? "▸" : "▾"}</span>`
        : `<span class="why-toggle leaf"></span>`;
      return `
        <div class="why-row" data-i="${i}" style="padding-left:${8 + depth * 16}px">
          ${toggle}
          <span class="exp-kind">${escapeHtml(kindLabel(node.kind))}</span>
          ${snippet}
        </div>`;
    })
    .join("");
}

function whyDetailHtml(i) {
  const { node, parent } = flat[i];
  const chips = node.component_ids
    .map((id) => `<button type="button" class="chip" data-id="${id}">#${id}</button>`)
    .join(" ");
  const up =
    parent == null
      ? ""
      : `<button type="button" class="chip chip-nav" data-goto="${parent}">↑ ${escapeHtml(
          kindLabel(flat[parent].node.kind),
        )}</button>`;
  const children = node.children.length
    ? `<div class="detail-children">made from:
        ${node.children
          .map((child) => {
            const j = indexByNode.get(child);
            return `<button type="button" class="chip chip-nav" data-goto="${j}">${escapeHtml(
              kindLabel(child.kind),
            )}</button>`;
          })
          .join(" ")}</div>`
    : "";
  const rendered =
    node.rendered == null
      ? `<p class="exp-silent">This part emits nothing — it explains why something is <em>not</em> in the formula.</p>`
      : `<pre class="detail-code">${escapeHtml(node.rendered)}</pre>`;
  return `
    <div class="detail-head">
      <span class="exp-kind exp-kind-big">${escapeHtml(kindLabel(node.kind))}</span>
      ${up}
    </div>
    ${rendered}
    <p class="detail-rationale">${escapeHtml(node.rationale)}</p>
    <div class="detail-chips">components: ${chips || "—"}</div>
    ${children}`;
}

function selectWhyNode(i, scrollTree = true) {
  unfoldTo(i);
  refreshTreeFolding();
  const tree = document.getElementById("why-tree");
  for (const row of tree.querySelectorAll(".why-row")) {
    row.classList.toggle("sel", Number(row.dataset.i) === i);
  }
  document.getElementById("why-detail").innerHTML = whyDetailHtml(i);
  if (scrollTree) {
    tree
      .querySelector(`.why-row[data-i="${i}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }
}

function crossHighlight(ids) {
  const set = new Set(ids);
  // Subtracted occurrences highlight red, added ones blue — per span,
  // read from the .formula-subtracted wrappers the renderer emits.
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
  // A canvas node turns red only when every covered occurrence of it
  // is subtracted.
  const subtracted = [...subIds].filter((id) => !addIds.has(id));
  formulaCanvas().highlight(ids, subtracted);
}

function clearCrossHighlight() {
  formulaCanvas().unhighlight();
  for (const ref of document.querySelectorAll("#formula-view .formula-ref")) {
    ref.classList.remove("hl", "hl-sub");
  }
}

// -------------------------------------------------------------- panel

// Guards against out-of-order responses: only the newest request may
// render (metric clicks and selection changes can overlap).
let requestSeq = 0;

// Show `message` in the error banner and clear every formula pane.
function showFormulaError(message) {
  const errorEl = document.getElementById("formula-error");
  errorEl.textContent = message;
  errorEl.hidden = false;
  document.getElementById("formula-view").innerHTML = "";
  document.getElementById("why-tree").innerHTML = "";
  document.getElementById("why-detail").innerHTML =
    `<p class="exp-silent">No formula to explain.</p>`;
  flat = [];
  byRendered = new Map();
  indexByNode = new Map();
  formulaText = "";
  commentedText = "";
}

export async function refreshFormula() {
  // A debounced call can land after the user left the microgrid.
  // Without a selected mg, mgPath would fall back to /api/formula —
  // a route that doesn't exist — and toast a spurious error.
  if (readSelectedMg() == null) return;
  const formulaEl = document.getElementById("formula-view");
  const errorEl = document.getElementById("formula-error");

  const params = new URLSearchParams({ metric });
  const selection = formulaCanvas().selectedIds();
  if (
    SINGLE_ID_METRICS.has(metric) ||
    (GROUP_ID_METRICS.has(metric) && useSelectionEl().checked && selection.length)
  ) {
    params.set("ids", selection.join(","));
  }
  // Engine options live client-side; every request carries them.
  if (document.getElementById("cfg-prefer-meters").checked) {
    params.set("prefer_meters", "true");
  }
  if (document.getElementById("cfg-phantom").checked) {
    params.set("phantom_loads", "true");
  }
  if (document.getElementById("cfg-no-fallback").checked) {
    params.set("no_fallback", "true");
  }
  if (document.getElementById("cfg-allow-unconnected").checked) {
    params.set("allow_unconnected", "true");
  }
  if (document.getElementById("cfg-allow-validation-failures").checked) {
    params.set("allow_validation_failures", "true");
  }
  if (document.getElementById("cfg-unspecified-inverters").checked) {
    params.set("allow_unspecified_inverters", "true");
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
  if (seq !== requestSeq) return; // a newer request superseded this one

  if (!data.ok) {
    showFormulaError(data.error);
    return;
  }
  errorEl.hidden = true;

  formulaText = data.formula;
  commentedText = data.commented ?? "";
  formulaEl.innerHTML = astToHtml(data.ast);
  // The AST must render back to the exact formula string; if the
  // grammars drift, surface it loudly during development.
  const roundTrip = astToText(data.ast);
  if (roundTrip !== data.formula) {
    console.error("AST rendering drift:", { server: data.formula, client: roundTrip });
  }

  flat = [];
  byRendered = new Map();
  indexByNode = new Map();
  lastSelectionKey = "";
  flatten(data.explanation, null, 0);
  defaultFolded();
  document.getElementById("why-tree").innerHTML = whyTreeHtml();
  refreshTreeFolding();
  // Select the first node below the metric root — it carries the
  // interesting top-level structure.
  selectWhyNode(Math.min(1, flat.length - 1), false);
}

// The innermost explanation for a formula span: try the span's own
// text, then walk outward through the enclosing spans.
function explanationForSpan(el) {
  let span = el.closest(".formula-node");
  while (span) {
    const i = byRendered.get(span.dataset.expr);
    if (i != null) return { i, span };
    span = span.parentElement?.closest(".formula-node") ?? null;
  }
  return null;
}

function setupFormulaTooltip() {
  const view = document.getElementById("formula-view");
  const tip = document.getElementById("formula-tip");
  let tipFor = null;

  view.addEventListener("mousemove", (ev) => {
    const found = explanationForSpan(ev.target);
    if (!found) {
      tip.hidden = true;
      tipFor?.classList.remove("expr-hl");
      tipFor = null;
      return;
    }
    const { i, span } = found;
    if (span !== tipFor) {
      tipFor?.classList.remove("expr-hl");
      tipFor = span;
      span.classList.add("expr-hl");
      const node = flat[i].node;
      tip.innerHTML = `
        <span class="exp-kind">${escapeHtml(kindLabel(node.kind))}</span>
        <p>${escapeHtml(node.rationale)}</p>
        <span class="tip-hint">click to open in the Why panel</span>`;
      tip.hidden = false;
    }
    // Follow the cursor, clamped to the viewport.
    const pad = 14;
    const rect = tip.getBoundingClientRect();
    let x = ev.clientX + pad;
    let y = ev.clientY + pad;
    if (x + rect.width > window.innerWidth - 4) x = ev.clientX - rect.width - pad;
    if (y + rect.height > window.innerHeight - 4) y = ev.clientY - rect.height - pad;
    tip.style.left = `${Math.max(4, x)}px`;
    tip.style.top = `${Math.max(4, y)}px`;
  });
  view.addEventListener("mouseleave", () => {
    tip.hidden = true;
    tipFor?.classList.remove("expr-hl");
    tipFor = null;
  });
  view.addEventListener("click", (ev) => {
    const ref = ev.target.closest(".formula-ref");
    if (ref) {
      // A #N selects the component on the canvas.
      selectOnCanvas(Number(ref.dataset.id));
      return;
    }
    // Any other part opens its explanation in the Why panel.
    const found = explanationForSpan(ev.target);
    if (found) selectWhyNode(found.i);
  });
}

function setupWhyDrawer() {
  const tree = document.getElementById("why-tree");
  tree.addEventListener("click", (ev) => {
    // The chevron folds/unfolds; anywhere else on the row selects.
    const toggle = ev.target.closest(".why-toggle:not(.leaf)");
    if (toggle) {
      const i = Number(toggle.dataset.toggle);
      if (folded.has(i)) folded.delete(i);
      else folded.add(i);
      refreshTreeFolding();
      return;
    }
    const row = ev.target.closest(".why-row");
    if (row) selectWhyNode(Number(row.dataset.i), false);
  });
  // mouseover re-fires for every child element inside a row; only
  // re-highlight when the hovered row actually changed.
  let hoveredRow = null;
  tree.addEventListener("mouseover", (ev) => {
    const row = ev.target.closest(".why-row");
    if (!row || row === hoveredRow) return;
    hoveredRow = row;
    crossHighlight(flat[Number(row.dataset.i)].node.component_ids);
  });
  tree.addEventListener("mouseleave", () => {
    hoveredRow = null;
    clearCrossHighlight();
  });

  const detail = document.getElementById("why-detail");
  detail.addEventListener("click", (ev) => {
    const nav = ev.target.closest(".chip-nav");
    if (nav) {
      selectWhyNode(Number(nav.dataset.goto));
      return;
    }
    const chip = ev.target.closest(".chip[data-id]");
    if (chip) selectOnCanvas(Number(chip.dataset.id));
  });
}

function setupMetricButtons() {
  const bar = document.getElementById("metric-buttons");
  bar.addEventListener("click", (ev) => {
    const btn = ev.target.closest(".metric-btn");
    if (!btn) return;
    metric = btn.dataset.metric;
    for (const b of bar.querySelectorAll(".metric-btn")) {
      b.classList.toggle("active", b === btn);
    }
    refreshFormula();
  });
}

function setupCopyButtons() {
  const copy = async (text, what) => {
    if (!text) {
      notify("No formula to copy.");
      return;
    }
    try {
      await navigator.clipboard.writeText(text);
      notify(`Copied the ${what}.`, "success");
    } catch (e) {
      notify(`Copy failed: ${e.message}`);
    }
  };
  document
    .getElementById("copy-formula")
    .addEventListener("click", () => copy(formulaText, "formula"));
  document
    .getElementById("copy-commented")
    .addEventListener("click", () => copy(commentedText, "commented formula"));
}

// The options card is collapsed by default, so a badge on its summary
// keeps active options visible ("2 on"); empty when nothing is set.
function updateConfigCount() {
  const boxes = document.querySelectorAll("#config-panel input[type=checkbox]");
  const on = [...boxes].filter((box) => box.checked).length;
  document.getElementById("config-count").textContent = on ? `${on} on` : "";
}

export function setupExplainPanel() {
  setupMetricButtons();
  setupFormulaTooltip();
  setupWhyDrawer();
  setupCopyButtons();
  useSelectionEl().addEventListener("change", refreshFormula);
  // Engine options are client-side request parameters, so a change
  // only needs a re-fetch — nothing to store on the server.
  for (const box of document.querySelectorAll("#config-panel input[type=checkbox]")) {
    box.addEventListener("change", () => {
      updateConfigCount();
      refreshFormula();
    });
  }
  formulaCanvas().setSelectionHandler(selectionChanged, selectionChanged);
  // T toggles the selection's telemetry — the keyboard twin of the
  // context-menu entry, only while the Formulas subview is showing.
  document.addEventListener("keydown", (e) => {
    if (e.key !== "t" && e.key !== "T") return;
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    if (document.body.dataset.subview !== "formulas") return;
    if (e.target.matches?.("input, textarea, [contenteditable]")) return;
    toggleTelemetry();
  });
}

let lastSelectionKey = "";
// True while a canvas selection comes from inside the explain UI (a
// formula #N or a detail chip): the Why panel must keep its place.
let internalSelect = false;

function selectOnCanvas(id) {
  internalSelect = true;
  try {
    formulaCanvas().select([id]);
  } finally {
    internalSelect = false;
  }
}

// The most specific explanation that mentions the component: the node
// covering the fewest components; the deepest one on a tie. (A
// component usually appears in several parts of a formula — its own
// reading, the sums, the metric root.)
function whyIndexForComponent(id) {
  let best = null;
  flat.forEach((entry, i) => {
    if (!entry.node.component_ids.includes(id)) return;
    if (best == null) {
      best = i;
      return;
    }
    const count = entry.node.component_ids.length;
    const bestCount = flat[best].node.component_ids.length;
    if (count < bestCount || (count === bestCount && entry.depth > flat[best].depth)) {
      best = i;
    }
  });
  return best;
}

// Canvas selection changes: re-run the formula when the selection
// feeds the metric's target ids; otherwise jump the Why panel to the
// selected component's place in the current explanation.
export function selectionChanged() {
  const selection = formulaCanvas().selectedIds();
  const key = selection.join(",");
  if (key === lastSelectionKey) return;
  lastSelectionKey = key;
  if (
    SINGLE_ID_METRICS.has(metric) ||
    (GROUP_ID_METRICS.has(metric) && useSelectionEl().checked)
  ) {
    refreshFormula();
    return;
  }
  if (selection.length === 1 && !internalSelect) {
    const i = whyIndexForComponent(selection[0]);
    if (i != null) selectWhyNode(i);
  }
}
