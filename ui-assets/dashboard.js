// Dashboard panels: top-level tile aggregator + per-component row
// modules (battery pairs, PV / EV / CHP rows). Each module exposes
// `refresh(snapshot)` for topology-driven shape changes + `applySample(ev)`
// for the per-tick stream updates. All renders are direct DOM
// innerHTML — no virtual DOM; the grid is small and the writes are
// batched per render() call.

import { escapeHtml, jumpToTopology, mgPath } from "./app.js";
import { loadFormulas } from "./formulas.js";
import { formatScaled } from "./live.js";

// Power auto-scale: W → kW → MW based on magnitude. Delegates to
// formatScaled from live.js so the Dashboard reads in the same units
// as every other readout in the app — one implementation of the
// scaling ladder.
function fmt(quantity, unit, value) {
  if (value == null || !Number.isFinite(value)) return "—";
  // The wire unit for reactive power is SI "var"; every user-facing
  // readout in the app spells it "VAr", so map it on display only.
  const shown = unit === "var" ? "VAr" : unit;
  if (quantity === "Power" || quantity === "ReactivePower" || unit === "W" || unit === "var") {
    return formatScaled(value, shown);
  }
  // Voltage, frequency, percentage etc. — fixed unit, modest precision.
  return `${value.toFixed(2)} ${shown}`;
}

// Site power factor as the Q tile's meta line prints it, derived from
// the latest grid P and Q. Pure so it can be unit-checked: the tile
// itself only owns the element lookup.
//
// Sign convention only: opposite signs on P and Q read as leading,
// same signs as lagging — the same rule the topology hover card uses.
// The qualifier is dropped differently here: this tile drops it once
// PF rounds to unity (>= 0.995), so a clean unity reading doesn't
// flicker between leading and lagging on noise, where the hover card
// drops it inside a |Q| dead band instead.
export function sitePfText(p, q) {
  if (!Number.isFinite(p) || !Number.isFinite(q) || (p === 0 && q === 0)) return "site PF —";
  const pf = Math.abs(p) / Math.hypot(p, q);
  const tag = pf >= 0.995 ? "" : p * q < 0 ? " leading" : " lagging";
  return `site PF ${pf.toFixed(2)}${tag}`;
}

// Latest value of one metric for one component, read off a short
// history window. The single fetch shape behind every row module's
// seed path — `null` when the stream has no recent sample.
async function latestMetric(id, metric) {
  const j = await fetch(`${mgPath("history")}?id=${id}&metric=${metric}&window_s=10`).then((r) =>
    r.json(),
  );
  return j.samples?.at(-1)?.[1] ?? null;
}
const latestMetrics = (id, metrics) => Promise.all(metrics.map((m) => latestMetric(id, m)));

