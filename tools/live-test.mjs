// Unit tests for the live overlay's pure edge-flow helpers
// (ui-assets/live.js): how a child's power becomes the colour and
// width an edge shows.
// Run: node tools/live-test.mjs   (exits non-zero on failure)
//
// live.js imports nothing and touches no DOM, so no shim is needed.
import assert from "node:assert/strict";

const { DEAD_FLOW, deadBandW, edgeFlow } = await import(new URL("../ui-assets/live.js", import.meta.url));

// ── edgeFlow ────────────────────────────────────────────────────
// Below the dead band the edge is dead: rest width, no direction.
assert.deepEqual(DEAD_FLOW, { direction: "dead", width: 1.5 });
assert.deepEqual(edgeFlow(10, 1, 30000), DEAD_FLOW);
assert.deepEqual(edgeFlow(null, 1, 30000), DEAD_FLOW);
assert.deepEqual(edgeFlow(Number.NaN, 1, 30000), DEAD_FLOW);
// Consumption-positive: the direction picks the edge colour.
assert.equal(edgeFlow(5000, 1, 30000).direction, "import");
assert.equal(edgeFlow(-5000, 1, 30000).direction, "export");
// Strength is the line width: from the rest 1.5 px up to 6 px on a
// square-root scale against the site's largest rating.
assert.equal(edgeFlow(30000, 1, 30000).width, 6);
assert.equal(edgeFlow(7500, 1, 30000).width, 3.5); // sqrt(0.25) = 0.5 → 1 + 5 * 0.5
assert.equal(edgeFlow(-10e6, 1, 30000).width, 6);
// The dead band is the threshold itself (>= not >): a watt under it
// is dead, on it the edge is live at the rest width.
assert.deepEqual(edgeFlow(deadBandW(30000) - 1, 1, 30000), DEAD_FLOW);
assert.equal(edgeFlow(deadBandW(30000), 1, 30000).direction, "import");
assert.equal(edgeFlow(deadBandW(30000), 1, 30000).width, 1.5);
// Parallel parents split the child's flow, so the width drops with
// the share.
assert.ok(edgeFlow(30000, 2, 30000).width < edgeFlow(30000, 1, 30000).width);
assert.equal(edgeFlow(30000, 0, 30000).width, 6); // zero parents treated as 1
// With nothing rated the 10 kW fallback is the reference.
assert.equal(edgeFlow(10000, 1, 0).width, 6);
assert.equal(edgeFlow(2500, 1, 0).width, 3.5);

console.log("live-test: all assertions passed");
