// The dock strips' arithmetic, kept apart from the shell that runs
// it: how the tiles split a strip between them, what order to store
// after a tile moves, and how big a strip may be. Pure functions over
// plain values — no document, no storage — so side-panel.js loads
// what it needs, hands it in, and stores what comes back, and
// tools/panel-dock-test.mjs can check the model with no DOM at all.

// Shares for exactly `names`, summing to 1: each name absent from
// `stored` (or stored as nothing usable) takes its 1/n, and the
// stored ones scale down to the rest in their stored proportions. So
// docking a third tile into an evenly split strip gives it a third
// and leaves the other two a third each, whatever they were.
export function normalizedShares(stored, names) {
  const known = names.filter((n) => Number.isFinite(stored[n]) && stored[n] > 0);
  const knownTotal = known.reduce((a, n) => a + stored[n], 0);
  const out = {};
  for (const n of names) {
    out[n] = known.includes(n) ? (stored[n] / knownTotal) * (known.length / names.length) : 1 / names.length;
  }
  // Sums to 1 by construction; this only guards against a rounding
  // drift or a stored share that slipped through the filter above.
  const total = Object.values(out).reduce((a, b) => a + b, 0) || 1;
  for (const n of names) out[n] = out[n] / total;
  return out;
}

// The order to store after a change: the names present now, in their
// current order, then every stored name no longer present, so a tile
// that left keeps an entry to come back to.
export function mergeOrder(present, stored) {
  return [...present, ...stored.filter((n) => !present.includes(n))];
}

// A strip's size, held between `min` and `maxFrac` of `bound` — the
// extent it is measured against, live, since a size saved on a bigger
// window may be most of a small one. The floor wins over the ceiling,
// as in the drag clamp; a size that is not a number at all takes
// `fallback`.
export function clampStripSize(size, { min, maxFrac, fallback }, bound) {
  const want = Number.isFinite(size) ? size : fallback;
  return Math.max(min, Math.min(bound * maxFrac, want));
}
