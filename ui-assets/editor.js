// Topology-canvas editor: clipboard + undo stack, copy / paste /
// cut / delete / select-all hooked into the topology selection,
// the floating right-click menu, the side-panel `Add component`
// form, and helpers around them.

import { escapeHtml, notify } from "./app.js";
import { showComponent } from "./inspect.js";
import { mgPath, readSelectedMg } from "./routing.js";
import { ALIGN_MODES, topology } from "./topology.js";

function makeFnFor(c) {
  if (c.category === "inverter") {
    return c.subtype === "solar" ? "make-solar-inverter" : "make-battery-inverter";
  }
  return {
    grid: "make-grid-connection-point",
    meter: "make-meter",
    battery: "make-battery",
    "ev-charger": "make-ev-charger",
    chp: "make-chp",
    "wind-turbine": "make-wind-turbine",
    "steam-boiler": "make-steam-boiler",
    "power-transformer": "make-power-transformer",
    breaker: "make-breaker",
  }[c.category] ?? null;
}

// Editor-style clipboard for copy / paste of node subgraphs. Holds a
// snapshot of the *structure* (categories, subtypes, edges between
// the captured nodes) — runtime state (SoC, setpoints) is not part
// of the snapshot, matching duplicate's structural-only semantics.
//
// Clipboard survives until replaced; paste can be repeated to drop
// multiple copies of the same subgraph. Cleared on hard reload (page
// refresh) since we don't persist it.
export const clipboard = (() => {
  let buf = null; // { components: [{id, category, subtype}], edges: [[from,to]] }
  return {
    get: () => buf,
    isEmpty: () => buf == null || buf.components.length === 0,
    set(snapshot) {
      buf = snapshot;
    },
  };
})();

// Per-microgrid undo / redo stacks over the overrides file. Each
// canvas-driven mutation snapshots the file BEFORE the eval; Ctrl-Z
// restores the snapshot via POST /api/mg/{id}/overrides/text, which
// rewrites + reloads in one shot. Stacks are keyed by mg id so
// switching microgrids doesn't lose either side's history.
export const undoMgr = (() => {
  const undoStacks = new Map(); // mgId -> string[]
  const redoStacks = new Map(); // mgId -> string[]
  const MAX_DEPTH = 50;

  function stackFor(map, mgId) {
    let s = map.get(mgId);
    if (!s) { s = []; map.set(mgId, s); }
    return s;
  }

  async function fetchText(mgId) {
    const res = await fetch(`/api/mg/${mgId}/overrides/text`);
    if (!res.ok) throw new Error(`GET ${res.status}`);
    return await res.text();
  }

  async function postText(mgId, text) {
    const res = await fetch(`/api/mg/${mgId}/overrides/text`, {
      method: "POST",
      body: text,
    });
    if (!res.ok) {
      const body = await res.text();
      throw new Error(`POST ${res.status}: ${body}`);
    }
  }

  // Snapshot the overrides file before a mutation. Pushes onto the
  // current mg's undo stack and clears its redo stack (the standard
  // editor rule — a fresh edit invalidates the redo history).
  async function record() {
    const mgId = readSelectedMg();
    if (mgId == null) return;
    try {
      const snap = await fetchText(mgId);
      const u = stackFor(undoStacks, mgId);
      u.push(snap);
      while (u.length > MAX_DEPTH) u.shift();
      redoStacks.set(mgId, []);
    } catch (e) {
      console.warn("undoMgr.record failed:", e);
    }
  }

  async function undo() {
    const mgId = readSelectedMg();
    if (mgId == null) return;
    const u = stackFor(undoStacks, mgId);
    if (!u.length) {
      notify("Nothing to undo on this microgrid.");
      return;
    }
    let current;
    try { current = await fetchText(mgId); } catch (e) {
      notify(`Undo failed: ${e.message}`);
      return;
    }
    const target = u.pop();
    try {
      await postText(mgId, target);
    } catch (e) {
      u.push(target);
      notify(`Undo failed: ${e.message}`);
      return;
    }
    stackFor(redoStacks, mgId).push(current);
  }

  async function redo() {
    const mgId = readSelectedMg();
    if (mgId == null) return;
    const r = stackFor(redoStacks, mgId);
    if (!r.length) {
      notify("Nothing to redo on this microgrid.");
      return;
    }
    let current;
    try { current = await fetchText(mgId); } catch (e) {
      notify(`Redo failed: ${e.message}`);
      return;
    }
    const target = r.pop();
    try {
      await postText(mgId, target);
    } catch (e) {
      r.push(target);
      notify(`Redo failed: ${e.message}`);
      return;
    }
    stackFor(undoStacks, mgId).push(current);
  }

  return { record, undo, redo };
})();

