// The REPL's command history and its Emacs keys, as plain functions over the
// input's text and caret: repl.js reads them off the textarea and applies what
// comes back. No DOM here, so tools/repl-keys-test.mjs runs it under bare node.

// The commands run so far, oldest first, and a walk through them. A walk starts
// at the input's own text (the draft) and keeps whatever the user typed into
// each entry it passed, the way readline does, until the next push. It keeps
// the newest `cap` commands of up to `maxChars` each, which bounds what it
// stores in the browser.
export class ReplHistory {
  constructor(entries = [], cap = 500, maxChars = 16 * 1024) {
    this.cap = cap;
    this.maxChars = maxChars;
    this.entries = entries.slice(-cap);
    this.reset();
  }

  reset() {
    this.index = this.entries.length;
    this.edits = new Map();
  }

  // Records a run command, unless it is blank, too long, or repeats the newest
  // entry.
  push(src) {
    if (src.trim() && src.length <= this.maxChars && src !== this.entries.at(-1)) {
      this.entries.push(src);
      if (this.entries.length > this.cap) this.entries.shift();
    }
    this.reset();
  }

  // The text one step older (dir -1) or newer (+1) than where the walk is,
  // given the input's current text; null when the walk is already at that end.
  step(dir, current) {
    const next = this.index + dir;
    if (next < 0 || next > this.entries.length) return null;
    this.edits.set(this.index, current);
    this.index = next;
    return this.edits.get(next) ?? this.entries[next];
  }
}

const CTRL = {
  a: "beginning-of-line",
  e: "end-of-line",
  f: "forward-char",
  b: "backward-char",
  p: "previous-line",
  n: "next-line",
  d: "delete-char",
  k: "kill-line",
  y: "yank",
};
// Meta keys by the character the layout types for them.
const META = {
  f: "forward-word",
  b: "backward-word",
  d: "kill-word",
  p: "history-previous",
  n: "history-next",
  "<": "beginning-of-buffer",
  ">": "end-of-buffer",
};
// The character a physical key types on a US layout, for when Option on a Mac
// types another character (ƒ for f) or starts a dead key.
const usCharacter = (code, shift) =>
  shift ? { Comma: "<", Period: ">" }[code] : /^Key([A-Z])$/.exec(code)?.[1].toLowerCase();

// The Emacs command a keydown asks for, or null to leave the key to the
// browser. Ctrl and Alt each count only alone: Ctrl+Alt is AltGr on many
// layouts, and Cmd is always the browser's, as is Ctrl+Shift.
export function emacsCommand(e) {
  if (e.metaKey || (e.ctrlKey && e.altKey)) return null;
  if (e.ctrlKey) return !e.shiftKey && e.key.length === 1 ? (CTRL[e.key.toLowerCase()] ?? null) : null;
  if (!e.altKey) return null;
  if (e.key === "Backspace") return e.shiftKey ? null : "backward-kill-word";
  // A printable ASCII character is what the layout types: Shift is part of it.
  if (/^[ -~]$/.test(e.key)) return META[e.key] ?? null;
  return META[usCharacter(e.code, e.shiftKey)] ?? null;
}

const lineStart = (v, i) => v.lastIndexOf("\n", i - 1) + 1;
const lineEnd = (v, i) => {
  const n = v.indexOf("\n", i);
  return n === -1 ? v.length : n;
};
// Words are letters and digits, as in Emacs: `-`, `:` and parens split them.
const isWord = (c) => /[\p{L}\p{N}]/u.test(c);
function wordEnd(v, i) {
  while (i < v.length && !isWord(v[i])) i++;
  while (i < v.length && isWord(v[i])) i++;
  return i;
}
function wordStart(v, i) {
  while (i > 0 && !isWord(v[i - 1])) i--;
  while (i > 0 && isWord(v[i - 1])) i--;
  return i;
}
// The caret on the line starting at `start`, at the column it has now, clamped
// to that line's end.
const sameColumn = (v, caret, start) => Math.min(start + caret - lineStart(v, caret), lineEnd(v, start));

// Runs the editing commands, holding the kill ring (one entry, as C-y only ever
// yanks the last kill). Kills back to back join into one, in the order the text
// had. A kill joins only where the one before left the text: a click that moved
// the caret, or typing or a paste that changed the text, starts a fresh kill.
// interrupt() is the caller's note that some other key came in between.
export class EmacsKeys {
  constructor() {
    this.killed = "";
    // Where the last kill left the caret and the text's length, while a
    // following kill may join it; else null.
    this.chain = null;
  }

  interrupt() {
    this.chain = null;
  }

  // What the editing command `cmd` does to the text `v` with the caret at
  // `caret`: the new caret, and the edit, if any, as { start, end, text } over
  // the old text. The history commands are the caller's, not this one's.
  run(cmd, v, caret) {
    const chain = this.chain;
    this.chain = null;
    const move = (to) => ({ caret: to });
    // A kill with nothing to take leaves the kill ring as it was.
    const kill = (start, end) => {
      if (start === end) return move(caret);
      const text = v.slice(start, end);
      if (chain?.caret !== caret || chain.length !== v.length) this.killed = text;
      else this.killed = start < caret ? text + this.killed : this.killed + text;
      this.chain = { caret: start, length: v.length - text.length };
      return { caret: start, replace: { start, end, text: "" } };
    };
    switch (cmd) {
      case "beginning-of-line":
        return move(lineStart(v, caret));
      case "end-of-line":
        return move(lineEnd(v, caret));
      case "forward-char":
        return move(Math.min(caret + 1, v.length));
      case "backward-char":
        return move(Math.max(caret - 1, 0));
      case "previous-line": {
        const start = lineStart(v, caret);
        return move(start === 0 ? 0 : sameColumn(v, caret, lineStart(v, start - 1)));
      }
      case "next-line": {
        const end = lineEnd(v, caret);
        return move(end === v.length ? end : sameColumn(v, caret, end + 1));
      }
      case "beginning-of-buffer":
        return move(0);
      case "end-of-buffer":
        return move(v.length);
      case "forward-word":
        return move(wordEnd(v, caret));
      case "backward-word":
        return move(wordStart(v, caret));
      case "delete-char":
        return caret < v.length ? { caret, replace: { start: caret, end: caret + 1, text: "" } } : move(caret);
      case "kill-word":
        return kill(caret, wordEnd(v, caret));
      case "backward-kill-word":
        return kill(wordStart(v, caret), caret);
      case "kill-line": {
        const end = lineEnd(v, caret);
        // Only blanks left on the line: kill through the newline too.
        const through = /^[ \t]*$/.test(v.slice(caret, end)) && end < v.length ? end + 1 : end;
        return kill(caret, through);
      }
      case "yank":
        return { caret: caret + this.killed.length, replace: { start: caret, end: caret, text: this.killed } };
      default:
        throw new Error(`unknown Emacs command ${cmd}`);
    }
  }
}
