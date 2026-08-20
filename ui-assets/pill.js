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

// ── renderer ────────────────────────────────────────────────────
// Everything below draws in canvas units; vis-network applies the
// zoom and device-pixel scaling, so 14 px here is 14 px at scale 1.

export const FONT_SANS = '"IBM Plex Sans", system-ui, -apple-system, "Segoe UI", sans-serif';
export const FONT_MONO = '"IBM Plex Mono", ui-monospace, SFMono-Regular, Menlo, monospace';

const GEOM = {
  minWidth: 96,
  maxWidth: 200,
  padX: 10,
  padY: 6,
  dot: 9,
  dotGap: 8,
  idGap: 6,
  rowGap: 3,
  dividerGap: 8,
  radius: 10,
  socBarW: 40,
  socBarH: 5,
  socGap: 6,
};

function fonts(model) {
  return model.valuesOn
    ? { name: `500 11.5px ${FONT_SANS}`, id: `400 10px ${FONT_MONO}`, row1H: 14 }
    : { name: `500 13px ${FONT_SANS}`, id: `400 11px ${FONT_MONO}`, row1H: 16 };
}
const FONT_HERO = `600 14px ${FONT_MONO}`;
const FONT_AUX = `500 11px ${FONT_MONO}`;
const ROW2_H = 17;

// Canvas text is not a DOM use, so the browser never loads a web
// font for it on its own; ask for the faces explicitly.
export const pillFontsReady =
  typeof document !== "undefined" && document.fonts
    ? Promise.all([
        document.fonts.load('500 11.5px "IBM Plex Sans"'),
        document.fonts.load('600 14px "IBM Plex Mono"'),
        document.fonts.load('400 10px "IBM Plex Mono"'),
        document.fonts.load('500 11px "IBM Plex Mono"'),
      ]).then(() => undefined, () => undefined)
    : Promise.resolve();

const measureCache = new Map();
export function invalidateMeasureCache() {
  measureCache.clear();
}

function textWidth(ctx, font, text) {
  ctx.font = font;
  return ctx.measureText(text).width;
}

function hasRow2(model) {
  return Boolean(model.valuesOn && (model.hero || model.aux));
}

function auxWidth(ctx, aux) {
  if (!aux) return 0;
  if (aux.kind === "soc") return GEOM.socBarW + GEOM.socGap + textWidth(ctx, FONT_AUX, aux.text);
  return textWidth(ctx, FONT_AUX, aux.text);
}

// Measures the pill for `model`. Cached by the strings that affect
// size; hover/selection never change a pill's size.
export function measurePill(ctx, model) {
  const key = [model.valuesOn ? 1 : 0, model.name, model.idText, model.hero?.text ?? "", model.aux?.kind ?? "", model.aux?.text ?? ""].join("\u0000");
  const hit = measureCache.get(key);
  if (hit) return hit;
  const f = fonts(model);
  const textLeft = GEOM.padX + GEOM.dot + GEOM.dotGap;
  const row2 = hasRow2(model);
  let row2W = 0;
  let heroW = 0;
  if (row2) {
    heroW = textWidth(ctx, FONT_HERO, model.hero ? model.hero.text : "—");
    row2W = heroW;
    if (model.aux) row2W += GEOM.dividerGap * 2 + 1 + auxWidth(ctx, model.aux);
  }
  const idW = textWidth(ctx, f.id, model.idText);
  // Row 1 may need to give way: the pill must stay inside the
  // layout's column separation, so the name truncates until it fits.
  let name = model.name;
  let nameW = textWidth(ctx, f.name, name);
  let row1W = nameW + GEOM.idGap + idW;
  const budget = GEOM.maxWidth - textLeft - GEOM.padX;
  while (row1W > budget && name.length > 4) {
    name = `${name.replace(/…$/, "").slice(0, -1)}…`;
    nameW = textWidth(ctx, f.name, name);
    row1W = nameW + GEOM.idGap + idW;
  }
  const content = Math.max(row1W, row2W);
  const width = Math.round(Math.min(GEOM.maxWidth, Math.max(GEOM.minWidth, textLeft + content + GEOM.padX)));
  const height = GEOM.padY * 2 + f.row1H + (row2 ? GEOM.rowGap + ROW2_H : 0);
  const dims = { width, height, name, nameW, heroW, textLeft, row1H: f.row1H, row2H: row2 ? ROW2_H : 0 };
  // Live value strings make most keys single-use; the cap keeps a
  // day-long tab flat while still serving the repeated draws between flushes.
  if (measureCache.size > 1024) measureCache.clear();
  measureCache.set(key, dims);
  return dims;
}

function roundRect(ctx, x, y, w, h, r) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