// Aggregated metrics from the loopback Microgrid client flow into the
// Dashboard pane via two paths: (a) /api/microgrid/latest at mode-
// enter time so the tiles paint immediately with a real number, and
// (b) microgrid_sample WS frames for the per-second updates. Every
// tile selects its source via `data-stream="..."`; new tiles only
// have to declare the right stream name to participate.
export const dashboardTiles = (() => {
  // 900 samples × 1 Hz cadence = 15 min sparkline window. Backfilled
  // from `/api/microgrid/history` on each backfill() (page load + mode
  // re-enter) so the trace shows the past quarter-hour immediately
  // instead of growing from empty. Per-tick noise from formula
  // sample-time misalignment looks like signal at 60 s windows; at
  // 15 min the trend dominates. Stored as a flat Float32Array of
  // length SPARK_LEN with a write cursor; on each push we overwrite
  // the oldest slot and bump the cursor. Cheaper than Array.shift on
  // a long array. NaN means "no sample at this slot" (the ring isn't
  // yet full).
  const SPARK_LEN = 900;
  const sparkBuf = new Map(); // stream -> { values: Float32Array, cursor: int }
  // Handles for the auto-reseed timer + visibility listener, so a
  // second startAutoReseed() (a re-init / reconnect) doesn't stack a
  // duplicate interval and listener that would multiply the polling.
  let reseedTimer = null;
  let reseedVisHandler = null;
  function buf(stream) {
    let b = sparkBuf.get(stream);
    if (!b) {
      b = { values: new Float32Array(SPARK_LEN).fill(NaN), cursor: 0 };
      sparkBuf.set(stream, b);
    }
    return b;
  }
  function pushSample(stream, value) {
    const b = buf(stream);
    b.values[b.cursor] = value == null ? NaN : value;
    b.cursor = (b.cursor + 1) % SPARK_LEN;
  }
  // Latest value pushed for a stream, read straight off the spark
  // ring buffer — the store paint() already keeps per stream — so
  // the site-PF derivation below doesn't need a second snapshot map.
  // The most-recently-written slot sits one behind the write cursor
  // (pushSample() advances the cursor after writing); backfill()
  // leaves the cursor at 0 with the newest historical sample in the
  // last slot, which the same formula resolves to.
  function latestValue(stream) {
    const b = sparkBuf.get(stream);
    if (!b) return null;
    const v = b.values[(b.cursor - 1 + SPARK_LEN) % SPARK_LEN];
    return Number.isNaN(v) ? null : v;
  }
  // Grid power and grid reactive power are separate microgrid_sample
  // streams; PF only makes sense once both have landed. The default
  // pair is whatever the spark rings hold — the WS hot path; callers
  // that just painted fresher numbers (reseedLatest) pass those in.
  function updateSitePf(p = latestValue("grid_power"), q = latestValue("grid_reactive_power")) {
    const el = document.getElementById("site-pf");
    if (!el) return;
    el.textContent = sitePfText(p, q);
  }
  // The tile elements are static markup in index.html, so the
  // per-stream lookups are resolved once and cached — paint() runs
  // per stream per 1 Hz WS sample, and two whole-document
  // querySelectorAlls per sample add up. Only non-empty results are
  // cached so a stream that gains markup later still resolves.
  const elCache = new Map();
  const sparkCache = new Map();
  function cached(cache, stream, query) {
    let els = cache.get(stream);
    if (!els) {
      els = [...document.querySelectorAll(query)];
      if (els.length) cache.set(stream, els);
    }
    return els;
  }
  function findEls(stream) {
    // Any non-svg element tagged with this stream — covers the main
    // .dash-value number plus envelope `.env-lo` / `.env-hi`
    // siblings that share the same stream's value formatting.
    return cached(elCache, stream, `[data-stream="${stream}"]:not(svg)`);
  }
  function findSparks(stream) {
    return cached(sparkCache, stream, `.dash-spark[data-stream="${stream}"]`);
  }
  function renderSpark(stream) {
    const svgs = findSparks(stream);
    if (!svgs.length) return;
    // This runs per stream per 1 Hz WS sample over a 900-slot ring,
    // so it works straight off the Float32Array without building
    // intermediate arrays: one pass for min/max/count, one for the
    // points string, and the markup is built once however many svgs
    // carry the stream. Ring order is oldest to newest; a slot's
    // linearised position keeps its temporal x even across NaN gaps.
    const b = buf(stream);
    let min = Infinity;
    let max = -Infinity;
    let count = 0;
    for (let i = 0; i < SPARK_LEN; i++) {
      const v = b.values[(b.cursor + i) % SPARK_LEN];
      if (Number.isNaN(v)) continue;
      count++;
      if (v < min) min = v;
      if (v > max) max = v;
    }
    if (count < 2) {
      // Not enough points to draw a line — show nothing rather
      // than a misleading single dot.
      for (const svg of svgs) svg.innerHTML = "";
      return;
    }
    const range = max - min || 1;
    // viewBox = 0..100 wide, 0..30 tall. 1 px padding top + bottom
    // so the line never clips at the edges.
    let points = "";
    for (let i = 0; i < SPARK_LEN; i++) {
      const v = b.values[(b.cursor + i) % SPARK_LEN];
      if (Number.isNaN(v)) continue;
      const x = (i / (SPARK_LEN - 1)) * 100;
      const y = 30 - (((v - min) / range) * 28 + 1);
      points += `${points ? " " : ""}${x.toFixed(1)},${y.toFixed(1)}`;
    }
    // Draw a y=0 baseline only when the window crosses zero —
    // for power tiles this is the import/export divider, and
    // it's noise on a constant-positive (e.g. consumer) tile.
    let baseline = "";
    if (min < 0 && max > 0) {
      const yZero = 30 - (((0 - min) / range) * 28 + 1);
      baseline = `<line class="baseline" x1="0" y1="${yZero.toFixed(1)}" x2="100" y2="${yZero.toFixed(1)}" />`;
    }
    const html = `${baseline}<polyline class="trace" points="${points}" />`;
    for (const svg of svgs) svg.innerHTML = html;
  }
  function paint(stream, snap) {
    for (const el of findEls(stream)) {
      el.textContent = fmt(snap.quantity, snap.unit, snap.value);
      el.classList.toggle("muted", snap.value == null);
    }
    pushSample(stream, snap.value);
    renderSpark(stream);
  }
  // Re-paint the tile value boxes from the server's cached latest
  // sample. Value-only on purpose: no pushSample (the sparkline ring
  // stays aligned to the WS / history-backfill sample flow) and no
  // history / formula refetch (neither drifts per tick). The WS
  // Sample stream is the primary live path, but it drops frames on
  // lag (events_ws `Lagged(_) => continue`) and a backgrounded tab
  // throttles its receiver — so a tile can otherwise freeze on a
  // stale value (e.g. producer_power and pv_power ride different
  // component streams, so one can stall while the other tracks).
  async function reseedLatest() {
    try {
      const res = await fetch(mgPath("microgrid/latest"));
      if (!res.ok) return;
      const map = await res.json();
      for (const [stream, snap] of Object.entries(map)) {
        for (const el of findEls(stream)) {
          el.textContent = fmt(snap.quantity, snap.unit, snap.value);
          el.classList.toggle("muted", snap.value == null);
        }
      }
      // The site-PF meta line is derived from the two grid streams, so
      // it goes stale exactly in the dropped-frame case this reseed
      // exists for. Recompute it from what we just painted, falling
      // back to the ring for a stream this snapshot didn't carry.
      const painted = (stream) => map[stream]?.value ?? latestValue(stream);
      updateSitePf(painted("grid_power"), painted("grid_reactive_power"));
    } catch (_) {
      // Best-effort. If the loopback isn't up yet (503 elsewhere),
      // the tiles stay on their last value until the next tick.
    }
  }
  return {
    applySample(ev) {
      // WS frame shape matches the snapshot shape, minus the kind
      // discriminator. Pass straight through.
      paint(ev.stream, ev);
      if (ev.stream === "grid_power" || ev.stream === "grid_reactive_power") {
        updateSitePf();
      }
    },
    // Safety net against dropped WS frames: re-seed the tile values
    // on a slow timer, and immediately whenever the tab returns to
    // the foreground (where the WS receiver was throttled and most
    // likely missed samples). Scoped to a visible dashboard — the
    // same gate applyMode() uses for backfill() — so we don't poll or
    // repaint while another subview shows or no microgrid is selected.
    // Cheap — one small JSON fetch; the sparklines and per-component
    // rows stay on the WS hot path.
    startAutoReseed(periodMs = 5000) {
      // Idempotent: tear down any prior timer/listener first so a
      // repeated call (re-init, reconnect) replaces rather than stacks.
      this.stopAutoReseed();
      const onDashboard = () =>
        document.body.dataset.mode === "microgrids" &&
        document.body.dataset.mgView === "selected" &&
        document.body.dataset.subview === "dashboard";
      reseedTimer = setInterval(() => {
        if (onDashboard()) reseedLatest();
      }, periodMs);
      reseedVisHandler = () => {
        if (!document.hidden && onDashboard()) reseedLatest();
      };
      document.addEventListener("visibilitychange", reseedVisHandler);
    },
    stopAutoReseed() {
      if (reseedTimer !== null) {
        clearInterval(reseedTimer);
        reseedTimer = null;
      }
      if (reseedVisHandler !== null) {
        document.removeEventListener("visibilitychange", reseedVisHandler);
        reseedVisHandler = null;
      }
    },
    // Clear one stream's sparkline ring. For feeders that backfill
    // a stream from outside this module's history map (the grid-
    // frequency tile) — without the reset, each dashboard re-enter
    // appends the same 60 s of history again and the trace renders
    // a falsely repeated pattern.
    resetStream(stream) {
      const b = buf(stream);
      b.values.fill(NaN);
      b.cursor = 0;
    },
    async backfill() {
      // Past 15 min of samples per stream, server-side. Pre-populate
      // the ring so the spark shows the historical trend right away
      // instead of growing from empty (60 s of jitter dominates an
      // unbackfilled trace; 15 min flattens it into a small bar).
      try {
        const hres = await fetch(mgPath("microgrid/history"));
        if (hres.ok) {
          const hmap = await hres.json();
          for (const [stream, samples] of Object.entries(hmap)) {
            const b = buf(stream);
            b.values.fill(NaN);
            // Keep only the last SPARK_LEN samples — the server cap
            // sits a hair over 900, but a slow client tab could lag
            // and pull more than that on a future endpoint version.
            const slice = samples.slice(-SPARK_LEN);
            const start = SPARK_LEN - slice.length;
            for (let i = 0; i < slice.length; i++) {
              const v = slice[i]?.value;
              b.values[start + i] = v == null ? NaN : v;
            }
            b.cursor = 0;
            renderSpark(stream);
          }
        }
      } catch (_) {
        // Best-effort. WS frames will fill the ring forward from here.
      }
      await reseedLatest();
      updateSitePf();
      // Same path picks up the rendered formula strings for each
      // tile's hover tooltip. Static across samples (the formula
      // doesn't change per tick), so one fetch per mode-enter is
      // enough — topology mutations re-trigger this via the
      // refreshTopology path in init().
      await loadFormulas();
    },
  };
})();