function snapshotSelection(selectedIds) {
  const mainId = topology.mainMeterId();
  const components = selectedIds
    .map((id) => topology.get(id))
    .filter(Boolean)
    .map(({ id, category, subtype, hidden, operational_mode }) => ({
      id,
      category,
      subtype,
      hidden: !!hidden,
      main: id === mainId,
      operational_mode,
    }));
  if (!components.length) return null;
  const selected = new Set(selectedIds);
  const edges = topology
    .connections()
    .filter(([from, to]) => selected.has(from) && selected.has(to));
  return { components, edges };
}

export function copySelection() {
  const ids = topology.selectedIds();
  if (!ids.length) {
    notify("Nothing selected to copy.");
    return false;
  }
  const snap = snapshotSelection(ids);
  if (!snap) return false;
  const unknown = snap.components.find((c) => makeFnFor(c) == null);
  if (unknown) {
    notify(`Don't know how to copy a "${unknown.category}".`);
    return false;
  }
  clipboard.set(snap);
  const n = snap.components.length;
  notify(`Copied ${n} component${n > 1 ? "s" : ""} to clipboard.`, "success");
  return true;
}

// Paste the clipboard subgraph as a fresh set of components + edges
// via one let*-bound eval. Matches duplicate's old behavior — uses
// the public make-* wrappers so per-category defaults apply, threads
// component-id to wire reconnects atomically. One pending log entry.
export async function pasteClipboard() {
  if (clipboard.isEmpty()) {
    notify("Clipboard is empty — copy something first.");
    return;
  }
  const snap = clipboard.get();
  const bindings = snap.components
    .map((c) => {
      const flags = [];
      // make-meter's `:hidden t` and `:main t` only apply to meters,
      // but other categories ignore unknown kwargs gracefully — emit
      // when set so the snapshot round-trips. Sticky for cut+paste
      // and cross-mg copy+paste; same-mg copy+paste of an existing
      // `:main` meter will surface a "main meter already set" error
      // from make-meter, which is the expected guard.
      if (c.hidden) flags.push(":hidden t");
      if (c.main) flags.push(":main t");
      // The operational mode is config, so a clone keeps it.
      if (c.operational_mode && c.operational_mode !== "unspecified") {
        flags.push(`:operational-mode '${c.operational_mode}`);
      }
      const args = flags.length ? ` ${flags.join(" ")}` : "";
      return `(m${c.id} (${makeFnFor(c)}${args}))`;
    })
    .join(" ");
  const reconnects = snap.edges
    .map(([from, to]) => `(connect m${from} m${to})`)
    .join(" ");
  const src = reconnects
    ? `(let* (${bindings}) ${reconnects})`
    : `(let* (${bindings}) t)`;
  await undoMgr.record();
  const res = await fetch(mgPath("eval"), { method: "POST", body: src });
  const data = await res.json();
  if (!data.ok) notify(`Paste failed: ${data.error}`);
}

export async function deleteSelection() {
  const ids = topology.selectedIds();
  if (!ids.length) {
    notify("Nothing selected to delete.");
    return;
  }
  const removes = ids.map((id) => `(remove-component ${id})`).join(" ");
  const src = `(progn ${removes})`;
  await undoMgr.record();
  const res = await fetch(mgPath("eval"), { method: "POST", body: src });
  const data = await res.json();
  if (!data.ok) notify(`Delete failed: ${data.error}`);
}

export async function cutSelection() {
  if (copySelection()) await deleteSelection();
}

export function selectAllVisible() {
  const ids = topology.allIds();
  if (!ids.length) return;
  topology.select(ids);
  showComponent(topology.get(ids[0]));
}

