// REPL panel: live syntax-highlighted Lisp input, autocomplete,
// the log-line tap above it, and the WebSocket pump that fans out
// /ws/events frames to liveCharts / the metrics store / pulseBar /
// the log panel.

import { dispatchesPanel, setStatus } from "./app.js";
import { pulseBar } from "./chrome.js";
import { onReachedAgain } from "./connection.js";
import { escapeHtml } from "./html.js";
import { errorText, getJson } from "./http.js";
import { inspectorLive, liveCharts } from "./inspect.js";
import { appendLog } from "./logs.js";
import { metricsStore } from "./metrics-store.js";
import { logUi, notify } from "./notices.js";
import { EmacsKeys, emacsCommand, ReplHistory } from "./repl-keys.js";
import { callAtCursor, createSymbolStore, hintHtml, keyCompletions } from "./repl-symbols.js";
import { indentForNewline, rainbowHighlight, wordAtCursor } from "./repl-syntax.js";
import { mgPath, readSelectedMg, readSubview } from "./routing.js";
import { closePanel } from "./side-panel.js";
import { readJson, readStorage, writeStorage } from "./storage.js";
import { topology } from "./topology.js";


// The Logs panel's tail: /api/logs gives the load-time backfill (a ring of
// recent records); /ws/events kind:"log" appends each new record live.
export async function backfillLogs() {
  try {
    const lines = await getJson("/api/logs");
    for (const ln of lines) appendLog(ln);
  } catch (_) {
    // A one-off read at boot: a server that cannot be reached shows in the
    // banner, through the polls.
  }
}

// The Logs panel head: the minimum level goes on #logs as a class the
// stylesheet reads, and persists; clear empties the tail (new lines
// keep arriving).
const LOGS_LEVEL_KEY = "mc-logs-level";

export function setupLogsPanel() {
  const box = document.getElementById("logs");
  const level = document.getElementById("logs-level");
  // The select's own options are the list of levels: a level added
  // there gets its min-* class handled here without a second edit.
  const LOG_LEVELS = [...level.options].map((o) => o.value);
  const apply = () => {
    for (const l of LOG_LEVELS) box.classList.toggle(`min-${l}`, l === level.value);
    writeStorage(LOGS_LEVEL_KEY, level.value);
  };
  const stored = readStorage(LOGS_LEVEL_KEY);
  level.value = LOG_LEVELS.includes(stored) ? stored : "info";
  level.addEventListener("change", apply);
  apply();
  document.getElementById("logs-clear").addEventListener("click", () => box.replaceChildren());
}

// The commands run, per browser and across microgrids, like a shell's history;
// and whether the Emacs keys are on.
const HISTORY_KEY = "mc-repl-history";
const EMACS_KEY = "mc-repl-emacs";

function loadHistory() {
  const stored = readJson(HISTORY_KEY);
  return Array.isArray(stored) ? stored.filter((s) => typeof s === "string") : [];
}

