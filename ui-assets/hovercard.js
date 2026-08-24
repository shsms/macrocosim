// The node hover card: a pure model builder (what the card says) and
// a DOM widget (further down) that shows it beside a pill. Read-only
// — every action stays in the inspector.

import { formatScaled } from "./live.js";
import { powerColor, reactiveColor } from "./pill.js";

const finite = (v) => v != null && Number.isFinite(v);

function agoText(ms) {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} s ago`;
  const m = Math.floor(s / 60);
  return m < 60 ? `${m} min ago` : `${Math.floor(m / 60)} h ago`;
}

function powerSection(label, value, lo, hi, deadBand) {
  if (!finite(value)) return null;
  return {
    label,
    text: formatScaled(value, "W"),
    color: powerColor(value, deadBand),
    lo: finite(lo) ? lo : null,
    hi: finite(hi) ? hi : null,
    value,
  };
}

// |P| / sqrt(P² + Q²); lagging when P and Q share a sign (passive
// convention: +Q inductive), leading otherwise. Inside the reactive
// dead band neither word means anything — the sign of a Q that small
// is noise — so the qualifier is dropped and the bare PF stands.
function powerFactor(p, q, deadBand) {
  if (!finite(p) || !finite(q) || Math.abs(p) < deadBand) return null;
  const pf = Math.abs(p) / Math.hypot(p, q);
  if (Math.abs(q) < deadBand) return { text: `PF ${pf.toFixed(2)}` };
  return { text: `PF ${pf.toFixed(2)} ${Math.sign(p) === Math.sign(q) ? "lagging" : "leading"}` };
}

// A setpoint's value on the same scaled ladder as every other number
// on the card. `value` is a JSON number on the wire, but a future
// kind may carry a word, so anything non-numeric is shown raw.
// Neither augment kind carries a meaningful value (the server ignores
// it and sends 0), so for those the kind stands alone. Matched by
// prefix so the active (augment_bounds) and reactive
// (augment_reactive_bounds) routes are covered by the one rule —
// note augment_reactive_bounds does NOT match the "reactive" prefix
// used for the unit below, so without this it would render "0 W".
function commandValueText(kind, value) {
  if (kind.startsWith("augment")) return "";
  if (value == null || String(value).trim() === "") return String(value);
  const n = Number(value);
  if (!Number.isFinite(n)) return String(value);
  return formatScaled(n, kind.startsWith("reactive") ? "VAr" : "W");
}

export function hoverCardModel({ component: c, live, parents, children, lastCommand, nowMs, deadBand }) {
  const battery = c.category === "battery";
  const hasLive = Boolean(live);
  const showSoc = battery || c.category === "ev-charger";
  const soc = hasLive && showSoc && finite(live.soc) ? { pct: Math.round(live.soc), text: `${Math.round(live.soc)}%` } : null;
  const energy = hasLive && finite(live.energy) ? { text: `${formatScaled(live.energy, "Wh")} since start` } : null;
  let freshness;
  if (!hasLive || !finite(live.ts)) freshness = { text: "no data yet", stale: true };
  else {
    const age = nowMs - live.ts;
    freshness = { text: `updated ${agoText(age)}`, stale: age > 5000 };
  }
  let command = null;
  if (lastCommand) {
    const outcome = lastCommand.accepted ? "accepted" : `rejected: ${lastCommand.reason || "unknown"}`;
    const kind = String(lastCommand.kind);
    const head = [kind.replaceAll("_", " "), commandValueText(kind, lastCommand.value)].filter(Boolean).join(" ");
    command = { text: `${head} · ${agoText(nowMs - lastCommand.ts)} · ${outcome}` };
  }
  return {
    title: c.name,
    idLine: `#${c.id} · ${c.category}${c.subtype ? ` / ${c.subtype}` : ""}`,
    health: (c.health || "ok") === "ok" && c.provides_telemetry === false ? "standby" : c.health || "ok",
    power: hasLive && !battery ? powerSection("Active power", live.p, live.pLo, live.pHi, deadBand) : null,
    reactive:
      hasLive && !battery && finite(live.q)
        ? {
            label: "Reactive power",
            text: formatScaled(live.q, "VAr"),
            color: reactiveColor(live.q, deadBand),
            lo: finite(live.qLo) ? live.qLo : null,
            hi: finite(live.qHi) ? live.qHi : null,
            value: live.q,
          }
        : null,
    pf: hasLive && !battery ? powerFactor(live.p, live.q, deadBand) : null,
    energy,
    soc,
    dc: hasLive && battery ? powerSection("DC power", live.dc, null, null, deadBand) : null,
    spark: hasLive ? live.hist.slice() : [],
    lastCommand: command,
    wiring: {
      parents: parents.length ? parents.join(", ") : "—",
      children: children.length ? children.join(", ") : "—",
    },
    freshness,
  };
}

// ── widget ──────────────────────────────────────────────────────

