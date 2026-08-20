// The pill node: one model builder (pure) and one canvas renderer,
// shared by the Topology and Formulas canvases through vis-network's
// `shape: "custom"`. The model says *what* a node shows; the
// renderer (further down) says how it looks.

import { formatScaled } from "./live.js";

export const COLORS = {
  export: "#6bd9a5",
  import: "#79b8ff",
  dim: "#5a626d",
  exportDull: "#4f9a78",
  importDull: "#5a87bd",
  surface: "#242a33",
  border: "#323a45",
  fg: "#d5dbe3",
  muted: "#7d848e",
  accent: "#79b8ff",
  hover: "#b0b8c1",
  bad: "#e58275",
  standby: "#c4ad55",
  socFill: "#6bd9a5",
};

const finite = (v) => v != null && Number.isFinite(v);

// Consumption-positive: import blue, export green, dead band dim.
export function powerColor(value, deadBand) {
  if (!finite(value) || Math.abs(value) < deadBand) return COLORS.dim;
  return value < 0 ? COLORS.export : COLORS.import;
}

// Reactive power in duller versions of the same hues, by its own
// sign (+Q inductive). Beside the active colour, a matching hue reads
// as lagging and a contrasting one as leading.
export function reactiveColor(value, deadBand) {
  if (!finite(value) || Math.abs(value) < deadBand) return COLORS.dim;
  return value < 0 ? COLORS.exportDull : COLORS.importDull;
}

// Long names get shortened on the pill; the full name lives in the
// hover card / tooltip.
export function shortName(name) {
  return name.length > 22 ? `${name.slice(0, 20)}…` : name;
}

function effectiveHealth(c) {
  const health = c.health || "ok";
  return health === "ok" && c.provides_telemetry === false ? "standby" : health;
}

function socAux(soc) {
  if (!finite(soc)) return null;
  const pct = Math.round(soc);
  return { kind: "soc", pct, text: `${pct}%` };
}

// component: an /api/topology component; live: { p, q, soc, dc } or
// null; options: { valuesOn, dotColor, deadBand }.
export function pillModel(c, live, { valuesOn, dotColor, deadBand }) {
  let hero = null;
  let aux = null;
  if (valuesOn && live) {
    const power = c.category === "battery" ? live.dc : live.p;
    if (c.category === "battery" || c.category === "ev-charger") {
      aux = socAux(live.soc);
    } else if (finite(live.q)) {
      aux = { kind: "reactive", text: formatScaled(live.q, "VAr"), color: reactiveColor(live.q, deadBand) };
    }
    if (finite(power)) hero = { text: formatScaled(power, "W"), color: powerColor(power, deadBand) };
    else if (aux) hero = { text: "—", color: COLORS.dim };
  }
  return {
    id: c.id,
    name: shortName(c.name),
    fullName: c.name,
    idText: `#${c.id}`,
    dotColor,
    health: effectiveHealth(c),
    hidden: Boolean(c.hidden),
    valuesOn: Boolean(valuesOn),
    hero,
    aux,
    highlight: "none",
  };
}
