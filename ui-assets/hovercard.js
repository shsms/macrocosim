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
// convention: +Q inductive), leading otherwise.
function powerFactor(p, q, deadBand) {
  if (!finite(p) || !finite(q) || Math.abs(p) < deadBand) return null;
  const pf = Math.abs(p) / Math.hypot(p, q);
  const lagging = q === 0 || Math.sign(p) === Math.sign(q);
  return { text: `PF ${pf.toFixed(2)} ${lagging ? "lagging" : "leading"}` };
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
    command = { text: `${String(lastCommand.kind).replace("_", " ")} ${lastCommand.value} · ${agoText(nowMs - lastCommand.ts)} · ${outcome}` };
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