// One-rAF render coalescer shared by the row modules: applySample
// fires per metric per component per second, and re-building a
// section's innerHTML for every frame is wasted work — batch all
// the samples that land in one animation frame into one render.
function makeRenderScheduler(render) {
  let queued = false;
  return () => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(() => {
      queued = false;
      render();
    });
  };
}

// Shared formatters reused by all per-component dashboard rows.
const fmtRowPower = (v) => fmt("Power", "W", v);
function fmtRowSoc(v) {
  return v == null || !Number.isFinite(v) ? "—" : `${v.toFixed(1)}%`;
}
function socClass(v) {
  if (v == null || !Number.isFinite(v)) return "muted";
  if (v < 10 || v > 95) return "soc-warn";
  return "soc-ok";
}
// Column 6 of every dashboard row: the reactive-power readout, muted
// throughout because it reads as secondary to the active power beside
// it. Tiers that carry no Q at all (batteries, EV, CHP) still render
// the cell with a `—`: the shared six-column template in style.css
// only keeps the health pill aligned across tiers while every row
// fills every track. `VAr` is the display spelling of the "var"
// stream unit, as everywhere else in the UI.
const reactiveCell = (v) =>
  `<span class="tier-q muted">${v == null || !Number.isFinite(v) ? "—" : formatScaled(v, "VAr")}</span>`;

