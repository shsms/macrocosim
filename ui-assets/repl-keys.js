// The REPL's command history and its Emacs keys, as plain functions over the
// input's text and caret: repl.js reads them off the textarea and applies what
// comes back. No DOM here, so tools/repl-keys-test.mjs runs it under bare node.

// The commands run so far, oldest first, and a walk through them. A walk starts
// at the input's own text (the draft) and keeps whatever the user typed into
// each entry it passed, the way readline does, until the next push.
export class ReplHistory {
  constructor(entries = [], cap = 500) {
    this.cap = cap;
    this.entries = entries.slice(-cap);
    this.reset();
  }

  reset() {
    this.index = this.entries.length;
    this.edits = new Map();
  }

  // Records a run command, unless it is blank or repeats the newest entry.
  push(src) {
    if (src.trim() && src !== this.entries.at(-1)) {
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

export const onFirstLine = (value, caret) => !value.slice(0, caret).includes("\n");
export const onLastLine = (value, caret) => !value.slice(caret).includes("\n");

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
// Meta keys go by e.code, the physical key: macOS's Option turns e.key into
// another character (ƒ for f).
const META = {
  KeyF: "forward-word",
  KeyB: "backward-word",
  KeyD: "kill-word",
  KeyP: "history-previous",
  KeyN: "history-next",
  Backspace: "backward-kill-word",
};

// The Emacs command a keydown asks for, or null to leave the key to the
// browser. Ctrl and Alt each count only alone: Ctrl+Alt is AltGr on many
// layouts, and Cmd is always the browser's.
export function emacsCommand(e) {
  if (e.metaKey || (e.ctrlKey && e.altKey)) return null;
  if (e.ctrlKey) return e.key.length === 1 ? (CTRL[e.key.toLowerCase()] ?? null) : null;
  if (!e.altKey) return null;
  if (e.shiftKey && (e.key === "<" || e.code === "Comma")) return "beginning-of-buffer";
  if (e.shiftKey && (e.key === ">" || e.code === "Period")) return "end-of-buffer";
  if (e.shiftKey) return null;
  return META[e.code] ?? (e.key === "Backspace" ? META.Backspace : null);
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
// had; interrupt() is the caller's note that some other key came in between.
export class EmacsKeys {
  constructor() {
    this.killed = "";
    this.killing = false;
  }

  interrupt() {
    this.killing = false;
  }

  // What the editing command `cmd` does to the text `v` with the caret at
  // `caret`: the new caret, and the edit, if any, as { start, end, text } over
  // the old text. The history commands are the caller's, not this one's.
  run(cmd, v, caret) {
    const killing = this.killing;
    this.killing = false;
    const move = (to) => ({ caret: to });
    const kill = (start, end) => {
      const text = v.slice(start, end);
      if (!killing) this.killed = text;
      else this.killed = start < caret ? text + this.killed : this.killed + text;
      this.killing = true;
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
