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

  // formatScaled: the shared W → kW → MW ladder, byte-identical
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
  const opts = { valuesOn: true, catColor: "#abcdef", deadBand: 300 };
  const inv = { id: 12, name: "Battery Inverter 1", category: "inverter", subtype: "battery", hidden: false, health: "ok", provides_telemetry: true };
  const mInv = pill.pillModel(inv, { p: -19930, q: 1200, soc: null, dc: null }, opts);
  eq("model id text", mInv.idText, "#12");
  eq("model cat colour", mInv.catColor, "#abcdef");
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
  // minWidth: the per-node width ratchet's floor (topology.js keeps
  // the map). Below the content it changes nothing; above it pads.
  const shortModel = () => pill.pillModel({ ...meter, name: "m" }, null, { ...opts, valuesOn: false });
  eq("width floor pads a narrow pill", pill.measurePill(ctx, { ...shortModel(), minWidth: 150 }).width, 150);
  eq("width floor still clamped at max", pill.measurePill(ctx, { ...shortModel(), minWidth: 400 }).width, 200);
  eq("width floor under the content is ignored", pill.measurePill(ctx, { ...pill.pillModel(inv, { p: -19930, q: 1200, soc: null, dc: null }, opts), minWidth: 40 }).width, dLong.width);

  // bar + tinted border + live tint
  eq("mix 0", pill.mixHex("#000000", "#ffffff", 0), "#000000");
  eq("mix 1", pill.mixHex("#000000", "#ffffff", 1), "#ffffff");
  eq("mix half", pill.mixHex("#000000", "#ffffff", 0.5), "#808080");
  eq("mix rejects short hex", pill.mixHex("#888", "#ffffff", 0.5), "#888");
  eq("mix rejects rgb()", pill.mixHex("#242a33", "rgb(1,2,3)", 0.5), "#242a33");
  eq("border is 35 % category over border grey", pill.borderColor("#6fbf73"), pill.mixHex(pill.COLORS.border, "#6fbf73", 0.35));
  eq("surface neutral when dead", pill.surfaceColor(100, 300, true), pill.COLORS.surface);
  eq("surface neutral when null", pill.surfaceColor(null, 300, true), pill.COLORS.surface);
  eq("surface neutral with values off", pill.surfaceColor(-5000, 300, false), pill.COLORS.surface);
  eq("surface export tint", pill.surfaceColor(-5000, 300, true), pill.mixHex(pill.COLORS.surface, pill.COLORS.export, 0.07));
  eq("surface import tint", pill.surfaceColor(5000, 300, true), pill.mixHex(pill.COLORS.surface, pill.COLORS.import, 0.07));
  eq("text starts after the bar", pill.measurePill(ctx, pill.pillModel(inv, null, opts)).textLeft, 16);
  eq("width floor leaves the height alone", pill.measurePill(ctx, { ...shortModel(), minWidth: 150 }).height, dShort.height);
  // pillRenderer contract
  const sizes = [];
  const r = pill.pillRenderer(pill.pillModel(inv, null, { ...opts, valuesOn: false }), (id, w, h) => sizes.push([id, w, h]));
  const res = r({ ctx, id: 12, x: 0, y: 0, state: { selected: false, hover: false }, style: {}, label: "" });
  eq("renderer reports dimensions", res.nodeDimensions, { width: dOff.width, height: dOff.height });
  eq("renderer onSize", sizes, [[12, dOff.width, dOff.height]]);
  out.push({ name: "renderer drawNode is callable", ok: typeof res.drawNode === "function" && (res.drawNode(), true) });

  // level of detail by canvas scale, with 0.05 hysteresis
  eq("lod full at 1", pill.lodFor(1.0, "full"), "full");
  eq("lod hero at 0.6", pill.lodFor(0.6, "full"), "hero");
  eq("lod marker at 0.3", pill.lodFor(0.3, "hero"), "marker");
  eq("lod stays full just under 0.8", pill.lodFor(0.78, "full"), "full");
  eq("lod drops to hero under 0.75", pill.lodFor(0.74, "full"), "hero");
  eq("lod stays hero just over 0.8", pill.lodFor(0.82, "hero"), "hero");
  eq("lod back to full over 0.85", pill.lodFor(0.86, "hero"), "full");
  eq("lod full jumps to marker at 0.38", pill.lodFor(0.38, "full"), "marker");
  eq("lod stays marker just over 0.4", pill.lodFor(0.42, "marker"), "marker");
  eq("lod hero over 0.45", pill.lodFor(0.46, "marker"), "hero");
  eq("lod keeps prev on NaN", pill.lodFor(Number.NaN, "hero"), "hero");
  eq("lod no prev picks by threshold", pill.lodFor(0.5, undefined), "hero");
  // the renderer contract: nodeDimensions never depends on the LOD tier
  const mLod = pill.pillModel(inv, { p: -19930, q: 1200, soc: null, dc: null }, opts);
  const rFull = pill.pillRenderer(mLod, null, () => "full")({ ctx, id: 1, x: 0, y: 0, state: { selected: false, hover: false } });
  const rMarker = pill.pillRenderer(mLod, null, () => "marker")({ ctx, id: 1, x: 0, y: 0, state: { selected: false, hover: false } });
  eq("renderer dims identical across tiers", rFull.nodeDimensions, rMarker.nodeDimensions);

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
  eq("card last command", card.lastCommand.text, "power -2.00 kW · 15 s ago · accepted");
  eq("card wiring", card.wiring, { parents: "meter-2", children: "bat-1000" });
  eq("card freshness", card.freshness, { text: "updated 2 s ago", stale: false });
  eq("card spark", card.spark, liveInv.hist);
  const lag = hc.hoverCardModel({ component: inv, live: { ...liveInv, p: 8000, q: 6000 }, parents: [], children: [], lastCommand: null, nowMs: now, deadBand: 300 });
  eq("card pf lagging (same signs)", lag.pf.text, "PF 0.80 lagging");
  // |Q| inside the dead band: the sign of that Q is noise, so no qualifier.
  const noQ = hc.hoverCardModel({ component: inv, live: { ...liveInv, q: 0 }, parents: [], children: [], lastCommand: null, nowMs: now, deadBand: 300 });
  eq("card pf unqualified in the reactive dead band", noQ.pf.text, "PF 1.00");
  const smallQ = hc.hoverCardModel({ component: inv, live: { ...liveInv, q: -100 }, parents: [], children: [], lastCommand: null, nowMs: now, deadBand: 300 });
  eq("card pf unqualified for a tiny leading Q", smallQ.pf.text, "PF 1.00");
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
  eq("rejected command", hc.hoverCardModel({ component: inv, live: liveInv, parents: [], children: [], lastCommand: { kind: "power", value: "5", ts: now - 1000, accepted: false, reason: "out of bounds" }, nowMs: now, deadBand: 300 }).lastCommand.text, "power 5.0 W · 1 s ago · rejected: out of bounds");
  const cmdText = (lastCommand) => hc.hoverCardModel({ component: inv, live: liveInv, parents: [], children: [], lastCommand, nowMs: now, deadBand: 300 }).lastCommand.text;
  eq("reactive command scales in VAr", cmdText({ kind: "reactive_power", value: "1200", ts: now - 1000, accepted: true, reason: "" }), "reactive power 1.20 kVAr · 1 s ago · accepted");
  eq("every underscore in the kind is a space", cmdText({ kind: "active_power_w", value: 0, ts: now - 1000, accepted: true, reason: "" }), "active power w 0.0 W · 1 s ago · accepted");
  eq("non-numeric command value stays raw", cmdText({ kind: "mode", value: "idle", ts: now - 1000, accepted: true, reason: "" }), "mode idle · 1 s ago · accepted");
  eq("augment_bounds shows no value", cmdText({ kind: "augment_bounds", value: 0, ts: now - 3000, accepted: true, reason: "" }), "augment bounds · 3 s ago · accepted");
  eq("augment_reactive_bounds shows no value either", cmdText({ kind: "augment_reactive_bounds", value: 0, ts: now - 3000, accepted: true, reason: "" }), "augment reactive bounds · 3 s ago · accepted");
  // palette comes from :root tokens; values unchanged
  const css = (n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim();
  eq("token --pill-surface", css("--pill-surface"), "#242a33");
  eq("token --flow-export", css("--flow-export"), "#6bd9a5");
  eq("token --standby", css("--standby"), "#c4ad55");
  eq("COLORS.surface from token", pill.COLORS.surface, css("--pill-surface"));
  eq("COLORS.importDull from token", pill.COLORS.importDull, css("--flow-import-dull"));
  eq("COLORS.bad from token", pill.COLORS.bad, css("--bad"));
  return out;
});
for (const t of unit) check(`unit: ${t.name}`, t.ok, `got ${t.got} want ${t.want}`);

