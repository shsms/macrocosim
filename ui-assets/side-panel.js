// The floating panel shell. Each named panel — the node inspector
// ("node"), the formula explorer ("formula-btn"), the metrics panel
// ("metrics-btn"), the REPL ("repl-btn"), the log tail ("logs-btn"),
// the Defaults editor, the live Scenario report — is
// its own concurrently-openable, draggable, resizable card floating
// over #panel-dock. Any card can also dock into the bottom or right
// strip (`#dock-bottom`, `#dock-right`) as a tile — same element,
// `docked` class, laid out by `layoutStrip` — and float back out. The
// cards are absolute floats, not a column: opening one never changes
// another's size.
// Re-opening an open panel just re-renders it (running its teardown
// first); closing runs
// teardown and hides the card. The shell never knows what's inside a
// panel; each tenant supplies its own teardown since only it knows
// what live resources (charts, timers) it owns.

import { makeSplitter } from "./splitter.js";
import { clampStripSize, mergeOrder, normalizedShares } from "./strip-model.js";

// name → { el, contentEl, teardown, pos, cascade, isStatic, refitTimer,
// dock, floatStyle, dragging }
// dock is the edge the card is docked to, or null while it floats;
// floatStyle parks the card's inline float geometry while it is
// docked.
// dragging is true for as long as a pointer holds the card's head —
// either drag, and across the hand-over between them. The card is
// following the pointer then, so nothing else may move it:
// sanitizePanel, the one thing that does, gates on this.
// pos carries `bottom`: which dock edge the stored dx/dy were
// measured against, so a reload re-anchors the card the way its saved
// offset expects (see .anchor-bottom, savePos).
const panels = new Map();
// Open panels, oldest first — Esc closes the newest.
const openStack = [];

const POS_KEY_PREFIX = "sw-panel-pos-";
const SIZE_KEY_PREFIX = "sw-panel-size-";
// Where a panel nobody has placed lands. The dock's top edge is level
// with the floating canvas controls, so that row is draggable-to but
// not spawned-on: BASE clears the controls strip, and STEP staggers
// panels summoned in a row so no card lands perfectly on another's
// grab strip.
const CASCADE_BASE = 40;
const CASCADE_STEP = 32;
// A capped panel must stay tall enough to grab and re-open: the drag
// strip plus a row of content.
const MIN_HEIGHT = 60;
// Of a clamped panel, how much must stay on screen: enough of the
// card's width to grab horizontally, and its strip vertically.
const KEEP_X = 80;
const KEEP_Y = 40;
// Quiet time after the last resize tick that counts as "the gesture
// ended". Converting the dragged height into a cap mid-drag would
// fight the browser's own resize, which keeps sizing from the height
// it started with.
const RESIZE_SETTLE = 280;
// Same idea for the window's own resize: a maximize or a drag of the
// window edge arrives as a burst of events, and re-fitting the open
// cards on each one would measure geometry that is still moving.
const REFIT_SETTLE = 180;

// Per-panel defaults: the card's width, and where an unplaced card
// spawns. Unlisted panels take the stylesheet's 420px and the
// top-right cascade.
const PANEL_DEFAULTS = {
  "metrics-btn": { width: 430 },
  "repl-btn": { width: 560, spawn: "bottom-left" },
  "logs-btn": { width: 720, spawn: "bottom-left" },
};
// Panels whose markup is static in index.html, so a module can keep
// addressing their elements by id: name → [card id, content id].
const STATIC_PANELS = {
  node: ["inspector", "inspect"],
  "repl-btn": ["repl", "repl-body"],
  "logs-btn": ["logs-panel", "logs-body"],
};
// A bottom-left card sits this far in from the dock's left and
// bottom edges; further bottom-left cards line up to its right with
// ROW_GAP between them.
const CORNER_INSET = 40;
const ROW_GAP = 8;

// A dock strip per edge. `size` is the strip's height (bottom) or
// width (right); `axis` is the direction that size runs in, so the
// tiles run along the other one and the tile splitters cut across
// it. Each strip persists its size, tile order and shares under
// `key`; each panel remembers its edge under DOCK_KEY_PREFIX.
const DOCK_KEY_PREFIX = "sw-panel-dock-";
const STRIPS = {
  bottom: {
    el: "dock-bottom",
    splitter: "dock-bottom-splitter",
    key: "sw-strip-bottom",
    bodyClass: "has-bottom-dock",
    axis: "y",
    size: 260,
    min: 120,
    maxFrac: 0.8,
    title: "Dock to the bottom",
  },
  right: {
    el: "dock-right",
    splitter: "dock-right-splitter",
    key: "sw-strip-right",
    bodyClass: "has-right-dock",
    axis: "x",
    size: 560,
    min: 320,
    maxFrac: 0.6,
    title: "Dock to the right",
  },
};
// A tile cannot be squeezed below this share of its strip.
const TILE_MIN_SHARE = 0.15;
// Drag-to-dock: the outer SNAP_ZONE px of the dock's bottom and right
// edges dock a dragged card on release. Drag-out: a tile head pulled
// DRAG_OUT px past its strip's inner edge floats the card. A zone
// only arms once the drag has travelled DRAG_ARM px, so a card
// already sitting flush against one cannot be docked by a nudge.
const SNAP_ZONE = 40;
const DRAG_OUT = 24;
const DRAG_ARM = 8;
const stripEl = (edge) => document.getElementById(STRIPS[edge].el);
// A strip's extent along `axis`, and main's: the bounds a size is
// clamped against.
const extentOf = (el, axis) => (axis === "y" ? el.getBoundingClientRect().height : el.getBoundingClientRect().width);
// The same pair the other way round: writing an extent along `axis`,
// and the axis the tiles (and so the tile splitters' drags) run
// along, which is the other one.
const setExtent = (el, axis, px) => {
  el.style[axis === "y" ? "height" : "width"] = px;
};
const crossAxis = (edge) => (STRIPS[edge].axis === "y" ? "x" : "y");

