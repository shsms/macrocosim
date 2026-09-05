// Unit tests for the floating panels' pure geometry
// (ui-assets/panel-geometry.js): where an unplaced card cascades to.
// Run: node tools/panel-geometry-test.mjs   (exits non-zero on failure)
//
// The module imports nothing and touches neither the document nor
// storage, so this needs no DOM shim.
import assert from "node:assert/strict";

const { cascadeColumn, cascadeSlot } = await import(new URL("../ui-assets/panel-geometry.js", import.meta.url));

// ── cascadeSlot ─────────────────────────────────────────────────
// Nothing open: the first slot.
assert.equal(cascadeSlot([], 40, 32), 40);
// Slots fill in order.
assert.equal(cascadeSlot([40], 40, 32), 72);
assert.equal(cascadeSlot([40, 72], 40, 32), 104);
// A freed slot is reused before a new one is opened: the card at 40
// closed, the ones at 72 and 104 stayed.
assert.equal(cascadeSlot([72, 104], 40, 32), 40);
assert.equal(cascadeSlot([40, 104], 40, 32), 72);
// A card nudged a few pixels still holds its slot.
assert.equal(cascadeSlot([40, 80], 40, 32), 104);
// Just inside half a step still holds the slot.
assert.equal(cascadeSlot([25], 40, 32), 72);
// Exactly half a step away holds neither slot.
assert.equal(cascadeSlot([56], 40, 32), 40);
// A card dragged well away from any slot holds none.
assert.equal(cascadeSlot([200], 40, 32), 40);
// Order of `taken` does not matter.
assert.equal(cascadeSlot([104, 40, 72], 40, 32), 136);

// ── cascadeColumn ─────────────────────────────────────────────────
const card = (pos, extra = {}) => ({ pos, dock: null, ...extra });
const top = (dy, dx = 0) => ({ dx, dy, bottom: false });
// Floating, top-anchored cards near the column's x claim their dy.
assert.deepEqual(cascadeColumn([card(top(40)), card(top(72, 8))], 16), [40, 72]);
// A docked card is a tile, not a column card.
assert.deepEqual(cascadeColumn([card(top(40), { dock: "bottom" })], 16), []);
// A bottom-anchored card sits in the bottom row, even near the
// column's x.
assert.deepEqual(cascadeColumn([card({ dx: -8, dy: -40, bottom: true })], 16), []);
// A card dragged sideways out of the column holds no slot.
assert.deepEqual(cascadeColumn([card(top(40, -300))], 16), []);
// Exactly the drift is out; just inside is in.
assert.deepEqual(cascadeColumn([card(top(40, 16)), card(top(72, -15))], 16), [72]);
// A missing record is skipped.
assert.deepEqual(cascadeColumn([undefined, card(top(40))], 16), [40]);

console.log("panel-geometry: ok");
