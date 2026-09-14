// Unit tests for the clipboard paste's pure form builder
// (ui-assets/paste-forms.js): the let* the editor evals to clone a
// copied subgraph, and the order its bindings come out in.
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

// ── pasteSource: children first ─────────────────────────────────
// Copied in selection order with the inverter first, the paste still
// binds the battery before the inverter that pushes into it — the
// tick order an authored config gets, since Lisp evaluates
// :successors before the surrounding make-*.
{
  const src = pasteSource({
    components: [
      c(1, "grid"),
      c(3, "inverter", { subtype: "battery" }),
      c(2, "meter"),
      c(4, "battery"),
    ],
    edges: [
      [1, 2],
      [2, 3],
      [3, 4],
    ],
  });
  assert.ok(pos(src, "(m4 ") < pos(src, "(m3 "), `battery 4 before inverter 3:\n${src}`);
  assert.ok(pos(src, "(m3 ") < pos(src, "(m2 "), `inverter 3 before meter 2:\n${src}`);
  assert.ok(pos(src, "(m2 ") < pos(src, "(m1 "), `meter 2 before grid 1:\n${src}`);
  // The connects still follow every binding.
  assert.ok(pos(src, "(connect") > pos(src, "(m1 "));
}

// A diamond (1→2, 1→3, 2→4, 3→4) plus an edge-less 5 listed between
// 2 and 3: 4 has two parents and is bound exactly once; 5 is emitted
// where the sweep reaches it, which is after 1's whole subtree here.
{
  const src = pasteSource({
    components: [c(1, "meter"), c(2, "meter"), c(5, "meter"), c(3, "meter"), c(4, "meter")],
    edges: [
      [1, 2],
      [1, 3],
      [2, 4],
      [3, 4],
    ],
  });
  assert.equal(src.split("(m4 ").length - 1, 1, `component 4 bound exactly once:\n${src}`);
  assert.ok(pos(src, "(m4 ") < pos(src, "(m2 "), `4 before 2:\n${src}`);
  assert.ok(pos(src, "(m4 ") < pos(src, "(m3 "), `4 before 3:\n${src}`);
  assert.ok(pos(src, "(m2 ") < pos(src, "(m1 "), `2 before 1:\n${src}`);
  assert.ok(pos(src, "(m3 ") < pos(src, "(m1 "), `3 before 1:\n${src}`);
  assert.ok(pos(src, "(m1 ") < pos(src, "(m5 "), `unconnected 5 after 1:\n${src}`);
}

// An edge to a component that was not copied is dropped: connected,
// it would name a symbol the let* never bound and abort the eval.
{
  assert.equal(pasteSource({ components: [c(1, "meter")], edges: [[1, 9]] }), "(let* ((m1 (make-meter))) t)");
}

console.log("paste-forms tests: PASS");