// Every card lives in the dock, and the dock's box is both the drag
// floor and the height bound.
const dockEl = () => document.getElementById("panel-dock");

function ensurePanel(name) {
  let p = panels.get(name);
  if (p) return p;
  const isStatic = name in STATIC_PANELS;
  let el;
  let contentEl;
  if (isStatic) {
    const [cardId, contentId] = STATIC_PANELS[name];
    el = document.getElementById(cardId);
    contentEl = document.getElementById(contentId);
    // The inspector wires its own close button (app.js, it also
    // deselects the node); every other static card uses the shared
    // one.
    if (name !== "node") {
      el.querySelector(".float-close").addEventListener("click", () => closePanel(name));
    }
  } else {
    el = document.createElement("aside");
    el.className = "float-panel";
    el.id = `panel-${name}`;
    // Same landmark label + close hint the static inspector carries.
    // The chrome-button names end in "-btn"; the label is the panel.
    el.setAttribute("aria-label", name.replace(/-btn$/, ""));
    el.innerHTML = `
      <div class="panel-drag" title="Drag to move"><span class="drag-grip"></span></div>
      <button class="float-close" type="button" title="Close (Esc)">×</button>
      <button class="float-dock" type="button" title="Dock…" aria-haspopup="menu">⤓</button>
      <div class="panel-content"></div>`;
    dockEl().appendChild(el);
    contentEl = el.querySelector(".panel-content");
    el.querySelector(".float-close").addEventListener("click", () => closePanel(name));
  }
  // The tile head shows the card's landmark label as its title: the
  // grab strip's own ::before renders it, and attr() only reads the
  // attributes of the element it is on.
  el.querySelector(".panel-drag").dataset.title = el.getAttribute("aria-label") ?? name;
  // The strip reads a tile's panel name off the card itself.
  el.dataset.panelName = name;
  const dockBtn = el.querySelector(".float-dock");
  // The menu's own outside-press closer sits on the document; a press
  // on this button must not reach it, or the toggle below would only
  // ever see a closed menu and re-open it.
  dockBtn.addEventListener("pointerdown", (e) => e.stopPropagation());
  dockBtn.addEventListener("click", (e) => {
    const rec = panels.get(name);
    // A tile's button floats it with no menu of its own, but an
    // unrelated menu may be open: the pointerdown above keeps the
    // document's outside-press closer from seeing this press, so
    // dismiss it here.
    if (rec?.dock) {
      closeDockMenu();
      floatPanel(name);
    } else if (openMenu?.dataset.panel === name) closeDockMenu();
    else dockMenu(name, e.currentTarget);
  });
  const width = PANEL_DEFAULTS[name]?.width;
  if (width) el.style.width = `${width}px`;
  const stored = loadPos(name);
  // Which dock edge the card hangs from. A stored offset was measured
  // against one particular edge, so it decides; only an unplaced card
  // takes the anchor its spawn implies. Getting this backwards after a
  // reload would re-anchor a card whose saved dx/dy mean the other
  // edge, and throw it across the dock.
  const bottomAnchored = stored ? stored.bottom : PANEL_DEFAULTS[name]?.spawn === "bottom-left";
  el.classList.toggle("anchor-bottom", bottomAnchored);
  // Without a stored position the panel is still unplaced, so its
  // first open cascades off whatever is already open.
  p = {
    el,
    contentEl,
    teardown: null,
    pos: stored ?? { dx: 0, dy: 0, bottom: bottomAnchored },
    cascade: !stored,
    isStatic,
    refitTimer: 0,
    dock: null,
    floatStyle: "",
    dragging: false,
  };
  // A bottom-left card hangs from the dock's bottom edge (see
  // .anchor-bottom), so content growing or shrinking — a log tail
  // filling, a cleared one — moves its top and keeps its bottom. That
  // makes content the one thing that can push the card's grab strip
  // up out of the dock without anyone touching it, and the window
  // listener below only fires for the window. So watch the card's own
  // box too, and re-fit it once the growth settles. Wired for every
  // card that can land on that edge, since one may also be sitting in
  // the top-right cascade for now (placePanel).
  if (PANEL_DEFAULTS[name]?.spawn === "bottom-left") {
    new ResizeObserver(() => {
      // A hidden card measures as nothing, and an inline height is
      // wireResize's in-gesture signal — the user is holding the
      // gripper and a re-fit would fight the drag. p.dragging is the
      // same story for the head: a drag-out re-parents the card, which
      // resizes it, and the re-fit would land REFIT_SETTLE into a
      // gesture that is still holding the card. Checked again when the
      // timer fires, not just here: a card closed or grabbed inside
      // REFIT_SETTLE would otherwise be sanitized as a display:none
      // zero box, and clampOffset would write that nonsense back into
      // p.pos. closePanel cancels it too.
      if (p.dragging || p.dock || !el.classList.contains("open") || el.style.height) return;
      clearTimeout(p.refitTimer);
      p.refitTimer = setTimeout(() => {
        if (p.dragging || p.dock || !el.classList.contains("open") || el.style.height) return;
        sanitizePanel(p, name, false);
      }, REFIT_SETTLE);
    }).observe(el);
  }
  wireDrag(el, name, p);
  wireResize(el, name);
  panels.set(name, p);
  return p;
}

// The card's untransformed viewport box — the frame both clamps and
// the drag offset are expressed against.
function anchorOf(el, pos) {
  const rect = el.getBoundingClientRect();
  return { left: rect.left - pos.dx, top: rect.top - pos.dy, width: rect.width };
}

