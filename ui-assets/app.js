// Phase-1 SPA. Renders /api/topology with vis-network, and on node
// selection shows category-appropriate live charts in the floating
// inspector.
// Visual editing (add / connect / rename / delete) + REPL +
// Defaults / Scenarios all hang off the same /api/eval mutation
// path so anything done in the UI is also scriptable from outside.

import { clockState, pulseBar } from "./chrome.js";
import { dashboardTiles } from "./dashboard.js";
import {
  setupDefaultsToggle,
  setupHelpButton,
  setupScenarioReportToggle,
  setupSnapshotsDialog,
} from "./dialogs.js";
import { dispatchForm } from "./dispatch-form.js";
import {
  copySelection,
  cutSelection,
  deleteSelection,
  pasteClipboard,
  selectAllVisible,
  setupAddForm,
  setupContextMenu,
  undoMgr,
} from "./editor.js";
import { formulaCanvas, refreshFormula, setupExplainPanel } from "./explain.js";
import { setupFormulaTileClicks } from "./formulas.js";
import { setupInspectorChips, showComponent } from "./inspect.js";
import { microgridsPanel, scenariosPanel } from "./panels.js";
import { backfillLogs, openWebSocket, setupRepl } from "./repl.js";
import {
  jumpToTopology,
  mgPath,
  navigateTo,
  refreshTopology,
  selectMicrogrid,
  setupDensityToggle,
  setupModeToggle,
  setupReplMgChip,
  visibleSubview,
} from "./routing.js";
import { closePanel } from "./side-panel.js";
import { setupDrawerSplitter, setupFormulaDrawerSplitter } from "./splitter.js";
import { topology } from "./topology.js";

// Re-export the routing helpers that other modules still pull
// via `./app.js` so consumers (dashboard / formulas / panels /
// chrome) keep working without rewiring every import site.
export {
  jumpToTopology,
  mgPath,
  navigateTo,
  refreshTopology,
  selectMicrogrid,
  setupDensityToggle,
};

const status = document.getElementById("status");
// `inspect` holds the inspector's swappable content; `inspector` is the
// floating card around it. The `inspector-open` class on <body> shows
// the card, which overlays the right side of the canvas — it never
// resizes it, so a double-click's second click lands on an unmoved
// graph. Set when something is selected (or a chrome panel is
// opened); cleared on deselect, Esc, the × button, or a tab switch —
// all via closePanel() (side-panel.js — it also owns openPanel(),
// which sets these two elements up on open).
export const inspectEl = document.getElementById("inspect");
export const inspectorEl = document.getElementById("inspector");

export function setStatus(text, klass) {
  status.textContent = text;
  status.className = `status ${klass || ""}`;
}

// Surface a transient toast in the bottom-right. Auto-dismisses after
// ~5s. Use this — not alert() — for action-failure feedback so the
// chrome stays unblocking when the server hiccups during, say, a WS
// reconnect storm. Three places fall outside this rule:
//   * `setStatus` for the persistent connection-state pill (top bar).
//   * `console.error` for diagnostics that only matter in the dev tools.
//   * confirm() prompts that genuinely need a synchronous yes/no.
export function notify(message, kind = "error") {
  let host = document.getElementById("toast-host");
  if (!host) {
    host = document.createElement("div");
    host.id = "toast-host";
    document.body.appendChild(host);
  }
  const t = document.createElement("div");
  t.className = `toast toast-${kind}`;
  t.textContent = message;
  host.appendChild(t);
  setTimeout(() => t.remove(), 5000);
}

// Layout picker + snap toggle wiring for one canvas-controls strip.
// Clicking an algorithm applies it (and drops any manual
// arrangement); clicking the active one re-runs it. The snap toggle
// is the magnetic grid for node drags; Alt-drag locks the movement
// to one axis, with snap on or off.
export function setupCanvasControls(stripId, canvas) {
  const strip = document.getElementById(stripId);
  strip.addEventListener("click", (ev) => {
    const layoutBtn = ev.target.closest(".layout-btn");
    if (layoutBtn) {
      for (const b of strip.querySelectorAll(".layout-btn")) {
        b.classList.toggle("active", b === layoutBtn);
      }
      canvas.resetLayout(layoutBtn.dataset.layout);
      return;
    }
    const snapBtn = ev.target.closest(".snap-btn");
    if (snapBtn) {
      snapBtn.classList.toggle("active");
      canvas.setSnap(snapBtn.classList.contains("active"));
    }
    const valuesBtn = ev.target.closest(".values-btn");
    if (valuesBtn && canvas.setValues) {
      valuesBtn.classList.toggle("active");
      canvas.setValues(valuesBtn.classList.contains("active"));
    }
  });
}

