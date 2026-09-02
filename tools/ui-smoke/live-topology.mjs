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

// ── e2e: the import dialog asks for the microgrid id ──────────────
// Importing a site export used to fire a bare prompt() for a name and
// let the server pick the id silently. It now opens a dialog with a
// name field and an id pre-filled by the same free-id walk the create
// dialog uses, and the whole flow is driven here through the real
// hidden file input: chosen id, collision, seeded retry, the busy
// guard, and the blank-means-auto-assign path.
//
// There is no delete-microgrid endpoint, so whatever this section
// imports stays in the registry for the rest of the run. It therefore
// claims ids far ABOVE the demo's range and names nothing "Berlin
// demo", leaving the later sections' card and id lookups untouched.
// The blank-id check is the one exception — it lands on the lowest
// free id by definition — and nothing below depends on that id being
// free.
const IMPORT_ID_A = 9801;
const IMPORT_ID_B = 9802;
// A one-component site export, as the file input receives it. Built
// in memory rather than through a temp file, so the smoke leaves
// nothing on disk. Component ids are enterprise-unique, so every
// fixture takes its own, clear of the demo's (1, 2, 100, 1000, 1001).
const exportFixture = (componentId) => [
  {
    name: "components.json",
    mimeType: "application/json",
    buffer: Buffer.from(
      JSON.stringify({
        electricalComponents: [
          {
            id: String(componentId),
            name: "grid",
            category: "ELECTRICAL_COMPONENT_CATEGORY_GRID_CONNECTION_POINT",
          },
        ],
      }),
    ),
  },
];
const mgIds = () =>
  page.evaluate(async () => (await (await fetch("/api/microgrids")).json()).map((m) => m.id));
// What the dialog should pre-fill: the lowest free id from 2200 up,
// the walk both nextFreeId() and the server's next_free_id_in do.
const lowestFreeMgId = async () => {
  const taken = new Set(await mgIds());
  let id = 2200;
  while (taken.has(id)) id += 1;
  return id;
};
const importDialogOpen = () =>
  page.evaluate(() => document.getElementById("import-mg-dialog").open);
const importErrorText = () =>
  page.evaluate(() => {
    const el = document.getElementById("import-mg-error");
    return el && !el.hidden && el.textContent ? el.textContent : null;
  });
// A successful import selects the microgrid it just made, which hides
// the list the file input lives in — so come back to the list first,
// the way a user starting a second import would. Via the header's
// back button, not location.hash: the router moves on popstate, which
// a hash write does not raise.
const backToMgList = async () => {
  if (await page.locator("#microgrid-list").isHidden()) await page.click("#mg-back");
  await waitFor(async () => !(await page.locator("#microgrid-list").isHidden()), 8000);
};
const openImport = async (componentId) => {
  await backToMgList();
  await page.setInputFiles("#import-files", exportFixture(componentId));
  await waitFor(importDialogOpen, 5000);
};

check("e2e: import dialog exists", (await page.locator("#import-mg-dialog").count()) === 1);
const wantPrefill = await lowestFreeMgId();
await openImport(99401);
check("e2e: picking a site export opens the import dialog", await importDialogOpen());
const importPrefill = await page.inputValue("#import-mg-id");
check(
  "e2e: import dialog pre-fills the lowest free microgrid id",
  importPrefill === String(wantPrefill),
  `${importPrefill} vs ${wantPrefill}`,
);

// A chosen id is honoured verbatim, not treated as a hint.
await page.fill("#import-mg-name", "smoke import A");
await page.fill("#import-mg-id", String(IMPORT_ID_A));
await page.click("#import-mg-form button[type=submit]");
await waitFor(async () => (await mgIds()).includes(IMPORT_ID_A), 15000).catch(() => {});
check(
  "e2e: the import registers under the id the dialog asked for",
  (await mgIds()).includes(IMPORT_ID_A),
  JSON.stringify(await mgIds()),
);

// A taken id is the server's call. The dialog reopens carrying its
// wording and what was typed, so the files never have to be re-picked.
await openImport(99402);
await page.fill("#import-mg-name", "smoke import collides");
await page.fill("#import-mg-id", String(IMPORT_ID_A));
await page.click("#import-mg-form button[type=submit]");
const seededError = await waitFor(
  async () => ((await importDialogOpen()) ? await importErrorText() : null),
  10000,
).catch(() => null);
check(
  "e2e: a taken id reopens the import dialog with the server's message",
  seededError?.includes(String(IMPORT_ID_A)) === true,
  String(seededError),
);
check(
  "e2e: the reopened import dialog keeps what was typed",
  (await page.inputValue("#import-mg-name")) === "smoke import collides" &&
    (await page.inputValue("#import-mg-id")) === String(IMPORT_ID_A),
);

// The dialog's resolver is module-scoped, so two flows may never
// overlap: a pick landing mid-retry would strand this one on a
// promise nobody settles. It is turned away with a toast instead.
const openDialogCount = () => page.evaluate(() => document.querySelectorAll("dialog[open]").length);
const dialogsBefore = await openDialogCount();
await page.setInputFiles("#import-files", exportFixture(99403));
const busyToast = await waitFor(
  async () =>
    (await page.evaluate(() => [...document.querySelectorAll(".toast")].map((t) => t.textContent)))
      .find((t) => /already in progress/i.test(t)) || null,
  5000,
).catch(() => null);
check("e2e: a second import mid-flow is refused with a toast", busyToast !== null, String(busyToast));
check("e2e: the refused second import opens no dialog", (await openDialogCount()) === dialogsBefore);
check(
  "e2e: the in-flight import's dialog survives the refused one",
  (await page.inputValue("#import-mg-name")) === "smoke import collides" &&
    (await importErrorText()) !== null,
);

