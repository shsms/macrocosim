// The theme preference: stored choice, OS-following auto, listeners.  Run: node
// tools/theme-test.mjs (exits non-zero on failure)
import assert from "node:assert/strict";
import * as theme from "../ui-assets/theme.js";

// A stand-in window: storage, a media query we can flip, a root element.
function fakeWindow(dark, stored) {
  const store = new Map(stored == null ? [] : [["macrocosim-theme", stored]]);
  const mq = {
    matches: dark,
    listeners: [],
    addEventListener(_event, fn) {
      this.listeners.push(fn);
    },
  };
  return {
    localStorage: { getItem: (k) => store.get(k) ?? null, setItem: (k, v) => store.set(k, v) },
    matchMedia: () => mq,
    document: { documentElement: { dataset: {} } },
    mq,
    store,
  };
}
const flipOs = (w, dark) => {
  w.mq.matches = dark;
  for (const fn of w.mq.listeners) fn();
};

let w = fakeWindow(true, null);
theme.init(w);
assert.equal(theme.preference(), "auto");
assert.equal(theme.resolved(), "dark");
assert.equal(w.document.documentElement.dataset.theme, "dark");

// Auto follows the OS when it changes, and listeners hear it.
let heard = 0;
theme.onChange(() => heard++);
flipOs(w, false);
assert.equal(theme.resolved(), "light");
assert.equal(w.document.documentElement.dataset.theme, "light");
assert.equal(heard, 1);

// An explicit choice wins over the OS and is stored.
theme.choose("dark");
assert.equal(theme.resolved(), "dark");
assert.equal(w.store.get("macrocosim-theme"), "dark");
assert.equal(heard, 2);
flipOs(w, false);
assert.equal(theme.resolved(), "dark");
assert.equal(heard, 2, "an OS change under an explicit choice changes nothing");

// A stored value that is not a preference reads as auto.
w = fakeWindow(false, "sepia");
theme.init(w);
assert.equal(theme.preference(), "auto");
assert.equal(theme.resolved(), "light");

// With no matchMedia (an old browser, a test shim) auto reads as light.
w = fakeWindow(false, null);
delete w.matchMedia;
theme.init(w);
assert.equal(theme.resolved(), "light");

// No data-theme yet, and storage that throws: init resolves light from the OS
// and tells the listeners, which had read their colours from the dark defaults.
w = fakeWindow(false, null);
w.localStorage = {
  getItem() {
    throw new Error("storage blocked");
  },
};
let told = null;
theme.onChange((t) => {
  told = t;
});
theme.init(w);
assert.equal(w.document.documentElement.dataset.theme, "light");
assert.equal(told, "light", "init tells the listeners when it changes the theme");

// When data-theme already holds the resolved theme, nobody is told.
w = fakeWindow(false, null);
w.document.documentElement.dataset.theme = "light";
told = null;
theme.init(w);
assert.equal(told, null);

// The chip's cycle.
assert.equal(theme.nextPreference("auto"), "light");
assert.equal(theme.nextPreference("light"), "dark");
assert.equal(theme.nextPreference("dark"), "auto");

console.log("theme-test: all assertions passed");