// Keep a drag offset reachable: the grab strip may not slide up under
// the chrome, nor off the other three edges. The floor is the dock's
// own top edge, measured live — the dock starts below every piece of
// chrome (header, pulse bar, microgrid header) by construction, so
// there is nothing left to measure or hardcode separately. That edge
// is level with the canvas controls row, so a card can be dragged up
// beside (and over) the controls, which is as high as it goes.
function clampOffset(anchor, dx, dy) {
  const floor = dockEl()?.getBoundingClientRect().top ?? 0;
  const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
  return {
    dx: clamp(dx, -(anchor.left + anchor.width - KEEP_X), window.innerWidth - anchor.left - KEEP_X),
    dy: clamp(dy, floor - anchor.top, window.innerHeight - anchor.top - KEEP_Y),
  };
}

// The clamp a card gets when nobody is holding it. A drag may leave a
// card hanging off an edge on purpose — the pointer is on the strip,
// so the user can always pull it back, and KEEP_X is all that has to
// survive. A card the geometry moved under has nobody holding it and
// no way back, so wherever the card still fits across the window, put
// the whole of it on screen rather than settle for that sliver. It is
// the same story vertically, but clampOffset's floor and KEEP_Y
// already leave the whole strip in reach there.
function fitOffset(anchor, dx, dy) {
  const fit = clampOffset(anchor, dx, dy);
  if (anchor.width > window.innerWidth) return fit;
  const over = anchor.left + fit.dx + anchor.width - window.innerWidth;
  if (over > 0) fit.dx -= over;
  if (anchor.left + fit.dx < 0) fit.dx = -anchor.left;
  return fit;
}

function applyPos(el, pos) {
  el.style.transform = `translate(${pos.dx}px, ${pos.dy}px)`;
}

// Drag-to-move via the grab strip; the offset is a transform on the
// panel, persisted per panel so it sticks across sessions. A docked
// card's head drags the tile along its strip instead, until it is
// pulled out.
function wireDrag(el, name, p) {
  const strip = el.querySelector(".panel-drag");
  strip.addEventListener("pointerdown", (e) => {
    e.preventDefault();
    strip.setPointerCapture(e.pointerId);
    if (p.dock) reorderDrag(el, strip, p.dock, name, p, e.pointerId);
    else beginFloatDrag(el, strip, name, p, e);
  });
}

// The float drag proper, from wherever the pointer already is. The
// anchor is measured once per gesture: re-reading it per pointermove
// would force a layout on every frame. A bottom-anchored card whose
// content changes mid-drag does move its untransformed top under
// this anchor; its resize observer re-fits it once that growth
// settles, which may well be mid-drag — harmless, since that re-fit
// persists nothing and the next pointermove writes the offset from
// this anchor again. Over a snap zone the zone arms, and release
// there docks the card instead of leaving it.
//
// `suppressEdge` is the edge a tile was just dragged out of: the
// hand-over lands the pointer inside that edge's own snap zone (the
// strip's inner edge is only DRAG_OUT px from it), so arming there
// straight away would re-dock the card the gesture just freed. The
// zone is dead until the pointer has been somewhere else once.
function beginFloatDrag(el, strip, name, p, e, suppressEdge = null) {
  p.dragging = true;
  const startX = e.clientX - p.pos.dx;
  const startY = e.clientY - p.pos.dy;
  const anchor = anchorOf(el, p.pos);
  // Where the card floated before the gesture. A drag that ends in a
  // zone docks the card, and docking is not a placement: the drop
  // point is a point on the dock's rim, so persisting it would send a
  // later ⤒ back to the rim with the card mostly off screen.
  const before = { ...p.pos };
  let zone = null;
  let suppressed = suppressEdge;
  // The zone under the pointer, once the drag has travelled far
  // enough to be a drag at all — and once the pointer has left the
  // suppressed edge, if there is one.
  const zoneAt = (ev) => {
    if (Math.hypot(ev.clientX - e.clientX, ev.clientY - e.clientY) < DRAG_ARM) return null;
    const at = snapZoneAt(ev.clientX, ev.clientY);
    if (!suppressed) return at;
    if (at === suppressed) return null;
    suppressed = null;
    return at;
  };
  const move = (ev) => {
    p.pos = { ...clampOffset(anchor, ev.clientX - startX, ev.clientY - startY), bottom: p.pos.bottom };
    applyPos(el, p.pos);
    zone = zoneAt(ev);
    armSnapZone(zone);
  };
  // The release point decides, not the last move the card followed: a
  // pointerup can land somewhere no pointermove reported.
  const up = (ev) => {
    zone = zoneAt(ev);
    stop();
  };
  // A cancelled gesture is not a drop, so it docks nothing.
  const cancel = () => {
    zone = null;
    stop();
  };
  const stop = () => {
    p.dragging = false;
    strip.removeEventListener("pointermove", move);
    strip.removeEventListener("pointerup", up);
    strip.removeEventListener("pointercancel", cancel);
    armSnapZone(null);
    if (zone) {
      p.pos = before;
      applyPos(el, p.pos);
      savePos(name, p.pos);
      // That position is the card's placement now, so a float back
      // out of the strip goes there rather than cascading.
      p.cascade = false;
      dockPanel(name, zone);
      return;
    }
    p.cascade = false;
    savePos(name, p.pos);
  };
  strip.addEventListener("pointermove", move);
  strip.addEventListener("pointerup", up);
  strip.addEventListener("pointercancel", cancel);
}

