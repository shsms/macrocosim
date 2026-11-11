// The microgrid's unsaved chip: none, "edited here" or "in memory only".
// Run: node tools/unsaved-chip-test.mjs   (exits non-zero on failure)
import assert from "node:assert/strict";
import { unsavedChip } from "../ui-assets/unsaved-chip.js";

assert.equal(unsavedChip({ unsaved: false, source: "/srv/site.json" }), "");
assert.equal(unsavedChip({ unsaved: false, source: null }), "");

// A file-booted microgrid changed since: the file name, escaped, and no claim
// that saving the file brings anything back.
const edited = unsavedChip({ unsaved: true, source: "/srv/topo/<north>.json" });
assert.match(edited, /class="mg-chip edited"/);
assert.match(edited, /title="edited here: &lt;north&gt;\.json does not have these changes; export to keep them"/);
assert.doesNotMatch(edited, /saving/);
assert.match(unsavedChip({ unsaved: true, source: "C:\\topo\\south.json" }), /south\.json does not have/);

// A microgrid made in the UI or created by a script's declaration has no source.
assert.equal(
  unsavedChip({ unsaved: true, source: null }),
  '<span class="mg-chip unsaved" title="in memory only: export to keep it">in memory only</span>',
);

console.log("unsaved-chip: all assertions passed");