// Same flow, same parsed export, corrected id — no second file pick.
await page.fill("#import-mg-id", String(IMPORT_ID_B));
await page.click("#import-mg-form button[type=submit]");
await waitFor(async () => (await mgIds()).includes(IMPORT_ID_B), 15000).catch(() => {});
check(
  "e2e: correcting the id imports without re-picking the files",
  (await mgIds()).includes(IMPORT_ID_B),
  JSON.stringify(await mgIds()),
);
check("e2e: the import dialog closes once the import lands", !(await importDialogOpen()));

// Blank means "let the server allocate", exactly as in create: the
// field is dropped from the request rather than sent as 0 or null.
const wantAuto = await lowestFreeMgId();
await openImport(99404);
await page.fill("#import-mg-name", "smoke import auto");
await page.fill("#import-mg-id", "");
const importPost = page.waitForRequest(
  (r) => r.url().endsWith("/api/microgrids/import") && r.method() === "POST",
);
await page.click("#import-mg-form button[type=submit]");
const postedKeys = Object.keys(JSON.parse((await importPost).postData()));
check("e2e: a blank id omits mid from the import request", !postedKeys.includes("mid"), JSON.stringify(postedKeys));
await waitFor(async () => (await mgIds()).includes(wantAuto), 15000).catch(() => {});
check(
  "e2e: a blank id auto-assigns the lowest free microgrid id",
  (await mgIds()).includes(wantAuto),
  `${wantAuto} in ${JSON.stringify(await mgIds())}`,
);

// Escape settles the dialog's promise as a cancel — nothing posted.
// It also stops at the dialog: app.js's global Esc bails out while a
// `dialog[open]` is up, so the cancel must not peel a floating panel
// off the dock behind it. The REPL is the panel to prove that with
// here — the panel pills live on a microgrid's Topology view and this
// section runs on the list, but a backtick opens the REPL anywhere.
const idsBeforeCancel = (await mgIds()).length;
// Back to the list BEFORE the panel opens: a real route change
// dismisses every floating card (routing.js), and the import that just
// landed selected its new microgrid, so openImport navigates.
await backToMgList();
// The submit above can leave a dialog field focused, and the backtick
// shortcut stands down inside a text field.
await page.evaluate(() => document.activeElement?.blur());
await page.keyboard.press("`");
await waitFor(async () => await page.evaluate(() => document.getElementById("repl").classList.contains("open")), 5000);
await openImport(99405);
await page.keyboard.press("Escape");
await waitFor(async () => !(await importDialogOpen()), 5000).catch(() => {});
check("e2e: Escape closes the import dialog", !(await importDialogOpen()));
check(
  "e2e: Escape on the import dialog leaves the panel behind it open",
  await page.evaluate(() => document.getElementById("repl").classList.contains("open")),
);
await page.click("#repl .float-close");
check(
  "e2e: a cancelled import registers nothing",
  (await mgIds()).length === idsBeforeCancel,
  `${(await mgIds()).length} vs ${idsBeforeCancel}`,
);

// Back to the list: the sections below start by clicking a card.
await backToMgList();
await waitFor(async () => (await page.locator('.mglist-card:has-text("Berlin demo")').count()) > 0, 8000);

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
// Widths come from side-panel.js's PANEL_DEFAULTS, not per-panel CSS.
const metricsWidth = await page.evaluate(() => document.getElementById("panel-metrics-btn").getBoundingClientRect().width);
check("e2e: the metrics panel takes its 430px width from PANEL_DEFAULTS", Math.abs(metricsWidth - 430) <= 1 && (await page.evaluate(() => document.getElementById("panel-metrics-btn").style.width === "430px")), `${metricsWidth}`);
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
// Closed by its own ×, not the chrome pill: the dock's top edge is
// level with the canvas controls now, so a card healed all the way up
// to the floor sits over that strip and swallows the pill's clicks.
await page.click("#panel-metrics-btn .float-close");
await page.evaluate(() => localStorage.removeItem("sw-panel-pos-metrics-btn"));