// ── e2e: managed-file chrome on the microgrid list ────────────────
// The overrides file is gone from the server, so nothing in the
// chrome may still reach for it.
check("e2e: the overrides pill is gone", (await page.locator("#pending-pill").count()) === 0);
check("e2e: the overrides dialog is gone", (await page.locator("#pending-dialog").count()) === 0);

// Create replaces the old prompt(): a dialog with a name field and
// an id pre-filled from the microgrid list the panel already holds.
check("e2e: create dialog exists", (await page.locator("#create-mg-dialog").count()) === 1);
await page.click("#mglist-new-btn");
check(
  "e2e: the New-microgrid card opens the create dialog",
  await page.evaluate(() => document.getElementById("create-mg-dialog").open),
);
const prefilledId = await page.inputValue("#create-mg-id");
check(
  "e2e: create dialog pre-fills a free microgrid id",
  /^\d+$/.test(prefilledId) && Number(prefilledId) >= 2200,
  prefilledId,
);
await page.click("#create-mg-close");

// The load picker opens on microgrids/ — where managed files live.
await page.click("#mglist-load-btn");
const crumb = await waitFor(async () => {
  const t = (await page.textContent("#load-script-breadcrumb")) || "";
  return t.trim() ? t : null;
});
check("e2e: load dialog opens on the microgrids dir", /microgrids/.test(crumb), crumb);
const listed = await page.$$eval("#load-script-list button", (bs) => bs.map((b) => b.textContent));
check("e2e: load dialog lists that directory's entries", listed.length > 0, JSON.stringify(listed));
check("e2e: the collision bar starts hidden", await page.locator("#load-script-collision").isHidden());
await page.click("#load-script-close");

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