// ─── Battery pairs: battery + paired battery-inverter, one row each ───────
//
// Each row pairs a battery (`tier2-row` CSS, left side) with the
// battery inverter wired immediately upstream of it (`tier3-row`
// CSS, right side). The pairing is read off the topology snapshot's
// connections list: walk parents of each visible battery, keep the
// first inverter with subtype "battery". Multi-battery inverters
// produce one row per battery; bare batteries with no inverter
// upstream still render (right cell muted).
//
// Refreshed on every /api/topology fetch + live-updated via
// applySample. Clicking the battery cell selects the battery;
// clicking the inverter cell selects the inverter — both jump
// the canvas to Topology with that node selected.
export const batteryPairs = (() => {
  // id -> { battery: {…}, inverterId?: u64 }
  const pairs = new Map();
  // inverterId -> { name, subtype, health, measured, lower, upper }
  const inverters = new Map();
  // battery id -> backref to the inverter id it pairs with (for
  // sample dispatch). Keeps lookup O(1) on the WS hot path.
  const invByBattery = new Map();
  let order = []; // battery ids, sort by SoC ascending then id
  const TRACKED_BATTERY = new Set(["soc_pct", "dc_power_w"]);
  const TRACKED_INVERTER = new Set([
    "active_power_w",
    "active_power_lower_bound_w",
    "active_power_upper_bound_w",
    "reactive_power_var",
  ]);

  function sortKey(id) {
    const s = pairs.get(id)?.battery?.soc;
    return s == null ? Infinity : s;
  }
  function resort() {
    order = [...pairs.keys()].sort((a, b) => sortKey(a) - sortKey(b) || a - b);
  }
  function render() {
    const grid = document.getElementById("battery-rows");
    const section = grid?.closest(".dash-batteries");
    if (!grid || !section) return;
    section.hidden = pairs.size === 0;
    grid.innerHTML = "";
    for (const id of order) {
      const { battery: b, inverterId } = pairs.get(id);
      const inv = inverterId != null ? inverters.get(inverterId) : null;
      const wrap = document.createElement("div");
      wrap.className = "bat-pair";
      const socPct = b.soc == null ? 0 : Math.max(0, Math.min(100, b.soc));
      const bhCls = b.health === "ok" ? "health-ok" : "health-bad";
      const batCell = document.createElement("div");
      batCell.className = "tier2-row";
      batCell.dataset.id = id;
      batCell.innerHTML = `
        <span class="tier2-name">${escapeHtml(b.name)}</span>
        <span class="tier2-subtype">—</span>
        <span class="tier2-health ${bhCls}">${b.health}</span>
        <span class="tier2-soc-wrap">
          <span class="tier2-soc-bar ${socClass(b.soc)}" style="width:${socPct.toFixed(1)}%"></span>
          <span class="tier2-soc-text">${fmtRowSoc(b.soc)}</span>
        </span>
        <span class="tier2-power">${fmtRowPower(b.power_w)}</span>
        ${reactiveCell(null)}
      `;
      batCell.addEventListener("click", () => jumpToTopology(id));
      wrap.appendChild(batCell);
      const invCell = document.createElement("div");
      invCell.className = "tier3-row bat-pair-inv";
      if (inv) {
        invCell.dataset.id = inverterId;
        const ihCls = inv.health === "ok" ? "health-ok" : "health-bad";
        invCell.innerHTML = `
          <span class="tier3-name">${escapeHtml(inv.name)}</span>
          <span class="tier3-subtype muted">${inv.subtype || "—"}</span>
          <span class="tier3-health ${ihCls}">${inv.health}</span>
          ${envelopeBar(inv.lower, inv.measured, inv.upper, fmtRowPower)}
          ${reactiveCell(inv.reactive)}
        `;
        invCell.addEventListener("click", () => jumpToTopology(inverterId));
      } else {
        invCell.classList.add("muted");
        invCell.innerHTML = `<span class="tier3-name muted">no battery inverter</span>`;
      }
      wrap.appendChild(invCell);
      grid.appendChild(wrap);
    }
  }
  const scheduleRender = makeRenderScheduler(render);
  async function seedBattery(id) {
    try {
      const [soc, dc] = await latestMetrics(id, ["soc_pct", "dc_power_w"]);
      const p = pairs.get(id);
      if (!p) return;
      p.battery.soc = soc;
      p.battery.power_w = dc;
    } catch (_) {}
  }
  async function seedInverter(id) {
    try {
      const [m, lo, hi, q] = await latestMetrics(id, [
        "active_power_w",
        "active_power_lower_bound_w",
        "active_power_upper_bound_w",
        "reactive_power_var",
      ]);
      const inv = inverters.get(id);
      if (!inv) return;
      inv.measured = m;
      inv.lower = lo;
      inv.upper = hi;
      inv.reactive = q;
    } catch (_) {}
  }
  async function seedAll() {
    await Promise.all([
      ...[...pairs.keys()].map(seedBattery),
      ...[...inverters.keys()].map(seedInverter),
    ]);
    resort();
    render();
  }
  return {
    // Same seed contract as makeRowModule: `seed: false` while the
    // Dashboard is hidden, reseed() on subview enter.
    async refresh(snapshot, { seed: doSeed = true } = {}) {
      const components = snapshot?.components || [];
      const allConns = [
        ...(snapshot?.connections || []),
        ...(snapshot?.hidden_connections || []),
      ];
      const byId = new Map(components.map((c) => [c.id, c]));
      // Map each battery id → its first parent that's a battery
      // inverter (walking edges where dest == battery id). Multi-
      // parent batteries land on the first matching parent in
      // edge order; same heuristic the loopback's BatteryPool uses.
      function findInverter(batteryId) {
        for (const [from, to] of allConns) {
          if (to !== batteryId) continue;
          const parent = byId.get(from);
          if (parent?.category === "inverter" && parent.subtype === "battery") {
            return parent.id;
          }
        }
        return null;
      }
      const nextPairs = new Map();
      const nextInverters = new Map();
      const nextInvByBattery = new Map();
      const batteries = components.filter(
        (c) => c.category === "battery" && !c.hidden,
      );
      for (const b of batteries) {
        const inverterId = findInverter(b.id);
        const prev = pairs.get(b.id);
        nextPairs.set(b.id, {
          battery: {
            name: b.name,
            health: b.health,
            soc: prev?.battery?.soc ?? null,
            power_w: prev?.battery?.power_w ?? null,
          },
          inverterId,
        });
        if (inverterId != null) {
          const invMeta = byId.get(inverterId);
          const prevInv = inverters.get(inverterId);
          nextInverters.set(inverterId, {
            name: invMeta?.name ?? `#${inverterId}`,
            subtype: invMeta?.subtype ?? null,
            health: invMeta?.health ?? "unknown",
            measured: prevInv?.measured ?? null,
            lower: prevInv?.lower ?? null,
            upper: prevInv?.upper ?? null,
            reactive: prevInv?.reactive ?? null,
          });
          nextInvByBattery.set(b.id, inverterId);
        }
      }
      pairs.clear();
      for (const [k, v] of nextPairs) pairs.set(k, v);
      inverters.clear();
      for (const [k, v] of nextInverters) inverters.set(k, v);
      invByBattery.clear();
      for (const [k, v] of nextInvByBattery) invByBattery.set(k, v);
      resort();
      render();
      if (doSeed) await seedAll();
    },
    reseed: seedAll,
    applySample(ev) {
      if (TRACKED_BATTERY.has(ev.metric)) {
        const p = pairs.get(ev.id);
        if (!p) return;
        if (ev.metric === "soc_pct") p.battery.soc = ev.value;
        else if (ev.metric === "dc_power_w") p.battery.power_w = ev.value;
        if (ev.metric === "soc_pct") resort();
        scheduleRender();
      } else if (TRACKED_INVERTER.has(ev.metric)) {
        const inv = inverters.get(ev.id);
        if (!inv) return;
        if (ev.metric === "active_power_w") inv.measured = ev.value;
        else if (ev.metric === "active_power_lower_bound_w") inv.lower = ev.value;
        else if (ev.metric === "active_power_upper_bound_w") inv.upper = ev.value;
        else if (ev.metric === "reactive_power_var") inv.reactive = ev.value;
        scheduleRender();
      }
    },
  };
})();

