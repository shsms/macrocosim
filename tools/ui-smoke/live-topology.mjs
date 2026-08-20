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

  // liveLabelLine
  eq("line inverter p+q", m.liveLabelLine({ category: "inverter", p: -24000, q: 1200, soc: null }), "-24.00 kW · 1.20 kVAr");
  eq("line meter p only", m.liveLabelLine({ category: "meter", p: 500, q: null, soc: null }), "500.0 W");
  eq("line battery soc", m.liveLabelLine({ category: "battery", p: 0, q: null, soc: 85.2 }), "0.0 W · SoC 85%");
  eq("line ev soc", m.liveLabelLine({ category: "ev-charger", p: 3000, q: null, soc: 40 }), "3.00 kW · SoC 40%");
  eq("line battery no soc yet", m.liveLabelLine({ category: "battery", p: 0, q: null, soc: null }), "0.0 W");
  eq("line no sample", m.liveLabelLine({ category: "meter", p: null, q: null, soc: null }), null);

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
check("e2e: battery node shows SoC", labels.some((l) => /SoC \d+%/.test(l)), JSON.stringify(labels));

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
