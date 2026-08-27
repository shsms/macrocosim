// The floating panel shell. Each named panel — the node inspector
// ("node"), the formula explorer ("formula-btn"), the metrics panel
// ("metrics-btn"), the Defaults editor, the live Scenario report — is
// its own concurrently-openable, draggable card inside #panel-dock.
// Re-opening an open panel just re-renders it (running its teardown
// first); closing runs teardown and hides the card. The shell never
// knows what's inside a panel; each tenant supplies its own teardown
// since only it knows what live resources (charts, timers) it owns.

// name → { el, contentEl, teardown }
const panels = new Map();
// Open panels, oldest first — Esc closes the newest.
const openStack = [];

const POS_KEY_PREFIX = "sw-panel-pos-";

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
  wireDrag(el, name);
  p = { el, contentEl, teardown: null };
  panels.set(name, p);
  return p;
}

// Drag-to-move via the grab strip; the offset is a transform on the
// panel, persisted per panel so it sticks across sessions.
function wireDrag(el, name) {
  const strip = el.querySelector(".panel-drag");
  let { dx, dy } = loadPos(name);
  if (dx || dy) el.style.transform = `translate(${dx}px, ${dy}px)`;
  strip.addEventListener("pointerdown", (e) => {
    e.preventDefault();
    strip.setPointerCapture(e.pointerId);
    const startX = e.clientX - dx;
    const startY = e.clientY - dy;
    const rect = el.getBoundingClientRect();
    const baseLeft = rect.left - dx;
    const baseTop = rect.top - dy;
    const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
    const move = (ev) => {
      dx = clamp(
        ev.clientX - startX,
        -(baseLeft + rect.width - 80),
        window.innerWidth - baseLeft - 80,
      );
      dy = clamp(ev.clientY - startY, -baseTop, window.innerHeight - baseTop - 40);
      el.style.transform = `translate(${dx}px, ${dy}px)`;
    };
    const stop = () => {
      strip.removeEventListener("pointermove", move);
      savePos(name, dx, dy);
    };
    strip.addEventListener("pointermove", move);
    strip.addEventListener("pointerup", stop, { once: true });
    strip.addEventListener("pointercancel", stop, { once: true });
  });
}

function loadPos(name) {
  try {
    return JSON.parse(localStorage.getItem(POS_KEY_PREFIX + name)) ?? { dx: 0, dy: 0 };
  } catch (_) {
    return { dx: 0, dy: 0 };
  }
}
function savePos(name, dx, dy) {
  try {
    localStorage.setItem(POS_KEY_PREFIX + name, JSON.stringify({ dx, dy }));
  } catch (_) {
    // Storage unavailable — the position just doesn't stick.
  }
}

// The matching chrome toggle lights up while its panel is open, so
// its state tracks the actual panel instead of a private flag.
function syncButton(name, open) {
  document
    .getElementById(name)
    ?.classList.toggle("primary", open && name.endsWith("-btn"));
}

function syncBodyClass() {
  document.body.classList.toggle("panel-open", openStack.length > 0);
}

// Open (or re-render) the panel `name`. `render(contentEl)` fills the
// panel's own content element; `teardown()` runs when this panel
// re-renders or closes — each tenant cleans up ONLY its own resources.
export function openPanel(name, render, teardown = null) {
  const p = ensurePanel(name);
  p.teardown?.();
  p.teardown = teardown;
  if (!openStack.includes(name)) openStack.push(name);
  p.el.classList.add("open");
  syncBodyClass();
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
  syncBodyClass();
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