// Shared envelope renderer for a (lower, current, upper) triple.
// Returns an HTML fragment that draws a horizontal track with a
// marker at `current`'s position between `lower` and `upper`,
// pinned-hi / pinned-lo classes when the marker hits either edge
// within 0.5 % of the span. Falls back to a muted "—" placeholder
// when bounds are missing or degenerate so the row still aligns.
//
// `fmtValue` formats both the marker readout and the hover-tooltip
// endpoints; callers pass it pre-bound to whatever unit family the
// row deals in (W / kW for Power, % for Percentage, etc.) — keeps
// the helper agnostic of the tiles' quantity table.
function envelopeBar(lower, current, upper, fmtValue) {
  const finite = (v) => v != null && Number.isFinite(v);
  if (!finite(lower) || !finite(upper) || upper <= lower) {
    return `<div class="envelope muted"><span class="envelope-current">—</span></div>`;
  }
  const hasCurrent = finite(current);
  const span = upper - lower;
  const pos = hasCurrent ? Math.max(0, Math.min(1, (current - lower) / span)) : 0.5;
  const tol = 0.005 * span;
  let markerCls = "envelope-marker";
  if (hasCurrent && current >= upper - tol) markerCls += " pinned-hi";
  else if (hasCurrent && current <= lower + tol) markerCls += " pinned-lo";
  const readout = hasCurrent ? fmtValue(current) : "—";
  const title = `${fmtValue(lower)} → ${fmtValue(upper)}`;
  return `
    <div class="envelope" title="${title}">
      <div class="envelope-track">
        <span class="${markerCls}" style="left:${(pos * 100).toFixed(1)}%"></span>
      </div>
      <span class="envelope-current">${readout}</span>
    </div>
  `;
}