export function setupRepl() {
  const form = document.getElementById("repl-form");
  const input = document.getElementById("repl-input");
  const overlay = document.getElementById("repl-input-overlay");
  const output = document.getElementById("repl-output");
  const entries = document.getElementById("repl-entries");
  const inputWrap = document.getElementById("repl-input-wrap");
  const popup = document.getElementById("repl-popup");
  const completions = document.getElementById("repl-completions");
  const docPane = document.getElementById("repl-doc");
  const hint = document.getElementById("repl-hint");
  let selectedIdx = 0;
  let active = []; // current list of candidates
  let filledAt = -1; // where the cursor was when `active` was filled
  let typed = false; // whether the user typed since focus, a pick or Escape
  // The names the interpreter defines, read each time the input gets focus and
  // after each eval the server answers, since an eval can define new ones.
  const symbols = createSymbolStore(getJson);
  async function refreshSymbols() {
    await symbols.refresh();
    if (document.activeElement === input) {
      // A word already at the cursor opens no popup until the user types.
      if (typed) refresh();
      updateHint();
    }
  }

  const emacsToggle = document.getElementById("repl-emacs");
  let history = new ReplHistory(loadHistory());
  // The text a history step left in the input: while it is untouched, with the
  // caret at its end, Up and Down keep walking history from any line of a
  // multi-line entry.
  let recalled = null;
  const emacs = new EmacsKeys();

  emacsToggle.checked = readStorage(EMACS_KEY) === "1";
  emacsToggle.addEventListener("change", () => {
    writeStorage(EMACS_KEY, emacsToggle.checked ? "1" : "0");
    input.focus();
  });
  // The head's eval button, for a pointer with no keyboard at hand.
  document.getElementById("repl-eval").addEventListener("click", () => {
    input.focus();
    run();
  });

  // Electric-pair: typed open chars insert their close + leave the
  // cursor between. Closing char typed when the next char is the
  // same close just steps over instead of doubling up. Backspace
  // immediately after an empty pair eats both halves.
  const PAIRS = { "(": ")", "[": "]", "{": "}", "\"": "\"" };
  const CLOSES = new Set(Object.values(PAIRS));

  function refreshOverlay() {
    overlay.innerHTML = rainbowHighlight(input.value);
    fitInput();
  }

  // The input grows with its text instead of scrolling, so the prompt reads as
  // part of the transcript; the transcript scrolls to keep its end in view. A
  // closed panel has no layout to measure: its input keeps the height it had
  // until the observer below fits it again on open.
  function fitInput() {
    if (!input.offsetParent) return;
    input.style.height = "auto";
    input.style.height = `${input.scrollHeight}px`;
    const below = form.getBoundingClientRect().bottom - output.getBoundingClientRect().bottom;
    if (below > 0) output.scrollTop += below;
  }
  // Opening the panel and changing its width both re-wrap the input, and the
  // transcript can grow under the prompt without the panel changing size (a
  // density switch): each brings the prompt back into view.
  const fitObserver = new ResizeObserver(fitInput);
  for (const el of [output, entries]) fitObserver.observe(el);

  function renderCompletions() {
    if (!active.length) {
      popup.hidden = true;
      completions.innerHTML = "";
      docPane.innerHTML = "";
      return;
    }
    popup.hidden = false;
    completions.innerHTML = active
      .map(
        (c, i) =>
          `<li class="${i === selectedIdx ? "selected" : ""}" data-i="${i}">${escapeHtml(c)}</li>`,
      )
      .join("");
    renderDoc();
    // The popup opens on the side of the prompt with more of the transcript's
    // visible height: below while the transcript is short, above once the
    // prompt has reached the bottom. The transcript clips what overhangs it,
    // which a docked tile's short height would, so the popup is no taller than
    // that room, and scrolls the rest.
    const view = output.getBoundingClientRect();
    const wrap = popup.parentElement.getBoundingClientRect();
    const down = view.bottom - wrap.bottom > wrap.top - view.top;
    popup.classList.toggle("below", down);
    popup.style.maxHeight = "";
    const s = getComputedStyle(popup);
    const room = down
      ? view.bottom - wrap.bottom - Number.parseFloat(s.marginTop)
      : wrap.top - view.top - Number.parseFloat(s.marginBottom);
    const cap = Math.max(0, Math.min(Number.parseFloat(s.maxHeight), Math.floor(room)));
    popup.style.maxHeight = `${cap}px`;
    // The list and the doc pane each scroll within the popup's height.
    const borders = Number.parseFloat(s.borderTopWidth) + Number.parseFloat(s.borderBottomWidth);
    completions.style.maxHeight = docPane.style.maxHeight = `${Math.max(0, cap - borders)}px`;
    completions.querySelector(".selected")?.scrollIntoView({ block: "nearest" });
    // The popup opens at the word being completed: its left edge at the word's
    // start, sliding left to stay within the input.
    const v = input.value;
    const { start } = wordAtCursor(input);
    const [word] = measureMarkers(`${escapeHtml(v.slice(0, start))}<span></span>${escapeHtml(v.slice(start))}`);
    popup.style.left = `${Math.max(0, Math.min(word.left, input.clientWidth - popup.offsetWidth))}px`;
  }

  // Where the empty marker spans in `html` land in the overlay, as { left,
  // top } each. The overlay mirrors the textarea's text in the same font and
  // box, so it wraps where the textarea does; plain text measures as the
  // textarea lays it out, and the highlighting goes back right after.
  function measureMarkers(html) {
    overlay.innerHTML = html;
    const at = [...overlay.children].map((marker) => ({ left: marker.offsetLeft, top: marker.offsetTop }));
    overlay.innerHTML = rainbowHighlight(input.value);
    return at;
  }

  // The selected name's signature, kind and full docstring, beside the list.
  function renderDoc() {
    const sym = symbols.lookup(active[selectedIdx]);
    if (!sym) {
      docPane.innerHTML = "";
      return;
    }
    const head = sym.signature ? sym.signature.text : sym.name;
    const text = sym.doc ? `<pre class="repl-doc-text">${escapeHtml(sym.doc)}</pre>` : "";
    docPane.innerHTML =
      `<div class="repl-doc-head"><span class="repl-doc-sig">${escapeHtml(head)}</span>` +
      `<span class="repl-doc-kind">${escapeHtml(sym.kind)}</span></div>${text}`;
    docPane.scrollTop = 0;
  }

  // The hint line: the signature of the call the cursor is in.
  function updateHint() {
    const call = callAtCursor(input.value, input.selectionStart);
    const sym = call && symbols.lookup(call.head);
    hint.innerHTML = sym ? hintHtml(sym, call.argIndex) : "";
    hint.title = hint.textContent;
  }

  function refresh() {
    const { prefix } = wordAtCursor(input);
    // At a key of a call, the popup lists the keys the call can still take;
    // when none match, or elsewhere, it lists names.
    const call = callAtCursor(input.value, input.selectionStart);
    const keys = keyCompletions(call && symbols.lookup(call.head), call, prefix);
    active = keys.length > 0 ? keys : symbols.complete(prefix);
    // If the only match is exactly what's typed, no point showing a popup.
    if (active.length === 1 && active[0] === prefix) active = [];
    filledAt = input.selectionStart;
    selectedIdx = 0;
    renderCompletions();
  }

  function applyCompletion() {
    if (!active.length) return;
    const choice = active[selectedIdx];
    const { start, end } = wordAtCursor(input);
    const v = input.value;
    input.value = v.slice(0, start) + choice + v.slice(end);
    const newCursor = start + choice.length;
    input.setSelectionRange(newCursor, newCursor);
    active = [];
    typed = false;
    renderCompletions();
    // Programmatic .value assignment doesn't fire `input`; nudge
    // the overlay (and other input listeners) explicitly.
    refreshOverlay();
    updateHint();
  }

  // Send the current textarea contents through /api/format and
  // replace them with the result. Cursor preservation is best-
  // effort: we count non-whitespace characters before the old
  // cursor and place the new cursor after the same count of
  // non-whitespace characters in the formatted output. The
  // formatter only rearranges whitespace, so this lands the
  // cursor at the same logical token.
  async function formatInput() {
    const src = input.value;
    if (!src.trim()) return;
    const oldCursor = input.selectionStart;
    let nonWsBefore = 0;
    for (let i = 0; i < oldCursor; i++) {
      if (!/\s/.test(src[i])) nonWsBefore++;
    }
    let res;
    try {
      res = await fetch("/api/format?width=60", {
        method: "POST",
        body: src,
      });
    } catch (e) {
      formatFailed(`transport error: ${e.message}`);
      logUi("error", `REPL: format: transport error: ${e.message}`);
      return;
    }
    if (!res.ok) {
      // A 400 carries the parse diagnostic (unbalanced parens etc.).
      formatFailed(await errorText(res));
      return;
    }
    let formatted = await res.text();
    // tulisp-fmt always emits a trailing newline; the textarea
    // looks tidier without one for typical REPL fragments.
    if (formatted.endsWith("\n")) formatted = formatted.slice(0, -1);
    let newCursor = formatted.length;
    let seen = 0;
    for (let i = 0; i < formatted.length; i++) {
      if (!/\s/.test(formatted[i])) {
        if (seen === nonWsBefore) {
          newCursor = i;
          break;
        }
        seen++;
      }
    }
    // Text typed while the format was out wins over the formatted copy.
    if (input.value !== src) return;
    input.value = formatted;
    input.setSelectionRange(newCursor, newCursor);
    refreshOverlay();
    updateHint();
  }

  // Replaces the whole input, caret at the end, with no completion popup.
  function setInput(text) {
    input.value = text;
    input.setSelectionRange(text.length, text.length);
    active = [];
    renderCompletions();
    refreshOverlay();
    updateHint();
  }

  // One step through history (dir -1 older, +1 newer); false when there is
  // nothing further that way, so the key can do what it does natively.
  function historyStep(dir) {
    const text = history.step(dir, input.value);
    if (text == null) return false;
    setInput(text);
    recalled = text;
    return true;
  }

  // Up/Down (and C-p/C-n) go to history from the input's first/last row, or
  // from anywhere while the input still holds what history put there.
  function atHistoryEdge(dir) {
    const v = input.value;
    const caret = input.selectionStart;
    if (caret !== input.selectionEnd) return false;
    if (recalled === v && caret === v.length) return true;
    // A newline on that side settles it; otherwise the caret's own line may
    // wrap onto several rows, so empty markers at the start, at the caret and
    // at the end measure which row it is on.
    const before = v.slice(0, caret);
    const after = v.slice(caret);
    if ((dir < 0 ? before : after).includes("\n")) return false;
    const [start, at, end] = measureMarkers(
      `<span></span>${escapeHtml(before)}<span></span>${escapeHtml(after)}<span></span>`,
    ).map((marker) => marker.top);
    return at === (dir < 0 ? start : end);
  }

  // Puts a past command in the input in place of what is there, as one edit
  // that Ctrl+Z takes back, and brings the prompt into view.
  function recall(src) {
    history.reset();
    recalled = null;
    input.focus();
    applyEmacs({ caret: src.length, replace: { start: 0, end: input.value.length, text: src } });
  }

  // Saves a run command. History is re-read first, since another tab may have
  // added to it since this one loaded.
  function remember(src) {
    history = new ReplHistory(loadHistory());
    history.push(src);
    recalled = null;
    writeStorage(HISTORY_KEY, JSON.stringify(history.entries));
  }

  // Applies an Emacs command's result, or any edit given the same way. Edits go
  // through execCommand so they land on the browser's undo stack and Ctrl+Z
  // still works; setRangeText is the fallback for a browser without it.
  function applyEmacs({ caret, replace }) {
    if (replace && (replace.start !== replace.end || replace.text)) {
      const { start, end, text } = replace;
      input.setSelectionRange(start, end);
      const done = text ? document.execCommand("insertText", false, text) : document.execCommand("delete");
      if (!done) {
        input.setRangeText(text, start, end);
        input.dispatchEvent(new Event("input", { bubbles: true }));
      }
      // A kill, a yank or a recall is not typing: no completion popup.
      active = [];
      renderCompletions();
    }
    input.setSelectionRange(caret, caret);
  }

  // A failed format gets a transcript entry of its own, like a failed eval.
  function formatFailed(message) {
    const entry = document.createElement("div");
    entry.className = "repl-entry";
    entries.appendChild(entry);
    addResult(entry, "repl-error", `format: ${message}`);
    output.scrollTop = output.scrollHeight;
  }

  // A result line under a transcript entry, of class "repl-value" or
  // "repl-error". A value is Lisp, coloured as the input is; an error stays
  // plain, so it reads as one.
  function addResult(entry, klass, text) {
    const out = document.createElement("pre");
    out.className = klass;
    if (klass === "repl-value") out.innerHTML = rainbowHighlight(text);
    else out.textContent = text;
    entry.appendChild(out);
  }

  // Sends the prompt to the server. The input empties at once, as in a
  // terminal, so what the user types while the eval runs is kept.
  async function run() {
    const src = input.value.trim();
    if (!src) return;
    remember(src);
    setInput("");
    const entry = document.createElement("div");
    entry.className = "repl-entry";
    entry.innerHTML = `<pre class="repl-command" title="Click to edit again"><span class="repl-prompt-mark">▸</span> ${rainbowHighlight(src)}</pre>`;
    entry.firstChild.dataset.src = src;
    entries.appendChild(entry);
    output.scrollTop = output.scrollHeight;
    try {
      const res = await fetch(mgPath("eval") ?? "/api/eval", { method: "POST", body: src });
      const klass = res.ok ? "repl-value" : "repl-error";
      const text = res.ok ? (await res.json()).value : await errorText(res);
      addResult(entry, klass, text);
      // A failed eval can still have defined names before its error.
      refreshSymbols();
    } catch (err) {
      addResult(entry, "repl-error", `transport error: ${err.message}`);
      logUi("error", `REPL: transport error: ${err.message}`);
    }
    output.scrollTop = output.scrollHeight;
  }

  // A click in the transcript moves to the prompt, as in a terminal, without
  // scrolling to it. A click on a past command also puts it back in the input,
  // a moment later, so that a double-click can select a word instead. Anything
  // the user does with the input meanwhile, or closing the panel, cancels it. A
  // drag that selects text is a copy, so it leaves the input alone.
  let recallTimer = 0;
  const cancelRecall = () => clearTimeout(recallTimer);
  output.addEventListener("click", (e) => {
    cancelRecall();
    if (inputWrap.contains(e.target) || !window.getSelection().isCollapsed) return;
    input.focus({ preventScroll: true });
    // A selection left in the input would come back with the focus, and
    // Chromium then keeps it over the word a double-click goes on to select,
    // so the selection is collapsed to the caret first.
    if (input.selectionStart !== input.selectionEnd) {
      const caret = input.selectionDirection === "backward" ? input.selectionStart : input.selectionEnd;
      input.setSelectionRange(caret, caret);
    }
    const command = e.target.closest(".repl-command");
    if (command && e.detail < 2) {
      recallTimer = setTimeout(() => {
        if (input.offsetParent) recall(command.dataset.src);
      }, 300);
    }
  });
  for (const type of ["keydown", "input", "blur"]) input.addEventListener(type, cancelRecall);

  input.addEventListener("input", () => {
    typed = true;
    refreshOverlay();
    refresh();
    updateHint();
  });
  input.addEventListener("focus", () => {
    typed = false;
    refreshSymbols();
  });
  // The cursor can move without an `input` event: on arrow keys, a click, and a
  // typed closer that steps over the one already there. An open popup then
  // follows the word at the new cursor; Up and Down move only the selection.
  function cursorMoved() {
    updateHint();
    if (active.length && input.selectionStart !== filledAt) refresh();
  }
  input.addEventListener("keyup", cursorMoved);
  input.addEventListener("click", cursorMoved);
  // A press anywhere in the popup keeps the focus in the input, so its blur
  // does not hide the popup; a press on a name picks it.
  popup.addEventListener("mousedown", (e) => {
    e.preventDefault();
    // Another button picks nothing; a right click opens the context menu.
    if (e.button !== 0) return;
    const li = e.target.closest("#repl-completions li");
    if (li) {
      selectedIdx = Number(li.dataset.i);
      applyCompletion();
    }
  });
  input.addEventListener("blur", () => {
    active = [];
    renderCompletions();
  });
  input.addEventListener("keydown", (e) => {
    // Keys that pick a candidate in an input method are the method's.
    if (e.isComposing || e.keyCode === 229) return;
    const command = emacsToggle.checked ? emacsCommand(e) : null;
    // Any other key ends a run of kills.
    if (!command?.includes("kill") && !["Control", "Alt", "Shift", "Meta"].includes(e.key)) {
      emacs.interrupt();
    }
    // Completion popup keys take priority when it's open.
    if (active.length) {
      const down = e.key === "ArrowDown" || command === "next-line";
      if (down || e.key === "ArrowUp" || command === "previous-line") {
        e.preventDefault();
        selectedIdx = (selectedIdx + (down ? 1 : active.length - 1)) % active.length;
        renderCompletions();
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.ctrlKey && !e.metaKey)) {
        e.preventDefault();
        applyCompletion();
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        active = [];
        typed = false;
        renderCompletions();
        return;
      }
    }
    // With no completion popup to dismiss, Esc closes the panel,
    // matching Esc on every other card.
    if (e.key === "Escape") {
      e.preventDefault();
      closePanel("repl-btn");
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      run();
      return;
    }
    // History, like a shell's: Up from the first row, Down from the last.
    const plain = !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey;
    if (plain && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
      const dir = e.key === "ArrowUp" ? -1 : 1;
      if (atHistoryEdge(dir) && historyStep(dir)) e.preventDefault();
      return;
    }
    if (command) {
      e.preventDefault();
      if (command === "history-previous" || command === "history-next") {
        historyStep(command === "history-previous" ? -1 : 1);
        return;
      }
      if (command === "previous-line" || command === "next-line") {
        const dir = command === "previous-line" ? -1 : 1;
        if (atHistoryEdge(dir) && historyStep(dir)) return;
      }
      const caret = input.selectionDirection === "backward" ? input.selectionStart : input.selectionEnd;
      applyEmacs(emacs.run(command, input.value, caret));
      return;
    }
    // Tab when the completion popup isn't open: roundtrip the
    // textarea contents through /api/format. The popup-open case
    // is handled in the block above.
    if (e.key === "Tab" && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      formatInput();
      return;
    }
    // Plain Enter: walk back through the typed text to the
    // innermost still-open paren and indent the new line at its
    // column + 2. Strings / comments are skipped during the walk
    // so a `;` inside a comment (and an unbalanced `(` inside a
    // string) doesn't perturb the depth count. Doesn't replicate
    // tulisp-fmt's special-form rules (let bindings align under
    // first arg, etc.) — Tab roundtrips through the formatter for
    // canonical layout.
    if (e.key === "Enter" && !e.ctrlKey && !e.metaKey && !e.altKey) {
      const cursor = input.selectionStart;
      const end = input.selectionEnd;
      const indent = indentForNewline(input.value, cursor);
      const insert = `\n${" ".repeat(indent)}`;
      const v = input.value;
      e.preventDefault();
      input.value = v.slice(0, cursor) + insert + v.slice(end);
      input.setSelectionRange(cursor + insert.length, cursor + insert.length);
      input.dispatchEvent(new Event("input", { bubbles: true }));
      return;
    }
    // Electric-pair: skip if user is also holding a modifier (so
    // Ctrl-9 etc. on layouts that produce `(` directly still
    // works as the user expects).
    if (e.ctrlKey || e.metaKey || e.altKey) return;

    const v = input.value;
    const s = input.selectionStart;
    const e2 = input.selectionEnd;
    if (e.key in PAIRS) {
      e.preventDefault();
      const open = e.key;
      const close = PAIRS[open];
      // Step-over when typing a quote and cursor is already
      // immediately before that same quote.
      if (open === close && s === e2 && v[s] === open) {
        input.setSelectionRange(s + 1, s + 1);
        return;
      }
      if (s === e2) {
        input.value = v.slice(0, s) + open + close + v.slice(s);
        input.setSelectionRange(s + 1, s + 1);
      } else {
        input.value = v.slice(0, s) + open + v.slice(s, e2) + close + v.slice(e2);
        input.setSelectionRange(s + 1, e2 + 1);
      }
      input.dispatchEvent(new Event("input", { bubbles: true }));
      return;
    }
    if (CLOSES.has(e.key) && s === e2 && v[s] === e.key) {
      // Cursor sitting right before a matching close — just step
      // past instead of double-typing.
      e.preventDefault();
      input.setSelectionRange(s + 1, s + 1);
      return;
    }
    if (e.key === "Backspace" && s === e2 && s > 0) {
      const before = v[s - 1];
      const after = v[s];
      if (before in PAIRS && PAIRS[before] === after) {
        e.preventDefault();
        input.value = v.slice(0, s - 1) + v.slice(s + 1);
        input.setSelectionRange(s - 1, s - 1);
        input.dispatchEvent(new Event("input", { bubbles: true }));
      }
    }
  });
  // Initial paint so the overlay shows whatever the textarea was
  // pre-filled with (e.g. browser back-button restored content).
  refreshOverlay();
}