// ── e2e: shrinking the window re-fits the open panels ─────────────
// A resize moves the geometry out from under an open card: the dock
// narrows past the card's right edge, wrapping chrome pushes its top
// edge down. Sanitizing at open time cannot see that, so without a
// re-clamp on resize the card is stranded — grab strip off the window,
// nothing to click, no way back short of Esc. The refit is debounced,
// so the assertions wait for the gesture to settle.
await page.click("#metrics-btn");
const stripBox = await page.locator("#panel-metrics-btn .panel-drag").boundingBox();
await page.mouse.move(stripBox.x + stripBox.width / 2, stripBox.y + stripBox.height / 2);
await page.mouse.down();
await page.mouse.move(stripBox.x + stripBox.width / 2, 0, { steps: 8 });
await page.mouse.up();
await page.setViewportSize({ width: 500, height: 700 });
// The last geometry the poll saw, kept so a timeout reports which
// bound broke instead of a bare null.
let refitSeen = null;
const refit = await waitFor(async () => {
  const g = await page.evaluate(() => {
    const s = document.querySelector("#panel-metrics-btn .panel-drag").getBoundingClientRect();
    const hit = document.elementFromPoint(s.left + s.width / 2, s.top + s.height / 2);
    return {
      box: { l: Math.round(s.left), t: Math.round(s.top), r: Math.round(s.right), b: Math.round(s.bottom) },
      vw: window.innerWidth,
      vh: window.innerHeight,
      strip: hit?.closest(".panel-drag") != null,
      dockTop: Math.round(document.getElementById("panel-dock").getBoundingClientRect().top),
    };
  });
  refitSeen = g;
  return g.strip && g.box.l >= 0 && g.box.t >= 0 && g.box.r <= g.vw && g.box.b <= g.vh ? g : null;
}, 5000).catch(() => null);
check("e2e: a narrowed window re-fits the open panel's strip into view", refit != null, JSON.stringify(refitSeen));
check(
  "e2e: the re-fitted strip stays at/below the dock's top edge",
  refit != null && refit.box.t >= refit.dockTop - 1,
  JSON.stringify(refitSeen),
);
// Back to the size the rest of the suite runs at, and the panel it
// left open is still open — the refit moves cards, never closes them.
await page.setViewportSize({ width: 1600, height: 950 });
await waitFor(async () => (await page.locator("#panel-metrics-btn .panel-drag").boundingBox()) != null, 5000).catch(
  () => null,
);
check(
  "e2e: the metrics panel is still open after the window is restored",
  await page.evaluate(() => document.getElementById("panel-metrics-btn")?.classList.contains("open") === true),
);
await page.click("#panel-metrics-btn .float-close");
await page.evaluate(() => localStorage.removeItem("sw-panel-pos-metrics-btn"));

// ── e2e: the canvas controls collapse ─────────────────────────────
// The chevron folds the layout / drag / show groups away so the strip
// stops eating the canvas's top-right corner. The `panels` pills are
// deliberately outside the fold — collapsed still opens the metrics
// and formula panels — and the choice persists like the other UI
// preferences (localStorage, read back on the next load).
await page.click("#ctl-collapse");
check(
  "e2e: the chevron collapses the canvas controls",
  await page.evaluate(() => {
    const strip = document.getElementById("topology-controls");
    const layout = document.querySelector(".layout-btn");
    return (
      strip.classList.contains("collapsed") &&
      layout.offsetParent === null &&
      document.getElementById("metrics-btn").offsetParent !== null
    );
  }),
);
await page.reload({ waitUntil: "networkidle" });
await page.click(DEMO_CARD).catch(() => {});
check(
  "e2e: the collapsed controls survive a reload",
  await page.evaluate(
    () =>
      document.getElementById("topology-controls").classList.contains("collapsed") &&
      document.querySelector(".layout-btn").offsetParent === null,
  ),
);
// Collapsed is not a dead strip: the panel pills still toggle their
// panels and light up (side-panel.js's syncButton paints .primary).
await page.click("#metrics-btn");
check(
  "e2e: the metrics pill still works while collapsed",
  await page.evaluate(
    () =>
      document.getElementById("panel-metrics-btn")?.classList.contains("open") === true &&
      document.getElementById("metrics-btn").classList.contains("primary"),
  ),
);
await page.click("#metrics-btn");
await page.click("#ctl-collapse");
check(
  "e2e: the chevron expands the canvas controls again",
  await page.evaluate(
    () =>
      !document.getElementById("topology-controls").classList.contains("collapsed") &&
      document.querySelector(".layout-btn").offsetParent !== null,
  ),
);

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
// Fill the knob the way a user does — fill + Enter, the keydown path
// the input's own listener wires (inspect.js commits on Enter-keydown
// only; blur/Esc restore the pre-edit value instead, on purpose). The
// answer is read back with a direct eval from Node, so the assertion
// lands on the sim's own state and not on anything the page happens
// to be holding.
const evalNumber = async (expr) => {
  const r = await fetch(`${BASE}/api/mg/2200/eval`, { method: "POST", body: expr, signal: AbortSignal.timeout(5000) });
  const j = await r.json();
  return j.ok ? Number(j.value) : Number.NaN;
};
// The Component card that holds the knobs is folded by default
// (CARD_DEFAULT_OPEN in inspect.js) — open it, or Playwright's
// actionability check on fill/press times out against a display:none
// row.
if (!(await page.evaluate(() => document.getElementById("card-component")?.classList.contains("open")))) {
  await page.click("#card-component [data-fold-toggle]");
}
await page.fill('.knob-input[data-defun="set-meter-reactive-power"]', "500");
await page.press('.knob-input[data-defun="set-meter-reactive-power"]', "Enter");
const knobQ = await waitFor(async () => {
  const q = await evalNumber(`(component-reactive-power ${meterId})`);
  return Math.abs(q - 500) < 1 ? q : null;
}, 10000).catch(() => evalNumber(`(component-reactive-power ${meterId})`));
check("e2e: the reactive knob writes through to the sim", Math.abs(knobQ - 500) < 1, String(knobQ));
// inspect.js's Enter-commit contract (~519-521): on success the
// commit handler remembers the committed text as the new "live"
// baseline (data-live) rather than clearing the field — a blur
// afterward restores THAT, not the pre-edit value. waitFor rather
// than an immediate read: the commit success handler resolves off
// evalQuoted's own fetch, a separate promise from the sim write
// knobQ above already confirmed landed, so data-live can still be
// stale for a moment even once the sim itself is caught up. No blur
// before this check — blurring before data-live updates would race
// the blur listener's own restore-to-data-live against the commit,
// which is exactly what made the old (deleted) "clears itself"
// assertion pass for the wrong reason.
const rememberedLive = await waitFor(async () =>
  (await page.evaluate(
    () => document.querySelector('.knob-input[data-defun="set-meter-reactive-power"]').dataset.live,
  )) === "500"
    ? "500"
    : null,
);
check("e2e: the knob remembers the committed value", rememberedLive === "500", `data-live ${rememberedLive}`);
// The check above only proves the Enter-commit handler updated
// data-live — it says nothing about blur. Proving blur actually
// restores data-live (rather than just leaving an already-"500"
// field alone) needs a value blur can visibly change: fill an
// UNCOMMITTED "999" (no Enter, so data-live stays "500"), blur, and
// confirm the visible text snaps back to "500" rather than sticking
// at "999". Deleting inspect.js's blur listener (~535-539) makes
// this fail with afterBlur === "999"; the old version of this check
// asserted afterBlur === "500" right after page.fill'ing "500" and
// pressing Enter, a value the field already held and the blur
// listener's own restore-to-data-live would also have produced — so
// deleting that listener couldn't have turned it red.
await page.fill('.knob-input[data-defun="set-meter-reactive-power"]', "999");
await page.evaluate(() => document.activeElement?.blur());
const afterBlur = await page.evaluate(
  () => document.querySelector('.knob-input[data-defun="set-meter-reactive-power"]').value,
);
check(
  "e2e: blurring an uncommitted edit snaps the knob back to the committed value",
  afterBlur === "500",
  `value after blur "${afterBlur}"`,
);
// (Not exercised: the power-factor knob's .knob-flag-input rides the
// same blur path (flag.checked = flag.dataset.live === "1"), but that
// checkbox only exists on set-meter-power-factor, a different knob
// from the one this section already has open/selected — covering it
// here would mean switching knobs mid-section rather than reusing
// this one, so it's left for a future section instead.)

