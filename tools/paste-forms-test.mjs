// Unit tests for the clipboard paste's pure form builder
// (ui-assets/paste-forms.js): the let* the editor evals to clone a
// copied subgraph.
// Run: node tools/paste-forms-test.mjs   (exits non-zero on failure)
//
// paste-forms.js imports nothing and touches no DOM, so no shim is
// needed.
import assert from "node:assert/strict";

const { makeFnFor, pasteSource } = await import(new URL("../ui-assets/paste-forms.js", import.meta.url));

const c = (id, category, extra = {}) => ({ id, category, hidden: false, operational_mode: undefined, ...extra });
const pos = (src, needle) => {
  const i = src.indexOf(needle);
  assert.notEqual(i, -1, `${needle} missing in ${src}`);
  return i;
};

// ── makeFnFor ───────────────────────────────────────────────────
assert.equal(makeFnFor(c(1, "inverter", { subtype: "solar" })), "make-solar-inverter");
assert.equal(makeFnFor(c(1, "inverter", { subtype: "battery" })), "make-battery-inverter");
assert.equal(makeFnFor(c(1, "battery")), "make-battery");
assert.equal(makeFnFor(c(1, "steam-boiler")), "make-steam-boiler");
assert.equal(makeFnFor(c(1, "unicorn")), null);

// ── pasteSource: shape ──────────────────────────────────────────
// A lone component with no edges: one binding, a `t` body.
assert.equal(pasteSource({ components: [c(5, "meter")], edges: [] }), "(let* ((m5 (make-meter))) t)");
// Flags ride along on the binding.
assert.equal(
  pasteSource({ components: [c(5, "meter", { hidden: true, operational_mode: "autonomous" })], edges: [] }),
  "(let* ((m5 (make-meter :hidden t :operational-mode 'autonomous))) t)",
);
// `unspecified` is the absence of a mode, not a mode to clone.
assert.equal(
  pasteSource({ components: [c(5, "meter", { operational_mode: "unspecified" })], edges: [] }),
  "(let* ((m5 (make-meter))) t)",
);
// Edges become connects in the body.
assert.equal(
  pasteSource({ components: [c(4, "battery"), c(3, "inverter", { subtype: "battery" })], edges: [[3, 4]] }),
  "(let* ((m4 (make-battery)) (m3 (make-battery-inverter))) (connect m3 m4))",
);

console.log("paste-forms tests: PASS");
