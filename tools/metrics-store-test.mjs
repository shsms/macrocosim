// Unit tests for the metrics store's pure surface: ring push/read
// windowing, PF derivation, and display formatting. DOM/fetch shims
// are inert — the tested paths never call them.
//
// metrics-store.js imports routing.js for mgPath, and routing.js
// imports app.js — so this pulls in the same whole-graph-plus-init()
// load as tools/boot-smoke.mjs, not just metrics-store.js's own
// surface. Same Proxy-stub shim as boot-smoke, for the same reason:
// every property access answers with another callable stub, enough
// for the DOM/browser API surface the graph touches at import time.
import assert from "node:assert/strict";

function noop() {}
const stub = () =>
  new Proxy(noop, {
    get(_t, p) {
      if (p === Symbol.toPrimitive || p === "toString") return () => "";
      if (p === "then") return undefined;
      return stub();
    },
    apply: () => stub(),
    construct: () => stub(),
  });

globalThis.document = stub();
globalThis.window = globalThis;
Object.defineProperty(globalThis, "navigator", { value: stub(), configurable: true });
globalThis.localStorage = stub();
globalThis.location = { hash: "", pathname: "/", search: "" };
globalThis.history = stub();
globalThis.WebSocket = stub();
globalThis.requestAnimationFrame = () => 0;
globalThis.addEventListener = () => {};
globalThis.getComputedStyle = stub();
globalThis.CSS = stub();
globalThis.ResizeObserver = stub();
globalThis.fetch = () => new Promise(() => {});

// app.js and routing.js import each other (app.js for its own
// exports, routing.js back for dispatchesPanel/setStatus). Entering
// the graph via metrics-store.js -> routing.js -> app.js — instead
// of boot-smoke's app.js -> ... -> routing.js — flips which side of
// that cycle finishes its own top-level body first, and routing.js's
// init()-triggered setupModeToggle() ran before routing.js reached
// its own `const MODE_KEY` line: a real TDZ, but an artifact of this
// test's entry point, not of production (index.html always enters
// through app.js). Warm the cache with app.js first, same entry
// boot-smoke uses, so the cycle resolves in the same safe order.
await import(new URL("../ui-assets/app.js", import.meta.url));

const { metricsStore, fmtValue, pfValue, pfText } = await import(
  new URL("../ui-assets/metrics-store.js", import.meta.url)
);

// ── pf helpers ──────────────────────────────────────────────────
assert.equal(pfValue(null, 5), null);
assert.equal(pfValue(0, 0), null);
assert.ok(Math.abs(pfValue(100, 0) - 1) < 1e-9);
assert.ok(Math.abs(pfValue(30, 40) - 0.6) < 1e-9);
// Same signs → lagging, opposite → leading, unity (>= 0.995) drops
// the qualifier — the site-PF rule the old dashboard tile used.
assert.equal(pfText(30, 40), "PF 0.60 lag");
assert.equal(pfText(30, -40), "PF 0.60 lead");
assert.equal(pfText(100, 1), "PF 1.00");
assert.equal(pfText(null, 40), "PF —");
// Leading with the P sign flipped, and either side missing/NaN reads
// as undefined PF rather than a sign artifact.
assert.equal(pfText(-8000, 6000), "PF 0.80 lead");
assert.equal(pfText(8000, null), "PF —");
assert.equal(pfText(Number.NaN, 6000), "PF —");

// ── fmtValue ────────────────────────────────────────────────────
assert.equal(fmtValue("Power", "W", 1_234_000), "1.23 MW");
assert.equal(fmtValue("ReactivePower", "var", -1234), "-1.23 kVAr");
assert.equal(fmtValue("Frequency", "Hz", 50.0171), "50.02 Hz");
assert.equal(fmtValue("Power", "W", null), "—");

// ── ring + series windowing ─────────────────────────────────────
const t0 = 1_700_000_000_000;
for (let i = 0; i < 10; i++) {
  metricsStore.applySample({
    stream: "grid_power",
    quantity: "Power",
    unit: "W",
    ts_ms: t0 + i * 1000,
    value: i,
  });
}
assert.deepEqual(metricsStore.latest("grid_power"), {
  value: 9,
  quantity: "Power",
  unit: "W",
});
const s5 = metricsStore.series("grid_power", 5);
assert.equal(s5.xs.length, 5);
assert.equal(s5.ys.length, 5);
// Newest-last, values 5..9; older slots fell outside the window.
assert.deepEqual(s5.ys, [5, 6, 7, 8, 9]);
// xs are 1 Hz apart, monotonically increasing.
for (let i = 1; i < s5.xs.length; i++) {
  assert.ok(Math.abs(s5.xs[i] - s5.xs[i - 1] - 1) < 1e-9);
}
// A null-value sample lands as a null gap.
metricsStore.applySample({
  stream: "grid_power",
  quantity: "Power",
  unit: "W",
  ts_ms: t0 + 10_000,
  value: null,
});
const s3 = metricsStore.series("grid_power", 3);
assert.deepEqual(s3.ys, [8, 9, null]);
// A window wider than the ring's fill pads the front with nulls.
const wide = metricsStore.series("grid_power", 20);
assert.equal(wide.ys.length, 20);
assert.equal(wide.ys[0], null);
// resetStream empties the ring but keeps no stale latest ghost.
metricsStore.resetStream("grid_power");
assert.deepEqual(metricsStore.series("grid_power", 5).ys, [null, null, null, null, null]);

// ── subscribe ───────────────────────────────────────────────────
let fired = 0;
const un = metricsStore.subscribe(() => {
  fired++;
});
metricsStore.applySample({
  stream: "pv_power",
  quantity: "Power",
  unit: "W",
  ts_ms: t0,
  value: 1,
});
assert.equal(fired, 1);
un();
metricsStore.applySample({
  stream: "pv_power",
  quantity: "Power",
  unit: "W",
  ts_ms: t0 + 1000,
  value: 2,
});
assert.equal(fired, 1);

console.log("metrics-store-test: all assertions passed");
