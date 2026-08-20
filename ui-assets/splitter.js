// The drawer splitter: horizontal between the topology row and the
// bottom drawer. Goes through `makeSplitter` so the
// mousedown / mousemove / mouseup handshake stays in one place.

import { refitCharts } from "./inspect.js";

/// Generic drag-to-resize handler. The drawer splitter (between the
/// topology row and the bottom drawer) uses it: capture the
/// starting state on mousedown, compute a delta on mousemove,
/// hand it back to the caller as a clamped px value, refit any
/// open uPlot charts on every frame so they keep up with the
/// container width.
///
///   axis: "x" | "y"             which mouse coord to track
///   splitter: HTMLElement       drag handle
///   getStart(): number          current size we're modifying
///   apply(value: number): void  write the new size somewhere
///   clamp(value, viewportSize): clamp to a sensible range
function makeSplitter({ axis, splitter, getStart, apply, clamp }) {
  const isHoriz = axis === "y";
  const cursor = isHoriz ? "row-resize" : "col-resize";

  let dragging = false;
  let moved = false;
  let start = 0;
  let startSize = 0;

  splitter.addEventListener("mousedown", (e) => {
    dragging = true;
    moved = false;
    start = isHoriz ? e.clientY : e.clientX;
    startSize = getStart();
    splitter.classList.add("dragging");
    document.body.style.cursor = cursor;
    document.body.style.userSelect = "none";
    e.preventDefault();
  });
  document.addEventListener("mousemove", (e) => {
    if (!dragging) return;
    const here = isHoriz ? e.clientY : e.clientX;
    const delta = start - here; // positive = drag toward the start
    // Ignore the few px of jitter a click (or the first half of a
    // double-click) produces — applying it would resize, and for
    // the drawer splitter also expand-and-clobber the saved height
    // right before the dblclick toggle reads the collapsed state.
    if (!moved && Math.abs(delta) < 4) return;
    moved = true;
    const viewport = isHoriz ? window.innerHeight : window.innerWidth;
    apply(clamp(startSize + delta, viewport));
    refitCharts();
  });
  document.addEventListener("mouseup", () => {
    if (!dragging) return;
    dragging = false;
    splitter.classList.remove("dragging");
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
  });
}

/// Horizontal splitter between the Formulas canvas row and the Why
/// drawer. Updates #formulas' grid-template-rows to resize the
/// drawer; the height persists so the arrangement survives reloads.
export function setupFormulaDrawerSplitter() {
  const pane = document.getElementById("formulas");
  const drawer = document.getElementById("why-drawer");
  const KEY = "switchyard-why-drawer-h";
  const MIN_DRAWER = 100;
  const MIN_TOP_FRAC = 0.25;
  const applyH = (h) => {
    // CSS min() keeps a persisted height from swallowing the pane on
    // a smaller window than it was saved on: the drawer never takes
    // more than 75% of the pane, whatever localStorage says.
    pane.style.gridTemplateRows = `1fr 5px min(${h}px, 75%)`;
  };
  const saved = Number(localStorage.getItem(KEY));
  if (Number.isFinite(saved) && saved >= MIN_DRAWER) applyH(saved);
  makeSplitter({
    axis: "y",
    splitter: document.getElementById("formula-drawer-splitter"),
    getStart: () => drawer.getBoundingClientRect().height,
    apply: (h) => {
      applyH(h);
      localStorage.setItem(KEY, String(Math.round(h)));
    },
    clamp: (h, vh) => {
      const paneH = pane.getBoundingClientRect().height;
      void vh;
      return Math.max(MIN_DRAWER, Math.min(paneH * (1 - MIN_TOP_FRAC), h));
    },
  });
}

/// Horizontal splitter between topology row and bottom drawer.
/// Updates main's grid-template-rows to resize the drawer. The
/// height persists; double-clicking the splitter collapses the
/// drawer to just the REPL input row (logs + output hidden, also
/// persisted) so the content panes get the vertical space back.
export function setupDrawerSplitter() {
  const main = document.getElementById("app");
  const drawer = document.getElementById("repl");
  const splitter = document.getElementById("drawer-splitter");
  const HEIGHT_KEY = "switchyard-drawer-h";
  const COLLAPSED_KEY = "switchyard-drawer-collapsed";
  const MIN_DRAWER = 120;
  const MIN_TOP_FRAC = 0.2; // keep at least 20% of main for the canvas

  const applyH = (h) => {
    // Main's grid template has FOUR rows: the auto mgheader, the
    // 1fr topology row, the 5px drawer-splitter, the drawer.
    // An earlier shape rewrote only three values here, dropping
    // the mgheader's `auto` track — the grid then collapsed
    // and the canvas disappeared as soon as the user dragged the
    // splitter at all. Keep all four tracks. CSS min() keeps a
    // persisted height from swallowing the pane on a smaller
    // window than it was saved on.
    main.style.gridTemplateRows = `auto 1fr 5px min(${h}px, 75%)`;
  };
  const setCollapsed = (on) => {
    document.body.classList.toggle("drawer-collapsed", on);
    if (on) {
      // `auto` sizes the row to the repl form alone (logs + output
      // are display:none under the body class).
      main.style.gridTemplateRows = "auto 1fr 5px auto";
      localStorage.setItem(COLLAPSED_KEY, "1");
    } else {
      const saved = Number(localStorage.getItem(HEIGHT_KEY));
      applyH(Number.isFinite(saved) && saved >= MIN_DRAWER ? saved : 260);
      localStorage.removeItem(COLLAPSED_KEY);
    }
    refitCharts();
  };

  const savedH = Number(localStorage.getItem(HEIGHT_KEY));
  if (Number.isFinite(savedH) && savedH >= MIN_DRAWER) applyH(savedH);
  if (localStorage.getItem(COLLAPSED_KEY)) setCollapsed(true);

  splitter.title = "Drag to resize · double-click to collapse";
  splitter.addEventListener("dblclick", () => {
    setCollapsed(!document.body.classList.contains("drawer-collapsed"));
  });

  makeSplitter({
    axis: "y",
    splitter,
    getStart: () => drawer.getBoundingClientRect().height,
    apply: (h) => {
      // Dragging a collapsed drawer expands it at the dragged size.
      if (document.body.classList.contains("drawer-collapsed")) {
        document.body.classList.remove("drawer-collapsed");
        localStorage.removeItem(COLLAPSED_KEY);
      }
      applyH(h);
      localStorage.setItem(HEIGHT_KEY, String(Math.round(h)));
    },
    clamp: (h, vh) => {
      const mainH = main.getBoundingClientRect().height;
      // mainH excludes the header; we use it (not vh) for the upper
      // clamp so the canvas stays at MIN_TOP_FRAC of the drawer's
      // own container.
      void vh;
      return Math.max(MIN_DRAWER, Math.min(mainH * (1 - MIN_TOP_FRAC), h));
    },
  });
}