// Self-reconnecting WS with exponential backoff. Starts at 1 s,
// doubles on each close, caps at 30 s, resets to 1 s on a clean
// onopen. A laptop returning from sleep, a server bounce, or a
// notify-reload that briefly drops connections all heal without
// a manual page refresh — important for an overnight soak run.
// While it is closed the header status says so; the banner is for the
// background requests (connection.js).
//
// An open after a close also nudges a topology refresh, because samples may
// have moved while we were away and the status line was overwritten. A first
// open that went through is a no-op there because init() already awaited
// refreshTopology before opening the WS.
export function openWebSocket(onTopologyChanged) {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const url = `${proto}//${location.host}/ws/events`;
  const MIN_DELAY = 1000;
  const MAX_DELAY = 30000;
  let delay = MIN_DELAY;
  let lastConfigError = { message: null, tsMs: 0 };
  // The pending reconnect while the socket is closed, else null.
  let retry = null;
  // Whether the socket has closed since it last opened.
  let dropped = false;
  // The server is back after the background requests failed to reach it: try
  // now instead of waiting out the backoff.
  onReachedAgain(() => {
    if (retry == null) return;
    clearTimeout(retry);
    retry = null;
    connect();
  });
  function connect() {
    const ws = new WebSocket(url);
    ws.onopen = () => {
      delay = MIN_DELAY;
      if (dropped) {
        dropped = false;
        setStatus("", "");
        logUi("info", "live updates reconnected");
        // Catch up state the canvas and inspector cached from
        // before the drop. Loopback pill + metrics panel also
        // self-heal via their next poll / WS frame.
        onTopologyChanged(0);
      }
    };
    ws.onmessage = (msg) => {
      // Defensive: vis-network and other libs sometimes pump non-string
      // frames (binary, blob) through fetch / WS pipelines that look
      // identical from a try/catch perspective. Surface the actual
      // payload type to console so a "JSON.parse undefined" surprise
      // points straight at the offending frame.
      if (typeof msg.data !== "string") {
        console.warn("WS: non-string payload, skipping:", msg.data);
        return;
      }
      let ev;
      try {
        ev = JSON.parse(msg.data);
      } catch (e) {
        console.warn("WS: JSON parse failed:", e.message, "payload was:", msg.data);
        return;
      }
      // Per-microgrid events carry microgrid_id; we filter out
      // anything from a microgrid other than the currently-selected
      // one, and all of them with nothing selected, so the metrics
      // panel doesn't paint with samples from a neighbour. The one
      // exception is topology_changed with nothing selected: the list
      // view refreshes its microgrid cards on it. Enterprise-scoped
      // events (log, lagged) ship microgrid_id = undefined and pass through
      // regardless.
      const selectedMg = readSelectedMg();
      const perMg = ev.kind === "sample" || ev.kind === "microgrid_sample"
                 || ev.kind === "topology_changed" || ev.kind === "setpoint"
                 || ev.kind === "dispatch_changed" || ev.kind === "knob_changed";
      const forList = ev.kind === "topology_changed" && selectedMg == null;
      if (perMg && !forList && ev.microgrid_id != null && ev.microgrid_id !== selectedMg) {
        return;
      }
      if (ev.kind === "sample") {
        liveCharts.pushSample(ev.component_id, ev.metric, Date.parse(ev.ts) / 1000, ev.value);
        topology.applySample(ev);
        inspectorLive.applySample(ev);
      } else if (ev.kind === "microgrid_sample") {
        metricsStore.applySample({ ...ev, t_s: Date.parse(ev.ts) / 1000 });
        topology.applyMicrogridSample(ev);
      } else if (ev.kind === "topology_changed") {
        onTopologyChanged(ev.version);
      } else if (ev.kind === "setpoint") {
        topology.noteSetpoint(ev);
        pulseBar.recordSetpoint();
        inspectorLive.applySetpoint(ev);
      } else if (ev.kind === "knob_changed") {
        inspectorLive.applyKnob(ev);
        // Some knobs also move the canvas — the plug knob is the only
        // signal an unplug gives the live overlay, since an empty
        // charger stops sending soc_pct rather than sending a null
        // one. topology ignores the tokens it does not draw.
        topology.applyKnob(ev);
      } else if (ev.kind === "log") {
        appendLog(ev);
      } else if (ev.kind === "config_error") {
        // A failed hot-reload (site already reset) or an eval the
        // microgrid's file could not record — the one moment the user most
        // needs an explanation, so toast it (which also logs it).
        // A reload failure is broadcast on every registered site's
        // bus (the WS pump has no enterprise channel for it), so a
        // multi-microgrid setup delivers N copies — collapse repeats
        // of the same message arriving close together into one toast.
        const tsMs = Date.parse(ev.ts);
        if (ev.message === lastConfigError.message && tsMs - lastConfigError.tsMs < 1000) {
          return;
        }
        lastConfigError = { message: ev.message, tsMs };
        notify(ev.message);
      } else if (ev.kind === "dispatch_changed") {
        // The dispatch store changed for ev.microgrid_id; refetch only if
        // we're actually looking at that microgrid's Dispatches tab.
        if (selectedMg != null && readSubview() === "dispatches") {
          dispatchesPanel.render(selectedMg);
        }
      }
    };
    ws.onclose = () => {
      if (!dropped) logUi("warn", "live updates disconnected");
      dropped = true;
      const secs = Math.round(delay / 1000);
      setStatus(`disconnected — retry in ${secs}s`, "error");
      retry = setTimeout(() => {
        retry = null;
        connect();
      }, delay);
      delay = Math.min(delay * 2, MAX_DELAY);
    };
    // onerror fires alongside onclose; setStatus message stays as
    // the "retry in Xs" we just set so the user sees the recovery
    // plan rather than an opaque "ws error".
    ws.onerror = () => {};
  }
  connect();
}

