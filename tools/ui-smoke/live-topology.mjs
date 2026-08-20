// Live-topology smoke: in-browser unit tests for ui-assets/live.js
// plus (later tasks) e2e assertions against a running switchyard.
// Run: SW_UI=http://127.0.0.1:PORT node tools/ui-smoke/live-topology.mjs
import { chromium } from "playwright";

const BASE = process.env.SW_UI;
if (!BASE) throw new Error("set SW_UI to a running switchyard UI, e.g. http://127.0.0.1:8801");

let failures = 0;
const check = (name, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"} ${name}${ok ? "" : ` — ${detail}`}`);
  if (!ok) failures++;
};

// Bounded poll: await `fn()` until truthy, or throw after `ms`.
async function waitFor(fn, ms = 10000, every = 200) {
  const deadline = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > deadline) throw new Error(`waitFor: timed out after ${ms} ms`);
    await new Promise((r) => setTimeout(r, every));
  }
}

const browser = await chromium.launch({ args: ["--no-sandbox"] });
const page = await (await browser.newContext({ viewport: { width: 1600, height: 950 } })).newPage();
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));
await page.goto(BASE, { waitUntil: "networkidle" });

// ── unit tests: import the module in the browser ──────────────────
const unit = await page.evaluate(async () => {
  const m = await import("/assets/live.js");
  const out = [];
  const eq = (name, got, want) =>
    out.push({ name, ok: Object.is(got, want) || JSON.stringify(got) === JSON.stringify(want), got: JSON.stringify(got), want: JSON.stringify(want) });

  // formatScaled: the dashboard ladder, byte-identical
  eq("fmt W", m.formatScaled(107.3, "W"), "107.3 W");
  eq("fmt kW", m.formatScaled(-24000, "W"), "-24.00 kW");
  eq("fmt MW", m.formatScaled(1500000, "W"), "1.50 MW");
  eq("fmt kVAr", m.formatScaled(1200, "VAr"), "1.20 kVAr");
  eq("fmt null", m.formatScaled(null, "W"), "—");
  eq("fmt NaN", m.formatScaled(Number.NaN, "W"), "—");

  // edgeFlow: dead band, direction, sharing, clamps
  eq("flow dead", m.edgeFlow(10, 1, 30000).chevron, false);
  eq("flow consume", m.edgeFlow(5000, 1, 30000).towardParent, false);
  eq("flow export", m.edgeFlow(-5000, 1, 30000).towardParent, true);
  eq("flow shared halves", m.edgeFlow(-5000, 2, 30000).chevron, true);
  out.push({ name: "flow shared magnitude", ok: m.edgeFlow(-5000, 2, 30000).scale < m.edgeFlow(-5000, 1, 30000).scale });
  out.push({ name: "flow width clamp hi", ok: m.edgeFlow(-10e6, 1, 30000).width <= 6 });
  out.push({ name: "flow width clamp lo", ok: m.edgeFlow(-400, 1, 30000).width >= 1 });
  eq("flow zero parents treated as 1", m.edgeFlow(-5000, 0, 30000).chevron, true);
  eq("flow fallback max", m.edgeFlow(-5000, 1, 0).chevron, true);

  // pill.js: the pure node model
  const pill = await import("/assets/pill.js");
  const EXP = "#6bd9a5", IMP = "#79b8ff", DIM = "#5a626d", EXPQ = "#4f9a78", IMPQ = "#5a87bd";
  eq("dead band floor", m.deadBandW(0), 100);        // 1 % of the 10 kW fallback
  eq("dead band 1 %", m.deadBandW(30000), 300);
  eq("dead band min 50", m.deadBandW(1000), 50);
  eq("powerColor export", pill.powerColor(-5000, 300), EXP);
  eq("powerColor import", pill.powerColor(5000, 300), IMP);
  eq("powerColor dead", pill.powerColor(120, 300), DIM);
  eq("powerColor null", pill.powerColor(null, 300), DIM);
  eq("reactiveColor lagging-with-import", pill.reactiveColor(800, 300), IMPQ);
  eq("reactiveColor leading", pill.reactiveColor(-800, 300), EXPQ);
  eq("reactiveColor dead", pill.reactiveColor(10, 300), DIM);
  const opts = { valuesOn: true, dotColor: "#abcdef", deadBand: 300 };
  const inv = { id: 12, name: "Battery Inverter 1", category: "inverter", subtype: "battery", hidden: false, health: "ok", provides_telemetry: true };
  const mInv = pill.pillModel(inv, { p: -19930, q: 1200, soc: null, dc: null }, opts);
  eq("model id text", mInv.idText, "#12");
  eq("model dot", mInv.dotColor, "#abcdef");
  eq("model hero", mInv.hero, { text: "-19.93 kW", color: EXP });
  eq("model aux reactive", mInv.aux, { kind: "reactive", text: "1.20 kVAr", color: IMPQ });
  eq("model health ok", mInv.health, "ok");
  eq("model highlight default", mInv.highlight, "none");
  const bat = { id: 1000, name: "bat-1000", category: "battery", subtype: null, hidden: false, health: "ok", provides_telemetry: true };
  const mBat = pill.pillModel(bat, { p: null, q: null, soc: 85.4, dc: -3000 }, opts);
  eq("battery hero is dc", mBat.hero, { text: "-3.00 kW", color: EXP });
  eq("battery aux is soc", mBat.aux, { kind: "soc", pct: 85, text: "85%" });
  const mBatSocOnly = pill.pillModel(bat, { p: null, q: null, soc: 40, dc: null }, opts);
  eq("battery without dc shows dash hero", mBatSocOnly.hero, { text: "—", color: DIM });
  const ev = { id: 7, name: "ev-7", category: "ev-charger", subtype: null, hidden: false, health: "ok", provides_telemetry: true };
  eq("ev aux is soc", pill.pillModel(ev, { p: 3000, q: 7, soc: 40, dc: null }, opts).aux, { kind: "soc", pct: 40, text: "40%" });
  const meter = { id: 2, name: "meter-2", category: "meter", subtype: null, hidden: true, health: "ok", provides_telemetry: true };
  const mMeter = pill.pillModel(meter, { p: 500, q: null, soc: null, dc: null }, opts);
  eq("meter p only", mMeter.aux, null);
  eq("meter hidden", mMeter.hidden, true);
  eq("no sample → no row 2", pill.pillModel(meter, null, opts).hero, null);
  eq("values off → no row 2 even with sample", pill.pillModel(meter, { p: 500, q: 1, soc: null, dc: null }, { ...opts, valuesOn: false }).hero, null);
  eq("values off flag", pill.pillModel(meter, null, { ...opts, valuesOn: false }).valuesOn, false);
  const standby = { ...meter, provides_telemetry: false };
  eq("standby health", pill.pillModel(standby, null, opts).health, "standby");
  eq("error health wins", pill.pillModel({ ...standby, health: "error" }, null, opts).health, "error");
  const longName = { ...meter, name: "A very long component name indeed" };
  eq("name truncated", pill.pillModel(longName, null, opts).name, "A very long componen…");
  eq("full name kept", pill.pillModel(longName, null, opts).fullName, "A very long component name indeed");

  // renderer: measured sizes, content-derived and clamped
  await pill.pillFontsReady;
  const ctx = document.createElement("canvas").getContext("2d");
  const dShort = pill.measurePill(ctx, pill.pillModel({ ...meter, name: "m" }, null, { ...opts, valuesOn: false }));
  const dLong = pill.measurePill(ctx, pill.pillModel(inv, { p: -19930, q: 1200, soc: null, dc: null }, opts));
  const dHuge = pill.measurePill(ctx, pill.pillModel({ ...longName, id: 1234567 }, { p: -19930, q: -12500, soc: null, dc: null }, opts));
  eq("min width", dShort.width, 96);
  out.push({ name: "long wider than short", ok: dLong.width > dShort.width, got: `${dLong.width} vs ${dShort.width}` });
  out.push({ name: "max width clamp", ok: dHuge.width <= 200 && dHuge.width >= 150, got: String(dHuge.width) });
  out.push({ name: "clamped name re-truncated", ok: dHuge.name.endsWith("…") && dHuge.name.length < 21, got: dHuge.name });
  out.push({ name: "two rows taller than one", ok: dLong.height > dShort.height, got: `${dLong.height} vs ${dShort.height}` });
  const dOff = pill.measurePill(ctx, pill.pillModel(inv, null, { ...opts, valuesOn: false }));
  eq("values-off height single row", dOff.height, dShort.height);
  // pillRenderer contract
  const sizes = [];
  const r = pill.pillRenderer(pill.pillModel(inv, null, { ...opts, valuesOn: false }), (id, w, h) => sizes.push([id, w, h]));
  const res = r({ ctx, id: 12, x: 0, y: 0, state: { selected: false, hover: false }, style: {}, label: "" });
  eq("renderer reports dimensions", res.nodeDimensions, { width: dOff.width, height: dOff.height });
  eq("renderer onSize", sizes, [[12, dOff.width, dOff.height]]);
  out.push({ name: "renderer drawNode is callable", ok: typeof res.drawNode === "function" && (res.drawNode(), true) });

  // hovercard.js: the pure card model
  const hc = await import("/assets/hovercard.js");
  const now = 1787252990000;
  const liveInv = { p: -19930, q: 1200, soc: null, dc: null, energy: 12400, pLo: -30000, pHi: 30000, qLo: -5000, qHi: 5000, ts: now - 2000, hist: [[now - 3000, -19000], [now - 2000, -19930]] };
  const card = hc.hoverCardModel({ component: inv, live: liveInv, parents: ["meter-2"], children: ["bat-1000"], lastCommand: { kind: "power", value: "-2000", ts: now - 15000, accepted: true, reason: "" }, nowMs: now, deadBand: 300 });
  eq("card title", card.title, "Battery Inverter 1");
  eq("card id line", card.idLine, "#12 · inverter / battery");
  eq("card power text", card.power.text, "-19.93 kW");
  eq("card power envelope", [card.power.lo, card.power.hi, card.power.value], [-30000, 30000, -19930]);
  eq("card pf leading (opposite signs)", card.pf.text, "PF 1.00 leading");
  eq("card energy", card.energy.text, "12.40 kWh since start");
  eq("card last command", card.lastCommand.text, "power -2000 · 15 s ago · accepted");
  eq("card wiring", card.wiring, { parents: "meter-2", children: "bat-1000" });
  eq("card freshness", card.freshness, { text: "updated 2 s ago", stale: false });
  eq("card spark", card.spark, liveInv.hist);
  const lag = hc.hoverCardModel({ component: inv, live: { ...liveInv, p: 8000, q: 6000 }, parents: [], children: [], lastCommand: null, nowMs: now, deadBand: 300 });
  eq("card pf lagging (same signs)", lag.pf.text, "PF 0.80 lagging");
  eq("card wiring empty", lag.wiring, { parents: "—", children: "—" });
  eq("card no command", lag.lastCommand, null);
  const stale = hc.hoverCardModel({ component: inv, live: { ...liveInv, ts: now - 9000 }, parents: [], children: [], lastCommand: null, nowMs: now, deadBand: 300 });
  eq("card stale", stale.freshness, { text: "updated 9 s ago", stale: true });
  const none = hc.hoverCardModel({ component: inv, live: null, parents: [], children: [], lastCommand: null, nowMs: now, deadBand: 300 });
  eq("card no data", none.freshness, { text: "no data yet", stale: true });
  eq("card no data → no pf", none.pf, null);
  const batCard = hc.hoverCardModel({ component: bat, live: { ...liveInv, p: null, q: null, dc: -3000, soc: 85.4 }, parents: ["inv-bat-1001"], children: [], lastCommand: null, nowMs: now, deadBand: 300 });
  eq("battery card soc", batCard.soc, { pct: 85, text: "85%" });
  eq("battery card dc", batCard.dc.text, "-3.00 kW");
  eq("battery card no ac power section", batCard.power, null);
  eq("battery card no pf", batCard.pf, null);
  eq("rejected command", hc.hoverCardModel({ component: inv, live: liveInv, parents: [], children: [], lastCommand: { kind: "power", value: "5", ts: now - 1000, accepted: false, reason: "out of bounds" }, nowMs: now, deadBand: 300 }).lastCommand.text, "power 5 · 1 s ago · rejected: out of bounds");
  return out;
});
for (const t of unit) check(`unit: ${t.name}`, t.ok, `got ${t.got} want ${t.want}`);