function borderStyle(model, state) {
  if (model.highlight === "subtracted") return { color: COLORS.bad, width: 2 };
  if (state.selected) return { color: COLORS.accent, width: 2 };
  if (state.hover) return { color: COLORS.hover, width: 1.5 };
  return { color: COLORS.border, width: model.hidden ? 1.5 : 1 };
}

// Draws the pill centred on (x, y). `state` is vis's
// { selected, hover }; a formula "subtracted" highlight lives in the
// model itself (it is not a vis selection).
export function drawPill(ctx, x, y, model, state) {
  const d = measurePill(ctx, model);
  const f = fonts(model);
  const left = x - d.width / 2;
  const top = y - d.height / 2;
  // surface + border
  roundRect(ctx, left, top, d.width, d.height, GEOM.radius);
  ctx.fillStyle = COLORS.surface;
  ctx.fill();
  const b = borderStyle(model, state);
  ctx.setLineDash(model.hidden ? [4, 3] : []);
  ctx.lineWidth = b.width;
  ctx.strokeStyle = b.color;
  ctx.stroke();
  ctx.setLineDash([]);
  // category dot + health ring
  const dotX = left + GEOM.padX + GEOM.dot / 2;
  const dotY = top + GEOM.padY + f.row1H / 2;
  ctx.beginPath();
  ctx.arc(dotX, dotY, GEOM.dot / 2, 0, Math.PI * 2);
  ctx.fillStyle = model.dotColor;
  ctx.fill();
  if (model.health !== "ok") {
    ctx.beginPath();
    ctx.arc(dotX, dotY, GEOM.dot / 2 + 2.5, 0, Math.PI * 2);
    ctx.lineWidth = 1.5;
    ctx.strokeStyle = model.health === "error" ? COLORS.bad : COLORS.standby;
    ctx.stroke();
  }
  // row 1: name + id
  ctx.textBaseline = "middle";
  ctx.textAlign = "left";
  let tx = left + d.textLeft;
  ctx.font = f.name;
  ctx.fillStyle = COLORS.fg;
  ctx.fillText(d.name, tx, dotY);
  tx += d.nameW + GEOM.idGap;
  ctx.font = f.id;
  ctx.fillStyle = COLORS.muted;
  ctx.fillText(model.idText, tx, dotY + 0.5);
  if (!d.row2H) return;
  // row 2: hero | aux
  const row2Y = top + GEOM.padY + f.row1H + GEOM.rowGap + ROW2_H / 2;
  tx = left + d.textLeft;
  ctx.font = FONT_HERO;
  const hero = model.hero || { text: "—", color: COLORS.dim };
  ctx.fillStyle = hero.color;
  ctx.fillText(hero.text, tx, row2Y);
  if (!model.aux) return;
  tx += d.heroW + GEOM.dividerGap;
  ctx.fillStyle = COLORS.border;
  ctx.fillRect(Math.round(tx), row2Y - 6, 1, 12);
  tx += 1 + GEOM.dividerGap;
  if (model.aux.kind === "soc") {
    const barY = row2Y - GEOM.socBarH / 2;
    roundRect(ctx, tx, barY, GEOM.socBarW, GEOM.socBarH, GEOM.socBarH / 2);
    ctx.fillStyle = COLORS.border;
    ctx.fill();
    const fillW = Math.max(0, Math.min(GEOM.socBarW, (GEOM.socBarW * model.aux.pct) / 100));
    if (fillW > 0) {
      roundRect(ctx, tx, barY, fillW, GEOM.socBarH, GEOM.socBarH / 2);
      ctx.fillStyle = COLORS.socFill;
      ctx.fill();
    }
    tx += GEOM.socBarW + GEOM.socGap;
    ctx.font = FONT_AUX;
    ctx.fillStyle = COLORS.fg;
    ctx.fillText(model.aux.text, tx, row2Y);
  } else {
    ctx.font = FONT_AUX;
    ctx.fillStyle = model.aux.color;
    ctx.fillText(model.aux.text, tx, row2Y);
  }
}

// The vis-network `ctxRenderer` for one node. vis calls it on every
// draw with { ctx, id, x, y, state, style, label }; `onSize` lets the
// canvas owner notice size changes (vis applies `nodeDimensions` one
// draw late — see topology.js).
export function pillRenderer(model, onSize) {
  return ({ ctx, id, x, y, state }) => {
    const d = measurePill(ctx, model);
    if (onSize) onSize(id, d.width, d.height);
    return {
      drawNode() {
        drawPill(ctx, x, y, model, state || { selected: false, hover: false });
      },
      nodeDimensions: { width: d.width, height: d.height },
    };
  };
}
