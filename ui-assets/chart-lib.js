// The vendored uPlot is a classic <script> global (index.html), not
// a module: when it does not load — a blocked asset, a bad vendor
// bump, its own load-time `new Intl.NumberFormat(navigator.language)`
// on a browser with an invalid language tag — nothing in the import
// graph fails, and the first `new uPlot(...)` throws from inside a
// render. Every chart builder asks here first.

import { FONT_MONO, FONT_SANS } from "./pill.js";

// The uPlot constructor, or null. Resolved once: a classic <script>
// is settled before any module runs, so the answer never changes.
// The loops that retry a chart build once its data arrives ask this
// before re-entering a builder that can never succeed.
export const uplot = typeof uPlot !== "undefined" ? uPlot : null;

// The theme's chart colours, read when a chart is built: axis labels and ticks,
// grid lines, the zero line, and a single series' line.
export function chartColors() {
  const style = getComputedStyle(document.documentElement);
  const token = (name) => style.getPropertyValue(name).trim();
  return {
    axis: token("--chart-axis"),
    grid: token("--chart-grid"),
    zero: token("--chart-zero"),
    series: token("--accent"),
  };
}

// A uPlot axis in those colours, with mono ticks and a semi-bold sans label at
// 12px in both densities (the axis widths are fixed pixels); `extra` adds or
// overrides options.
export function chartAxis(colors, extra = {}) {
  return {
    stroke: colors.axis,
    grid: { stroke: colors.grid, width: 0.5 },
    font: `12px ${FONT_MONO}`,
    labelFont: `600 12px ${FONT_SANS}`,
    ...extra,
  };
}

// The uPlot constructor, or null after writing a note into `slot` —
// the element the chart would have filled, in the panels' usual hint
// style — so the panel around it keeps working and the user sees why
// the chart is missing. A slot already carrying the note is left
// alone, so a builder called again on a poll does not rewrite it.
export function requireUplot(slot) {
  if (uplot) return uplot;
  if (!slot.querySelector("[data-uplot-note]")) {
    slot.innerHTML = '<p class="hint" data-uplot-note>charts unavailable: uPlot did not load</p>';
  }
  return null;
}
