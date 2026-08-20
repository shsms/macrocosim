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

  // liveLabelLines: one metric per line, category-specific order
  eq("lines inverter p then q", m.liveLabelLines({ category: "inverter", p: -24000, q: 1200, soc: null, dc: null }), ["-24.00 kW", "1.20 kVAr"]);
  eq("lines meter p only", m.liveLabelLines({ category: "meter", p: 500, q: null, soc: null, dc: null }), ["500.0 W"]);
  eq("lines battery soc then dc", m.liveLabelLines({ category: "battery", p: null, q: null, soc: 85.2, dc: -3000 }), ["SoC 85%", "-3.00 kW"]);
  eq("lines battery dc only", m.liveLabelLines({ category: "battery", p: null, q: null, soc: null, dc: 0 }), ["0.0 W"]);
  eq("lines battery ignores ac", m.liveLabelLines({ category: "battery", p: 1, q: 1, soc: null, dc: null }), []);
  eq("lines ev p then soc", m.liveLabelLines({ category: "ev-charger", p: 3000, q: 7, soc: 40 }), ["3.00 kW", "SoC 40%"]);
  eq("lines no sample", m.liveLabelLines({ category: "meter", p: null, q: null, soc: null, dc: null }), []);

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
  return out;
});
for (const t of unit) check(`unit: ${t.name}`, t.ok, `got ${t.got} want ${t.want}`);

// ── e2e: live labels on the canvas ────────────────────────────────
const DEMO_CARD = '.mglist-card:has-text("Berlin demo")';
const getLabels = () =>
  page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugLiveLabels();
  });
const getEdges = () =>
  page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugLiveEdges();
  });
const hasLiveLine = (ls) => ls.some((l) => l.includes("\n"));
const hasChevron = (es) => es.some((e) => e.middleEnabled);

await page.click(DEMO_CARD);
await page.click('#mg-subtoggle .mode-btn[data-subview="topology"]');
// Labels land on the next 1 Hz flush; chevrons ride the same flush
// but need a power sample for the child first.
const labels = await waitFor(async () => {
  const ls = await getLabels();
  return hasLiveLine(ls) ? ls : null;
});
check("e2e: some node has a kW/W line", labels.some((l) => /\n-?\d+(\.\d+)? (W|kW|MW)/.test(l)), JSON.stringify(labels));
check("e2e: battery shows SoC line then DC power line", labels.some((l) => /^bat-\d+\nSoC \d+%\n-?\d+(\.\d+)? (W|kW|MW)$/.test(l)), JSON.stringify(labels));
check("e2e: inverter shows power then reactive on separate lines", labels.some((l) => /^inv-\S+\n-?\d+(\.\d+)? (W|kW|MW)\n-?\d+(\.\d+)? (VAr|kVAr|MVAr)$/.test(l)), JSON.stringify(labels));
const nodeWidths = () =>
  page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugNodeWidths();
  });
const widthsA = await nodeWidths();
await new Promise((r) => setTimeout(r, 2500)); // two more 1 Hz flushes
const widthsB = await nodeWidths();
check("e2e: live nodes keep their width across flushes", widthsA.length > 0 && JSON.stringify(widthsA) === JSON.stringify(widthsB), `${JSON.stringify(widthsA)} vs ${JSON.stringify(widthsB)}`);

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
const afterRefresh = { labels: await getLabels(), edges: await getEdges() };
check("e2e: labels survive a topology refresh", hasLiveLine(afterRefresh.labels), JSON.stringify(afterRefresh.labels));
check("e2e: chevrons survive a topology refresh", hasChevron(afterRefresh.edges), JSON.stringify(afterRefresh.edges));

// ── e2e: live toggle ──────────────────────────────────────────────
await page.click("#topology-controls .live-btn");
const off = await waitFor(async () => {
  const st = await page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return { labels: topology.debugLiveLabels(), edges: topology.debugLiveEdges(), on: topology.liveOn() };
  });
  return st.on === false && !hasLiveLine(st.labels) ? st : null;
});
check("e2e: toggle off clears label lines", off.labels.every((l) => !l.includes("\n")), JSON.stringify(off.labels));
check("e2e: toggle off clears chevrons", off.edges.every((e) => !e.middleEnabled));
check("e2e: toggle off reverts edge color", off.edges.every((e) => e.color !== "#79b8ff"), JSON.stringify(off.edges));
check("e2e: end arrowhead stays default size", off.edges.every((e) => e.toScale == null || e.toScale === 0.6), JSON.stringify(off.edges));
check("e2e: liveOn() reports off", off.on === false);
await page.reload({ waitUntil: "networkidle" });
await page.click(DEMO_CARD).catch(() => {});
// liveOn() reads the persisted flag at module load, so no flush to
// wait for here.
const persisted = await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  return topology.liveOn();
});
check("e2e: off state survives reload", persisted === false);
await page.evaluate(() => localStorage.removeItem("switchyard-topology-live"));

check("no page errors", errors.length === 0, JSON.stringify(errors));
await browser.close();
if (failures) { console.error(`${failures} FAILED`); process.exit(1); }
console.log("ALL PASS");
