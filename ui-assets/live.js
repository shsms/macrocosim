// Pure helpers for the live topology overlay: number formatting,
// the dead band and edge flow attributes. No DOM, no vis-network —
// unit-testable alone.

// W → kW → MW ladder. The one implementation: metrics-store.js's
// fmtValue() delegates its power branch here, so every power readout
// in the app scales identically.
export function formatScaled(value, unit) {
  if (value == null || !Number.isFinite(value)) return "—";
  const a = Math.abs(value);
  if (a >= 1e6) return `${(value / 1e6).toFixed(2)} M${unit}`;
  if (a >= 1e3) return `${(value / 1e3).toFixed(2)} k${unit}`;
  return `${value.toFixed(1)} ${unit}`;
}

// The "nothing is flowing" threshold shared by the edge flow and the
// pill colours: 1 % of the site's largest rated bound, never under
// 50 W. Falls back to a 10 kW site when nothing is rated.
export function deadBandW(siteMaxRatedW) {
  const max = siteMaxRatedW > 0 ? siteMaxRatedW : 10_000;
  return Math.max(0.01 * max, 50);
}

// What edgeFlow() returns below the dead band: the rest look.
export const DEAD_FLOW = Object.freeze({ direction: "dead", width: 1.5 });

// Flow attributes for a parent→child edge. `childPowerW` is the
// child's active power (consumption-positive); the edge's share is
// 1/parentCount (the meter aggregation rule, so parallel paths
// split visually too). `direction` picks the edge colour by the
// same import/export/dead rule the pills use, and `width` is the
// magnitude: from the rest 1.5 px up to 6 px on a square-root scale
// against the site's largest rating. Below the dead band the edge
// is dead, so dead legs look dead.
export function edgeFlow(childPowerW, parentCount, siteMaxRatedW) {
  const max = siteMaxRatedW > 0 ? siteMaxRatedW : 10_000;
  const flow = (childPowerW ?? 0) / Math.max(parentCount, 1);
  const dead = deadBandW(siteMaxRatedW);
  if (!Number.isFinite(flow) || Math.abs(flow) < dead) return DEAD_FLOW;
  const norm = Math.min(1, Math.sqrt(Math.abs(flow) / max));
  return {
    direction: flow < 0 ? "export" : "import",
    width: Math.min(6, Math.max(1.5, 1 + 5 * norm)),
  };
}

