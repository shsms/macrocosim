// Unit tests for the hover card's pure model builder
// (ui-assets/hovercard.js): which rows a node's card carries.
// Run: node tools/hovercard-test.mjs   (exits non-zero on failure)
//
// hovercard.js touches the DOM only inside its show/hide functions,
// so the model builder loads without a shim.
import assert from "node:assert/strict";

const { hoverCardModel } = await import(new URL("../ui-assets/hovercard.js", import.meta.url));
const { COLORS } = await import(new URL("../ui-assets/pill.js", import.meta.url));
const { blankLiveEntry } = await import(new URL("../ui-assets/live.js", import.meta.url));

const base = { parents: [], children: [], lastCommand: null, nowMs: 1000, deadBand: 50 };
const grid = { id: 1, category: "grid", name: "grid" };
const live = (extra) => ({ ...blankLiveEntry(), ts: 900, ...extra });

// ── the grid card: a frequency section ────────────────────────
assert.deepEqual(hoverCardModel({ ...base, component: grid, live: live({ hz: 50.024 }) }).frequency, {
  label: "Frequency",
  text: "50.02 Hz",
  color: COLORS.fg,
});
// No reading yet, or no live entry at all: no section.
assert.equal(hoverCardModel({ ...base, component: grid, live: live({}) }).frequency, null);
assert.equal(hoverCardModel({ ...base, component: grid, live: null }).frequency, null);

// ── other categories: no frequency section, power as before ────
{
  const m = hoverCardModel({ ...base, component: { id: 2, category: "meter", name: "m" }, live: live({ p: 1500, hz: 50 }) });
  assert.equal(m.frequency, null);
  assert.equal(m.power.text, "1.50 kW");
}

console.log("hovercard tests: ok");