// Which snap zone the pointer is in, if any: the dock's bottom edge
// wins over its right one in the corner.
function snapZoneAt(x, y) {
  const d = dockEl().getBoundingClientRect();
  if (x < d.left || x > d.right || y < d.top || y > d.bottom) return null;
  if (y > d.bottom - SNAP_ZONE) return "bottom";
  if (x > d.right - SNAP_ZONE) return "right";
  return null;
}
function armSnapZone(edge) {
  for (const z of dockEl().querySelectorAll(".snap-zone")) {
    z.classList.toggle("armed", z.dataset.edge === edge);
  }
}

// Dragging a tile's head along the strip moves the tile: it takes
// the slot whose midpoint the pointer has crossed, the strip re-lays
// itself out on release, and the new order is stored. Pulled past
// the strip's inner edge, the card floats under the pointer and the
// same gesture carries on as a float drag.
function reorderDrag(el, head, edge, name, p, pointerId) {
  p.dragging = true;
  const along = crossAxis(edge);
  el.classList.add("reordering");
  const move = (ev) => {
    const r = stripEl(edge).getBoundingClientRect();
    // The strip's inner edge is the one the canvas is on: the top of
    // a bottom strip (tiles run along x), the left of a right one.
    const out = along === "x" ? ev.clientY < r.top - DRAG_OUT : ev.clientX < r.left - DRAG_OUT;
    if (out) {
      stop();
      floatPanel(name);
      // Moving the card back to the float dock re-parents the head
      // that captured the pointer, and Chromium releases the capture
      // on that move. Re-take it, or the handed-over float drag never
      // sees another pointermove and the card is stranded where the
      // gesture left the strip.
      try {
        head.setPointerCapture(pointerId);
      } catch (_) {
        // The pointer is already gone — the drag ends at the float.
      }
      // Under the pointer: the head's middle at the pointer's x, its
      // vertical middle at the pointer's y.
      const a = anchorOf(el, p.pos);
      p.pos = {
        ...clampOffset(a, ev.clientX - a.left - el.offsetWidth / 2, ev.clientY - a.top - head.offsetHeight / 2),
        bottom: p.pos.bottom,
      };
      applyPos(el, p.pos);
      p.cascade = false;
      savePos(name, p.pos);
      beginFloatDrag(el, head, name, p, ev, edge);
      return;
    }
    const others = openTiles(edge).filter((t) => t !== el);
    const here = along === "x" ? ev.clientX : ev.clientY;
    const idx = others.filter((t) => {
      const b = t.getBoundingClientRect();
      return here > (along === "x" ? (b.left + b.right) / 2 : (b.top + b.bottom) / 2);
    }).length;
    const current = openTiles(edge).indexOf(el);
    if (idx === current) return;
    stripEl(edge).insertBefore(el, others[idx] ?? null);
    // Moving the tile re-parents the head that captured the pointer,
    // which can release the capture. Without it the gesture's own
    // pointerup may never reach the head: `stop` would never run, the
    // tile would keep the .reordering class, and the still-attached
    // pointermove would go on re-ordering the strip under a plain
    // hover. Re-take it every time the tile moves.
    try {
      head.setPointerCapture(pointerId);
    } catch (_) {
      // Still held, or the pointer is already gone — nothing to do.
    }
  };
  const stop = () => {
    // Dropped for the hand-over too, which raises it again the moment
    // beginFloatDrag takes the gesture over.
    p.dragging = false;
    head.removeEventListener("pointermove", move);
    head.removeEventListener("pointerup", stop);
    head.removeEventListener("pointercancel", stop);
    el.classList.remove("reordering");
    saveOrderFromDom(edge);
    layoutStrip(edge);
  };
  head.addEventListener("pointermove", move);
  head.addEventListener("pointerup", stop);
  head.addEventListener("pointercancel", stop);
}

// What the gripper stores is a CAP, not a height: the card's height
// stays `auto`, so folding a card open or shut re-sizes the panel by
// itself, and the user's drag only says how far that may go. Dragging
// past the content therefore leaves no dead space — the card snaps
// back to its content once the gesture ends.
const capOf = (el, h) =>
  Math.max(MIN_HEIGHT, Math.min(h, el.parentElement?.clientHeight ?? window.innerHeight));

// `min(cap, 100% - CASCADE_BASE)` rather than the bare cap: the
// dock's own bound has to keep winning as the window resizes, and an
// inline max-height would otherwise override
// the stylesheet's — including the stylesheet's own 40px shave (kept
// in sync with CASCADE_BASE here), which a bare `100%` would silently
// undo for every capped panel.
function applyCap(el, cap) {
  el.style.maxHeight = `min(${cap}px, calc(100% - ${CASCADE_BASE}px))`;
}

// CSS `resize` gives the height affordance; there is no resize event
// for it, so the observer is how we notice. An inline height is the
// one signal that a height is the user's doing and not the content's
// — content growth leaves the style empty, and the conversion below
// clears it again. It watches the style attribute, not the box: with
// a cap in force a drag downward writes a taller inline height while
// max-height keeps the box pinned, so a size observer never fires,
// the cap never clears, and the drag looks dead in that direction.
function wireResize(el, name) {
  let timer = 0;
  const settle = () => {
    const h = Number.parseFloat(el.style.height);
    el.style.height = "";
    if (!h) return;
    const cap = capOf(el, h);
    applyCap(el, cap);
    saveSize(name, cap);
  };
  new MutationObserver(() => {
    if (!el.style.height) return;
    // Let the card follow the pointer while the gesture runs: the old
    // cap would otherwise pin it and the drag would look dead.
    el.style.maxHeight = "";
    clearTimeout(timer);
    timer = setTimeout(settle, RESIZE_SETTLE);
  }).observe(el, { attributes: true, attributeFilter: ["style"] });
}

