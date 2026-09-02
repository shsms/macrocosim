// Drag-to-resize handshake shared by the layout splitters. side-panel.js
// imports it for the dock strips.

/// Generic drag-to-resize handler for the layout splitters — the
/// bottom and right dock strips, and the splitters between the tiles
/// inside them: capture the
/// starting state on mousedown, compute a delta on mousemove,
/// hand it back to the caller as a clamped px value; the charts
/// inside re-size themselves to their containers (each panel
/// observes its own).
///
///   axis: "x" | "y"             which mouse coord to track
///   splitter: HTMLElement       drag handle
///   getStart(): number          current size we're modifying
///   apply(value: number): void  write the new size somewhere
///   clamp(value: number): number  clamp to a sensible range
///
/// Returns a dispose function that ends any drag in flight and
/// removes the document listeners.
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
  const onMove = (e) => {
    if (!dragging) return;
    const here = isHoriz ? e.clientY : e.clientX;
    const delta = start - here; // positive = drag toward the start
    // Ignore the few px of jitter a click (or the first half of a
    // double-click) produces — applying it would resize on what the
    // user meant as a click.
    if (!moved && Math.abs(delta) < 4) return;
    moved = true;
    apply(clamp(startSize + delta));
  };
  const onUp = () => {
    if (!dragging) return;
    dragging = false;
    splitter.classList.remove("dragging");
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
  };
  document.addEventListener("mousemove", onMove);
  document.addEventListener("mouseup", onUp);
  // A splitter that is rebuilt (the dock strips re-lay their tile
  // splitters on every change) has to take its document listeners
  // with it.
  return () => {
    // A splitter can be disposed under the pointer — a shortcut that
    // opens or closes a panel re-lays the strip mid-drag — and the
    // mouseup that would have ended the gesture goes to a listener
    // that is no longer there. End it here, or the body keeps the
    // drag cursor and the text selection lock for good.
    onUp();
    document.removeEventListener("mousemove", onMove);
    document.removeEventListener("mouseup", onUp);
  };
}