// ── e2e: the microgrid header's file state ────────────────────────
// Adopt is the way out of read-only, so it shows exactly when the
// file is unmanaged — checked against the listing rather than
// against a hard-coded expectation, so it holds however the demo
// example is shipped.
const demoManaged = await page.evaluate(async () => {
  const rows = await (await fetch("/api/microgrids")).json();
  return rows.find((m) => m.id === 2200)?.managed;
});
const headerState = await waitFor(async () => {
  const s = await page.evaluate(() => ({
    adopt: !document.getElementById("mg-adopt-btn").hidden,
    chip: Boolean(document.querySelector("#mg-file-chips .unmanaged")),
  }));
  return s.adopt === !demoManaged ? s : null;
}, 8000).catch(() => null);
check(
  "e2e: Adopt + unmanaged chip show exactly when the file is unmanaged",
  headerState !== null && headerState.chip === !demoManaged,
  JSON.stringify({ demoManaged, headerState }),
);

// Undo is the server's: Ctrl+Z posts to /api/mg/{id}/undo instead of
// replaying a client-side stack. With no structural edit behind it
// the server answers 409, which is fine — the check is on the call.
let undoPosts = 0;
await page.route("**/api/mg/*/undo", (route) => {
  if (route.request().method() === "POST") undoPosts++;
  route.continue();
});
await page.keyboard.press("Control+z");
await waitFor(() => undoPosts > 0, 5000).catch(() => {});
check("e2e: Ctrl+Z posts the server's undo endpoint", undoPosts > 0, `${undoPosts} posts`);
await page.unroute("**/api/mg/*/undo");

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
// Each node's width is ratcheted upward: a value that loses a digit
// never narrows the pill, it only pads it. Between two refreshes
// (which reset the ratchet) widths may therefore grow but never
// shrink — that, not equality, is the invariant.
check(
  "e2e: live node widths never shrink across flushes",
  widthsA.length > 0 && widthsB.length === widthsA.length && widthsA.every((w, i) => widthsB[i] >= w),
  `${JSON.stringify(widthsA)} vs ${JSON.stringify(widthsB)}`,
);
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

