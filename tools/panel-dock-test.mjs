// Unit tests for the dock strips' pure model (ui-assets/strip-model.js):
// the share split between tiles, the stored-order merge, and the strip
// size clamp.
// Run: node tools/panel-dock-test.mjs   (exits non-zero on failure)
//
// The module imports nothing and touches neither the document nor
// storage, so this needs no DOM shim: side-panel.js does the loading
// and storing, and hands these functions plain values.
import assert from "node:assert/strict";

const { normalizedShares, mergeOrder, clampStripSize } = await import(
  new URL("../ui-assets/strip-model.js", import.meta.url)
);

const near = (a, b, what) => assert.ok(Math.abs(a - b) < 1e-9, `${what}: ${a} vs ${b}`);
const sumsToOne = (shares) => near(Object.values(shares).reduce((a, b) => a + b, 0), 1, "total");

// ── normalizedShares ────────────────────────────────────────────
// Nothing stored: an even split.
{
  const s = normalizedShares({}, ["a", "b", "c"]);
  for (const n of ["a", "b", "c"]) near(s[n], 1 / 3, n);
  sumsToOne(s);
}
// Every tile known: the stored proportions survive, normalized.
{
  const s = normalizedShares({ a: 0.5, b: 0.25, c: 0.25 }, ["a", "b", "c"]);
  near(s.a, 0.5, "a");
  near(s.b, 0.25, "b");
  near(s.c, 0.25, "c");
  sumsToOne(s);
}
// A stored share for a tile that is not in the strip does not count:
// the two present ones still take the whole strip.
{
  const s = normalizedShares({ a: 0.7, b: 0.3, gone: 5 }, ["a", "b"]);
  near(s.a, 0.7, "a");
  near(s.b, 0.3, "b");
  sumsToOne(s);
}
// Mixed: the two known tiles keep their 0.7/0.3 proportion inside the
// two thirds left to them, and the new tile takes its third.
{
  const s = normalizedShares({ a: 0.7, b: 0.3 }, ["a", "b", "c"]);
  near(s.a, 0.7 * (2 / 3), "a");
  near(s.b, 0.3 * (2 / 3), "b");
  near(s.c, 1 / 3, "c");
  sumsToOne(s);
}
// A stored zero, negative or non-numeric share is no share at all:
// the tile is unknown and takes its 1/n like any newcomer.
for (const bad of [0, -0.4, Number.NaN, Number.POSITIVE_INFINITY, null, "0.5"]) {
  const s = normalizedShares({ a: 0.8, b: bad }, ["a", "b"]);
  near(s.b, 0.5, `bad share ${String(bad)}`);
  near(s.a, 0.5, `partner of bad share ${String(bad)}`);
  sumsToOne(s);
}
// A single tile takes the whole strip either way.
near(normalizedShares({}, ["a"]).a, 1, "lone unknown");
near(normalizedShares({ a: 0.2 }, ["a"]).a, 1, "lone known");

// ── mergeOrder ──────────────────────────────────────────────────
// A tile floated out of the strip keeps its slot at the end.
assert.deepEqual(mergeOrder(["a", "c"], ["a", "b", "c"]), ["a", "c", "b"]);
// Present order wins over the stored one — the DOM is the record.
assert.deepEqual(mergeOrder(["c", "a"], ["a", "b", "c"]), ["c", "a", "b"]);
// Names never stored just appear.
assert.deepEqual(mergeOrder(["a", "d"], ["a", "b"]), ["a", "d", "b"]);
assert.deepEqual(mergeOrder(["a", "b"], []), ["a", "b"]);
// An empty strip still remembers what was in it.
assert.deepEqual(mergeOrder([], ["a", "b"]), ["a", "b"]);
assert.deepEqual(mergeOrder([], []), []);

// ── clampStripSize ──────────────────────────────────────────────
const cfg = { min: 120, maxFrac: 0.8, fallback: 260 };
// Between the two bounds: the stored size, untouched.
assert.equal(clampStripSize(300, cfg, 1000), 300);
// Above the ceiling: cut back to maxFrac of the bound.
assert.equal(clampStripSize(900, cfg, 1000), 800);
// Below the floor: lifted to the minimum.
assert.equal(clampStripSize(40, cfg, 1000), 120);
// A window too short for the floor: the floor wins over the ceiling,
// so the strip stays usable rather than collapsing.
assert.equal(clampStripSize(100, cfg, 100), 120);
assert.equal(clampStripSize(900, cfg, 100), 120);
// Nothing stored, or nonsense stored: the default, clamped the same.
assert.equal(clampStripSize(Number.NaN, cfg, 1000), 260);
assert.equal(clampStripSize(undefined, cfg, 1000), 260);
assert.equal(clampStripSize(Number.POSITIVE_INFINITY, cfg, 1000), 260);
assert.equal(clampStripSize(Number.NaN, cfg, 200), 160);

console.log("panel-dock: all tests passed");