// ── e2e: live pill models on the canvas ───────────────────────────
const DEMO_CARD = '.mglist-card:has-text("Berlin demo")';
const getModels = () =>
  page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugNodeModels();
  });
const getEdges = () =>
  page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugLiveEdges();
  });
const hasValues = (ms) => ms.some((m) => m.hero);
const hasChevron = (es) => es.some((e) => e.middleEnabled);

await page.click(DEMO_CARD);
await page.click('#mg-subtoggle .mode-btn[data-subview="topology"]');
// Values land on the next 1 Hz flush; chevrons ride the same flush
// but need a power sample for the child first.
const models = await waitFor(async () => {
  const ms = await getModels();
  return hasValues(ms) ? ms : null;
});
check("e2e: some node shows a power hero", models.some((m) => m.hero && /-?\d+(\.\d+)? (W|kW|MW)/.test(m.hero.text)), JSON.stringify(models));
check("e2e: battery shows DC power hero and SoC aux", models.some((m) => /^bat-\d+$/.test(m.fullName) && m.hero && m.aux?.kind === "soc"), JSON.stringify(models));
check("e2e: inverter shows reactive aux", models.some((m) => /^inv-/.test(m.fullName) && m.aux?.kind === "reactive" && /VAr/.test(m.aux.text)), JSON.stringify(models));
check("e2e: every node carries its #id", models.every((m) => m.idText === `#${m.id}`), JSON.stringify(models));
const nodeWidths = () =>
  page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugNodeWidths();
  });