// ── e2e: zoom tiers ───────────────────────────────────────────────
const lodAt = (s) =>
  page.evaluate(async (scale) => {
    const { topology } = await import("/assets/topology.js");
    topology.debugSetScale(scale);
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    return { lod: topology.debugLod(), widths: topology.debugNodeWidths(), heights: topology.debugNodeHeights() };
  }, s);
const atFull = await lodAt(1.0);
const atHero = await lodAt(0.6);
const atMarker = await lodAt(0.3);
check("e2e: lod full at 1.0", atFull.lod === "full", atFull.lod);
check("e2e: lod hero at 0.6", atHero.lod === "hero", atHero.lod);
check("e2e: lod marker at 0.3", atMarker.lod === "marker", atMarker.lod);
// Hysteresis only makes sense as a sequence, so set the starting tier
// here rather than inheriting it from the checks above: from marker,
// 0.78 climbs to hero (over 0.45) and must not reach full (needs 0.85).
await lodAt(0.3);
const atEdge = await lodAt(0.78);
check("e2e: marker to hero climbs past 0.78 (below the 0.85 full threshold)", atEdge.lod === "hero", atEdge.lod);
await lodAt(1.0);
const fromFull = await lodAt(0.78);
check("e2e: hysteresis holds full at 0.78 coming from full", fromFull.lod === "full", fromFull.lod);
await lodAt(0.6);
const fromHero = await lodAt(0.82);
check("e2e: hysteresis holds hero at 0.82 coming from hero", fromHero.lod === "hero", fromHero.lod);
check("e2e: tiers keep node widths", JSON.stringify(atFull.widths) === JSON.stringify(atMarker.widths), `${JSON.stringify(atFull.widths)} vs ${JSON.stringify(atMarker.widths)}`);
check("e2e: tiers keep node heights", JSON.stringify(atFull.heights) === JSON.stringify(atMarker.heights));
await lodAt(1.0);

// The tier has to change what is *painted*, not just what debugLod()
// reports: count fillText calls across one redraw at each tier. The
// spy is installed around a single synchronous redraw and removed in
// a finally, so a live flush between frames cannot inflate the count.
const paintAt = (scale) =>
  page.evaluate(async (s) => {
    const { topology } = await import("/assets/topology.js");
    topology.debugSetScale(s);
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    const models = topology.debugNodeModels();
    const proto = CanvasRenderingContext2D.prototype;
    const orig = proto.fillText;
    let calls = 0;
    try {
      proto.fillText = function (...args) {
        calls += 1;
        return orig.apply(this, args);
      };
      topology.debugRedraw();
    } finally {
      proto.fillText = orig;
    }
    return { lod: topology.debugLod(), calls, nodes: models.length, heroes: models.filter((m) => m.hero).length };
  }, scale);
const paintMarker = await paintAt(0.3);
check("e2e: marker tier paints no text at all", paintMarker.lod === "marker" && paintMarker.calls === 0, JSON.stringify(paintMarker));
const paintHero = await paintAt(0.6);
check("e2e: hero tier paints one string per pill with a hero", paintHero.lod === "hero" && paintHero.calls === paintHero.heroes && paintHero.heroes > 0, JSON.stringify(paintHero));
const paintFull = await paintAt(1.0);
check("e2e: full tier paints name and id on every pill", paintFull.lod === "full" && paintFull.calls >= 2 * paintFull.nodes && paintFull.nodes > 0, JSON.stringify(paintFull));
await page.evaluate(async () => { const { topology } = await import("/assets/topology.js"); topology.fit(); });