function loadPos(name) {
  try {
    const raw = JSON.parse(localStorage.getItem(POS_KEY_PREFIX + name));
    if (!raw || !Number.isFinite(raw.dx) || !Number.isFinite(raw.dy)) return null;
    // `bottom` post-dates the other two: an offset stored before it
    // existed was measured against the top edge.
    return { dx: raw.dx, dy: raw.dy, bottom: raw.bottom === true };
  } catch (_) {
    return null;
  }
}
function savePos(name, pos) {
  try {
    localStorage.setItem(
      POS_KEY_PREFIX + name,
      JSON.stringify({ dx: pos.dx, dy: pos.dy, bottom: pos.bottom === true }),
    );
  } catch (_) {
    // Storage unavailable — the position just doesn't stick.
  }
}
function loadSize(name) {
  try {
    const raw = JSON.parse(localStorage.getItem(SIZE_KEY_PREFIX + name));
    return raw && Number.isFinite(raw.h) ? raw.h : null;
  } catch (_) {
    return null;
  }
}
function saveSize(name, h) {
  try {
    localStorage.setItem(SIZE_KEY_PREFIX + name, JSON.stringify({ h }));
  } catch (_) {
    // Storage unavailable — the cap just doesn't stick.
  }
}

// Fit a visible card to the geometry it currently finds itself in: a
// stored cap re-measured against the dock, and the offset clamped so
// the grab strip stays reachable. Anything that would leave the strip
// out of reach (a drag off-screen, a cap from a bigger window) is
// pulled back. The card has to be visible to be measured, so every
// caller runs this on an open panel.
//
// `persist` says whether the correction is the user's new intent or
// just this window's arithmetic. An open is their own toggle, so what
// gets clamped there is their new cap and placement, written through.
// A correction the window forced is transient: the live `p.pos` and
// the applied cap follow the new geometry either way — that is what
// keeps the card reachable — but storage keeps saying what the user
// last chose, so widening the window gives the height and the
// placement back instead of ratcheting them away. Storage going stale
// against a much-changed window costs nothing: the next open sanitizes
// it again, with `persist` on.
function sanitizePanel(p, name, persist = true) {
  // A tile's geometry is the strip's business, and a card the pointer
  // is holding is the gesture's: it is already following the pointer,
  // and every caller here would move it out from under one. This is
  // the single gate — refitFloating reaches a held card from three
  // sides (a strip appearing or vanishing, the window-resize settle,
  // the strip splitter's settle), not just off the card's own
  // observer.
  if (p.dock || p.dragging) return;
  const { el } = p;
  const stored = loadSize(name);
  if (stored != null) {
    const cap = capOf(el, stored);
    applyCap(el, cap);
    if (persist && cap !== stored) saveSize(name, cap);
  }
  applyPos(el, p.pos);
  const clamped = fitOffset(anchorOf(el, p.pos), p.pos.dx, p.pos.dy);
  if (clamped.dx === p.pos.dx && clamped.dy === p.pos.dy) return;
  p.pos = { ...clamped, bottom: p.pos.bottom };
  applyPos(el, p.pos);
  // Only a position the user chose is worth correcting on disk; a
  // clamped cascade is this open's arithmetic, not their placement.
  if (persist && !p.cascade) savePos(name, p.pos);
}

// Place the card as it opens: an unplaced one cascades off the panels
// already open — top-right by default, or in a row along the dock's
// bottom edge for the panels PANEL_DEFAULTS puts there — then the
// same sanitize every open card gets.
function placePanel(p, name, order) {
  if (p.cascade) {
    const bottomLeft = PANEL_DEFAULTS[name]?.spawn === "bottom-left";
    const spot = bottomLeft ? bottomLeftSpawn(p.el, name) : null;
    // A bottom-left card too wide for what is left of the row has
    // nowhere down there to go — clamping it back would just drop it
    // on a neighbour — so it joins the top-right cascade instead, and
    // is top-anchored for as long as it sits there. A later re-open
    // asks again, against whatever is open then.
    p.pos = spot
      ? { ...spot, bottom: true }
      : { dx: 0, dy: CASCADE_BASE + CASCADE_STEP * order, bottom: false };
    // The offset is saved with the edge it was measured against, so
    // the class has to follow it here and on the next ensurePanel.
    p.el.classList.toggle("anchor-bottom", p.pos.bottom);
  }
  sanitizePanel(p, name);
}

// The offset that puts a bottom-anchored card CORNER_INSET up from the
// dock's bottom edge and CORNER_INSET in from its left, to the right
// of every bottom-left card already open: they sit in a row along the
// bottom. Cards are anchored top-right by default (see .float-panel),
// so dx is negative; a bottom-anchored card's dy lifts it off the
// bottom edge. Occupancy, not a running sum of the other cards'
// widths: closing the leftmost card frees its slot while the ones
// still open keep theirs, so the row has to be read from where those
// cards actually sit. Returns null when the card fits nowhere in the
// row — see placePanel, which cascades it top-right instead.
function bottomLeftSpawn(el, name) {
  const dock = dockEl();
  const dockLeft = dock.getBoundingClientRect().left;
  const taken = [];
  for (const other of openStack) {
    // A docked card is a tile in the strip, not an occupant of this
    // row — its full-width rect would otherwise crowd the row out.
    if (other === name || PANEL_DEFAULTS[other]?.spawn !== "bottom-left" || panels.get(other).dock)
      continue;
    const r = panels.get(other).el.getBoundingClientRect();
    taken.push({ left: r.left - dockLeft, right: r.right - dockLeft });
  }
  const width = el.offsetWidth;
  // Walk right past every card the slot collides with. Each move can
  // bring a card that was clear back into range, so re-check them all
  // until a pass moves nothing; x only ever grows past one more card,
  // so the walk ends.
  let x = CORNER_INSET;
  for (let moved = true; moved; ) {
    moved = false;
    for (const t of taken) {
      if (x < t.right + ROW_GAP && x + width + ROW_GAP > t.left) {
        x = t.right + ROW_GAP;
        moved = true;
      }
    }
  }
  if (x + width > dock.clientWidth) return null;
  return { dx: -(dock.clientWidth - width - x), dy: -CORNER_INSET };
}

