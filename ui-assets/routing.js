// SPA routing + persistence: which mode / microgrid / subview is
// active, how state round-trips through localStorage + the URL
// hash, and the keyboard / button hooks that drive transitions.
// Owns mgPath (URL helper), the density toggle, and the
// refreshTopology fetch that ferries /api/topology data into the
// canvas + chrome pulse bar.

import { dispatchesPanel, setStatus } from "./app.js";
import { gridFrequency, pulseBar } from "./chrome.js";
import {
  batteryPairs,
  chpRows,
  dashboardTiles,
  evRows,
  pvRows,
} from "./dashboard.js";
import { overrideState } from "./dialogs.js";
import { formulaCanvas, refreshFormula } from "./explain.js";
import { clearSide, refitCharts, showComponent } from "./inspect.js";
import { microgridsPanel, scenariosPanel } from "./panels.js";
import { topology } from "./topology.js";

// ─── Per-mg URL helper ─────────────────────────────────────────────────────
// Prefixes /api/mg/{selected_id}/ when a microgrid is selected,
// falls back to /api/{suffix} otherwise (used by the loopback HTTP
// backfill on legacy endpoints that haven't been migrated yet,
// e.g. /api/setpoints + /api/format + /api/snapshots).
export function mgPath(suffix) {
  const id = readSelectedMg();
  return id == null ? `/api/${suffix}` : `/api/mg/${id}/${suffix}`;
}

// ─── Density toggle ────────────────────────────────────────────────────────
// CSS-only mode that shrinks tile + pulse-bar paddings and fonts.
// For power users on long soak runs who want more tiles + more
// rows on screen at once. Default = normal (the 32" 4K target
// keeps the comfortable layout the landing one). Preference
// persists in localStorage so a refresh keeps you put.
const DENSITY_KEY = "switchyard-density";

function applyDensity(mode) {
  if (mode === "compact") {
    document.body.dataset.density = "compact";
  } else {
    delete document.body.dataset.density;
  }
}

export function setupDensityToggle() {
  const chip = document.getElementById("density-toggle");
  if (chip) {
    const stored = localStorage.getItem(DENSITY_KEY);
    applyDensity(stored);
    chip.classList.toggle("active", stored === "compact");
    chip.addEventListener("click", () => {
      const next =
        document.body.dataset.density === "compact" ? "normal" : "compact";
      localStorage.setItem(DENSITY_KEY, next);
      applyDensity(next);
      chip.classList.toggle("active", next === "compact");
    });
  }
}

// ─── Route state keys + read helpers ───────────────────────────────────────
// The last /api/topology snapshot as {mg, data}, for catching the
// Formulas canvas up when its subview becomes visible (it is not fed
// while hidden). `mg` is the microgrid the fetch was for, so the
// catch-up can refuse a stale snapshot after a microgrid switch.
let lastTopologySnapshot = null;

const MODE_KEY = "switchyard-mode";
const MG_SELECTED_KEY = "switchyard-selected-mg";
const MG_SUBVIEW_KEY = "switchyard-mg-subview";
const VALID_MODES = new Set(["microgrids", "scenarios"]);
const VALID_SUBVIEWS = new Set(["dashboard", "topology", "formulas", "dispatches"]);

export function readSelectedMg() {
  const raw = localStorage.getItem(MG_SELECTED_KEY);
  if (raw == null || raw === "" || raw === "null") return null;
  const n = Number(raw);
  return Number.isFinite(n) ? n : null;
}

export function readSubview() {
  const v = localStorage.getItem(MG_SUBVIEW_KEY);
  return VALID_SUBVIEWS.has(v) ? v : "dashboard";
}

// The subview name, but only when the mode and mgView flags say the
// subview pane is actually on screen — null otherwise. Keyboard
// shortcuts and WS refreshes must gate on this, never on
// data-subview alone: the subview persists across mode switches and
// would leave canvas shortcuts armed on Scenarios or the microgrid
// list, firing against a hidden canvas's selection.
export function visibleSubview() {
  const { mode, mgView, subview } = document.body.dataset;
  return mode === "microgrids" && mgView === "selected" ? subview : null;
}

// ─── URL routing ────────────────────────────────────────────────────────────
function currentRoute() {
  return {
    mode: localStorage.getItem(MODE_KEY) || "microgrids",
    selectedMg: readSelectedMg(),
    subview: readSubview(),
  };
}

function routeToHash({ mode, selectedMg, subview }) {
  if (mode === "scenarios") return "#scenarios";
  if (selectedMg == null) return "#microgrids";
  return `#microgrids/${selectedMg}/${subview}`;
}

