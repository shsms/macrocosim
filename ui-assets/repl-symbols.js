// The REPL's knowledge of the names the interpreter defines: the completion
// list and each name's signature and docstring, from /api/symbols, plus the
// pure helpers the signature hint uses. No DOM, so tools/repl-symbols-test.mjs
// imports it under plain node.

import { escapeHtml } from "./html.js";

// The names from /api/symbols, refreshed by `refresh`. `fetchJson` is (url) =>
// Promise of the parsed body; it throws on a failed request.
export function createSymbolStore(fetchJson) {
  let byName = new Map();
  let reads = 0; // reads started
  let applied = 0; // the newest read whose list is in use
  return {
    // Reads the list again. A failed read keeps the last list.
    async refresh() {
      const read = ++reads;
      try {
        const { symbols } = await fetchJson("/api/symbols");
        // A read older than the list in use is dropped.
        if (read > applied) {
          applied = read;
          byName = new Map(symbols.map((s) => [s.name, s]));
        }
      } catch (_) {
        // The REPL still works without completion; the next focus or eval tries
        // again.
      }
    },
    // Up to `limit` names that start with `prefix`, in name order. A number
    // completes to nothing, so typing one never offers 1+ or 1-.
    complete(prefix, limit = 12) {
      if (!prefix || /^[+-]?(\d+\.?\d*|\.\d+)$/.test(prefix)) return [];
      const out = [];
      for (const name of byName.keys()) {
        if (name.startsWith(prefix)) {
          out.push(name);
          if (out.length === limit) break;
        }
      }
      return out;
    },
    lookup(name) {
      return byName.get(name);
    },
  };
}

// The call the cursor is in: the head of the innermost open form and the index
// of the argument the cursor is on (-1 on the head itself). Strings and
// comments are skipped, a nested form or a string is one argument, and a quote
// belongs to what follows it. null outside any form, when the form has no head
// yet, or when its head is a form or a string.
export function callAtCursor(text, cursor) {
  // The innermost form still open at the cursor.
  const open = [];
  for (let i = 0; i < cursor; i++) {
    const c = text[i];
    if (c === '"') i = stringEnd(text, i, cursor);
    else if (c === ";") i = lineEnd(text, i, cursor);
    else if (c === "(") open.push(i);
    else if (c === ")") open.pop();
  }
  if (!open.length) return null;
  const start = open[open.length - 1] + 1;
  // Count the form's elements up to the cursor.
  let count = 0;
  let inElement = false;
  let headStart = -1;
  let headEnd = -1;
  for (let i = start; i < cursor; i++) {
    const c = text[i];
    if (/\s/.test(c)) {
      inElement = false;
      continue;
    }
    if (c === ";") {
      i = lineEnd(text, i, cursor);
      inElement = false;
      continue;
    }
    if (!inElement) {
      count++;
      inElement = true;
      if (count === 1 && (c === "(" || c === '"')) return null;
      if (count === 1) headStart = i;
    }
    if (c === '"') i = stringEnd(text, i, cursor);
    else if (c === "(") i = formEnd(text, i, cursor);
    if (count === 1) headEnd = i + 1;
  }
  if (count === 0) return null;
  const head = text.slice(headStart, headEnd);
  return { head, argIndex: (inElement ? count - 1 : count) - 1 };
}

// The index of the string's closing quote, for the string whose opening quote
// is at `i`, or `limit` - 1 when it does not close before `limit`.
function stringEnd(text, i, limit) {
  for (let j = i + 1; j < limit; j++) {
    if (text[j] === "\\") j++;
    else if (text[j] === '"') return j;
  }
  return limit - 1;
}

// The index of the newline that ends the comment at `i`, or `limit` - 1.
function lineEnd(text, i, limit) {
  const j = text.indexOf("\n", i);
  return j === -1 || j >= limit ? limit - 1 : j;
}

// The index of the paren that closes the form opening at `i`, or `limit` - 1.
function formEnd(text, i, limit) {
  let depth = 0;
  for (let j = i; j < limit; j++) {
    const c = text[j];
    if (c === '"') j = stringEnd(text, j, limit);
    else if (c === ";") j = lineEnd(text, j, limit);
    else if (c === "(") depth++;
    else if (c === ")" && --depth === 0) return j;
  }
  return limit - 1;
}

// The index in `params` of the parameter argument `argIndex` fills, or -1 for
// none. Past the positional parameters, a rest or key parameter takes every
// argument.
export function paramFor(params, argIndex) {
  if (argIndex < 0) return -1;
  const positional = params.filter((p) => p.position === "required" || p.position === "optional");
  if (argIndex < positional.length) return params.indexOf(positional[argIndex]);
  return params.findIndex((p) => p.position === "rest" || p.position === "key");
}

// The hint line for `symbol` with the cursor on argument `argIndex`: its
// signature, the current parameter marked, then its docstring's first line. ""
// when it has no signature.
export function hintHtml(symbol, argIndex) {
  const sig = symbol.signature;
  if (!sig) return "";
  const param = sig.params[paramFor(sig.params, argIndex)];
  const text = param
    ? `${escapeHtml(sig.text.slice(0, param.start))}<span class="repl-hint-arg">${escapeHtml(sig.text.slice(param.start, param.end))}</span>${escapeHtml(sig.text.slice(param.end))}`
    : escapeHtml(sig.text);
  const summary = symbol.doc?.split("\n", 1)[0];
  const doc = summary ? `<span class="repl-hint-doc">${escapeHtml(summary)}</span>` : "";
  return `<span class="repl-hint-sig">${text}</span>${doc}`;
}
