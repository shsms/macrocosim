// The floating side-panel shell. Different "tenants" — the node
// inspector, the Defaults editor, the live Scenario report, a
// formula tree — take turns filling the same card via `openPanel`.
// Swapping tenants (or closing the panel) tears down whichever one
// was showing; each tenant supplies its own `teardown` since only it
// knows what live resources (charts, polling timers) it owns. The
// shell itself never has to know what's inside.
//
// The `inspector-open` class on <body> shows the card, which overlays
// the right side of the canvas — it never resizes it, so a
// double-click's second click lands on an unmoved graph.

import { inspectEl, inspectorEl } from "./app.js";

// Currently open tenant's name (or null) and its registered teardown.
let panelName = null;
let panelTeardown = null;

// Run whichever teardown is currently registered, then clear the slot
// so a later call (e.g. closePanel right after an openPanel swap
// already ran it) is a no-op instead of double-tearing-down the old
// tenant.
function runTeardown() {
  const teardown = panelTeardown;
  panelTeardown = null;
  teardown?.();
}

// The matching chrome toggle (Defaults / Report) lights up, so its
// state tracks the actual panel instead of a private flag that a
// ×/tab-switch close would leave stale.
function syncButtons(name) {
  for (const b of document.querySelectorAll("#defaults-btn, #scenario-report-btn")) {
    b.classList.toggle("primary", b.id === name);
  }
}

// Open the floating panel showing `name` ("node" / "formula" /
// "defaults-btn" / "scenario-report-btn"). `render()` fills
// inspectEl; `teardown()` runs when this tenant is replaced or the
// panel closes — each tenant cleans up ONLY its own resources.
export function openPanel(name, render, teardown = null) {
  runTeardown(); // previous tenant's, before this one's content lands
  panelName = name;
  panelTeardown = teardown;
  document.body.classList.add("inspector-open");
  inspectorEl.dataset.panel = name;
  syncButtons(name);
  render();
}

// Close the floating inspector: stop the current tenant's live
// resources, reset its content back to the empty hint, and hide the
// card.
export function closePanel() {
  runTeardown();
  inspectEl.innerHTML =
    '<p class="hint">Click a node to inspect. Right-click for the context menu.</p>';
  document.body.classList.remove("inspector-open");
  delete inspectorEl.dataset.panel;
  panelName = null;
  syncButtons(null);
}

export function currentPanel() {
  return panelName;
}