// ── e2e: hover card ──────────────────────────────────────────────
// The Berlin demo's battery inverter idles at 0 W, and a card with
// no power through it has no power factor to show. Command it (the
// setpoint expires on its own, so re-runs start from the same
// state) and wait for the ramp to reach the live overlay.
const setpointOk = await page.evaluate(async () => {
  const r = await fetch("/api/mg/2200/eval", { method: "POST", body: "(set-active-power 1001 -8000 60000)" });
  return (await r.json()).ok;
});
check("e2e: hover setup — inverter setpoint accepted", setpointOk === true, String(setpointOk));
await waitFor(async () => {
  const e = await page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugLiveEntry(1001);
  });
  return e && Number.isFinite(e.p) && Math.abs(e.p) > 1000;
}, 15000);
// With the inverter charging, the battery (which reports only DC
// power) must get a chevron on its edge from inverter 1001.
const batteryEdge = await waitFor(async () => (await getEdges()).find((e) => e.id === "1001-1000" && e.middleEnabled), 15000).catch(() => null);
check("e2e: the battery edge gets a chevron from DC power", Boolean(batteryEdge), JSON.stringify((await getEdges()).find((e) => e.id === "1001-1000")));
const readCard = () =>
  page.evaluate(async () => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugHoverCard();
  });
// Parking the pointer once is not enough on a live canvas: a pill
// whose value text changes width re-measures and relayouts, and the
// node slides out from under coordinates read a moment earlier. Also
// vis recomputes hover only when a mousemove *changes* what is under
// the pointer, so the nudge has to be two moves. Re-read and retry
// until the card opens.
async function hoverNodeCard(id, ms = 10000) {
  const deadline = Date.now() + ms;
  for (;;) {
    const r = await page.evaluate(async (nid) => {
      const { topology } = await import("/assets/topology.js");
      return topology.debugNodeScreenRect(nid);
    }, id);
    if (r) {
      await page.mouse.move(r.x + r.width / 2, r.y + r.height / 2 - 2);
      await page.mouse.move(r.x + r.width / 2, r.y + r.height / 2);
    }
    const s = await readCard();
    if (s?.visible) return s;
    if (Date.now() > deadline) throw new Error(`hoverNodeCard(${id}): the card never opened`);
    await new Promise((again) => setTimeout(again, 400));
  }
}
const cardState = await hoverNodeCard(1001); // inv-bat-1001
check("e2e: hover card names the component", /inv-bat-1001/.test(cardState.text) && /#1001/.test(cardState.text), cardState.text);
// The qualifier is only printed when |Q| clears the dead band, and
// the demo inverter often runs at Q = 0 — so it is optional here.
check("e2e: hover card has a PF line", /PF \d\.\d\d( (lagging|leading))?\b/.test(cardState.text), cardState.text);
check("e2e: hover card has freshness", /updated \d+ s ago|no data yet/.test(cardState.text), cardState.text);
check("e2e: hover card is inert to the pointer", await page.evaluate(() => getComputedStyle(document.querySelector(".hover-card")).pointerEvents === "none"));
// The reactive envelope rides the same bar helper as the active one,
// so an inverter that reports Q bounds draws a second `.hc-bar` and
// labels that bar's ends in VAr (the helper used to hardcode W).
const hcBars = await page.evaluate(() => ({
  bars: document.querySelectorAll(".hover-card .hc-bar").length,
  ends: [...document.querySelectorAll(".hover-card .hc-bar-ends")].map((e) => e.textContent),
}));
check("e2e: hover card draws the reactive envelope bar", hcBars.bars >= 2 && hcBars.ends.some((t) => /VAr/.test(t)), JSON.stringify(hcBars));
// A WS setpoint event writes through to the card's "Last command"
// (the card is still open on 1001), so it never shows a stale fetch.
await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  topology.noteSetpoint({ id: 1001, ts_ms: Date.now(), setpoint_kind: "active_power", value: -8000, accepted: true, reason: null });
});
const notedCard = await waitFor(async () => {
  const s = await readCard();
  return s?.visible && /Last command/.test(s.text) && /-8\.00 kW/.test(s.text) ? s : null;
}, 2000);
check("e2e: WS setpoint refreshes the card's last command", /active power -8\.00 kW/.test(notedCard.text), notedCard.text);
await page.mouse.move(5, 5);
const hiddenCard = await waitFor(async () => {
  const s = await readCard();
  return s && !s.visible ? s : null;
}, 3000);
check("e2e: hover card hides on blur", hiddenCard.visible === false);

