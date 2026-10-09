// Unit tests for the REPL's symbol helpers (ui-assets/repl-symbols.js):
// which call the cursor is in, which parameter an argument fills, how
// the hint line renders, and the completion store; and the word at the cursor
// that completion replaces (ui-assets/repl-syntax.js).
// Run: node tools/repl-symbols-test.mjs   (exits non-zero on failure)
//
// repl-symbols.js touches no DOM, so no shim is needed.
import assert from "node:assert/strict";

const { callAtCursor, createSymbolStore, hintHtml, paramFor } = await import(
  new URL("../ui-assets/repl-symbols.js", import.meta.url)
);
const { wordAtCursor } = await import(new URL("../ui-assets/repl-syntax.js", import.meta.url));

// The cursor is where `|` is.
const at = (src) => callAtCursor(src.replace("|", ""), src.indexOf("|"));

// ── callAtCursor ────────────────────────────────────────────────
// Outside any form there is no call.
assert.equal(at("|"), null);
assert.equal(at("foo|"), null);
assert.equal(at("(f 1) |"), null);
// On the head itself the argument index is -1.
assert.deepEqual(at("(set-meter-po|"), { head: "set-meter-po", argIndex: -1 });
// A space after the head starts the first argument.
assert.deepEqual(at("(f |"), { head: "f", argIndex: 0 });
assert.deepEqual(at("(f 1|"), { head: "f", argIndex: 0 });
assert.deepEqual(at("(f 1 |"), { head: "f", argIndex: 1 });
assert.deepEqual(at("(f 1 2|)"), { head: "f", argIndex: 1 });
// A nested form counts as one argument, and the innermost open form is the
// call.
assert.deepEqual(at("(f (g 1 2) |"), { head: "f", argIndex: 1 });
assert.deepEqual(at("(f (g 1 |"), { head: "g", argIndex: 1 });
assert.deepEqual(at("(f '(1 2) |"), { head: "f", argIndex: 1 });
// Parens in a string or a comment don't count, and a string is one argument;
// the cursor inside a string is on that argument.
assert.deepEqual(at('(f "a (b" |'), { head: "f", argIndex: 1 });
assert.deepEqual(at('(f "a b|'), { head: "f", argIndex: 0 });
assert.deepEqual(at("(f ; (g\n 1 |"), { head: "f", argIndex: 1 });
assert.deepEqual(at("(; note\n f 1 |"), { head: "f", argIndex: 1 });
assert.deepEqual(at('(f "a\\"(" |'), { head: "f", argIndex: 1 });
// A form whose head is not a symbol is no call.
assert.equal(at("((f) |"), null);
assert.equal(at('("f" |'), null);
// Several lines.
assert.deepEqual(at("(make-battery\n  :id 7\n  |"), { head: "make-battery", argIndex: 2 });

// ── paramFor ────────────────────────────────────────────────────
const req = (label) => ({ label, position: "required" });
const opt = (label) => ({ label, position: "optional" });
const rest = (label) => ({ label, position: "rest" });
const key = (label) => ({ label, position: "key" });
assert.equal(paramFor([req("A"), opt("B")], 0), 0);
assert.equal(paramFor([req("A"), opt("B")], 1), 1);
// Past the last positional parameter, a rest or key parameter takes every
// argument, and without one there is no parameter.
assert.equal(paramFor([req("A"), opt("B")], 2), -1);
assert.equal(paramFor([req("A"), rest("R")], 1), 1);
assert.equal(paramFor([req("A"), rest("R")], 5), 1);
assert.equal(paramFor([key("ARGS")], 3), 0);
assert.equal(paramFor([req("A")], -1), -1);
assert.equal(paramFor([], 0), -1);