// Leave the meter as we found it: clear the reactive override so
// later sections (and anything appended after this one) aren't
// coupled to this section's 500 VAr state.
const reactiveCleared = await (async () => {
  const r = await fetch(`${BASE}/api/mg/2200/eval`, {
    method: "POST",
    body: `(clear-meter-reactive ${meterId})`,
    signal: AbortSignal.timeout(5000),
  });
  return (await r.json()).ok;
})();
check("e2e: the reactive override is cleared at the end of the section", reactiveCleared === true, String(reactiveCleared));

// ── e2e: the meter power knob's measure button clears an override ─
// Reuses `meterId` and `evalNumber` from above (still selected), and
// the Component card the reactive-knob section above already opened
// (fill/click need it open, or Playwright's actionability check
// times out against a display:none row).
// Capture the live P this meter reports while it's still following
// its children — clear-meter-power's whole job is to land back near
// this reading, not at zero or at whatever override gets set below.
const measureHidden = () =>
  page.evaluate(() =>
    document
      .querySelector('.knob-input[data-defun="set-meter-power"]')
      ?.closest("dd")
      ?.querySelector(".knob-measure-btn")?.hidden,
  );
const childrenP = await waitFor(async () => {
  const p = await evalNumber(`(component-active-power ${meterId})`);
  return Number.isFinite(p) ? p : null;
});
check("e2e: the power measure button starts hidden", (await measureHidden()) === true);
// Submit the real way — fill + Enter, the keydown path the input's
// own listener wires. Blur afterward: while the field is "editing",
// its visible text stays frozen against the WS repaint the clear
// below depends on (paintKnobEntry).
const OVERRIDE_P = 424242;
await page.fill('.knob-input[data-defun="set-meter-power"]', String(OVERRIDE_P));
await page.press('.knob-input[data-defun="set-meter-power"]', "Enter");
await page.evaluate(() => document.activeElement?.blur());
const overrideP = await waitFor(async () => {
  const p = await evalNumber(`(component-active-power ${meterId})`);
  return Math.abs(p - OVERRIDE_P) < 1 ? p : null;
});
check("e2e: the power knob overrides the meter's live P", Math.abs(overrideP - OVERRIDE_P) < 1, String(overrideP));
check(
  "e2e: the measure button appears once the override is live",
  await waitFor(async () => (await measureHidden()) === false),
);
await page.click('dd:has(.knob-input[data-defun="set-meter-power"]) .knob-measure-btn');
const blanked = await waitFor(async () =>
  (await page.evaluate(() => document.querySelector('.knob-input[data-defun="set-meter-power"]').value)) === "" || null,
);
check("e2e: the power knob blanks once cleared", blanked === true);
check(
  "e2e: the measure button disappears once cleared",
  await waitFor(async () => (await measureHidden()) === true),
);
// The demo's hidden consumer meter (id 100, one of this meter's
// children) drives ±500 W of per-tick random jitter plus a slow
// 15-min sine (examples/berlin-demo.lisp) — the round trip can't
// land on the exact pre-override reading, so this compares with a
// generous threshold, same idiom as the boiler section's power-level
// checks below.
const clearedP = await waitFor(async () => {
  const p = await evalNumber(`(component-active-power ${meterId})`);
  return Number.isFinite(p) && Math.abs(p - childrenP) < 2500 ? p : null;
}, 10000).catch(() => evalNumber(`(component-active-power ${meterId})`));
check(
  "e2e: clearing the power knob returns the meter to its children's sum",
  Math.abs(clearedP - childrenP) < 2500,
  `children ${childrenP}, after clear ${clearedP}`,
);

