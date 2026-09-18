// Unit tests for the pill's pure model builder (ui-assets/pill.js):
// what a node shows for its category and live sample.
// Run: node tools/pill-test.mjs   (exits non-zero on failure)
//
// pill.js reads its colours through cssToken, which falls back to
// the literal defaults without a `document`, so no DOM shim is
// needed.
import assert from "node:assert/strict";

const { COLORS, frequencyColor, frequencyReadout, pillModel } = await import(new URL("../ui-assets/pill.js", import.meta.url));
const { blankLiveEntry } = await import(new URL("../ui-assets/live.js", import.meta.url));

const opts = { valuesOn: true, catColor: "#888888", deadBand: 50 };
const grid = { id: 1, category: "grid", name: "grid" };
const meter = { id: 2, category: "meter", name: "meter" };
const live = (extra) => ({ ...blankLiveEntry(), ...extra });

// ── frequencyColor: bands around the 50 Hz nominal ─────────────
assert.equal(frequencyColor(50.0), COLORS.fg);
assert.equal(frequencyColor(50.09), COLORS.fg, "inside the ±0.1 Hz band");
assert.equal(frequencyColor(50.1), COLORS.standby, "the warning band at ±0.1 Hz");
assert.equal(frequencyColor(49.85), COLORS.standby);
assert.equal(frequencyColor(50.2), COLORS.bad, "the alarm band at ±0.2 Hz");
assert.equal(frequencyColor(49.7), COLORS.bad);
assert.equal(frequencyColor(null), COLORS.dim);
assert.equal(frequencyColor(Number.NaN), COLORS.dim);

// ── frequencyReadout: two decimals, coloured by band ───────────
assert.deepEqual(frequencyReadout(50.024), { text: "50.02 Hz", color: COLORS.fg });
assert.deepEqual(frequencyReadout(49.72), { text: "49.72 Hz", color: COLORS.bad });
assert.equal(frequencyReadout(null), null);

// ── the grid pill: frequency is its hero value ─────────────────
{
  const m = pillModel(grid, live({ hz: 50.024 }), opts);
  assert.equal(m.hero.text, "50.02 Hz");
  assert.equal(m.hero.color, COLORS.fg);
  assert.equal(m.aux, null);
  // No import/export tint: frequency has no flow direction.
  assert.equal(m.heroValue, null);
  assert.equal(m.surface, COLORS.surface);
}
// Nothing sampled yet: bare pill, like every other node.
assert.equal(pillModel(grid, live({}), opts).hero, null);
assert.equal(pillModel(grid, null, opts).hero, null);
// Values off: bare pill even with a reading.
assert.equal(pillModel(grid, live({ hz: 50.0 }), { ...opts, valuesOn: false }).hero, null);

// ── other categories keep their power hero ─────────────────────
{
  const m = pillModel(meter, live({ p: 1500, hz: 49.5 }), opts);
  assert.equal(m.hero.text, "1.50 kW");
  assert.equal(m.hero.color, COLORS.import);
}

console.log("pill tests: ok");