// ── hintHtml ────────────────────────────────────────────────────
const setMeterPower = {
  name: "set-meter-power",
  kind: "function",
  signature: {
    text: "(set-meter-power ID POWER-W)",
    params: [
      { label: "ID", position: "required", start: 17, end: 19 },
      { label: "POWER-W", position: "required", start: 20, end: 27 },
    ],
  },
  doc: "Make meter ID report POWER-W watts.\n\nMore text.",
};
// The current parameter is marked, and the docstring's first line follows the
// signature.
assert.equal(
  hintHtml(setMeterPower, 1),
  '<span class="repl-hint-sig">(set-meter-power ID <span class="repl-hint-arg">POWER-W</span>)</span>' +
    '<span class="repl-hint-doc">Make meter ID report POWER-W watts.</span>',
);
// On the head, or past the last parameter, nothing is marked.
assert.ok(!hintHtml(setMeterPower, -1).includes("repl-hint-arg"));
assert.ok(!hintHtml(setMeterPower, 2).includes("repl-hint-arg"));
// Text is escaped, and a name with no docstring shows the signature alone.
assert.equal(
  hintHtml({ name: "<", kind: "function", signature: { text: "(< A)", params: [] }, doc: null }, 0),
  '<span class="repl-hint-sig">(&lt; A)</span>',
);
// A variable has no signature, so no hint.
assert.equal(hintHtml({ name: "v", kind: "variable", signature: null, doc: "d" }, 0), "");

// ── createSymbolStore ───────────────────────────────────────────
const symbols = [
  { name: "set-meter-power", kind: "function", signature: null, doc: null },
  { name: "set-meter-power-factor", kind: "function", signature: null, doc: null },
  { name: "setq", kind: "special-form", signature: null, doc: null },
  { name: "1+", kind: "function", signature: null, doc: null },
];
let answer = { symbols };
const store = createSymbolStore(async (url) => {
  assert.equal(url, "/api/symbols");
  if (answer instanceof Error) throw answer;
  return answer;
});
// Empty until the first refresh.
assert.deepEqual(store.complete("set"), []);
await store.refresh();
assert.deepEqual(store.complete("set-meter"), ["set-meter-power", "set-meter-power-factor"]);
assert.deepEqual(store.complete("set", 2), ["set-meter-power", "set-meter-power-factor"]);
assert.deepEqual(store.complete("nope"), []);
// A number completes to nothing, so Enter after `:id 1` stays a newline.
for (const n of ["1", "-1", "+1", "1.", ".5", "10"]) assert.deepEqual(store.complete(n), [], n);
assert.deepEqual(store.complete("1+"), ["1+"]);
assert.deepEqual(store.complete(""), []);
assert.equal(store.lookup("setq").kind, "special-form");
assert.equal(store.lookup("nope"), undefined);
// A failed refresh keeps the last list.
answer = new Error("down");
await store.refresh();
assert.deepEqual(store.complete("setq"), ["setq"]);
// A later refresh picks up new names.
answer = { symbols: [...symbols, { name: "settle", kind: "function", signature: null, doc: null }] };
await store.refresh();
assert.deepEqual(store.complete("sett"), ["settle"]);
// A read that ends after a newer read's list is in use is dropped, so an
// older list never replaces a newer one.
const answers = [];
const raceStore = createSymbolStore(() => new Promise((resolve) => answers.push(resolve)));
const older = raceStore.refresh();
const newer = raceStore.refresh();
answers[1]({ symbols: [{ name: "newer-fn", kind: "function", signature: null, doc: null }] });
await newer;
answers[0]({ symbols: [{ name: "older-fn", kind: "function", signature: null, doc: null }] });
await older;
assert.deepEqual(raceStore.complete("newer"), ["newer-fn"]);
assert.deepEqual(raceStore.complete("older"), []);
// An older read that ends first is kept, even when the later read fails.
const calls = [];
const keepStore = createSymbolStore(() => new Promise((resolve, reject) => calls.push({ resolve, reject })));
const first = keepStore.refresh();
const second = keepStore.refresh();
calls[0].resolve({ symbols: [{ name: "first-fn", kind: "function", signature: null, doc: null }] });
await first;
calls[1].reject(new Error("down"));
await second;
assert.deepEqual(keepStore.complete("first"), ["first-fn"]);

// ── wordAtCursor ────────────────────────────────────────────────
// The word runs back to a delimiter, so names with operators or non-ASCII
// letters complete as one word.
const word = (src) => wordAtCursor({ value: src.replace("|", ""), selectionStart: src.indexOf("|") }).prefix;
assert.equal(word("(set-meter-po|"), "set-meter-po");
assert.equal(word("(1+|"), "1+");
assert.equal(word("(string=|"), "string=");
assert.equal(word("(größe|"), "größe");
assert.equal(word("(foo 'ba|"), "ba");
assert.equal(word('(foo "a|'), "a");
assert.equal(word("`(foo ,@ba|"), "ba");
assert.equal(word("(foo |"), "");

console.log("repl-symbols-test: ok");
