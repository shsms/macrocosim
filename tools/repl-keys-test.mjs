// Unit tests for the REPL's history and Emacs keys (ui-assets/repl-keys.js):
// stepping through history with a draft, the line edges that hand Up/Down
// to history, the key → command mapping, and each command's edit.
// Run: node tools/repl-keys-test.mjs   (exits non-zero on failure)
//
// repl-keys.js imports nothing and touches no DOM, so no shim is needed.
import assert from "node:assert/strict";

const { EmacsKeys, ReplHistory, emacsCommand, onFirstLine, onLastLine } = await import(
  new URL("../ui-assets/repl-keys.js", import.meta.url)
);

// ── ReplHistory ─────────────────────────────────────────────────
{
  const h = new ReplHistory();
  // Nothing to recall.
  assert.equal(h.step(-1, "draft"), null);
  h.push("(a)");
  h.push("(b)");
  // A repeat of the newest entry is not stored twice.
  h.push("(b)");
  assert.deepEqual(h.entries, ["(a)", "(b)"]);
  // Older, then the oldest, then nothing further back.
  assert.equal(h.step(-1, "draft"), "(b)");
  assert.equal(h.step(-1, "(b)"), "(a)");
  assert.equal(h.step(-1, "(a)"), null);
  // Newer brings back the draft that was there before Up.
  assert.equal(h.step(1, "(a)"), "(b)");
  assert.equal(h.step(1, "(b)"), "draft");
  assert.equal(h.step(1, "draft"), null);
}
{
  // An edit to a recalled entry survives stepping away and back, and a push
  // starts the next walk from the newest entry with the edits dropped.
  const h = new ReplHistory(["(a)", "(b)"]);
  assert.equal(h.step(-1, ""), "(b)");
  assert.equal(h.step(-1, "(b 2)"), "(a)");
  assert.equal(h.step(1, "(a)"), "(b 2)");
  h.push("(b 2)");
  assert.deepEqual(h.entries, ["(a)", "(b)", "(b 2)"]);
  assert.equal(h.step(-1, ""), "(b 2)");
  assert.equal(h.step(-1, "(b 2)"), "(b)");
}
{
  // The oldest entries fall off past the cap; blank input is not stored.
  const h = new ReplHistory(["1", "2", "3"], 3);
  h.push("4");
  h.push("   ");
  assert.deepEqual(h.entries, ["2", "3", "4"]);
  // A stored list that is longer than the cap is trimmed on load.
  assert.deepEqual(new ReplHistory(["1", "2", "3", "4"], 2).entries, ["3", "4"]);
}

// ── line edges ──────────────────────────────────────────────────
assert.equal(onFirstLine("(a\n b)", 2), true);
assert.equal(onFirstLine("(a\n b)", 3), false);
assert.equal(onLastLine("(a\n b)", 2), false);
assert.equal(onLastLine("(a\n b)", 3), true);
assert.equal(onLastLine("(a)", 0), true);

// ── emacsCommand ────────────────────────────────────────────────
const key = (k, mods = {}) => ({ key: k, code: "", ctrlKey: false, altKey: false, metaKey: false, shiftKey: false, ...mods });
const ctrl = (k) => key(k, { ctrlKey: true });
// Meta goes by the physical key: macOS's Option turns M-f's key into ƒ.
const alt = (letter) => key("ƒ", { altKey: true, code: `Key${letter.toUpperCase()}` });
assert.equal(emacsCommand(ctrl("a")), "beginning-of-line");
assert.equal(emacsCommand(ctrl("e")), "end-of-line");
assert.equal(emacsCommand(ctrl("f")), "forward-char");
assert.equal(emacsCommand(ctrl("b")), "backward-char");
assert.equal(emacsCommand(ctrl("d")), "delete-char");
assert.equal(emacsCommand(ctrl("k")), "kill-line");
assert.equal(emacsCommand(ctrl("y")), "yank");
assert.equal(emacsCommand(ctrl("p")), "previous-line");
assert.equal(emacsCommand(ctrl("n")), "next-line");
assert.equal(emacsCommand(alt("f")), "forward-word");
assert.equal(emacsCommand(alt("b")), "backward-word");
assert.equal(emacsCommand(alt("d")), "kill-word");
assert.equal(emacsCommand(alt("p")), "history-previous");
assert.equal(emacsCommand(alt("n")), "history-next");
assert.equal(emacsCommand(key("Backspace", { altKey: true })), "backward-kill-word");
assert.equal(emacsCommand(key("<", { altKey: true, shiftKey: true, code: "Comma" })), "beginning-of-buffer");
assert.equal(emacsCommand(key(">", { altKey: true, shiftKey: true, code: "Period" })), "end-of-buffer");
// Caps Lock and Shift do not change a Ctrl binding's letter.
assert.equal(emacsCommand(key("A", { ctrlKey: true })), "beginning-of-line");
// Everything else stays the browser's: plain keys, unbound letters, Cmd, and
// Ctrl+Alt (AltGr on many layouts).
assert.equal(emacsCommand(key("a")), null);
assert.equal(emacsCommand(ctrl("z")), null);
assert.equal(emacsCommand(key("a", { metaKey: true })), null);
assert.equal(emacsCommand(key("a", { ctrlKey: true, altKey: true })), null);
assert.equal(emacsCommand(key("Enter", { ctrlKey: true })), null);