export function escapeHtml(s) {
  return String(s).replace(
    /[<>&"']/g,
    (c) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;", '"': "&quot;", "'": "&#39;" })[c],
  );
}

// Wire the floating panels' chrome: the inspector's × (close +
// deselect the node so a re-click reopens it), and the + Add button /
// its panel's × (toggle the topology-only Add-component card).
function setupFloatingPanels() {
  document.getElementById("inspector-close").addEventListener("click", () => {
    closePanel();
    topology.select([]);
  });
  const addPanel = document.getElementById("add-panel");
  document
    .getElementById("add-toggle")
    .addEventListener("click", () => addPanel.classList.toggle("open"));
  document
    .getElementById("add-panel-close")
    .addEventListener("click", () => addPanel.classList.remove("open"));
}

// JSON mutation helper shared by the dispatches panel's row actions
// and the dispatch-form dialog: a non-2xx surfaces the server's
// error text (the store's 400 / 404 messages) as a thrown Error.
export async function mutate(method, path, body) {
  const opts = { method };
  if (body !== undefined) {
    opts.headers = { "content-type": "application/json" };
    opts.body = JSON.stringify(body);
  }
  const res = await fetch(path, opts);
  if (!res.ok) {
    const txt = await res.text().catch(() => "");
    throw new Error(txt || `HTTP ${res.status}`);
  }
  return res;
}

// ─── Dispatches (per-microgrid) ─────────────────────────────────────────────
//
// Read-only table of the dispatches switchyard's dispatch API holds for
// the selected microgrid. Rendered on entering the Dispatches sub-tab
// and refetched when a `dispatch_changed` WS event names this microgrid
// (the dispatch CLI created / updated / deleted one).
export const dispatchesPanel = (() => {
  const host = () => document.getElementById("dispatches-body");
  // The microgrid currently shown — set by render(), read by the
  // create form + row-button handlers (which are wired once in setup).
  let currentMg = null;

  function fmtTs(ms) {
    if (ms == null) return "—";
    try {
      return new Date(ms).toLocaleString("en-GB", {
        year: "numeric",
        month: "short",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        hour12: false,
        timeZone: clockState.tzInUse(),
      });
    } catch (_) {
      return new Date(ms).toISOString();
    }
  }

  function fmtDuration(s) {
    if (s == null) return "indefinite";
    if (s === 0) return "instant";
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = s % 60;
    return (
      [h && `${h}h`, m && `${m}m`, sec && `${sec}s`].filter(Boolean).join(" ") ||
      "0s"
    );
  }

  function payloadText(p) {
    if (p == null) return "—";
    if (typeof p === "object" && !Array.isArray(p) && Object.keys(p).length === 0)
      return "—";
    return JSON.stringify(p);
  }

  function rowHtml(d) {
    const status = d.active
      ? '<span class="disp-badge disp-on">active</span>'
      : '<span class="disp-badge disp-off">inactive</span>';
    const dry = d.dry_run ? ' <span class="disp-badge disp-dry">dry-run</span>' : "";
    const payload = payloadText(d.payload);
    const payloadCell =
      payload === "—"
        ? "—"
        : `<code title="${escapeHtml(payload)}">${escapeHtml(
            payload.length > 60 ? `${payload.slice(0, 59)}…` : payload,
          )}</code>`;
    const toggle = d.active ? "Pause" : "Resume";
    return `<tr>
      <td class="disp-id">#${d.id}</td>
      <td>${escapeHtml(d.type)}</td>
      <td>${status}${dry}</td>
      <td>${escapeHtml(fmtTs(d.start_ms))}</td>
      <td>${escapeHtml(fmtDuration(d.duration_s))}</td>
      <td>${escapeHtml(d.target)}</td>
      <td>${escapeHtml(d.recurrence || "once")}</td>
      <td class="disp-payload">${payloadCell}</td>
      <td class="disp-actions">
        <button class="link-btn" data-disp-toggle="${d.id}" data-next="${d.active ? 0 : 1}">${toggle}</button>
        <button class="link-btn disp-del" data-disp-del="${d.id}">Delete</button>
      </td>
    </tr>`;
  }

  function emptyHtml() {
    return `<p class="hint">No dispatches for this microgrid yet — create one with the + New dispatch button, <code>swctl dispatch create</code>, or the dispatch CLI.</p>`;
  }

  async function setActive(id, active) {
    if (currentMg == null) return;
    try {
      await mutate("POST", `/api/mg/${currentMg}/dispatches/${id}/active`, {
        active,
      });
      render(currentMg);
    } catch (err) {
      notify(`${active ? "resume" : "pause"} failed: ${err.message}`);
    }
  }

  async function remove(id) {
    if (currentMg == null) return;
    if (!confirm(`Delete dispatch #${id}? This can't be undone.`)) return;
    try {
      await mutate("DELETE", `/api/mg/${currentMg}/dispatches/${id}`);
      render(currentMg);
    } catch (err) {
      notify(`delete failed: ${err.message}`);
    }
  }

  // Bumped on every render() call. A fetch that resolves after the
  // user navigated to a different microgrid (or a newer WS-driven
  // refresh started) carries a stale generation and is dropped —
  // last-STARTED wins instead of last-RESOLVED, so mg A's rows can't
  // paint under mg B's header.
  let renderGen = 0;

  async function render(mgId) {
    currentMg = mgId;
    const gen = ++renderGen;
    const el = host();
    if (!el) return;
    let list;
    try {
      const res = await fetch(`/api/mg/${mgId}/dispatches`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      list = await res.json();
    } catch (err) {
      if (gen !== renderGen) return;
      el.innerHTML = `<p class="hint">dispatches unavailable: ${escapeHtml(
        err.message,
      )}</p>`;
      return;
    }
    if (gen !== renderGen) return;
    if (!Array.isArray(list) || list.length === 0) {
      el.innerHTML = emptyHtml();
      return;
    }
    el.innerHTML = `<table class="disp-table">
      <thead><tr>
        <th>ID</th><th>Type</th><th>Status</th><th>Start</th>
        <th>Duration</th><th>Target</th><th>Recurs</th><th>Payload</th><th></th>
      </tr></thead>
      <tbody>${list.map(rowHtml).join("")}</tbody>
    </table>`;
  }

  // Wire the New-dispatch button + row-action delegation once at
  // startup. The button and #dispatches-body are static in
  // index.html, so the listeners survive every render() (which only
  // swaps innerHTML). Creation itself lives in the dispatch-form
  // dialog; it re-renders this panel via its onCreated callback.
  function setup() {
    // Refresh only if the panel still shows the microgrid the
    // dispatch was created for — a slow create settling after the
    // user navigated away must not repaint (and repoint) the panel
    // to the old microgrid.
    dispatchForm.setup({
      onCreated: (mg) => {
        if (mg === currentMg) render(mg);
      },
    });
    const newBtn = document.getElementById("dispatch-new-btn");
    if (newBtn) {
      newBtn.addEventListener("click", () => dispatchForm.open(currentMg));
    }
    const body = host();
    if (body) {
      body.addEventListener("click", (e) => {
        const btn = e.target.closest("button");
        if (!btn) return;
        if (btn.dataset.dispToggle != null) {
          setActive(Number(btn.dataset.dispToggle), btn.dataset.next === "1");
        } else if (btn.dataset.dispDel != null) {
          remove(Number(btn.dataset.dispDel));
        }
      });
    }
  }

  return { render, setup };
})();

// Pending debounce handle for the WS-driven formula refresh.
let formulaRefreshTimer = null;
let topologyBackfillTimer = null;
let topologyBackfillPending = false;

async function init() {
  setupAddForm();
  setupDefaultsToggle();
  setupScenarioReportToggle();
  setupFloatingPanels();
  setupInspectorChips();
  setupDrawerSplitter();
  setupFormulaDrawerSplitter();
  setupSnapshotsDialog();
  backfillLogs();
  // The topology canvas calls back to showComponent (from inspect.js)
  // / closePanel (from side-panel.js) on node click + canvas click.
  // Wire it up before the first apply so the listeners are in place.
  topology.setSelectionHandler(showComponent, closePanel);
  setupCanvasControls("topology-controls", topology);
  const valuesBtn = document.querySelector("#topology-controls .values-btn");
  if (valuesBtn) valuesBtn.classList.toggle("active", topology.valuesOn());
  setupExplainPanel();
  setupCanvasControls("formulas-controls", formulaCanvas());
  // Editor-style keyboard shortcuts. All check that focus isn't in
  // a text editor (REPL textarea, dialog inputs) before firing, so
  // typing remains unaffected.
  document.addEventListener("keydown", (e) => {
    const inEditable = e.target.matches?.("input, textarea, select, [contenteditable]");
    if (inEditable) return;
    // The editing shortcuts drive the Topology canvas's selection.
    // Anywhere else they must stay inert — Delete pressed on the
    // Formulas tab, the Scenarios mode, or the microgrid list must
    // not remove whatever happens to be selected on the (hidden)
    // Topology canvas. visibleSubview() checks all three body flags.
    const visible = visibleSubview();
    if (visible !== "topology") {
      if (e.key === "Escape") {
        // Deselect on the raw subview flag, not visibleSubview():
        // data-subview persists across mode switches, and a hidden
        // formula canvas's surviving selection would re-apply (and
        // re-fire the selection handlers) on subview re-entry.
        if (document.body.dataset.subview === "formulas") {
          formulaCanvas().select([]);
        }
        closePanel();
      }
      return;
    }
    const meta = e.metaKey || e.ctrlKey;
    const key = e.key.toLowerCase();
    if (meta && e.shiftKey && key === "z") {
      e.preventDefault();
      undoMgr.redo();
    } else if (meta && key === "z") {
      e.preventDefault();
      undoMgr.undo();
    } else if (meta && key === "y") {
      // Common Windows-style redo alias.
      e.preventDefault();
      undoMgr.redo();
    } else if (meta && (key === "c" || key === "v" || key === "x")) {
      // With text selected somewhere on the page (inspector, log
      // panel, REPL output), the user means the NATIVE clipboard —
      // hijacking Ctrl+C there loses the copy, and Ctrl+X would
      // delete canvas components when they only meant text.
      if (!window.getSelection().isCollapsed) return;
      e.preventDefault();
      if (key === "c") copySelection();
      else if (key === "v") pasteClipboard();
      else cutSelection();
    } else if (meta && key === "a") {
      e.preventDefault();
      selectAllVisible();
    } else if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      deleteSelection();
    } else if (e.key === "Escape") {
      // Topology's own click handler closes the inspector on deselect;
      // mirror that here for keyboard parity.
      topology.select([]);
      closePanel();
    }
  });
  setupContextMenu();
  setupHelpButton();
  setupModeToggle();
  setupReplMgChip();
  setupFormulaTileClicks();
  scenariosPanel.setup();
  dispatchesPanel.setup();
  await clockState.init();
  pulseBar.setup();
  await refreshTopology();
  // WS push: refresh the topology (so the canvas reflects the
  // mutation) and the microgrid list (so the unsaved / unmanaged
  // chips track the edit) on TopologyChanged. Sample events go
  // straight into the live-charts router.
  const onTopologyChanged = () => {
    refreshTopology();
    microgridsPanel.refresh();
    // The formula depends on the topology; re-derive it while the
    // Formulas subview is showing (hidden, the subview-enter hook
    // refreshes on the next visit anyway). Debounced: each formula
    // refresh is a server-side graph build plus a full panel
    // re-render.
    if (visibleSubview() === "formulas") {
      clearTimeout(formulaRefreshTimer);
      formulaRefreshTimer = setTimeout(refreshFormula, 300);
    }
    // The loopback supervisor debounces ~300ms and rebuilds the
    // Microgrid handle; /api/microgrid/latest + /formulas return
    // 503 mid-rebuild. Delay the dashboard re-fetch so it lands
    // after the supervisor settles. At most one backfill timer is
    // armed at a time (each backfill refetches the full 15-min
    // history for every stream); events landing while it's armed
    // set the pending flag, and the callback re-arms once more when
    // the flag is set. So a sustained event storm gets one backfill
    // per 800 ms window (never starved), and the last event of a
    // burst always has a backfill land ≥ 800 ms after it — past the
    // supervisor's rebuild, so the final history refetch isn't the
    // one that ate a 503. backfill() is 503-tolerant — an undershoot
    // leaves the existing tooltip + values, and the next sample-flow
    // tick overwrites the displayed numbers.
    if (topologyBackfillTimer == null) {
      armTopologyBackfill();
    } else {
      topologyBackfillPending = true;
    }
  };
  const armTopologyBackfill = () => {
    topologyBackfillTimer = setTimeout(() => {
      topologyBackfillTimer = null;
      dashboardTiles.backfill();
      if (topologyBackfillPending) {
        topologyBackfillPending = false;
        armTopologyBackfill();
      }
    }, 800);
  };
  // A burst of edits (multi-step undo, scripted import, hot reload)
  // fires one topology_changed per accepted eval, and every refresh
  // is a topology fetch (a server-side graph build) plus a reseed of
  // all dashboard row modules. Leading-edge throttle with trailing
  // catch-up: the first event refreshes immediately, the rest of the
  // burst collapses into one refresh 300 ms later.
  let topologyBurstTimer = null;
  let topologyBurstPending = false;
  openWebSocket((_v) => {
    if (topologyBurstTimer !== null) {
      topologyBurstPending = true;
      return;
    }
    onTopologyChanged();
    topologyBurstTimer = setTimeout(() => {
      topologyBurstTimer = null;
      if (topologyBurstPending) {
        topologyBurstPending = false;
        onTopologyChanged();
      }
    }, 300);
  });
  // Periodically re-seed the dashboard tile values from the cached
  // latest sample, so a dropped/throttled WS frame can't leave a tile
  // frozen on a stale number between topology-driven backfills.
  dashboardTiles.startAutoReseed();
  setupRepl();
}

init();
