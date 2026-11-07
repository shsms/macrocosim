// Topology-canvas editor: clipboard, undo / redo, copy / paste /
// cut / delete / select-all hooked into the topology selection,
// the floating right-click menu, the side-panel `Add component`
// form, and helpers around them.

import { escapeHtml, mutate } from "./app.js";
import { OPERATIONAL_MODES, setStateKnob, showComponent } from "./inspect.js";
import { notify } from "./notices.js";
import { pasteIntoSpec, specTypeFor } from "./paste-forms.js";
import { readSelectedMg } from "./routing.js";
import { addComponent, editSpec, removeComponents } from "./spec-edit.js";
import { ALIGN_MODES, topology } from "./topology.js";

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

// Undo / redo over the selected microgrid's structure. The history
// lives on the server (one stacked spec per PUT), so there is nothing
// to record client-side and a page reload — or a second tab — walks
// the same history.
export const undoMgr = (() => {
  async function step(direction) {
    const mgId = readSelectedMg();
    if (mgId == null) {
      notify(`Select a microgrid to ${direction}.`);
      return;
    }
    try {
      await mutate("POST", `/api/mg/${mgId}/${direction}`);
    } catch (e) {
      // A 409 is "nothing left on that stack" — the server's own
      // wording says so, so pass it through.
      notify(`${cap(direction)} failed: ${e.message}`);
    }
  }
  const cap = (s) => s[0].toUpperCase() + s.slice(1);
  return {
    undo: () => step("undo"),
    redo: () => step("redo"),
  };
})();

function snapshotSelection(selectedIds) {
  const components = selectedIds
    .map((id) => topology.get(id))
    .filter(Boolean)
    .map(({ id, category, subtype, hidden, operational_mode }) => ({
      id,
      category,
      subtype,
      hidden: !!hidden,
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
  const unknown = snap.components.find((c) => specTypeFor(c) == null);
  if (unknown) {
    notify(`Don't know how to copy a "${unknown.category}".`);
    return false;
  }
  clipboard.set(snap);
  const n = snap.components.length;
  notify(`Copied ${n} component${n > 1 ? "s" : ""} to clipboard.`, "success");
  return true;
}

// Paste the clipboard subgraph as one spec edit (one undo step), its
// components under fresh ids; the splice comes from paste-forms.js.
export async function pasteClipboard() {
  if (clipboard.isEmpty()) {
    notify("Clipboard is empty — copy something first.");
    return;
  }
  const snap = clipboard.get();
  await editSpec("Paste failed", (spec, ids) => {
    pasteIntoSpec(spec, snap, ids);
  }, snap.components.length);
}

export async function deleteSelection() {
  const ids = topology.selectedIds();
  if (!ids.length) {
    notify("Nothing selected to delete.");
    return;
  }
  await removeComponents(ids);
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
// with a separator. Kept separate from the Topology menu (below) so
// any other canvas can offer the same align section without drifting.
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
      topology.focusCanvas();
      items[idx].action();
    });
  }
}

// Bulk operational-mode set: one runtime write per selected
// component. It changes the live mode, not the spec, so the next
// structural edit puts the spec's mode back; the toast says so.
async function setSelectionMode(ids, mode) {
  const results = await Promise.all(ids.map((id) => setStateKnob(id, "operational-mode", mode)));
  const done = results.filter(Boolean).length;
  if (done) {
    notify(
      `Operational mode ${mode} for ${done} component${done > 1 ? "s" : ""}, until the next structural edit.`,
      "success",
    );
  }
}

// The operational-mode section of the Topology canvas's right-click
// menu, factored out so every caller offers the same vocabulary.
export function modeMenuItems(ids) {
  return OPERATIONAL_MODES.map((m, i) => ({
    label: `Mode: ${m.value}`,
    title: `${m.hint}; live only: the next structural edit puts the spec's mode back`,
    separator: i === 0,
    action: () => setSelectionMode(ids, m.value),
  }));
}

// The Topology canvas's right-click menu. Items are context-
// dependent: Copy + Cut + Delete when something's selected (plus
// the operational-mode section), Paste when nothing's selected and
// the clipboard has content, and the align section when two or more
// nodes are selected.
export function showContextMenu(x, y) {
  const menu = document.getElementById("ctx-menu");
  const sel = topology.selectedIds();
  const items = [];
  if (sel.length) {
    items.push({ label: "Copy", shortcut: "Ctrl/Cmd+C", action: copySelection });
    items.push({ label: "Cut", shortcut: "Ctrl/Cmd+X", action: cutSelection });
    items.push({ label: "Delete", shortcut: "Del", action: deleteSelection });
    items.push(...modeMenuItems(sel));
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

// The component palette: one button per variant, click to add. A
// button's `data-type` is the spec type it adds, and `data-hidden`
// marks a hidden one.
export function setupAddForm() {
  document.getElementById("palette").addEventListener("click", async (ev) => {
    const btn = ev.target.closest(".pal-btn");
    if (!btn) return;
    btn.disabled = true;
    try {
      await addComponent(btn.dataset.type, "hidden" in btn.dataset ? { hidden: true } : {});
    } finally {
      btn.disabled = false;
    }
  });
}