// Dock `name` into the `edge` strip as a tile: the card leaves the
// float dock for the strip, parks its inline float geometry for the
// trip back, and the strip lays itself out again. A card already in
// the other strip goes through the float state on the way.
function dockPanel(name, edge = "bottom") {
  const p = panels.get(name);
  if (!p || p.dock === edge) return;
  if (p.dock) floatPanel(name);
  ensureStripSplitter(edge);
  p.floatStyle = p.el.style.cssText;
  p.el.style.cssText = "";
  p.dock = edge;
  p.el.classList.add("docked");
  const dockBtn = p.el.querySelector(".float-dock");
  dockBtn.title = "Float";
  dockBtn.textContent = "⤒";
  // Into its slot in the strip's stored order when it has one (a
  // closed tile keeps it); a panel not in that order yet goes on the
  // end.
  const order = stripOrder(edge);
  const idx = order.indexOf(name);
  const strip = stripEl(edge);
  const siblings = [...strip.querySelectorAll(".float-panel")];
  const before = idx < 0 ? null : (siblings.find((s) => order.indexOf(tileName(s)) > idx) ?? null);
  strip.insertBefore(p.el, before);
  if (idx < 0) saveStrip(edge, { order: [...order, name] });
  saveDock(name, { mode: edge });
  layoutStrip(edge);
}

// Float `name` back out of its strip: back into the float dock with
// the geometry it had, then placed (never placed before) or
// sanitized (it is visible again, so it can be measured).
function floatPanel(name) {
  const p = panels.get(name);
  if (!p?.dock) return;
  const edge = p.dock;
  p.dock = null;
  p.el.classList.remove("docked");
  const dockBtn = p.el.querySelector(".float-dock");
  dockBtn.title = "Dock…";
  dockBtn.textContent = "⤓";
  dockEl().appendChild(p.el);
  p.el.style.cssText = p.floatStyle;
  p.floatStyle = "";
  saveDock(name, { mode: "float" });
  layoutStrip(edge);
  // A card auto-docked from storage has never been placed as a float,
  // so it takes the usual cascade rather than the dock's bare corner.
  if (!openStack.includes(name)) return;
  if (p.cascade) placePanel(p, name, openStack.length - 1);
  else sanitizePanel(p, name);
}

// Lay a strip out from what it holds: shown, at its stored size,
// while any open tile is in it; hidden and sizeless otherwise. The
// open tiles take the strip in the stored order with their stored
// shares (flex-grow, so they add up to the strip whatever it is),
// and a splitter sits between each pair.
function layoutStrip(edge) {
  const cfg = STRIPS[edge];
  const strip = stripEl(edge);
  for (const s of strip.querySelectorAll(".tile-splitter")) {
    s.dispose?.();
    s.remove();
  }
  const tiles = openTiles(edge);
  const was = document.body.classList.contains(cfg.bodyClass);
  document.body.classList.toggle(cfg.bodyClass, tiles.length > 0);
  setExtent(strip, cfg.axis, tiles.length ? `${stripSize(edge)}px` : "");
  const shares = normalizedShares(storedShares(edge), tiles.map(tileName));
  tiles.forEach((tile, i) => {
    tile.style.flex = `${shares[tileName(tile)]} 1 0`;
    if (i > 0) strip.insertBefore(makeTileSplitter(edge, tiles[i - 1], tile), tile);
  });
  // A strip appearing or vanishing resizes the float dock beside it,
  // which is the box every floating card is clamped against — the
  // same thing a window resize does to them.
  if (was !== (tiles.length > 0)) refitFloating();
}

// Open tiles in strip order (DOM order is the order of record;
// dockPanel and the reorder drag keep the stored order in step).
const openTiles = (edge) => [...stripEl(edge).querySelectorAll(".float-panel.open")];
const tileName = (el) => el.dataset.panelName;

// The bar between two tiles: dragging it trades space between them,
// along the strip — width in the bottom strip, height in the right.
function makeTileSplitter(edge, first, second) {
  const along = crossAxis(edge);
  const sp = document.createElement("div");
  sp.className = "tile-splitter";
  sp.title = "Drag to resize the tiles";
  // makeSplitter sizes the element after the handle: `second`. Sizes
  // are read live and written back as shares of the pair, so a
  // window resize keeps the proportion.
  sp.dispose = makeSplitter({
    axis: along,
    splitter: sp,
    getStart: () => extentOf(second, along),
    apply: (v) => {
      const a = tileName(first);
      const b = tileName(second);
      const shares = normalizedShares(storedShares(edge), openTiles(edge).map(tileName));
      const pair = shares[a] + shares[b];
      const pairPx = extentOf(first, along) + extentOf(second, along);
      shares[b] = pair * (v / pairPx);
      shares[a] = pair - shares[b];
      first.style.flex = `${shares[a]} 1 0`;
      second.style.flex = `${shares[b]} 1 0`;
      // Merged over the stored map: a floated-out tile keeps its share.
      saveStrip(edge, { shares: { ...storedShares(edge), ...shares } });
    },
    clamp: (v) => {
      const pairPx = extentOf(first, along) + extentOf(second, along);
      const min = extentOf(stripEl(edge), along) * TILE_MIN_SHARE;
      return Math.max(min, Math.min(pairPx - min, v));
    },
  });
  return sp;
}