// A sick setpoints endpoint must not be re-polled by every 1 s card
// re-render. Failures are cached for 10 s, so once a card is open on
// a component nobody has hovered yet, a 4 s window adds no requests.
// The glob matches the per-mg route (/api/mg/{id}/setpoints) the
// card actually fetches; the open-hit check below fails loudly if
// the interception ever stops matching the SPA's URL again.
let setpointHits = 0;
await page.route("**/setpoints**", (route) => {
  setpointHits++;
  route.abort();
});
const failCard = await hoverNodeCard(1000); // bat-1000, not hovered before
const hitsAtOpen = setpointHits;
check("e2e: opening the card hits the setpoints endpoint", hitsAtOpen >= 1, `${hitsAtOpen} requests`);
await new Promise((r) => setTimeout(r, 4000));
check("e2e: a failing setpoints endpoint is not re-polled every second", setpointHits - hitsAtOpen === 0, `${hitsAtOpen} → ${setpointHits} requests`);
check("e2e: the card still renders when setpoints fails", /bat-1000/.test(failCard.text) && !/Last command/.test(failCard.text), failCard.text);
await page.unroute("**/setpoints**");
await page.mouse.move(5, 5);
await waitFor(async () => {
  const s = await readCard();
  return s && !s.visible;
}, 3000);

// ── e2e: the metrics panel ───────────────────────────────────────
// Took over the Dashboard subview's job as a floating panel instead
// of a subview: open via the chrome pill, values stream in off the
// loopback's aggregate streams, and a uPlot chart mounts on the
// Power card (open by default). The reactive card is folded by
// default, but its fold-summary keeps repainting off the store while
// folded, so that — not an unfolded chip — is where the restored
// reactive-aggregate coverage lives.
await page.click("#metrics-btn");
check("e2e: metrics panel opens", await page.evaluate(() => document.getElementById("panel-metrics-btn")?.classList.contains("open") === true));
await new Promise((r) => setTimeout(r, 3000)); // let a few 1 Hz samples land
const chipValue = await waitFor(async () => {
  const vs = await page.evaluate(() => [...document.querySelectorAll(".mchip .mchip-value")].map((e) => e.textContent));
  return vs.some((v) => v && v !== "—") ? vs : null;
}, 15000);
check("e2e: at least one metrics chip shows a live value", Array.isArray(chipValue), JSON.stringify(chipValue));
check("e2e: the Power card mounts a uPlot canvas", (await page.locator('.mcard[data-card="power"] canvas').count()) > 0);
const reactiveSummary = await waitFor(async () => {
  const t = await page.evaluate(() => document.querySelector('[data-summary="reactive"]')?.textContent);
  return t && /VAr/.test(t) ? t : null;
}, 15000);
check("e2e: the folded reactive card's fold-summary paints a VAr value", /VAr/.test(reactiveSummary ?? ""), reactiveSummary);
// The Frequency card is folded by default too (grid_frequency), and
// the Berlin demo's grid connection point streams it every second.
const frequencySummary = await waitFor(async () => {
  const t = await page.evaluate(() => document.querySelector('[data-summary="frequency"]')?.textContent);
  return t && /Hz/.test(t) ? t : null;
}, 15000);
check("e2e: the folded frequency card's fold-summary paints an Hz value", /Hz/.test(frequencySummary ?? ""), frequencySummary);
const chip = page.locator("#panel-metrics-btn .mchip[data-chip]").first();
await chip.click();
check("e2e: clicking a series chip marks it off", await chip.evaluate((el) => el.classList.contains("off")));
await chip.click();
check("e2e: clicking it again clears off", await chip.evaluate((el) => !el.classList.contains("off")));
// Panels are independent floats now, not a stacked column: opening
// one must never resize a different panel that's already on screen.
const metricsHeightBefore = await page.evaluate(() => document.getElementById("panel-metrics-btn").getBoundingClientRect().height);
await page.click("#formula-btn");
check("e2e: the formula panel opens", await page.evaluate(() => document.getElementById("panel-formula-btn")?.classList.contains("open") === true));
const metricsHeightAfter = await page.evaluate(() => document.getElementById("panel-metrics-btn").getBoundingClientRect().height);
check(
  "e2e: opening the formula panel leaves the metrics panel's height alone",
  Math.abs(metricsHeightAfter - metricsHeightBefore) <= 2,
  `${metricsHeightBefore} → ${metricsHeightAfter}`,
);
await page.click("#formula-btn");
check("e2e: the formula panel closes", await page.evaluate(() => document.getElementById("panel-formula-btn")?.classList.contains("open") === false));
await page.click("#metrics-btn");
check("e2e: metrics panel closes", await page.evaluate(() => document.getElementById("panel-metrics-btn")?.classList.contains("open") === false));
// Negative control: the Dashboard subview is gone outright, not just
// hidden — no element, no subtoggle entry to reach it by.
check("e2e: no #dashboard element remains", await page.evaluate(() => document.querySelector("#dashboard") === null));
check(
  "e2e: the subtoggle has no dashboard entry",
  await page.evaluate(() => document.querySelector('#mg-subtoggle [data-subview="dashboard"]') === null),
);

