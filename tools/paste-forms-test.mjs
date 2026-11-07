// Unit tests for the clipboard paste's pure spec splice
// (ui-assets/paste-forms.js): the components and connections a pasted
// subgraph adds to a spec, with ids from nextFreeIds.
// Run: node tools/paste-forms-test.mjs   (exits non-zero on failure)
//
// paste-forms.js and ids.js import nothing and touch no DOM, so no
// shim is needed.
import assert from "node:assert/strict";
import { nextFreeIds } from "../ui-assets/ids.js";
import { pasteIntoSpec, specTypeFor } from "../ui-assets/paste-forms.js";

const c = (id, category, extra = {}) => ({ id, category, hidden: false, operational_mode: undefined, ...extra });
const emptySpec = () => ({ id: 7, components: [], connections: [] });
// Paste `snap` into a fresh spec with ids from nextFreeIds over `others`.
const paste = (snap, others = []) => {
  const spec = emptySpec();
  pasteIntoSpec(spec, snap, nextFreeIds([spec, ...others], snap.components.length));
  return spec;
};

// ── specTypeFor ─────────────────────────────────────────────────
assert.equal(specTypeFor(c(1, "inverter", { subtype: "solar" })), "solar_inverter");
assert.equal(specTypeFor(c(1, "inverter", { subtype: "battery" })), "battery_inverter");
assert.equal(specTypeFor(c(1, "grid")), "grid_connection_point");
assert.equal(specTypeFor(c(1, "steam-boiler")), "steam_boiler");
assert.equal(specTypeFor(c(1, "unicorn")), null);

// ── shape ───────────────────────────────────────────────────────
// A lone component with no edges: one clone under the first free id.
assert.deepEqual(paste({ components: [c(5, "meter")], edges: [] }), {
  id: 7,
  components: [{ type: "meter", id: 5000 }],
  connections: [],
});
// Hidden and the operational mode ride along, the mode in snake case.
assert.deepEqual(
  paste({ components: [c(5, "meter", { hidden: true, operational_mode: "telemetry_only" })], edges: [] }).components,
  [{ type: "meter", id: 5000, hidden: true, operational_mode: "telemetry_only" }],
);
// `unspecified` is the absence of a mode, not a mode to clone.
assert.deepEqual(paste({ components: [c(5, "meter", { operational_mode: "unspecified" })], edges: [] }).components, [
  { type: "meter", id: 5000 },
]);
// Edges are rewritten to the new ids, and ids another microgrid uses
// are skipped.
assert.deepEqual(
  paste(
    { components: [c(4, "battery"), c(3, "inverter", { subtype: "battery" })], edges: [[3, 4]] },
    [{ components: [{ id: 5000 }] }],
  ),
  {
    id: 7,
    components: [
      { type: "battery", id: 5001 },
      { type: "battery_inverter", id: 5002 },
    ],
    connections: [[5002, 5001]],
  },
);
// Pasting keeps what the spec held.
{
  const spec = { id: 7, components: [{ type: "meter", id: 1 }], connections: [[1, 2]] };
  const ids = pasteIntoSpec(spec, { components: [c(1, "meter")], edges: [] }, nextFreeIds([spec], 1));
  assert.deepEqual(ids, [5000]);
  assert.deepEqual(spec.components, [
    { type: "meter", id: 1 },
    { type: "meter", id: 5000 },
  ]);
  assert.deepEqual(spec.connections, [[1, 2]]);
}

// ── children first ──────────────────────────────────────────────
// Copied in selection order with the inverter first, the paste still
// lists the battery before the inverter that pushes into it.
{
  const spec = paste({
    components: [c(1, "grid"), c(3, "inverter", { subtype: "battery" }), c(2, "meter"), c(4, "battery")],
    edges: [
      [1, 2],
      [2, 3],
      [3, 4],
    ],
  });
  assert.deepEqual(
    spec.components.map((x) => x.type),
    ["battery", "battery_inverter", "meter", "grid_connection_point"],
  );
  assert.deepEqual(spec.connections, [
    [5003, 5002],
    [5002, 5001],
    [5001, 5000],
  ]);
}

// A diamond (1→2, 1→3, 2→4, 3→4) plus an edge-less 5 listed between
// 2 and 3: 4 has two parents and is cloned exactly once; 5 comes
// where the sweep reaches it, which is after 1's whole subtree here.
{
  const spec = paste({
    components: [c(1, "meter"), c(2, "meter"), c(5, "meter"), c(3, "meter"), c(4, "meter")],
    edges: [
      [1, 2],
      [1, 3],
      [2, 4],
      [3, 4],
    ],
  });
  // Clone order 4, 2, 3, 1, 5 → ids 5000..5004.
  assert.equal(spec.components.length, 5);
  assert.deepEqual(spec.connections, [
    [5003, 5001],
    [5003, 5002],
    [5001, 5000],
    [5002, 5000],
  ]);
}

// An edge to a component that was not copied is dropped.
assert.deepEqual(paste({ components: [c(1, "meter")], edges: [[1, 9]] }).connections, []);

console.log("paste-forms tests: PASS");
