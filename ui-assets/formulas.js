// Formula side panel + dashboard tile tooltips. The graph crate's
// `*_formula()` accessors return a rendered string like
//   MAX(#2 - COALESCE(#1002, #1001, 0.0), 0.0)
// — parseFormula (in formula-ast.js) chews that into an AST and
// formulaToHtml renders nested HTML with each `#N` as a clickable
// link back to the topology canvas. loadFormulas /
// setupFormulaTileClicks wire up the dashboard tile titles + the
// click → openFormulaPanel handoff.

import { inspectEl, jumpToTopology, mgPath } from "./app.js";
import { formulaToHtml, parseFormula } from "./formula-ast.js";
import { openPanel } from "./side-panel.js";

// Open the formula tree for the given stream in the inspector. Re-uses
// the inspector (same pattern as
// renderScenarioReport / renderDefaults) so the layout stays
// uniform. No teardown — the panel is static markup with no live
// resources to tear back down.
function openFormulaPanel(stream) {
  openPanel("formula", () => renderFormulaPanel(stream));
}

async function renderFormulaPanel(stream) {
  try {
    const res = await fetch(mgPath("microgrid/formulas"));
    if (!res.ok) return;
    const map = await res.json();
    const src = map[stream];
    if (!src) return;
    inspectEl.innerHTML = `
      <div class="formula-panel">
        <h2>Formula · <code>${stream}</code></h2>
        <pre class="formula-tree">${formulaToHtml(parseFormula(src))}</pre>
        <p class="hint">Click any <code>#N</code> to jump to that component on the Topology canvas.</p>
      </div>
    `;
    // Delegate refs: one listener per panel-open, no per-span hookup.
    inspectEl.querySelector(".formula-tree")?.addEventListener("click", (ev) => {
      const t = ev.target.closest(".formula-ref");
      if (!t) return;
      jumpToTopology(Number(t.dataset.id));
    });
  } catch (_) {
    // Best-effort.
  }
}
export async function loadFormulas() {
  try {
    const res = await fetch(mgPath("microgrid/formulas"));
    if (!res.ok) return;
    const map = await res.json();
    for (const [stream, formula] of Object.entries(map)) {
      for (const tile of document.querySelectorAll(`.dash-tile`)) {
        const v = tile.querySelector(`.dash-value[data-stream="${stream}"]`);
        if (v) {
          // Tile-level title so hovering anywhere on the card
          // (number + sparkline + meta) surfaces the formula. The
          // click handler installed below opens the side-panel
          // formula tree with each #N linked to the canvas.
          tile.title = `${stream} = ${formula}`;
          tile.classList.add("dash-tile-interactive");
          tile.dataset.formulaStream = stream;
        }
      }
    }
  } catch (_) {
    // Best-effort — tile tooltips just show their default `title`
    // (none) if this fails.
  }
}

// One delegated click handler covers every formula-bearing tile
// (existing pool tiles + any future ones loadFormulas tags). Tiles
// without a formulaStream are non-interactive and short-circuit
// here.
export function setupFormulaTileClicks() {
  document.getElementById("dashboard")?.addEventListener("click", (ev) => {
    const tile = ev.target.closest(".dash-tile-interactive");
    if (!tile) return;
    const stream = tile.dataset.formulaStream;
    if (!stream) return;
    openFormulaPanel(stream);
  });
}
