// Unit tests for the REPL's symbol store (ui-assets/repl-symbols.js): what it
// completes and looks up, and how a refresh changes that; and the word at the
// cursor that completion replaces (ui-assets/repl-syntax.js).
// Run: node tools/repl-symbols-test.mjs   (exits non-zero on failure)
//
// repl-symbols.js touches no DOM, so no shim is needed.
import assert from "node:assert/strict";

const { createSymbolStore } = await import(new URL("../ui-assets/repl-symbols.js", import.meta.url));
const { wordAtCursor } = await import(new URL("../ui-assets/repl-syntax.js", import.meta.url));

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
