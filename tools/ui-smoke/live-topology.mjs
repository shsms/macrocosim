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
await page.click(".mglist-card:not(.mglist-new)");
await new Promise((r) => setTimeout(r, 1000));
await page.click('#mg-subtoggle .mode-btn[data-subview="topology"]');
await new Promise((r) => setTimeout(r, 3500)); // > one 1 Hz flush
const labels = await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  return topology.debugLiveLabels();
});
check("e2e: some node has a kW/W line", labels.some((l) => /\n-?\d+(\.\d+)? (W|kW|MW)/.test(l)), JSON.stringify(labels));
check("e2e: battery node shows SoC", labels.some((l) => /SoC \d+%/.test(l)), JSON.stringify(labels));

check("no page errors", errors.length === 0, JSON.stringify(errors));
await browser.close();
if (failures) { console.error(`${failures} FAILED`); process.exit(1); }
console.log("ALL PASS");
