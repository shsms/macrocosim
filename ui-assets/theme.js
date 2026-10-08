// The colour theme: the stored preference (auto, light or dark) and the theme
// it resolves to, which `data-theme` on <html> carries for the stylesheet. Auto
// follows the OS setting and changes with it.  index.html sets `data-theme` the
// same way before first paint.

const PREF_KEY = "macrocosim-theme";
const PREFS = ["auto", "light", "dark"];
let win = null;
let pref = "auto";
let media = null;
const listeners = new Set();

export const preference = () => pref;
export const resolved = () => (pref === "auto" ? (media?.matches ? "dark" : "light") : pref);
export const nextPreference = (p) => PREFS[(PREFS.indexOf(p) + 1) % PREFS.length];

// `fn(theme)` runs after the resolved theme changes; the returned function
// unsubscribes.
export function onChange(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

function apply() {
  const next = resolved();
  const root = win.document.documentElement;
  if (root.dataset.theme === next) return;
  root.dataset.theme = next;
  for (const fn of listeners) fn(next);
}

// Reads the stored preference (anything else reads as auto) and starts
// following the OS setting. Listeners hear it when `data-theme` is unset or
// differs from the theme it resolves.
export function init(w = window) {
  win = w;
  let stored = null;
  try {
    stored = w.localStorage.getItem(PREF_KEY);
  } catch (_) {
    // No storage: auto.
  }
  pref = PREFS.includes(stored) ? stored : "auto";
  media = w.matchMedia?.("(prefers-color-scheme: dark)") ?? null;
  media?.addEventListener("change", apply);
  apply();
}

// The chip's choice: remembered, then applied.
export function choose(p) {
  pref = PREFS.includes(p) ? p : "auto";
  try {
    win.localStorage.setItem(PREF_KEY, pref);
  } catch (_) {
    // Applied without being remembered.
  }
  apply();
}