// ── e2e: a poisoned panel position self-heals on open ──────────────
// A stored offset from a bygone (larger) window can leave the strip
// unreachable above the chrome. The stored dx/dy is only read once, when
// a panel is first created (side-panel.js's ensurePanel/loadPos), so the
// poison has to survive a reload to matter — same discipline as the
// values-off persistence check below.
await page.evaluate(() => localStorage.setItem("sw-panel-pos-metrics-btn", JSON.stringify({ dx: 0, dy: -999 })));
await page.reload({ waitUntil: "networkidle" });
await page.click(DEMO_CARD).catch(() => {});
await page.click("#metrics-btn");
check(
  "e2e: the metrics panel reopens after a reload with a poisoned position",
  await page.evaluate(() => document.getElementById("panel-metrics-btn")?.classList.contains("open") === true),
);
const dockTop = await page.evaluate(() => document.getElementById("panel-dock").getBoundingClientRect().top);
const stripTop = await page.evaluate(() => document.querySelector("#panel-metrics-btn .panel-drag").getBoundingClientRect().top);
check(
  "e2e: a poisoned panel position self-heals to at/below the dock's top edge",
  stripTop >= dockTop - 1,
  `strip ${stripTop} vs dock ${dockTop}`,
);
await page.click("#metrics-btn");
await page.evaluate(() => localStorage.removeItem("sw-panel-pos-metrics-btn"));

// ── e2e: the GCP inspector slims to Charts + Connections ───────────
// The grid connection point (id 1 in the Berlin demo) takes no knobs,
// no setpoints, and publishes no per-component telemetry — its
// inspector renders only a Charts card (the site frequency stream,
// open by default) and Connections, not Component/Power/Setpoints.
await waitFor(async () => (await getModels()).length > 0, 15000);
await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  topology.select([1]);
});
await waitFor(async () => (await page.locator("#card-charts canvas").count()) > 0, 10000);
const gcpCards = await page.evaluate(() => ({
  charts: Boolean(document.getElementById("card-charts")),
  component: Boolean(document.getElementById("card-component")),
  power: Boolean(document.getElementById("card-power")),
  setpoints: Boolean(document.getElementById("card-setpoints")),
}));
check(
  "e2e: the GCP inspector shows only a Charts card (+ Connections)",
  gcpCards.charts && !gcpCards.component && !gcpCards.power && !gcpCards.setpoints,
  JSON.stringify(gcpCards),
);
check("e2e: the GCP Charts card mounts a canvas", (await page.locator("#card-charts canvas").count()) > 0);
await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  topology.select([]);
});