function parseHash(hash) {
  // Empty / bare `#` → fall through to localStorage. Returning a
  // default here would overwrite the user's last-seen state every
  // time they refresh `/`. Explicit `#microgrids` (no trailing
  // segments) still resets to the list view, matching what
  // `routeToHash` emits when selectedMg is null.
  if (!hash || hash === "#") return null;
  if (hash === "#microgrids") {
    return { mode: "microgrids", selectedMg: null, subview: "dashboard" };
  }
  if (hash === "#scenarios") {
    return { mode: "scenarios", selectedMg: null, subview: "dashboard" };
  }
  const m = /^#microgrids\/(\d+)(?:\/(dashboard|topology|formulas|dispatches))?$/.exec(hash);
  if (m) {
    return {
      mode: "microgrids",
      selectedMg: Number(m[1]),
      subview: m[2] || "dashboard",
    };
  }
  return null;
}

function writeRouteToStorage({ mode, selectedMg, subview }) {
  if (mode) localStorage.setItem(MODE_KEY, mode);
  if (selectedMg != null) {
    localStorage.setItem(MG_SELECTED_KEY, String(selectedMg));
  } else if (selectedMg === null) {
    localStorage.removeItem(MG_SELECTED_KEY);
  }
  if (subview) localStorage.setItem(MG_SUBVIEW_KEY, subview);
}

export function navigateTo(next) {
  const cur = currentRoute();
  const merged = { ...cur, ...next };
  writeRouteToStorage(merged);
  const hash = routeToHash(merged);
  if (location.hash !== hash) {
    history.pushState(merged, "", hash);
  }
  applyMode(merged.mode);
}

function setupRouterPopstate() {
  window.addEventListener("popstate", () => {
    const parsed = parseHash(location.hash);
    if (!parsed) return;
    writeRouteToStorage(parsed);
    applyMode(parsed.mode);
  });
}

function applyInitialRoute() {
  const parsed = parseHash(location.hash);
  if (parsed) {
    writeRouteToStorage(parsed);
  }
  const cur = currentRoute();
  // Replace rather than push so the back button doesn't pop into a
  // synthetic empty entry — the user lands on the page; the first
  // back press should leave the SPA, not bounce them inside it.
  history.replaceState(cur, "", routeToHash(cur));
  applyMode(cur.mode);
}

function applyMode(mode) {
  if (!VALID_MODES.has(mode)) mode = "microgrids";
  const selected = readSelectedMg();
  const subview = readSubview();
  document.body.dataset.mode = mode;
  document.body.dataset.mgView = selected == null ? "list" : "selected";
  document.body.dataset.subview = subview;
  // Switching tab/mode dismisses the floating panels — the inspector's
  // selection no longer applies, and the add panel is topology-only.
  // The canvases keep their selection, so tell them the inspector is
  // gone: without resetNotify, re-clicking the still-selected node
  // after switching back would dedup and never reopen the inspector.
  clearSide();
  topology.resetNotify();
  formulaCanvas().resetNotify();
  document.getElementById("add-panel").classList.remove("open");
  for (const btn of document.querySelectorAll("#mode-toggle .mode-btn")) {
    btn.classList.toggle("active", btn.dataset.mode === mode);
  }
  for (const btn of document.querySelectorAll("#mg-subtoggle .mode-btn")) {
    btn.classList.toggle("active", btn.dataset.subview === subview);
  }
  // vis-network needs a redraw nudge when its container goes from
  // display:none back to visible — the canvas was sized to 0×0 while
  // hidden. Same shape the splitter resize handler uses. Defer the
  // fit one animation-frame so the just-flipped `data-subview` has
  // settled the CSS visibility before vis-network measures.
  if (mode === "microgrids" && selected != null && subview === "topology") {
    refitCharts();
    requestAnimationFrame(() => topology.fit());
  }
  if (mode === "microgrids" && selected != null && subview === "formulas") {
    // The Formulas canvas is fed lazily: while its subview is hidden
    // refreshTopology() only stashes the snapshot, so catch up now
    // that the container is visible and measurable. Only a snapshot
    // of THIS microgrid: a stale one from a previously viewed mg
    // would render the wrong site (and leak its component ids into
    // eval calls) until — or unless — the fresh fetch lands.
    if (lastTopologySnapshot?.mg === selected) {
      formulaCanvas().apply(lastTopologySnapshot.data);
    }
    requestAnimationFrame(() => formulaCanvas().fit());
    refreshFormula();
  }
  if (mode === "microgrids" && selected != null && subview === "dashboard") {
    dashboardTiles.backfill();
    gridFrequency.backfill();
  }
  if (mode === "microgrids" && selected != null && subview === "dispatches") {
    dispatchesPanel.render(selected);
  }
  if (mode === "microgrids") microgridsPanel.refresh();
  if (mode === "scenarios") scenariosPanel.refresh();
}

// Jump to the topology subview within the current mode and select
// `id` on the canvas. Used by dashboard tier rows + the formula-tree
// chip clicks. Pushes a history entry so the back button returns
// the user to where they clicked from.
export function jumpToTopology(id) {
  navigateTo({ subview: "topology" });
  topology.select([id]);
  const c = topology.get(id);
  if (c) showComponent(c);
}