const widthsA = await nodeWidths();
await new Promise((r) => setTimeout(r, 2500)); // two more 1 Hz flushes
const widthsB = await nodeWidths();
check("e2e: live nodes keep their width across flushes", widthsA.length > 0 && JSON.stringify(widthsA) === JSON.stringify(widthsB), `${JSON.stringify(widthsA)} vs ${JSON.stringify(widthsB)}`);
check("e2e: widths are content-derived (not all equal)", new Set(widthsA).size > 1, JSON.stringify(widthsA));
check("e2e: widths inside [96, 200]", widthsA.every((w) => w >= 96 && w <= 200), JSON.stringify(widthsA));

// ── e2e: flow chevrons ────────────────────────────────────────────
const edges = await waitFor(async () => {
  const es = await getEdges();
  return hasChevron(es) ? es : null;
});
const withChevron = edges.filter((e) => e.middleEnabled);
check("e2e: some edge has a flow chevron", withChevron.length > 0, JSON.stringify(edges));
// berlin demo: the hidden consumer meter (id 100, under meter-2)
// always consumes, so its chevron points away from the parent
// (positive scaleFactor) regardless of PV sunlight.
const consumer = await waitFor(async () => (await getEdges()).find((e) => e.id === "2-100" && e.middleEnabled));
check("e2e: the consumer edge's chevron points at the child", consumer.scaleFactor > 0, JSON.stringify(consumer));
check("e2e: chevron widths clamped", withChevron.every((e) => e.width >= 1.5 && e.width <= 6), JSON.stringify(withChevron));
check("e2e: live edges keep the 0.6 end arrowhead", edges.every((e) => e.toScale == null || e.toScale === 0.6), JSON.stringify(edges));