// ── e2e: main_meter_id is gone from the per-mg topology payload ────
const topoPayload = await page.evaluate(async () => {
  const r = await fetch("/api/mg/2200/topology");
  return r.json();
});
check(
  "e2e: the per-mg topology payload has no main_meter_id key",
  !Object.hasOwn(topoPayload, "main_meter_id"),
  JSON.stringify(Object.keys(topoPayload)),
);

// ── e2e: the inspector's reactive knobs ──────────────────────────
// Any visible meter will do — the demo drives no meter's reactive
// slot, so the knob is the only writer. Read the id off the live
// topology rather than pinning one here.
const meterId = await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  return topology
    .allIds()
    .map((id) => topology.get(id))
    .filter((c) => c && c.category === "meter" && !c.hidden)
    .map((c) => c.id)
    .sort((a, b) => a - b)[0];
});
check("e2e: the demo has a meter to inspect", Number.isFinite(meterId), String(meterId));
await page.evaluate(async (id) => {
  const { topology } = await import("/assets/topology.js");
  topology.select([id]);
}, meterId);
const knobDefuns = await waitFor(async () => {
  const ds = await page.evaluate(() => [...document.querySelectorAll(".knob-input")].map((i) => i.dataset.defun));
  return ds.length ? ds : null;
});
check("e2e: meter reactive knobs present", knobDefuns.includes("set-meter-reactive-power") && knobDefuns.includes("set-meter-power-factor"), JSON.stringify(knobDefuns));
check(
  "e2e: the power-factor knob carries its leading flag",
  await page.evaluate(() => Boolean(document.querySelector('.knob-input[data-defun="set-meter-power-factor"]')?.closest("dd")?.querySelector(".knob-flag-input"))),
);
// Fill the knob the way a user does and let its change handler build
// the defun call. The answer is read back with a direct eval from
// Node, so the assertion lands on the sim's own state and not on
// anything the page happens to be holding.
const evalNumber = async (expr) => {
  const r = await fetch(`${BASE}/api/mg/2200/eval`, { method: "POST", body: expr });
  const j = await r.json();
  return j.ok ? Number(j.value) : Number.NaN;
};
await page.evaluate(() => {
  const input = document.querySelector('.knob-input[data-defun="set-meter-reactive-power"]');
  input.value = "500";
  input.dispatchEvent(new Event("change", { bubbles: true }));
});
const knobQ = await waitFor(async () => {
  const q = await evalNumber(`(component-reactive-power ${meterId})`);
  return Math.abs(q - 500) < 1 ? q : null;
}, 10000).catch(() => evalNumber(`(component-reactive-power ${meterId})`));
check("e2e: the reactive knob writes through to the sim", Math.abs(knobQ - 500) < 1, String(knobQ));
check(
  "e2e: the knob clears itself once submitted",
  await page.evaluate(() => document.querySelector('.knob-input[data-defun="set-meter-reactive-power"]').value === ""),
);
await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  topology.select([]);
});

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
// The hover card reads the live map, not the pill overlay, so it
// must open and keep ticking with values off. Component 100 is the
// always-consuming demo load: its power moves every second, so a
// card that stopped re-rendering would repeat itself.
const cardOff = await hoverNodeCard(100);
check("e2e: hover card opens with values off", /consumer/.test(cardOff.text) && /updated \d+ s ago/.test(cardOff.text), cardOff.text);
// With values off flushLive returns before it touches anything, so
// the only thing that can re-render the card is its own 1 s timer.
// That timer is what keeps "updated N s ago" honest when the sample
// stream dies — and a live server can't be made to drop the stream
// here, so the assertion is on the timer itself: the card re-renders
// on wall time. (Comparing the rendered *text* would be flaky — a
// slow-moving load can print the same kW twice in a row.)
await new Promise((r) => setTimeout(r, 1200));
const cardOffLater = await readCard();
check(
  "e2e: hover card freshness keeps counting",
  Boolean(cardOffLater?.visible) && cardOffLater.renders > cardOff.renders,
  `${cardOff.renders} → ${cardOffLater?.renders} renders`,
);
await page.mouse.move(5, 5);
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