export function selectMicrogrid(id) {
  navigateTo({ mode: "microgrids", selectedMg: id });
  renderReplMgChip();
  // Refetch the per-mg topology so the canvas + the empty-hint
  // overlay (D5) reflect the newly-selected microgrid. Without
  // this the canvas keeps showing the previous microgrid's
  // components until a WS topology_changed event arrives — which
  // never happens just because the selection changed client-side.
  // Same for the overrides pill: its snapshot is per-microgrid now.
  if (id != null) {
    refreshTopology();
    overrideState.refresh();
  }
}

// REPL chip — surfaces which microgrid the REPL form's POSTs
// route to. Mirrors mgPath()'s logic: shows "→ {name}" when a
// microgrid is selected, "→ enterprise" otherwise. Clicking
// jumps to the Microgrids list so the operator can pick a
// different one.
export function renderReplMgChip() {
  const chip = document.getElementById("repl-mg-chip");
  if (!chip) return;
  const id = readSelectedMg();
  if (id == null) {
    chip.textContent = "→ enterprise";
    chip.classList.add("muted");
    return;
  }
  chip.classList.remove("muted");
  // Pull the name from the microgridsPanel's cache if available;
  // fall back to "#id" so the chip never sits empty.
  const cached = (window.__mgPanelCache || []).find((m) => m.id === id);
  chip.textContent = `→ ${cached ? cached.name || `#${id}` : `#${id}`}`;
}

export function setupReplMgChip() {
  const chip = document.getElementById("repl-mg-chip");
  if (!chip) return;
  chip.addEventListener("click", () => {
    navigateTo({ mode: "microgrids", selectedMg: null });
    renderReplMgChip();
  });
  renderReplMgChip();
}

export function setupModeToggle() {
  for (const btn of document.querySelectorAll("#mode-toggle .mode-btn")) {
    btn.addEventListener("click", () => {
      const mode = btn.dataset.mode;
      // Microgrids button returns the user to the list. Picking a
      // microgrid (D2 cards) re-enters the selected view.
      navigateTo({
        mode,
        selectedMg: mode === "microgrids" ? null : currentRoute().selectedMg,
      });
    });
  }
  for (const btn of document.querySelectorAll("#mg-subtoggle .mode-btn")) {
    btn.addEventListener("click", () => {
      const sv = btn.dataset.subview;
      if (!VALID_SUBVIEWS.has(sv)) return;
      navigateTo({ subview: sv });
    });
  }
  const backBtn = document.getElementById("mg-back");
  if (backBtn) backBtn.addEventListener("click", () => selectMicrogrid(null));
  applyInitialRoute();
  setupRouterPopstate();
  // Keyboard chord — 1 → Microgrids list, 2 → Scenarios. Skip
  // when a text input has focus so digits typed into the REPL /
  // search boxes don't trigger a mode flip.
  document.addEventListener("keydown", (ev) => {
    if (ev.ctrlKey || ev.metaKey || ev.altKey) return;
    const t = ev.target;
    const tag = t?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t?.isContentEditable)
      return;
    let mode = null;
    if (ev.key === "1") mode = "microgrids";
    else if (ev.key === "2") mode = "scenarios";
    if (!mode) return;
    ev.preventDefault();
    navigateTo({
      mode,
      selectedMg: mode === "microgrids" ? null : currentRoute().selectedMg,
    });
  });
}

export async function refreshTopology() {
  // Stamp the stash with the microgrid the fetch was for: applyMode's
  // catch-up must not paint one microgrid's snapshot into another's
  // formula canvas (the user can switch mg before this fetch lands).
  const mg = readSelectedMg();
  try {
    const res = await fetch(mgPath("topology"));
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const data = await res.json();
    // The user can switch microgrid while the fetch is in flight
    // (selectMicrogrid fires a fresh one). A response for a mg that
    // is no longer selected must be dropped whole: applying it would
    // repaint every panel — and clobber the stash — with the old
    // site's data.
    if (readSelectedMg() !== mg) return;
    topology.apply(data);
    // The Formulas canvas renders the same snapshot, but only while
    // its subview is showing — feeding a hidden (0×0) canvas would
    // pay a full layout for invisible pixels and measure junk node
    // sizes. The stash catches it up on subview enter (applyMode).
    lastTopologySnapshot = { mg, data };
    if (visibleSubview() === "formulas") {
      formulaCanvas().apply(data);
    }
    // Pulse bar's health counters + graph pill read from the
    // same /api/topology fetch — one round-trip carries both
    // signals + a hot-reload's WS topology_changed nudge
    // already drives a refresh.
    pulseBar.applyTopology(data.components || [], data.graph_status);
    batteryPairs.refresh(data);
    pvRows.refresh(data);
    evRows.refresh(data);
    chpRows.refresh(data);
    gridFrequency.applyTopology(data);
  } catch (err) {
    setStatus(`error: ${err.message}`, "error");
  }
}
