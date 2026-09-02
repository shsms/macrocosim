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

/// Horizontal splitter between topology row and bottom drawer, and
/// the two pulse-bar chips that decide what the drawer shows. Both
/// are off until the user turns them on, so a fresh browser gives the
/// canvas the whole main area:
///   `repl` — the drawer itself (input row + eval output). Off, the
///            drawer and its splitter leave main's grid entirely.
///   `logs` — the log tail above the REPL. Off, the drawer is just
///            the input row plus the last result, and main's row for
///            it is `auto`; on, the drawer takes the dragged height.
/// The dragged height persists on its own; double-clicking the
/// splitter toggles `logs`, and dragging with logs off turns them on
/// at the dragged size.
export function setupDrawerSplitter() {
  const main = document.getElementById("app");
  const drawer = document.getElementById("repl");
  const splitter = document.getElementById("drawer-splitter");
  const HEIGHT_KEY = "switchyard-drawer-h";
  const LOGS_KEY = "switchyard-drawer-logs";
  const REPL_KEY = "switchyard-drawer-repl";
  const MIN_DRAWER = 120;
  const MIN_TOP_FRAC = 0.2; // keep at least 20% of main for the canvas
  const logsChip = document.getElementById("logs-toggle");
  const replChip = document.getElementById("repl-toggle");

  const shown = (key) => localStorage.getItem(key) === "1";
  const remember = (key, on) => localStorage.setItem(key, on ? "1" : "0");
  const savedHeight = () => {
    const h = Number(localStorage.getItem(HEIGHT_KEY));
    return Number.isFinite(h) && h >= MIN_DRAWER ? h : 260;
  };

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
  // One place derives the grid rows and body flags from the two
  // stored switches, so every toggle path lands on the same layout.
  const layout = () => {
    const repl = shown(REPL_KEY);
    const logs = shown(LOGS_KEY);
    document.body.classList.toggle("repl-hidden", !repl);
    document.body.classList.toggle("drawer-collapsed", !logs);
    if (!repl) main.style.gridTemplateRows = "auto 1fr 0 0";
    else if (!logs) main.style.gridTemplateRows = "auto 1fr 5px auto";
    else applyH(savedHeight());
    logsChip?.classList.toggle("active", logs);
    replChip?.classList.toggle("active", repl);
    // Lines appended while the tail was display:none could not
    // scroll it (its scrollHeight was 0), so it would reopen on its
    // oldest lines; pin it to the newest the moment it shows.
    if (repl && logs) {
      const logsEl = document.getElementById("logs");
      logsEl.scrollTop = logsEl.scrollHeight;
    }
    refitCharts();
  };
  const setLogs = (on) => {
    remember(LOGS_KEY, on);
    // Asking for the log tail is asking for the drawer it lives in.
    if (on) remember(REPL_KEY, true);
    layout();
  };
  const setRepl = (on) => {
    remember(REPL_KEY, on);
    // Hiding the drawer hides the tail with it, so the logs chip
    // never stays lit over nothing.
    if (!on) remember(LOGS_KEY, false);
    layout();
  };

  layout();
  logsChip?.addEventListener("click", () => setLogs(!shown(LOGS_KEY)));
  replChip?.addEventListener("click", () => setRepl(!shown(REPL_KEY)));

  splitter.title = "Drag to resize · double-click to toggle the log tail";
  splitter.addEventListener("dblclick", () => setLogs(!shown(LOGS_KEY)));

  makeSplitter({
    axis: "y",
    splitter,
    getStart: () => drawer.getBoundingClientRect().height,
    apply: (h) => {
      localStorage.setItem(HEIGHT_KEY, String(Math.round(h)));
      // Dragging a collapsed drawer expands it at the dragged size.
      if (!shown(LOGS_KEY)) setLogs(true);
      else applyH(h);
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
