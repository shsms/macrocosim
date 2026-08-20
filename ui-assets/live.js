// Pure helpers for the live topology overlay: label text and edge
// flow attributes. No DOM, no vis-network — unit-testable alone.

// W → kW → MW ladder, shared with the dashboard's fmt() so every
// power readout in the app scales identically.
export function formatScaled(value, unit) {
  if (value == null || !Number.isFinite(value)) return "—";
  const a = Math.abs(value);
  if (a >= 1e6) return `${(value / 1e6).toFixed(2)} M${unit}`;
  if (a >= 1e3) return `${(value / 1e3).toFixed(2)} k${unit}`;
  return `${value.toFixed(1)} ${unit}`;
}

// The node's live second line, or null when no power sample has
// arrived yet (the node then keeps its structural one-line label).
// Batteries and EV chargers show SoC instead of reactive power;
// everything else appends reactive only when the component reports
// it.
export function liveLabelLine({ category, p, q, soc }) {
  if (p == null || !Number.isFinite(p)) return null;
  const power = formatScaled(p, "W");
  if (category === "battery" || category === "ev-charger") {
    if (soc == null || !Number.isFinite(soc)) return power;
    return `${power} · SoC ${soc.toFixed(0)}%`;
  }
  if (q == null || !Number.isFinite(q)) return power;
  return `${power} · ${formatScaled(q, "VAr")}`;
}

// Flow attributes for a parent→child edge. `childPowerW` is the
// child's active power (consumption-positive); the edge's share is
// 1/parentCount (the meter aggregation rule, so parallel paths
// split visually too). The chevron shows *physical* flow: export
// (negative) points toward the parent. Below the dead band the
// chevron disappears so dead legs look dead.
export function edgeFlow(childPowerW, parentCount, siteMaxRatedW) {
  const max = siteMaxRatedW > 0 ? siteMaxRatedW : 10_000;
  const flow = (childPowerW ?? 0) / Math.max(parentCount, 1);
  const dead = Math.max(0.01 * max, 50);
  if (!Number.isFinite(flow) || Math.abs(flow) < dead) {
    return { chevron: false, towardParent: false, width: 1.5, scale: 0 };
  }
  const norm = Math.min(1, Math.sqrt(Math.abs(flow) / max));
  return {
    chevron: true,
    towardParent: flow < 0,
    width: Math.min(6, Math.max(1.5, 1 + 5 * norm)),
    scale: Math.max(0.5, 1.4 * norm),
  };
}