// ── EmacsKeys.run ───────────────────────────────────────────────
// `|` marks the caret in each case's text.
function run(cmd, marked, keys = new EmacsKeys()) {
  const caret = marked.indexOf("|");
  const value = marked.replace("|", "");
  const r = keys.run(cmd, value, caret);
  let out = value;
  if (r.replace) out = out.slice(0, r.replace.start) + r.replace.text + out.slice(r.replace.end);
  return out.slice(0, r.caret) + "|" + out.slice(r.caret);
}
assert.equal(run("beginning-of-line", "(a\n  b|c)"), "(a\n|  bc)");
assert.equal(run("end-of-line", "(a| b\n c)"), "(a b|\n c)");
assert.equal(run("forward-char", "a|b"), "ab|");
assert.equal(run("forward-char", "ab|"), "ab|");
assert.equal(run("backward-char", "|ab"), "|ab");
// Line moves keep the column, clamped to a shorter line.
assert.equal(run("previous-line", "(abc\n (d|e)"), "(ab|c\n (de)");
assert.equal(run("previous-line", "(a\n (de|)"), "(a|\n (de)");
assert.equal(run("next-line", "(a|bc\n (de)"), "(abc\n (|de)");
assert.equal(run("next-line", "(abc|\n d"), "(abc\n d|");
assert.equal(run("beginning-of-buffer", "(a\n b|)"), "|(a\n b)");
assert.equal(run("end-of-buffer", "(a|\n b)"), "(a\n b)|");
// Words are letters and digits, as in Emacs: `-` and parens split them.
assert.equal(run("forward-word", "(|make-meter :id 1)"), "(make|-meter :id 1)");
assert.equal(run("forward-word", "(make|-meter :id 1)"), "(make-meter| :id 1)");
assert.equal(run("forward-word", "(f 1|)  "), "(f 1)  |");
assert.equal(run("backward-word", "(make-meter| :id 1)"), "(make-|meter :id 1)");
assert.equal(run("backward-word", "  |(a)"), "|  (a)");
assert.equal(run("delete-char", "a|bc"), "a|c");
assert.equal(run("delete-char", "abc|"), "abc|");
assert.equal(run("kill-word", "(|make-meter)"), "(|-meter)");
assert.equal(run("backward-kill-word", "(make-meter|)"), "(make-|)");
// C-k kills to the end of the line; with only blanks left, through the newline.
assert.equal(run("kill-line", "(a| b)\n(c)"), "(a|\n(c)");
assert.equal(run("kill-line", "(a b)|  \n(c)"), "(a b)|(c)");
assert.equal(run("kill-line", "(a b)|"), "(a b)|");
{
  // Kills land in the kill ring for C-y; back-to-back kills join up, in text
  // order, and anything in between starts a fresh kill.
  const keys = new EmacsKeys();
  assert.equal(run("kill-line", "(a| b)\n(c)", keys), "(a|\n(c)");
  assert.equal(run("kill-line", "(a|\n(c)", keys), "(a|(c)");
  assert.equal(run("yank", "|", keys), " b)\n|");
  keys.interrupt();
  assert.equal(run("backward-kill-word", "(foo bar|)", keys), "(foo |)");
  assert.equal(run("backward-kill-word", "(foo |)", keys), "(|)");
  assert.equal(run("yank", "x|y", keys), "xfoo bar|y");
  keys.interrupt();
  assert.equal(run("kill-word", "(|zap)", keys), "(|)");
  assert.equal(run("yank", "|", keys), "zap|");
}
// A kill or yank reports its edit; a motion reports none.
assert.equal(new EmacsKeys().run("forward-char", "ab", 0).replace, undefined);
// run() edits; a history command, or a name emacsCommand never returns, is a
// bug in the caller, not a no-op.
assert.throws(() => new EmacsKeys().run("history-previous", "ab", 0), /history-previous/);
assert.throws(() => new EmacsKeys().run("transpose-chars", "ab", 0), /transpose-chars/);

console.log("repl-keys: all tests passed");