function esc(s) {
  return String(s).replace(/[&<>"']/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[ch]);
}

function envelopeBar(section, unit = "W") {
  if (!section) return "";
  let marker = "";
  if (section.lo != null && section.hi != null && section.hi > section.lo) {
    const pct = Math.max(0, Math.min(100, ((section.value - section.lo) / (section.hi - section.lo)) * 100));
    marker = `<div class="hc-bar"><div class="hc-bar-marker" style="left:${pct.toFixed(1)}%;background:${section.color}"></div></div>
      <div class="hc-bar-ends"><span>${esc(formatScaled(section.lo, unit))}</span><span>${esc(formatScaled(section.hi, unit))}</span></div>`;
  }
  return `<div class="hc-row"><span class="hc-label">${esc(section.label)}</span><span class="hc-value" style="color:${section.color}">${esc(section.text)}</span></div>${marker}`;
}

function sparkSvg(points) {
  if (points.length < 2) return '<div class="hc-spark hc-spark-empty">collecting…</div>';
  // The line is drawn inside a 3 px inset so the extreme sample and
  // the 2.5 px end dot sit inside the box instead of half outside it.
  const W = 276, H = 36, PAD = 3;
  const plotW = W - PAD * 2, plotH = H - PAD * 2;
  const ys = points.map((p) => p[1]);
  const lo = Math.min(0, ...ys), hi = Math.max(0, ...ys);
  const span = hi - lo || 1;
  const x = (i) => (PAD + (i / (points.length - 1)) * plotW).toFixed(1);
  const y = (v) => (PAD + plotH - ((v - lo) / span) * plotH).toFixed(1);
  const d = points.map((p, i) => `${i ? "L" : "M"}${x(i)},${y(p[1])}`).join(" ");
  const zero = y(0);
  return `<svg class="hc-spark" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" aria-hidden="true">
    <line x1="0" y1="${zero}" x2="${W}" y2="${zero}" class="hc-spark-zero"/>
    <path d="${d}" class="hc-spark-line"/>
    <circle cx="${x(points.length - 1)}" cy="${y(ys[ys.length - 1])}" r="2.5" class="hc-spark-end"/>
  </svg>`;
}

function render(m) {
  const healthChip = m.health === "ok" ? "" : `<span class="hc-chip hc-chip-${esc(m.health)}">${esc(m.health)}</span>`;
  const soc = m.soc
    ? `<div class="hc-row"><span class="hc-label">SoC</span><span class="hc-value">${esc(m.soc.text)}</span></div>
       <div class="hc-bar hc-soc"><div class="hc-soc-fill" style="width:${m.soc.pct}%"></div></div>`
    : "";
  const row = (label, value, cls = "") => `<div class="hc-row"><span class="hc-label">${esc(label)}</span><span class="hc-value ${cls}">${esc(value)}</span></div>`;
  return `
    <div class="hc-head"><span class="hc-title">${esc(m.title)}</span>${healthChip}</div>
    <div class="hc-id">${esc(m.idLine)}</div>
    ${sparkSvg(m.spark)}
    ${envelopeBar(m.power)}${envelopeBar(m.dc)}
    ${envelopeBar(m.reactive, "VAr")}
    ${m.pf ? `<div class="hc-row hc-pf">${esc(m.pf.text)}</div>` : ""}
    ${soc}
    ${m.energy ? row("Energy", m.energy.text) : ""}
    ${m.lastCommand ? row("Last command", m.lastCommand.text, "hc-cmd") : ""}
    ${row("Parents", m.wiring.parents)}
    ${row("Children", m.wiring.children)}
    <div class="hc-foot"><span class="${m.freshness.stale ? "hc-stale" : ""}">${esc(m.freshness.text)}</span><span>click for inspector</span></div>`;
}

// One card per canvas, appended lazily. `anchor` is the pill's rect
// in page pixels; the card sits 8 px below it, or above when below
// would leave the viewport, and never takes the pointer.
export function createHoverCard() {
  let el = null;
  function ensure() {
    if (el) return el;
    el = document.createElement("div");
    el.className = "hover-card";
    el.setAttribute("aria-hidden", "true");
    el.hidden = true;
    document.body.appendChild(el);
    return el;
  }
  return {
    show(model, anchor) {
      const node = ensure();
      node.innerHTML = render(model);
      node.hidden = false;
      const W = 300;
      const left = Math.max(8, Math.min(window.innerWidth - W - 8, anchor.x + anchor.width / 2 - W / 2));
      node.style.left = `${left}px`;
      node.style.top = "0px";
      const h = node.offsetHeight;
      const below = anchor.y + anchor.height + 8;
      node.style.top = `${below + h > window.innerHeight - 8 ? Math.max(8, anchor.y - 8 - h) : below}px`;
    },
    hide() {
      if (el) el.hidden = true;
    },
    visible() {
      return Boolean(el && !el.hidden);
    },
    text() {
      return el && !el.hidden ? el.textContent : "";
    },
  };
}
