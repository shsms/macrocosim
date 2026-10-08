// How the UI tells the user about errors: a toast for an action, a form's own
// .form-error for a form, and the logs panel for each of them but a form's own
// check of its input.
import { appendLog } from "./logs.js";

const MAX_TOASTS = 5;

// A UI entry in the logs panel; `level` is "error", "warn" or "info".
export function logUi(level, message) {
  appendLog({ ts: new Date().toISOString(), level: level.toUpperCase(), message: `ui: ${message}` });
}

// A toast in the bottom-right corner. An error toast stays until its × is
// pressed and is logged; a success toast leaves after 5 s. A message that is
// already shown raises that toast's count instead of adding a toast (and is not
// logged again).
export function notify(message, kind = "error") {
  const host = document.getElementById("toast-host");
  const shown = [...host.children].find((t) => t.dataset.message === message);
  if (shown) {
    const count = Number(shown.dataset.count) + 1;
    shown.dataset.count = String(count);
    shown.querySelector(".toast-count").textContent = `×${count}`;
  } else {
    if (kind === "error") logUi("error", message);
    host.append(toast(message, kind));
    while (host.children.length > MAX_TOASTS) host.firstChild.remove();
  }
  watchDialogs(host);
  raise(host);
}

// A modal dialog makes the rest of the page inert, popovers included, so the
// toast host lives inside the open modal, if any, else in the body; a click on
// a toast's × left under a backdrop would land on the backdrop and close the
// dialog. This keeps it there as dialogs open and close.
let watchingDialogs = false;
function watchDialogs(host) {
  if (watchingDialogs) return;
  watchingDialogs = true;
  new MutationObserver(() => {
    if (host.children.length > 0 && host.parentElement !== hostParent()) raise(host);
  }).observe(document.body, { subtree: true, attributeFilter: ["open"] });
}

const hostParent = () => document.querySelector("dialog:modal") ?? document.body;

// Shows the toast host above everything: in its place (see watchDialogs), and
// shown again, which lifts it to the top of the top layer.
function raise(host) {
  const parent = hostParent();
  if (host.parentElement !== parent) parent.append(host);
  if (host.matches(":popover-open")) host.hidePopover();
  host.showPopover();
}

function toast(message, kind) {
  const t = document.createElement("div");
  t.className = `toast toast-${kind}`;
  t.dataset.message = message;
  t.dataset.count = "1";
  const text = document.createElement("span");
  text.className = "toast-msg";
  text.textContent = message;
  const count = document.createElement("span");
  count.className = "toast-count";
  t.append(text, count);
  if (kind === "error") {
    const close = document.createElement("button");
    close.type = "button";
    close.className = "btn btn-quiet btn-icon toast-close";
    close.title = "Dismiss";
    close.textContent = "×";
    close.addEventListener("click", () => dismiss(t));
    t.append(close);
  } else {
    setTimeout(() => dismiss(t), 5000);
  }
  return t;
}

function dismiss(t) {
  const host = t.parentElement;
  t.remove();
  if (host?.children.length === 0 && host.matches(":popover-open")) host.hidePopover();
}

// A form's inline error from the server: shown, and logged unless it is already
// the one shown.
export function showFormError(el, message) {
  if (el.hidden || el.textContent !== message) logUi("error", message);
  showInputError(el, message);
}

// A form's inline error from its own check of the input: shown, not logged.
export function showInputError(el, message) {
  el.textContent = message;
  el.hidden = false;
}

export function clearFormError(el) {
  el.textContent = "";
  el.hidden = true;
}

// showFormError with a message, clearFormError without one.
export function setFormError(el, message) {
  if (message) showFormError(el, message);
  else clearFormError(el);
}