// ─── Per-category row modules ─────────────────────────────────────────────
//
// PV / EV / CHP rows are one pattern instantiated three times: a
// data map keyed by component id, a topology-driven refresh that
// preserves live values across shape changes, a seed pass pulling
// each tracked metric's latest sample, a rAF-coalesced render, and
// a WS applySample dispatch. The factory owns that plumbing; each
// instance declares only its filter, its metric→field table, and
// its row markup.
//
// `fields` maps data-object field → history/WS metric name — one
// table drives both the seed fetches and the applySample dispatch.
function makeRowModule({ gridId, sectionSel, filter, fields, rowClass, rowHtml }) {
  const data = new Map(); // id -> { name, subtype, health, ...fields }
  let order = [];
  const fieldNames = Object.keys(fields);
  const metricToField = new Map(fieldNames.map((f) => [fields[f], f]));

  // Stable id order: live values must not move rows.
  function resort() {
    order = [...data.keys()].sort((a, b) => a - b);
  }
  function render() {
    const grid = document.getElementById(gridId);
    const section = grid?.closest(sectionSel);
    if (!grid || !section) return;
    section.hidden = data.size === 0;
    grid.innerHTML = "";
    for (const id of order) {
      const d = data.get(id);
      const row = document.createElement("div");
      row.className = rowClass(d);
      row.dataset.id = id;
      row.innerHTML = rowHtml(d);
      row.addEventListener("click", () => jumpToTopology(id));
      grid.appendChild(row);
    }
  }
  const scheduleRender = makeRenderScheduler(render);
  async function seed(id) {
    try {
      const vals = await latestMetrics(id, fieldNames.map((f) => fields[f]));
      const d = data.get(id);
      if (!d) return;
      fieldNames.forEach((f, i) => {
        d[f] = vals[i];
      });
    } catch (_) {}
  }
  async function seedAll() {
    await Promise.all([...data.keys()].map(seed));
    resort();
    render();
  }
  return {
    // `seed: false` skips the per-component latest-sample fetches —
    // callers pass it while the Dashboard subview is hidden, and
    // reseed() catches up when it becomes visible.
    async refresh(snapshot, { seed: doSeed = true } = {}) {
      const components = snapshot?.components || [];
      const rows = components.filter((c) => !c.hidden && filter(c));
      const next = new Map();
      for (const c of rows) {
        const prev = data.get(c.id);
        const d = { name: c.name, subtype: c.subtype, health: c.health };
        for (const f of fieldNames) d[f] = prev?.[f] ?? null;
        next.set(c.id, d);
      }
      data.clear();
      for (const [k, v] of next) data.set(k, v);
      resort();
      render();
      if (doSeed) await seedAll();
    },
    reseed: seedAll,
    applySample(ev) {
      const f = metricToField.get(ev.metric);
      if (!f) return;
      const d = data.get(ev.id);
      if (!d) return;
      d[f] = ev.value;
      scheduleRender();
    },
  };
}

