// Unit tests for the REPL's history and Emacs keys (ui-assets/repl-keys.js):
// stepping through history with a draft, the key → command mapping, and each
// command's edit.
// Run: node tools/repl-keys-test.mjs   (exits non-zero on failure)
//
// repl-keys.js imports nothing and touches no DOM, so no shim is needed.
import assert from "node:assert/strict";

const { EmacsKeys, ReplHistory, emacsCommand } = await import(
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
{
  // By default the newest 500 commands are kept.
  const h = new ReplHistory();
  for (let i = 0; i <= 500; i++) h.push(`(${i})`);
  assert.equal(h.entries.length, 500);
  assert.equal(h.entries[0], "(1)");
  // A command longer than 16K characters runs but is not kept, so a few huge
  // pastes cannot fill the browser's storage.
  h.push(`("${"x".repeat(16 * 1024)}")`);
  assert.ok(h.entries.at(-1) === "(500)", "a command over 16K characters was kept");
  // One of exactly 16K characters is kept.
  const max = `(${"x".repeat(16 * 1024 - 2)})`;
  h.push(max);
  assert.ok(h.entries.at(-1) === max, "a command of 16K characters was dropped");
}

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
// Meta goes by the letter the layout types where it types one: Dvorak's f key
// sits where QWERTY has Y, and German has `<` on a key of its own.
assert.equal(emacsCommand(key("f", { altKey: true, code: "KeyY" })), "forward-word");
assert.equal(emacsCommand(key("u", { altKey: true, code: "KeyF" })), null);
assert.equal(emacsCommand(key("<", { altKey: true, code: "IntlBackslash" })), "beginning-of-buffer");
assert.equal(emacsCommand(key(";", { altKey: true, shiftKey: true, code: "Comma" })), null);
// A macOS dead key (Option+N on a US Mac) falls back to the physical key, as
// Option's special characters do; so does Option+Shift+Comma.
assert.equal(emacsCommand(key("Dead", { altKey: true, code: "KeyN" })), "history-next");
assert.equal(emacsCommand(key("¯", { altKey: true, shiftKey: true, code: "Comma" })), "beginning-of-buffer");
// Shift with a Meta letter is not bound.
assert.equal(emacsCommand(key("F", { altKey: true, shiftKey: true, code: "KeyF" })), null);
// Caps Lock does not change a Ctrl binding's letter; Ctrl+Shift stays the
// browser's (Ctrl+Shift+K is Firefox's web console).
assert.equal(emacsCommand(key("A", { ctrlKey: true })), "beginning-of-line");
assert.equal(emacsCommand(key("K", { ctrlKey: true, shiftKey: true })), null);
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
{
  // A kill only joins the one before it where that one left the text: a
  // click that moved the caret, or typing or a paste that changed the text,
  // starts a fresh kill, with no keydown in between.
  const keys = new EmacsKeys();
  // The caret moved.
  assert.equal(run("kill-line", "(a| b)\n(c)", keys), "(a|\n(c)");
  assert.equal(run("kill-line", "(a\n(c|)", keys), "(a\n(c|");
  assert.equal(run("yank", "|", keys), ")|");
  // The text changed after the caret, and the caret did not move.
  assert.equal(run("kill-line", "(a| b)\n(c)", keys), "(a|\n(c)");
  assert.equal(run("kill-line", "(a|Z\n(c)", keys), "(a|\n(c)");
  assert.equal(run("yank", "|", keys), "Z|");
}
{
  // A kill with nothing to take (C-k at the end, M-d at the end, M-DEL at the
  // start) leaves the kill ring as it was.
  const keys = new EmacsKeys();
  assert.equal(run("kill-word", "(|zap)", keys), "(|)");
  keys.interrupt();
  assert.equal(run("kill-line", "(a)|", keys), "(a)|");
  assert.equal(run("kill-word", "(a)|", keys), "(a)|");
  assert.equal(run("backward-kill-word", "|(a)", keys), "|(a)");
  assert.equal(run("yank", "|", keys), "zap|");
}
// A kill or yank reports its edit; a motion reports none.
assert.equal(new EmacsKeys().run("forward-char", "ab", 0).replace, undefined);
// run() edits; a history command, or a name emacsCommand never returns, is a
// bug in the caller, not a no-op.
assert.throws(() => new EmacsKeys().run("history-previous", "ab", 0), /history-previous/);
assert.throws(() => new EmacsKeys().run("transpose-chars", "ab", 0), /transpose-chars/);

console.log("repl-keys: all tests passed");
