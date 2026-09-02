// The floating panel shell. Each named panel — the node inspector
// ("node"), the formula explorer ("formula-btn"), the metrics panel
// ("metrics-btn"), the REPL ("repl-btn"), the log tail ("logs-btn"),
// the Defaults editor, the live Scenario report — is
// its own concurrently-openable, draggable, resizable card floating
// over #panel-dock. The cards are absolute floats, not a column:
// opening one never changes another's size. Re-opening an open panel
// just re-renders it (running its teardown first); closing runs
// teardown and hides the card. The shell never knows what's inside a
// panel; each tenant supplies its own teardown since only it knows
// what live resources (charts, timers) it owns.

// name → { el, contentEl, teardown, pos, cascade, isStatic, refitTimer }
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
};
// Panels whose markup is static in index.html, so a module can keep
// addressing their elements by id: name → [card id, content id].
const STATIC_PANELS = {
  node: ["inspector", "inspect"],
};
// A bottom-left card sits this far in from the dock's left and
// bottom edges; further bottom-left cards line up to its right with
// ROW_GAP between them.
const CORNER_INSET = 40;
const ROW_GAP = 8;

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
      <div class="panel-content"></div>`;
    dockEl().appendChild(el);
    contentEl = el.querySelector(".panel-content");
    el.querySelector(".float-close").addEventListener("click", () => closePanel(name));
  }
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
      // gripper and a re-fit would fight the drag. Checked again when
      // the timer fires, not just here: a card closed or grabbed
      // inside REFIT_SETTLE would otherwise be sanitized as a
      // display:none zero box, and clampOffset would write that
      // nonsense back into p.pos. closePanel cancels it too.
      if (!el.classList.contains("open") || el.style.height) return;
      clearTimeout(p.refitTimer);
      p.refitTimer = setTimeout(() => {
        if (!el.classList.contains("open") || el.style.height) return;
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
// panel, persisted per panel so it sticks across sessions.
function wireDrag(el, name, p) {
  const strip = el.querySelector(".panel-drag");
  strip.addEventListener("pointerdown", (e) => {
    e.preventDefault();
    strip.setPointerCapture(e.pointerId);
    const startX = e.clientX - p.pos.dx;
    const startY = e.clientY - p.pos.dy;
    // Measured once per gesture: re-reading it per pointermove would
    // force a layout on every frame. A bottom-anchored card whose
    // content changes mid-drag does move its untransformed top under
    // this anchor; its resize observer re-fits it once that growth
    // settles, which may well be mid-drag — harmless, since that
    // re-fit persists nothing and the next pointermove writes the
    // offset from this anchor again.
    const anchor = anchorOf(el, p.pos);
    const move = (ev) => {
      p.pos = { ...clampOffset(anchor, ev.clientX - startX, ev.clientY - startY), bottom: p.pos.bottom };
      applyPos(el, p.pos);
    };
    const stop = () => {
      strip.removeEventListener("pointermove", move);
      p.cascade = false;
      savePos(name, p.pos);
    };
    strip.addEventListener("pointermove", move);
    strip.addEventListener("pointerup", stop, { once: true });
    strip.addEventListener("pointercancel", stop, { once: true });
  });
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
    if (other === name || PANEL_DEFAULTS[other]?.spawn !== "bottom-left") continue;
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

// Sanitizing at open time only sees the geometry of that moment. The
// window moves it afterwards: a narrower viewport takes the card's
// edge off screen, wrapping chrome pushes the dock's top edge down,
// and a shorter window shrinks the height the cap is measured against.
// Nothing re-clamped for that, so an open card could be left stranded
// — its strip past the window edge, unreachable and unrecoverable
// short of closing the panel. Re-fit every open card once the gesture
// settles — a transient fit, since the window forced it and the user
// gets their own placement back when the window comes back. Installed
// once, at module scope: the listener outlives any one panel, and does
// nothing while none are open.
let refitTimer = 0;
window.addEventListener("resize", () => {
  clearTimeout(refitTimer);
  if (openStack.length === 0) return;
  refitTimer = setTimeout(() => {
    for (const name of openStack) {
      const p = panels.get(name);
      if (p) sanitizePanel(p, name, false);
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
  if (opening) placePanel(p, name, order);
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
  p.el.classList.remove("open");
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