await page.evaluate(async () => {
  const { topology } = await import("/assets/topology.js");
  topology.select([]);
});

// ── e2e: the steam boiler end to end ───────────────────────────────
// A controllable gas/electric hybrid: :demand (kg/h) is the base
// load, an active-power setpoint allots how much of it is actually
// drawn (min(allotment, demand-equivalent) at the target pressure —
// 100 kg/h ≈ 62.7 kW here), and the boiler's own pressure state can
// decline the allotment back toward zero once it drifts above the
// 8 bar target. Fresh fixture ids (9901/9902), clear of the demo's
// (1, 2, 100, 1000, 1001) and the import section's (9801/9802,
// 99401-99405). Connected the way the demo wires a branch meter: a
// new meter hangs off the site's main meter (2), the boiler hangs off
// that meter — same (connect parent child) shape as berlin-demo.lisp.
const BOILER_ID = 9901;
const BOILER_METER_ID = 9902;
const boilerSetupOk = await page.evaluate(
  async ({ meterId, boilerId }) => {
    const r = await fetch("/api/mg/2200/eval", {
      method: "POST",
      body: `(make-meter :id ${meterId}) (make-steam-boiler :id ${boilerId} :demand 100.0) (connect 2 ${meterId}) (connect ${meterId} ${boilerId})`,
    });
    return (await r.json()).ok;
  },
  { meterId: BOILER_METER_ID, boilerId: BOILER_ID },
);
check("e2e: boiler fixture created behind its own meter", boilerSetupOk === true, String(boilerSetupOk));
await waitFor(async () => (await getModels()).some((m) => m.id === BOILER_ID), 15000);

await page.evaluate(async (id) => {
  const { topology } = await import("/assets/topology.js");
  topology.select([id]);
}, BOILER_ID);
const boilerKnobDefuns = await waitFor(async () => {
  const ds = await page.evaluate(() => [...document.querySelectorAll(".knob-input")].map((i) => i.dataset.defun));
  return ds.includes("set-boiler-demand") && ds.includes("set-boiler-pressure") ? ds : null;
});
check(
  "e2e: the boiler inspector shows its demand and pressure knobs",
  boilerKnobDefuns.includes("set-boiler-demand") && boilerKnobDefuns.includes("set-boiler-pressure"),
  JSON.stringify(boilerKnobDefuns),
);
// Command mode: steam-boiler is in inspect.js's ACCEPTS_SETPOINTS, so
// the commands selector renders alongside health/telemetry.
check(
  "e2e: the command-mode selector renders for the boiler",
  (await page.locator('select[data-knob="command-mode"]').count()) === 1,
);
// Knobs are prefilled from the live reading: demand from the constant
// installed at construction, pressure from the boiler's own state —
// which starts pinned to the 8 bar target (no :initial-bar given).
const demandKnob = await waitFor(async () => {
  const v = await page.inputValue('.knob-input[data-defun="set-boiler-demand"]');
  return v || null;
}, 10000);
check("e2e: the demand knob is prefilled from construction", demandKnob === "100", demandKnob);
const pressureKnob = await waitFor(async () => {
  const v = await page.inputValue('.knob-input[data-defun="set-boiler-pressure"]');
  return v || null;
}, 10000);
check("e2e: the pressure knob is prefilled from the live target", pressureKnob === "8", pressureKnob);

// Charts fold: steam-boiler is the only category with a pressure_bar
// chart (CHARTS_BY_CATEGORY), titled "Steam pressure" (METRIC_TITLES).
// Folded by default like every category's Charts card, so it has to
// be opened before the title paints.
await page.click("#card-charts [data-fold-toggle]");
const boilerChartTitles = await waitFor(async () => {
  const t = await page.evaluate(() => [...document.querySelectorAll("#charts .u-title")].map((e) => e.textContent));
  return t.length ? t : null;
}, 10000);
check(
  "e2e: the boiler's Charts fold lists a Steam pressure chart",
  boilerChartTitles.some((t) => /Steam pressure/.test(t)),
  JSON.stringify(boilerChartTitles),
);

