// Drag-to-resize handshake shared by the layout splitters. side-panel.js
// imports it for the dock strips.

/// Generic drag-to-resize handler for the layout splitters (the dock
/// strips, spec Phase 2): capture the
/// starting state on mousedown, compute a delta on mousemove,
/// hand it back to the caller as a clamped px value; the charts
/// inside re-size themselves to their containers (each panel
/// observes its own).
///
///   axis: "x" | "y"             which mouse coord to track
///   splitter: HTMLElement       drag handle
///   getStart(): number          current size we're modifying
///   apply(value: number): void  write the new size somewhere
///   clamp(value, viewportSize): clamp to a sensible range
export function makeSplitter({ axis, splitter, getStart, apply, clamp }) {
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
    // double-click) produces — applying it would resize on what the
    // user meant as a click.
    if (!moved && Math.abs(delta) < 4) return;
    moved = true;
    const viewport = isHoriz ? window.innerHeight : window.innerWidth;
    apply(clamp(startSize + delta, viewport));
  });
  document.addEventListener("mouseup", () => {
    if (!dragging) return;
    dragging = false;
    splitter.classList.remove("dragging");
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
  });
}
