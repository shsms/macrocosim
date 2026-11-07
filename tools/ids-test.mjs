// Unit tests for the UI's component id picker (ui-assets/ids.js).
// Run: node tools/ids-test.mjs   (exits non-zero on failure)
import assert from "node:assert/strict";
import { nextFreeIds, UI_FIRST_ID } from "../ui-assets/ids.js";

const spec = (...ids) => ({ components: ids.map((id) => ({ type: "meter", id })) });

assert.equal(UI_FIRST_ID, 5000);
// No microgrid: the first ids of the range.
assert.deepEqual(nextFreeIds([], 3), [5000, 5001, 5002]);
assert.deepEqual(nextFreeIds([], 0), []);
// Ids below the range never count, wherever they are.
assert.deepEqual(nextFreeIds([spec(1, 2, 1000, 4999)], 1), [5000]);
// An id used in ANY microgrid is skipped, and gaps are filled first.
assert.deepEqual(nextFreeIds([spec(5000, 5002), spec(5001, 5004)], 3), [5003, 5005, 5006]);
// A spec without components is fine.
assert.deepEqual(nextFreeIds([{}, spec(5000)], 1), [5001]);

console.log("ids tests: PASS");