// Allotment flow: demand was set at construction, BEFORE this
// setpoint — with demand 0 the dynamic band is [0, 0] and nothing
// flows no matter what set-active-power asks for.
const boilerPowerOk = await page.evaluate(async (id) => {
  const r = await fetch("/api/mg/2200/eval", { method: "POST", body: `(set-active-power ${id} 50000.0)` });
  return (await r.json()).ok;
}, BOILER_ID);
check("e2e: the boiler's active-power setpoint is accepted", boilerPowerOk === true, String(boilerPowerOk));
const boilerDrawing = await waitFor(async () => {
  const e = await page.evaluate(async (id) => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugLiveEntry(id);
  }, BOILER_ID);
  return e && Number.isFinite(e.p) && Math.abs(e.p) > 1000 ? e : null;
}, 15000);
check(
  "e2e: consumption follows the allotment while demand is set",
  Boolean(boilerDrawing) && Math.abs(boilerDrawing.p) > 1000,
  JSON.stringify(boilerDrawing),
);

// A pressure poke above the 8 bar target: the boiler declines
// electricity, so consumption decays back toward zero. Decay back to
// the target takes ~14 min at this demand — far outside the smoke's
// timescale, so "declined" is stable for this assertion.
const boilerPressureOk = await page.evaluate(async (id) => {
  const r = await fetch("/api/mg/2200/eval", { method: "POST", body: `(set-boiler-pressure ${id} 9.5)` });
  return (await r.json()).ok;
}, BOILER_ID);
check("e2e: the pressure poke is accepted", boilerPressureOk === true, String(boilerPressureOk));
const boilerDeclined = await waitFor(async () => {
  const e = await page.evaluate(async (id) => {
    const { topology } = await import("/assets/topology.js");
    return topology.debugLiveEntry(id);
  }, BOILER_ID);
  return e && Number.isFinite(e.p) && Math.abs(e.p) < 500 ? e : null;
}, 15000);
check(
  "e2e: an above-target pressure declines consumption back to ~0",
  Boolean(boilerDeclined) && Math.abs(boilerDeclined.p) < 500,
  JSON.stringify(boilerDeclined),
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

// ── e2e: weather panel ──────────────────────────────────────────────
// Runs LAST: the weather this section installs (and the sunrise/sunset
// override below) persists on the site for the rest of the run.
// Berlin's PV (id 200) passes :sunlight% explicitly and is driven by a
// set-solar-sunlight timer (examples/berlin-demo.lisp), so it's Manual
// and its power does not follow weather — assert the panel's own
// site-% readout, not inverter power.
await page.click("#weather-btn");
check(
  "e2e: weather panel opens",
  await page.evaluate(() => document.getElementById("panel-weather-btn")?.classList.contains("open") === true),
);
// berlin-demo.lisp never calls (make-weather) — the site starts with
// no weather, so the panel opens on its empty state.
await waitFor(async () => (await page.locator("#weather-create").count()) > 0, 10000);
await page.click("#weather-create");
// Creating installs the default sky; the panel repaints from the POST
// response synchronously, so the live skeleton's readout shows up
// without waiting on the 3 s poll.
await waitFor(async () => (await page.locator("#weather-pct").count()) > 0, 10000);

// The spinner convention (AGENTS.md) is revert-silent: drop the CSS and
// nothing here fails, the arrows just come back. Pin it in the computed
// style — an Enter-commit knob hides them, and the button-committed
// pass-a-cloud row is the documented exception that keeps them.
const spinnerStyles = await page.evaluate(() => ({
  peak: getComputedStyle(document.getElementById("weather-peak-pct")).appearance,
  fire: getComputedStyle(document.getElementById("weather-cloud-depth")).appearance,
  fireClass: document.getElementById("weather-cloud-depth").classList.contains("wfield-fire"),
}));
check(
  "e2e: an Enter-commit weather knob hides its native spinner",
  spinnerStyles.peak === "textfield",
  spinnerStyles.peak,
);
check(
  "e2e: the button-committed pass-a-cloud field keeps its spinner",
  spinnerStyles.fire !== "textfield" && spinnerStyles.fireClass === true,
  `${spinnerStyles.fire} / wfield-fire=${spinnerStyles.fireClass}`,
);

// This smoke runs at wall-clock UTC: the default 06:00-20:00 window
// would leave clear-sky at 0 outside daylight hours and the cloud
// check below would be flaky depending on when the run happens.
// Widen the window via the panel's own fields (Enter-committing, the
// inspector's edit-in-place contract — weather-panel.js wireField) so
// the run is always inside daylight.
await page.fill("#weather-sunrise", "00:00");
await page.press("#weather-sunrise", "Enter");
await page.fill("#weather-sunset", "23:59");
await page.press("#weather-sunset", "Enter");
const daylightText = await waitFor(async () => {
  const t = await page.evaluate(() => document.getElementById("weather-clear-sky")?.textContent);
  return t && t.includes("00:00") && t.includes("23:59") ? t : null;
}, 10000);
check(
  "e2e: sunrise/sunset commit widens the daylight window",
  Boolean(daylightText) && daylightText.includes("00:00") && daylightText.includes("23:59"),
  daylightText,
);

const preCloudPct = await waitFor(async () => {
  const t = await page.evaluate(() => document.getElementById("weather-pct")?.textContent);
  const v = Number(t);
  return Number.isFinite(v) && v > 0 ? v : null;
}, 10000);
check(
  "e2e: the site-% readout is positive inside the widened daylight window",
  Number.isFinite(preCloudPct) && preCloudPct > 0,
  String(preCloudPct),
);

// A deep (100%), long cloud with a short ramp: bites fast, and stays
// down long enough for the assertion below to land inside it.
await page.fill("#weather-cloud-depth", "100");
await page.fill("#weather-cloud-duration", "3600");
await page.fill("#weather-cloud-ramp", "5");
await page.click("#weather-cloud-fire");
const postCloudPct = await waitFor(async () => {
  const t = await page.evaluate(() => document.getElementById("weather-pct")?.textContent);
  const v = Number(t);
  return Number.isFinite(v) && v < preCloudPct - 1 ? v : null;
}, 20000);
check(
  "e2e: the panel readout drops after firing a deep cloud",
  Number.isFinite(postCloudPct) && postCloudPct < preCloudPct,
  `${preCloudPct}% -> ${postCloudPct}%`,
);

// Enter in the pass-a-cloud row fires it too — there is no form here
// to submit the button for us, so it is wired by hand. Counted off the
// cloud list rather than the readout: both clouds are live, so the
// second one's arrival shows as a row without needing the sine to be
// bright enough for another measurable drop.
const cloudRows = async () =>
  await page.evaluate(() => document.querySelectorAll("#weather-events li:not(.hint)").length);
const rowsBeforeEnter = await cloudRows();
await page.fill("#weather-cloud-depth", "40");
await page.fill("#weather-cloud-duration", "1800");
await page.fill("#weather-cloud-ramp", "5");
await page.press("#weather-cloud-ramp", "Enter");
const rowsAfterEnter = await waitFor(async () => {
  const n = await cloudRows();
  return n > rowsBeforeEnter ? n : null;
}, 10000);
check(
  "e2e: Enter in the pass-a-cloud row fires the cloud",
  rowsAfterEnter > rowsBeforeEnter,
  `${rowsBeforeEnter} -> ${rowsAfterEnter} cloud rows`,
);
// The ghost preview's Esc dismissal has no assertion here: it is a
// canvas-ink difference, measurable only by screenshotting the chart.
// Hand-checked instead (see the branch's report), and the arithmetic
// under it is covered DOM-free by tools/weather-panel-test.mjs.

// ── e2e: growing a capped panel back ─────────────────────────────
// A drag stores a max-height cap. With one in force, a drag
// downward writes a taller inline height while max-height pins the
// box, so the gesture is noticed off the style attribute: the cap
// clears mid-drag and the settled height becomes the new cap.
if (await page.evaluate(() => document.getElementById("panel-metrics-btn")?.classList.contains("open") === true)) {
  await page.click("#metrics-btn");
}
await page.evaluate(() => localStorage.setItem("sw-panel-size-metrics-btn", JSON.stringify({ h: 120 })));
await page.click("#metrics-btn");
const cappedH = await page.evaluate(() => document.getElementById("panel-metrics-btn").getBoundingClientRect().height);
check("e2e: a stored cap pins the metrics panel's height", Math.abs(cappedH - 120) <= 2, `${cappedH}`);
// What the native resize handle does on every tick of a drag.
await page.evaluate(() => { document.getElementById("panel-metrics-btn").style.height = "320px"; });
const grown = await waitFor(async () => {
  const s = await page.evaluate(() => ({
    h: document.getElementById("panel-metrics-btn").getBoundingClientRect().height,
    stored: JSON.parse(localStorage.getItem("sw-panel-size-metrics-btn") ?? "null")?.h ?? null,
  }));
  return s.stored !== null && s.stored > 120 ? s : null;
}, 5000).catch(() => null);
check("e2e: a drag taller than the cap grows the panel and re-caps it", grown !== null && grown.h > cappedH + 50, JSON.stringify(grown));
await page.click("#metrics-btn");

// ── e2e: the REPL and Logs panels ────────────────────────────────
// Static-markup floating panels off the PANELS pills, closed by
// default. The REPL also answers a backtick from anywhere, since its
// pill only shows on the Topology subview.
// null, not false, when the card is missing: "closed" is asserted as
// `=== false` below, so a renamed or deleted card fails here instead
// of passing as a panel that is merely not open.
const panelOpen = async (id) =>
  await page.evaluate((i) => {
    const el = document.getElementById(i);
    return el ? el.classList.contains("open") : null;
  }, id);
check("e2e: the REPL and Logs panels start closed", (await panelOpen("repl")) === false && (await panelOpen("logs-panel")) === false);
check(
  "e2e: no drawer row is left in main",
  await page.evaluate(() => document.getElementById("drawer-splitter") === null && getComputedStyle(document.getElementById("app")).gridTemplateRows.split(" ").length === 2),
);
await page.click("#logs-btn");
check("e2e: the logs pill opens the Logs panel", await panelOpen("logs-panel"));
const logsBox = await page.evaluate(() => {
  const r = document.getElementById("logs-panel").getBoundingClientRect();
  const d = document.getElementById("panel-dock").getBoundingClientRect();
  return { w: r.width, left: r.left - d.left, bottom: d.bottom - r.bottom };
});
check(
  "e2e: the Logs panel spawns 720px wide, 40px in from the dock's bottom-left corner",
  Math.abs(logsBox.w - 720) <= 1 && Math.abs(logsBox.left - 40) <= 1 && Math.abs(logsBox.bottom - 40) <= 1,
  JSON.stringify(logsBox),
);
check(
  "e2e: the log tail opens pinned to its newest line",
  await page.evaluate(() => {
    const l = document.getElementById("logs");
    // A tail that fits in the card is pinned to its newest line for
    // free — fail loudly instead, so this stops proving anything the
    // day the backfill no longer overflows.
    if (l.scrollHeight <= l.clientHeight) return false;
    return l.children.length > 0 && Math.abs(l.scrollTop + l.clientHeight - l.scrollHeight) < 2;
  }),
);
// The head row: a minimum level (a class on #logs the stylesheet
// reads) and a clear button.
const infoLineDisplay = async () =>
  await page.evaluate(() => {
    const line = document.querySelector("#logs .log-line.info");
    return line ? getComputedStyle(line).display : "none-found";
  });
check("e2e: the log tail shows info lines at its default level", (await infoLineDisplay()) === "flex" && (await page.evaluate(() => document.getElementById("logs").classList.contains("min-info"))));
await page.selectOption("#logs-level", "warn");
check("e2e: raising the minimum level to warn hides info lines", (await infoLineDisplay()) === "none");
await page.reload({ waitUntil: "networkidle" });
await page.click(DEMO_CARD).catch(() => {});
await page.click("#logs-btn");
check("e2e: the minimum level persists across a reload", await page.evaluate(() => document.getElementById("logs-level").value === "warn" && document.getElementById("logs").classList.contains("min-warn")));
await page.selectOption("#logs-level", "info");
// Clearing is the one shrink the tail does on command, so it is where
// the bottom-anchor invariant is checkable: the card hangs off the
// dock's bottom edge, so losing content must move its top, not its
// bottom.
const logsBottomBefore = await page.evaluate(() => document.getElementById("logs-panel").getBoundingClientRect().bottom);
await page.click("#logs-clear");
const logsBottomAfter = await page.evaluate(() => document.getElementById("logs-panel").getBoundingClientRect().bottom);
check(
  "e2e: a bottom-anchored card keeps its bottom edge when its content shrinks",
  Math.abs(logsBottomAfter - logsBottomBefore) <= 1,
  `${logsBottomBefore} -> ${logsBottomAfter}`,
);
// A live /ws/events line can land between the click and this read, so
// assert the tail was emptied, not that it stayed empty.
check("e2e: clear empties the tail", await page.evaluate(() => document.getElementById("logs").children.length < 5));
await page.keyboard.press("`");
check("e2e: a backtick opens the REPL panel and focuses its input", (await panelOpen("repl")) && (await page.evaluate(() => document.activeElement?.id === "repl-input")));
const replBox = await page.evaluate(() => {
  const r = document.getElementById("repl").getBoundingClientRect();
  const l = document.getElementById("logs-panel").getBoundingClientRect();
  return { w: r.width, gap: r.left - l.right, bottomDiff: r.bottom - l.bottom };
});
check("e2e: the REPL panel spawns 560px wide, 8px to the right of the Logs panel along the bottom", Math.abs(replBox.w - 560) <= 1 && Math.abs(replBox.gap - 8) <= 1 && Math.abs(replBox.bottomDiff) <= 1, JSON.stringify(replBox));
await page.fill("#repl-input", "(+ 1 2)");
await page.keyboard.press("Control+Enter");
const replOut = await waitFor(async () => {
  const t = await page.evaluate(() => document.getElementById("repl-output").textContent);
  return /\b3\b/.test(t) ? t : null;
}, 10000);
check("e2e: an eval in the REPL panel lands its result in the output band", /\b3\b/.test(replOut ?? ""), replOut);
await page.fill("#repl-input", "");
await page.type("#repl-input", "(make-");
const popup = await waitFor(async () =>
  await page.evaluate(() => {
    const ul = document.getElementById("repl-completions");
    if (ul.hidden || ul.children.length === 0) return null;
    const u = ul.getBoundingClientRect();
    const card = document.getElementById("repl").getBoundingClientRect();
    return { entries: ul.children.length, top: u.top - card.top, bottom: card.bottom - u.bottom };
  }), 5000).catch(() => null);
check("e2e: the completion popup fits inside the default-sized REPL card", popup !== null && popup.entries > 5 && popup.top >= 0 && popup.bottom >= 0, JSON.stringify(popup));
await page.keyboard.press("Escape"); // dismisses the popup
await page.fill("#repl-input", "");
await page.keyboard.press("Escape");
check("e2e: Escape in the REPL input closes the panel", (await panelOpen("repl")) === false);
await page.click("#logs-btn");
check("e2e: the logs pill closes the Logs panel", (await panelOpen("logs-panel")) === false);
// Off the Topology subview there is no pill; the backtick still works.
await page.keyboard.press("2");
await page.keyboard.press("`");
// The card has to be on screen there, not just carrying the class:
// the Scenarios pane replaces the topology canvas, and the panel dock
// the cards float in shares that grid cell.
check(
  "e2e: the backtick opens the REPL in Scenarios mode",
  (await panelOpen("repl")) === true &&
    (await page.evaluate(() => document.body.dataset.mode === "scenarios" && document.getElementById("repl").getBoundingClientRect().height > 0)),
);
await page.keyboard.press("Escape");
await page.keyboard.press("1");
await page.click(DEMO_CARD).catch(() => {});

check("no page errors", errors.length === 0, JSON.stringify(errors));
await browser.close();
if (failures) { console.error(`${failures} FAILED`); process.exit(1); }
console.log("ALL PASS");