// ── e2e: a topology refresh keeps the overlay ─────────────────────
// An accepted eval broadcasts topology_changed → apply() diffs the
// DataSets. The live labels and chevrons must survive the diff.
const getApplyCount = () =>
  page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugApplyCount();
  });
const appliesBefore = await getApplyCount();
const evalRes = await page.evaluate(async () => {
  const r = await fetch("/api/mg/2200/eval", { method: "POST", body: "(+ 1 1)" });
  return r.status;
});
check("e2e: no-op eval accepted", evalRes === 200, `status ${evalRes}`);
// Wait for the refresh to land before asserting, so the check can't
// pass against the pre-refresh DataSets.
await waitFor(async () => (await getApplyCount()) > appliesBefore);
const afterRefresh = { models: await getModels(), edges: await getEdges() };
check("e2e: values survive a topology refresh", hasValues(afterRefresh.models), JSON.stringify(afterRefresh.models));
check("e2e: chevrons survive a topology refresh", hasChevron(afterRefresh.edges), JSON.stringify(afterRefresh.edges));

// ── e2e: formulas canvas uses the same pills, values off ─────────
await page.click('#mg-subtoggle .mode-btn[data-subview="formulas"]');
const formulaModels = await waitFor(async () => {
  const ms = await page.evaluate(async () => {
    const { formulaCanvas } = await import("/assets/explain.js");
    return formulaCanvas().debugNodeModels();
  });
  return ms.length ? ms : null;
});
check("e2e: formulas canvas shows #id on every node, values off", formulaModels.every((m) => m.idText === `#${m.id}` && m.valuesOn === false), JSON.stringify(formulaModels));
await page.click('#mg-subtoggle .mode-btn[data-subview="topology"]');

// ── e2e: live toggle ──────────────────────────────────────────────
// Geometry as vis applied it, not just the models: a custom shape
// binds its ctxRenderer once, so a model update that never reaches
// the canvas would still show up in debugNodeModels(). Every pill
// that carries a value row must lose height when values go off.
const getHeights = () =>
  page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugNodeHeights();
  });
