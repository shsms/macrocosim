// The floating panel shell. Each named panel — the node inspector
// ("node"), the formula explorer ("formula-btn"), the metrics panel
// ("metrics-btn"), the Defaults editor, the live Scenario report — is
// its own concurrently-openable, draggable, resizable card floating
// over #panel-dock. The cards are absolute floats, not a column:
// opening one never changes another's size. Re-opening an open panel
// just re-renders it (running its teardown first); closing runs
// teardown and hides the card. The shell never knows what's inside a
// panel; each tenant supplies its own teardown since only it knows
// what live resources (charts, timers) it owns.

// name → { el, contentEl, teardown, pos, cascade }
const panels = new Map();
// Open panels, oldest first — Esc closes the newest.
const openStack = [];

const POS_KEY_PREFIX = "sw-panel-pos-";
const SIZE_KEY_PREFIX = "sw-panel-size-";
// Vertical stagger for a panel opening without a stored position, so
// two panels summoned in a row never land perfectly on top of each
// other and each keeps a grabbable strip.
const CASCADE_STEP = 32;
// A resized panel must stay tall enough to grab and re-open: the drag
// strip plus a row of content.
const MIN_HEIGHT = 60;
// Of a clamped panel, how much must stay on screen: enough of the
// card's width to grab horizontally, and its strip vertically.
const KEEP_X = 80;
const KEEP_Y = 40;

function ensurePanel(name) {
  let p = panels.get(name);
  if (p) return p;
  let el;
  let contentEl;
  if (name === "node") {
    // The inspector's markup is static in index.html so inspect.js's
    // getElementById world keeps working untouched.
    el = document.getElementById("inspector");
    contentEl = document.getElementById("inspect");
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
    document.getElementById("panel-dock").appendChild(el);
    contentEl = el.querySelector(".panel-content");
    el.querySelector(".float-close").addEventListener("click", () => closePanel(name));
  }
  const stored = loadPos(name);
  // Without a stored position the panel is still unplaced, so its
  // first open cascades off whatever is already open.
  p = { el, contentEl, teardown: null, pos: stored ?? { dx: 0, dy: 0 }, cascade: !stored };
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

// Keep a drag offset reachable: the grab strip may not slide under the
// fixed header (measured, never hardcoded — the chrome's height is
// CSS's business), nor off the other three edges.
function clampOffset(anchor, dx, dy) {
  const headerBottom = document.querySelector("body > header")?.getBoundingClientRect().bottom ?? 0;
  const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
  return {
    dx: clamp(dx, -(anchor.left + anchor.width - KEEP_X), window.innerWidth - anchor.left - KEEP_X),
    dy: clamp(dy, headerBottom - anchor.top, window.innerHeight - anchor.top - KEEP_Y),
  };
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
    // Measured once: the anchor cannot move mid-drag, and re-reading
    // it per pointermove would force a layout on every frame.
    const anchor = anchorOf(el, p.pos);
    const move = (ev) => {
      p.pos = clampOffset(anchor, ev.clientX - startX, ev.clientY - startY);
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

// CSS `resize` gives the height affordance; there is no resize event
// for it, so the observer is how we notice. An inline height is the
// one signal that this height is the user's choice and not the
// content's — content growth leaves the style empty.
function wireResize(el, name) {
  let last = 0;
  new ResizeObserver(() => {
    const h = Number.parseFloat(el.style.height);
    if (!h || Math.abs(h - last) < 1) return;
    last = h;
    saveSize(name, h);
  }).observe(el);
}

function loadPos(name) {
  try {
    const raw = JSON.parse(localStorage.getItem(POS_KEY_PREFIX + name));
    return raw && Number.isFinite(raw.dx) && Number.isFinite(raw.dy) ? raw : null;
  } catch (_) {
    return null;
  }
}
function savePos(name, pos) {
  try {
    localStorage.setItem(POS_KEY_PREFIX + name, JSON.stringify({ dx: pos.dx, dy: pos.dy }));
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
    // Storage unavailable — the height just doesn't stick.
  }
}

// Place the card as it opens: an unplaced one cascades off the panels
// already open, a stored height is restored, and anything that would
// leave the strip out of reach (a drag off-screen, a height from a
// bigger window) is clamped back and the correction written through —
// so a panel lost off the edge comes back on the next toggle.
function placePanel(p, name, order) {
  const { el } = p;
  const h = loadSize(name);
  if (h != null) {
    const maxH = el.parentElement?.clientHeight ?? window.innerHeight;
    const fitted = Math.max(MIN_HEIGHT, Math.min(h, maxH));
    el.style.height = `${fitted}px`;
    if (fitted !== h) saveSize(name, fitted);
  }
  if (p.cascade) p.pos = { dx: 0, dy: CASCADE_STEP * order };
  applyPos(el, p.pos);
  const clamped = clampOffset(anchorOf(el, p.pos), p.pos.dx, p.pos.dy);
  if (clamped.dx === p.pos.dx && clamped.dy === p.pos.dy) return;
  p.pos = clamped;
  applyPos(el, p.pos);
  // Only a position the user chose is worth correcting on disk; a
  // clamped cascade is this open's arithmetic, not their placement.
  if (!p.cascade) savePos(name, p.pos);
}

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
  p.contentEl.innerHTML =
    name === "node"
      ? '<p class="hint">Click a node to inspect. Right-click for the context menu.</p>'
      : "";
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