// One context-menu entry per ALIGN_MODES row; each group starts
// with a separator. Shared between the Topology menu (below) and
// the Formulas canvas's align-only menu, so the two cannot drift.
export function alignMenuItems(canvas) {
  const items = [];
  let prevGroup = null;
  for (const m of ALIGN_MODES) {
    items.push({
      label: m.menu,
      title: m.title,
      action: () =>
        m.scale
          ? canvas.scaleSelection(m.scale.axis, m.scale.factor)
          : canvas.alignSelection(m.mode),
      separator: m.group !== prevGroup,
    });
    prevGroup = m.group;
  }
  return items;
}

// Renders `items` as the floating menu at (x, y): label + optional
// shortcut per row, separators between groups, viewport-clamped.
// Hidden on outside click, Esc, or after running an action.
export function showMenuItems(menu, items, x, y) {
  menu.innerHTML = items
    .map(
      (it) =>
        `${it.separator ? '<div class="ctx-separator"></div>' : ""}
        <button class="ctx-item" data-idx="${items.indexOf(it)}"${it.title ? ` title="${escapeHtml(it.title)}"` : ""}>
          <span>${it.label}</span>${it.shortcut ? `<kbd>${it.shortcut}</kbd>` : ""}
        </button>`,
    )
    .join("");
  menu.style.left = `${x}px`;
  menu.style.top = `${y}px`;
  menu.hidden = false;
  // Clamp to viewport — menu has a fixed width so we can compare
  // after layout settles.
  requestAnimationFrame(() => {
    const rect = menu.getBoundingClientRect();
    if (rect.right > window.innerWidth) {
      menu.style.left = `${window.innerWidth - rect.width - 4}px`;
    }
    if (rect.bottom > window.innerHeight) {
      menu.style.top = `${window.innerHeight - rect.height - 4}px`;
    }
  });
  for (const btn of menu.querySelectorAll(".ctx-item")) {
    btn.addEventListener("click", () => {
      const idx = Number(btn.dataset.idx);
      hideContextMenu();
      items[idx].action();
    });
  }
}

// The Topology canvas's right-click menu. Items are context-
// dependent: Copy + Cut + Delete when something's selected, Paste
// when nothing's selected and the clipboard has content, and the
// align section when two or more nodes are selected.
export function showContextMenu(x, y) {
  const menu = document.getElementById("ctx-menu");
  const sel = topology.selectedIds();
  const items = [];
  if (sel.length) {
    items.push({ label: "Copy", shortcut: "Ctrl/Cmd+C", action: copySelection });
    items.push({ label: "Cut", shortcut: "Ctrl/Cmd+X", action: cutSelection });
    items.push({ label: "Delete", shortcut: "Del", action: deleteSelection });
  } else if (!clipboard.isEmpty()) {
    items.push({ label: "Paste", shortcut: "Ctrl/Cmd+V", action: pasteClipboard });
  }
  if (sel.length >= 2) items.push(...alignMenuItems(topology));
  if (!items.length) return; // nothing relevant; keep menu hidden
  showMenuItems(menu, items, x, y);
}

function hideContextMenu() {
  const menu = document.getElementById("ctx-menu");
  if (menu) menu.hidden = true;
}

export function setupContextMenu() {
  // Outside-click and Esc dismiss the menu. Capture phase so the
  // click that picked the menu item runs first.
  document.addEventListener("mousedown", (e) => {
    const menu = document.getElementById("ctx-menu");
    if (!menu.hidden && !menu.contains(e.target)) hideContextMenu();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") hideContextMenu();
  });
}

export function setupAddForm() {
  const sel = document.getElementById("add-category");
  const btn = document.getElementById("add-btn");
  btn.addEventListener("click", async () => {
    const fn = sel.value;
    btn.disabled = true;
    try {
      await undoMgr.record();
      const res = await fetch(mgPath("eval"), {
        method: "POST",
        body: `(${fn})`,
      });
      const data = await res.json();
      if (!data.ok) notify(`Create failed: ${data.error}`);
    } finally {
      btn.disabled = false;
    }
  });
}