const onHeights = await getHeights();
const valueRows = (await getModels()).map((m) => Boolean(m.hero || m.aux));
await page.click("#topology-controls .values-btn");
const off = await waitFor(async () => {
  const st = await page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return { models: topology.debugNodeModels(), edges: topology.debugLiveEdges(), on: topology.valuesOn() };
  });
  return st.on === false && !hasValues(st.models) ? st : null;
});
check("e2e: toggle off clears row 2", off.models.every((m) => !m.hero && !m.aux && m.valuesOn === false), JSON.stringify(off.models));
const offHeights = await waitFor(
  async () => {
    const hs = await getHeights();
    return hs.length === onHeights.length && hs.every((h, i) => !valueRows[i] || h < onHeights[i]) ? hs : null;
  },
  5000,
).catch(getHeights);
check(
  "e2e: toggle off shrinks applied node heights",
  valueRows.some(Boolean) && offHeights.every((h, i) => !valueRows[i] || h < onHeights[i]),
  `${JSON.stringify(onHeights)} → ${JSON.stringify(offHeights)}`,
);
check("e2e: toggle off clears chevrons", off.edges.every((e) => !e.middleEnabled));
check("e2e: toggle off reverts edge color", off.edges.every((e) => e.color !== "#79b8ff"), JSON.stringify(off.edges));
check("e2e: end arrowhead stays default size", off.edges.every((e) => e.toScale == null || e.toScale === 0.6), JSON.stringify(off.edges));
check("e2e: valuesOn() reports off", off.on === false);
// Sampling continues with values off: the map keeps filling so the
// hover card and the sparkline are complete when values come back.
// The entries are already full before the toggle (hist is capped at
// 60), so snapshot the timestamp at toggle time and wait for both the
// entry and its latest history point to move past it.
const liveEntry = (id) =>
  page.evaluate(async (id) => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugLiveEntry(id);
  }, id);
const lastHistTs = (e) => (e && e.hist.length ? e.hist[e.hist.length - 1][0] : -Infinity);
const tsAtOff = (await liveEntry(100))?.ts ?? -Infinity;
const entryOff = await waitFor(async () => {
  const e = await liveEntry(100);
  return e && e.ts > tsAtOff && lastHistTs(e) > tsAtOff ? e : null;
}, 5000);
check("e2e: sampling continues while values are off", entryOff && Number.isFinite(entryOff.p) && entryOff.ts > tsAtOff && lastHistTs(entryOff) > tsAtOff, JSON.stringify(entryOff));
// Batteries never emit active_power_w, so a populated hist here can
// only come from the dc_power_w branch of histMetric — id 1000 is
// bat-1000 in the Berlin demo.
const batteryTsAtOff = (await liveEntry(1000))?.ts ?? -Infinity;
const batteryEntryOff = await waitFor(async () => {
  const e = await liveEntry(1000);
  return e && e.ts > batteryTsAtOff && lastHistTs(e) > batteryTsAtOff ? e : null;
}, 5000);
check(
  "e2e: battery history tracks dc_power_w, not active_power_w",
  batteryEntryOff && batteryEntryOff.p === null && Number.isFinite(batteryEntryOff.dc) && batteryEntryOff.hist[batteryEntryOff.hist.length - 1][1] === batteryEntryOff.dc,
  JSON.stringify(batteryEntryOff),
);
// Component 100 is a plain load meter: it has no operating envelope,
// so it never emits bound samples (only Inverter/Battery/EvCharger
// do — see src/sim/meter.rs vs. src/sim/inverter/mod.rs). Check the
// bounds/timestamp fields on 1001 (inv-bat-1001), which reports both
// active and reactive bounds.
const boundsEntryOff = await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  return topology.debugLiveEntry(1001);
});
check("e2e: live entry carries bounds and timestamp", boundsEntryOff && Number.isFinite(boundsEntryOff.ts) && Number.isFinite(boundsEntryOff.pLo) && Number.isFinite(boundsEntryOff.pHi), JSON.stringify(boundsEntryOff));
await page.click("#topology-controls .values-btn");
const backOn = await waitFor(async () => {
  const ms = await getModels();
  return hasValues(ms) ? ms : null;
});
check("e2e: toggle on restores row 2", hasValues(backOn));
await page.click("#topology-controls .values-btn"); // back off for the reload test below
await page.reload({ waitUntil: "networkidle" });
await page.click(DEMO_CARD).catch(() => {});
// valuesOn() reads the persisted flag at module load, so no flush to
// wait for here.
const persisted = await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  return topology.valuesOn();
});
check("e2e: off state survives reload", persisted === false);
await page.evaluate(() => localStorage.removeItem("switchyard-topology-live"));

check("no page errors", errors.length === 0, JSON.stringify(errors));
await browser.close();
if (failures) { console.error(`${failures} FAILED`); process.exit(1); }
console.log("ALL PASS");