// A strip's stored order, as panel names.
function stripOrder(edge) {
  const order = loadStrip(edge)?.order;
  return Array.isArray(order) ? order.filter((n) => typeof n === "string") : [];
}
// Every card in the strip, open or not, so a closed tile keeps its
// slot; mergeOrder keeps the names stored but no longer in the strip.
function saveOrderFromDom(edge) {
  const present = [...stripEl(edge).querySelectorAll(".float-panel")].map(tileName);
  saveStrip(edge, { order: mergeOrder(present, stripOrder(edge)) });
}

// A strip size held inside the strip's own bounds, measured against
// main as it is now (see clampStripSize) — what the stored size is
// read through, and what the splitter drag is clamped by.
const clampStrip = (edge, v) => {
  const cfg = STRIPS[edge];
  const bounds = { min: cfg.min, maxFrac: cfg.maxFrac, fallback: cfg.size };
  return clampStripSize(v, bounds, extentOf(document.getElementById("app"), cfg.axis));
};
// The stored size, so clamped.
const stripSize = (edge) => clampStrip(edge, loadStrip(edge)?.size);

// A strip's splitter is wired once, on the first dock into it. The
// drag re-fits the floating cards once it settles, the same way
// layoutStrip does when a strip appears or vanishes: the strip is
// taking its space from the float dock, which is the box every card
// is clamped against. One timer for both strips — only one splitter
// can be under the pointer.
let stripRefitTimer = 0;
const stripsWired = new Set();
function ensureStripSplitter(edge) {
  if (stripsWired.has(edge)) return;
  stripsWired.add(edge);
  const cfg = STRIPS[edge];
  makeSplitter({
    axis: cfg.axis,
    splitter: document.getElementById(cfg.splitter),
    getStart: () => extentOf(stripEl(edge), cfg.axis),
    apply: (v) => {
      setExtent(stripEl(edge), cfg.axis, `${v}px`);
      saveStrip(edge, { size: Math.round(v) });
      clearTimeout(stripRefitTimer);
      stripRefitTimer = setTimeout(refitFloating, REFIT_SETTLE);
    },
    clamp: (v) => clampStrip(edge, v),
  });
}

function loadStrip(edge) {
  try {
    const raw = JSON.parse(localStorage.getItem(STRIPS[edge].key));
    return raw && typeof raw === "object" ? raw : null;
  } catch (_) {
    return null;
  }
}
function saveStrip(edge, patch) {
  try {
    localStorage.setItem(STRIPS[edge].key, JSON.stringify({ ...(loadStrip(edge) ?? {}), ...patch }));
  } catch (_) {
    // Storage unavailable — the strip just doesn't stick.
  }
}
// A strip's stored shares as the model wants them: a plain map, empty
// when the strip has none.
const storedShares = (edge) => loadStrip(edge)?.shares ?? {};

function loadDock(name) {
  try {
    const raw = JSON.parse(localStorage.getItem(DOCK_KEY_PREFIX + name));
    // Own properties only: `"toString" in STRIPS` is true, and docking
    // to that edge would throw the moment a strip config was read.
    return Object.hasOwn(STRIPS, raw?.mode) ? raw : null;
  } catch (_) {
    return null;
  }
}
function saveDock(name, v) {
  try {
    localStorage.setItem(DOCK_KEY_PREFIX + name, JSON.stringify(v));
  } catch (_) {
    // Storage unavailable — the dock mode just doesn't stick.
  }
}

// The menu a floating card's dock button opens: one entry per
// strip. Anything pressed outside it closes it, as does Escape; a
// press inside stays inside so the entry's click still fires.
let openMenu = null;
// The button the open menu belongs to: the menu takes focus when it
// opens, so something has to hand it back.
let openMenuBtn = null;
function closeDockMenu() {
  // Cleared BEFORE the node goes: removing it blurs whichever entry
  // had focus, and that focusout closer calls back in here. Re-entering
  // with openMenu still set would try to remove the same node twice,
  // and the second remove lands mid-removal and throws.
  const menu = openMenu;
  const btn = openMenuBtn;
  openMenu = null;
  openMenuBtn = null;
  document.removeEventListener("pointerdown", closeDockMenu);
  document.removeEventListener("keydown", onMenuKey, true);
  // Focus goes back where the menu took it from, on every close path —
  // removing the node while an entry holds focus would drop the user
  // on <body> instead. Before the removal on purpose: the focusout
  // that fires carries the button as its relatedTarget, which the
  // closer below already exempts. Only when the menu still holds
  // focus (a click elsewhere has already moved it on), and only to a
  // button still in the document (its card may have closed).
  if (menu?.contains(document.activeElement) && btn?.isConnected) btn.focus();
  menu?.remove();
}
// Escape belongs to the menu while it is up, not to the panel behind
// it. app.js's Esc handler is a document listener installed at load,
// so it would run first on the way up; this one captures instead and
// stops the event before the bubble phase ever reaches it.
function onMenuKey(e) {
  if (e.key !== "Escape") return;
  e.stopPropagation();
  closeDockMenu();
}
function dockMenu(name, btn) {
  closeDockMenu();
  const menu = document.createElement("div");
  menu.className = "dock-menu";
  menu.dataset.panel = name;
  menu.setAttribute("role", "menu");
  menu.addEventListener("pointerdown", (e) => e.stopPropagation());
  // Focus leaving the menu closes it: the button is keyboard-openable,
  // so Tab has to be a way back out. The button itself is the one
  // exception — it takes focus on the press that is about to run its
  // own toggle, and closing here would leave that toggle re-opening a
  // menu the user meant to dismiss.
  menu.addEventListener("focusout", (e) => {
    if (!menu.contains(e.relatedTarget) && e.relatedTarget !== btn) closeDockMenu();
  });
  for (const edge of Object.keys(STRIPS)) {
    const item = document.createElement("button");
    item.type = "button";
    item.className = "dock-menu-item";
    item.setAttribute("role", "menuitem");
    item.textContent = STRIPS[edge].title;
    item.addEventListener("click", () => {
      closeDockMenu();
      dockPanel(name, edge);
    });
    menu.appendChild(item);
  }
  const r = btn.getBoundingClientRect();
  document.body.appendChild(menu);
  // Placed at the button, then held inside the window: a top-right
  // card's button sits a menu's width from the right edge, and the
  // menu would hang off it. Measured after the append — an unattached
  // menu has no offsetWidth to clamp against.
  menu.style.left = `${Math.max(0, Math.min(r.left, window.innerWidth - menu.offsetWidth - 4))}px`;
  menu.style.top = `${Math.max(0, Math.min(r.bottom + 4, window.innerHeight - menu.offsetHeight - 4))}px`;
  // Opened from the keyboard the menu has to be usable from it, so the
  // first entry takes focus; closeDockMenu hands it back to the button
  // however the menu goes away.
  menu.querySelector(".dock-menu-item")?.focus();
  openMenu = menu;
  openMenuBtn = btn;
  // The press that opened this menu is already past, so both closers
  // can go on now: neither can see it.
  document.addEventListener("pointerdown", closeDockMenu);
  document.addEventListener("keydown", onMenuKey, true);
}

