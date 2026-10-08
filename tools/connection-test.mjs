// The reachability model behind the "server unreachable" banner.
// Run: node tools/connection-test.mjs   (exits non-zero on failure)
import assert from "node:assert/strict";
import { createReachability } from "../ui-assets/connection.js";

const r = createReachability();
assert.equal(r.unreachable(), false, "no reports: reachable");
assert.equal(r.report(true), false, "a success changes nothing");
assert.equal(r.report(false), true, "a failure marks it unreachable");
assert.equal(r.unreachable(), true);
assert.equal(r.report(false), false, "another failure: no change");
// Any request that gets through clears it, whichever loop sent it: a loop that
// failed and then stopped (its panel closed) must not hold it down.
assert.equal(r.report(true), true, "a success marks it reachable again");
assert.equal(r.unreachable(), false);

console.log("connection-test: all assertions passed");