// One row per visible solar inverter, in stable id order — at-limit
// state must not move rows (a setpoint arriving or expiring flips it,
// and rows that jump around read as mysterious). The envelope
// marker's at-bound dot carries that signal instead. Battery
// inverters are intentionally absent from this section; they pair
// with their batteries in the Batteries section above — but they
// share the row shape, Q column included: a solar inverter publishes
// `reactive_power_var` on the same telemetry path.
export const pvRows = makeRowModule({
  gridId: "pv-rows",
  sectionSel: ".dash-pv",
  filter: (c) => c.category === "inverter" && c.subtype === "solar",
  fields: {
    measured: "active_power_w",
    lower: "active_power_lower_bound_w",
    upper: "active_power_upper_bound_w",
    reactive: "reactive_power_var",
  },
  rowClass: () => "tier3-row",
  rowHtml: (d) => `
        <span class="tier3-name">${escapeHtml(d.name)}</span>
        <span class="tier3-subtype muted">${d.subtype || "—"}</span>
        <span class="tier3-health ${d.health === "ok" ? "health-ok" : "health-bad"}">${d.health}</span>
        ${envelopeBar(d.lower, d.measured, d.upper, fmtRowPower)}
        ${reactiveCell(d.reactive)}
      `,
});

// EV rows mirror the battery row shape: name + health pill + SoC bar
// + DC power. Click → jump to Topology with the EV selected.
export const evRows = makeRowModule({
  gridId: "ev-rows",
  sectionSel: ".dash-ev",
  filter: (c) => c.category === "ev-charger",
  fields: { soc: "soc_pct", power_w: "dc_power_w" },
  rowClass: () => "tier5-row cat-ev-charger",
  rowHtml: (d) => {
    const socPct = d.soc == null ? 0 : Math.max(0, Math.min(100, d.soc));
    return `
        <span class="tier5-name">${escapeHtml(d.name)}</span>
        <span class="tier5-cat muted">ev-charger</span>
        <span class="tier5-health ${d.health === "ok" ? "health-ok" : "health-bad"}">${d.health}</span>
        <span class="tier5-soc-wrap">
          <span class="tier5-soc-bar" style="width:${socPct.toFixed(1)}%"></span>
          <span class="tier5-soc-text">${fmtRowSoc(d.soc)}</span>
        </span>
        <span class="tier5-power">${fmtRowPower(d.power_w)}</span>
        ${reactiveCell(null)}
      `;
  },
});

// CHP rows show name + health + AC active power; no SoC field. The
// AC reading is signed (-ve when generating into the grid).
export const chpRows = makeRowModule({
  gridId: "chp-rows",
  sectionSel: ".dash-chp",
  filter: (c) => c.category === "chp",
  fields: { power_w: "active_power_w" },
  rowClass: () => "tier5-row cat-chp",
  rowHtml: (d) => `
        <span class="tier5-name">${escapeHtml(d.name)}</span>
        <span class="tier5-cat muted">chp</span>
        <span class="tier5-health ${d.health === "ok" ? "health-ok" : "health-bad"}">${d.health}</span>
        <span class="tier5-soc-wrap muted">—</span>
        <span class="tier5-power">${fmtRowPower(d.power_w)}</span>
        ${reactiveCell(null)}
      `,
});