// Fit every open floating card to the dock as it is now — a
// transient fit, not the user's placement, so nothing is persisted.
// A docked tile is the strip's business and sanitizePanel skips it.
function refitFloating() {
  for (const name of openStack) {
    const p = panels.get(name);
    if (p) sanitizePanel(p, name, false);
  }
}

// Sanitizing at open time only sees the geometry of that moment. The
// window moves it afterwards: a narrower viewport takes the card's
// edge off screen, wrapping chrome pushes the dock's top edge down,
// and a shorter window shrinks the height the cap is measured against.
// Nothing re-clamped for that, so an open card could be left stranded
// — its strip past the window edge, unreachable and unrecoverable
// short of closing the panel. Re-fit every open card once the gesture
// settles, and the user gets their own placement back when the window
// comes back. Installed once, at module scope: the listener outlives
// any one panel, and does nothing while none are open.
let refitTimer = 0;
window.addEventListener("resize", () => {
  clearTimeout(refitTimer);
  if (openStack.length === 0) return;
  refitTimer = setTimeout(() => {
    refitFloating();
    // A strip's size is a stored number, so a smaller window has to
    // re-clamp it against the new ceiling (stripSize).
    for (const edge of Object.keys(STRIPS)) {
      if (stripEl(edge)?.querySelector(".float-panel.open")) layoutStrip(edge);
    }
  }, REFIT_SETTLE);
});

// The matching chrome toggle lights up while its panel is open, so
// its state tracks the actual panel instead of a private flag.
function syncButton(name, open) {
  document
    .getElementById(name)
    ?.classList.toggle("primary", open && name.endsWith("-btn"));
}

// Open (or re-render) the panel `name`. `render(contentEl)` fills the
// panel's own content element; `teardown()` runs when this panel
// re-renders or closes — each tenant cleans up ONLY its own resources.
export function openPanel(name, render, teardown = null) {
  const p = ensurePanel(name);
  p.teardown?.();
  p.teardown = teardown;
  const opening = !openStack.includes(name);
  const order = openStack.length;
  if (opening) openStack.push(name);
  p.el.classList.add("open");
  // A re-render keeps where the panel already is; only a fresh open
  // re-places it (the card has to be visible to be measured).
  if (opening) {
    // A panel that lives in the strip goes back to its tile; anything
    // else is placed as a float.
    const stored = loadDock(name);
    if (p.dock) layoutStrip(p.dock);
    else if (stored) dockPanel(name, stored.mode);
    else placePanel(p, name, order);
  }
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
  // A generated card's content is rebuilt by render() on the next
  // open; a static card keeps its markup (the inspector resets to
  // its hint).
  if (name === "node") {
    p.contentEl.innerHTML = '<p class="hint">Click a node to inspect. Right-click for the context menu.</p>';
  } else if (!p.isStatic) {
    p.contentEl.innerHTML = "";
  }
  // A resize that landed just before this close left a refit armed;
  // it would measure the hidden card as a zero box.
  clearTimeout(p.refitTimer);
  // The card is going away; its dock menu must not outlive it.
  if (openMenu?.dataset.panel === name) closeDockMenu();
  p.el.classList.remove("open");
  // A closed tile keeps its slot but no longer counts; the strip may
  // empty.
  if (p.dock) layoutStrip(p.dock);
  openStack.splice(openStack.indexOf(name), 1);
  syncButton(name, false);
}

export function closeAllPanels() {
  for (const name of [...openStack].reverse()) closePanel(name);
}

// Esc: close the most recently opened panel. Returns the name of the
// panel it closed, or null when there was none to close.
export function closeTopPanel() {
  const top = openStack[openStack.length - 1];
  if (top == null) return null;
  closePanel(top);
  return top;
}

/// Generic panel toggle: a chrome button (Defaults / Report /
/// Formulas) that opens its own floating panel with some custom
/// render. Clicking the lit button closes that panel again; other
/// panels are unaffected — the button's id doubles as the panel name.
export function makeSidePanelToggle(btnId, render, teardown = null) {
  const btn = document.getElementById(btnId);
  btn.addEventListener("click", () => {
    if (isPanelOpen(btnId)) {
      closePanel(btnId);
      return;
    }
    openPanel(btnId, render, teardown);
  });
}

export function isPanelOpen(name) {
  return openStack.includes(name);
}
