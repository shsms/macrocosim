// The metrics panel: three foldable cards — Power, Reactive power,
// Frequency — each one combined uPlot chart over the loopback's
// aggregate streams plus a chip row that is legend, live readout,
// and series toggle in one. It took over from the retired Dashboard
// subview's tiles; the store (metrics-store.js) owns the data, this
// module owns the DOM. Series colors follow the category palette so
// chart lines mean what the canvas already means.

import { gridFrequency } from "./chrome.js";
import { fmtValue, metricsStore, pfText, pfValue } from "./metrics-store.js";
import { isPanelOpen, makeSidePanelToggle } from "./side-panel.js";

const PANEL = "metrics-btn";
const WINDOWS = [
  { key: "1m", secs: 60 },
  { key: "5m", secs: 300 },
  { key: "10m", secs: 600 },
];

const CARDS = [
  {
    key: "power",
    title: "Power",
    headline: "grid_power",
    defaultOpen: true,
    series: [
      { stream: "grid_power", label: "grid", color: "--cat-grid" },
      {
        stream: "battery_pool_power",
        label: "battery",
        color: "--cat-battery",
        band: ["battery_pool_bounds_lower", "battery_pool_bounds_upper"],
      },
      { stream: "pv_power", label: "pv", color: "--cat-inverter-solar" },
      { stream: "consumer_power", label: "consumer", color: "--flow-import" },
      { stream: "producer_power", label: "producer", color: "--flow-export" },
    ],
  },
  {
    key: "reactive",
    title: "Reactive power",
    headline: "grid_reactive_power",
    defaultOpen: false,
    pfOverlay: true,
    series: [
      { stream: "grid_reactive_power", label: "grid", color: "--cat-grid", p: "grid_power" },
      { stream: "pv_reactive_power", label: "pv", color: "--cat-inverter-solar", p: "pv_power" },
      {
        stream: "battery_reactive_power",
        label: "battery",
        color: "--cat-battery",
        p: "battery_pool_power",
      },
    ],
  },
  {
    key: "frequency",
    title: "Frequency",
    headline: "grid_frequency",
    defaultOpen: false,
    series: [{ stream: "grid_frequency", label: "frequency", color: "--accent" }],
  },
];

// ── persisted knobs (same try/catch discipline as the inspector) ──
function loadKey(key, fallback) {
  try {
    const v = localStorage.getItem(key);
    return v == null ? fallback : v;
  } catch (_) {
    return fallback;
  }
}
function saveKey(key, value) {
  try {
    localStorage.setItem(key, value);
  } catch (_) {
    // Storage unavailable — the choice just doesn't stick.
  }
}
const cardOpen = (c) => loadKey(`sw-metrics-card-${c.key}`, c.defaultOpen ? "1" : "0") === "1";
const seriesOn = (s) => loadKey(`sw-metrics-series-${s.stream}`, "1") === "1";
const pfOn = () => loadKey("sw-metrics-pf", "0") === "1";
const windowSecs = () => {
  const k = loadKey("sw-metrics-window", "5m");
  return (WINDOWS.find((w) => w.key === k) ?? WINDOWS[1]).secs;
};

const cssColor = (v) => getComputedStyle(document.documentElement).getPropertyValue(v).trim();

// One live uPlot per unfolded card; rebuilt (not re-dataed) on any
// config change — window, series toggle, PF overlay, unfold — and
// destroyed on fold/close. plots: card key → { plot, assemble }.
let plots = new Map();
let unsubscribe = null;
let repaintQueued = false;

function destroyPlots() {
  for (const { plot } of plots.values()) plot.destroy();
  plots = new Map();
}

// Scale choice across every visible series of a card so they share
// one y-axis honestly (the inspector's chooseScale, multi-series).
function chooseDiv(values) {
  const max = Math.max(0, ...values.map((v) => Math.abs(v)));
  if (max >= 1e6) return { div: 1e6, prefix: "M" };
  if (max >= 1e3) return { div: 1e3, prefix: "k" };
  return { div: 1, prefix: "" };
}

// The spec's dashed y=0 line: the import/export divide, drawn only
// where it means something. Guarded twice — the "y" scale only, so
// the PF overlay's right-hand 0.85–1.02 axis never gets one, and
// only when the window's range actually straddles zero, so a chart
// living wholly above it (frequency, a site that never exports)
// doesn't grow a line welded to its floor. uPlot hands canvas hooks
// a device-pixel context (valToPos's third argument asks for the
// same space), so the width and dash lengths scale by the canvas's
// own pixel ratio or they'd be hairlines on a HiDPI screen.
function drawZeroLine(u) {
  const { min, max } = u.scales.y;
  if (min == null || max == null || min > 0 || max < 0) return;
  const dpr = u.ctx.canvas.width / (u.width || 1);
  const { left, top, width, height } = u.bbox;
  // Snap the stroke's centre so its edges land on device-pixel
  // boundaries: an unsnapped 1px line straddles two rows and
  // antialiases into a grey smear half the intended contrast.
  const y = Math.round(u.valToPos(0, "y", true) - dpr / 2) + dpr / 2;
  const ctx = u.ctx;
  ctx.save();
  // Clip to the plot area: a rounded zero on a range whose edge sits
  // a fraction of a pixel away would otherwise bleed into the axis.
  ctx.beginPath();
  ctx.rect(left, top, width, height);
  ctx.clip();
  ctx.strokeStyle = "#3d4450";
  ctx.lineWidth = dpr;
  ctx.setLineDash([4 * dpr, 4 * dpr]);
  ctx.beginPath();
  ctx.moveTo(left, y);
  ctx.lineTo(left + width, y);
  ctx.stroke();
  ctx.restore();
}

function buildChart(card, slot) {
  const active = card.series.filter(seriesOn);
  if (!active.length) {
    slot.innerHTML = '<p class="hint">all series toggled off</p>';
    return;
  }
  // Scale + shape decisions are made once per build; assemble() then
  // regenerates the full data array — active series, envelope-band
  // series, PF series — in exactly the built shape, so the repaint
  // path can setData() without ever guessing whether shapes match.
  const secs = windowSecs();
  const all = [];
  for (const s of active) {
    for (const v of metricsStore.series(s.stream, secs).ys) if (v != null) all.push(v);
  }
  const unit = metricsStore.latest(card.headline)?.unit ?? "";
  const shown = unit === "var" ? "VAr" : unit;
  const isPower = unit === "W" || unit === "var";
  const { div, prefix } = isPower ? chooseDiv(all) : { div: 1, prefix: "" };
  const scaled = (stream) =>
    metricsStore.series(stream, secs).ys.map((v) => (v == null ? null : v / div));
  // Battery envelope: lower/upper bounds as invisible series with a
  // translucent band between them, behind the battery trace. Both
  // edges need two samples in the window before the band earns its
  // place: one point has no area to fill, and an envelope drawn from
  // it would still stretch the shared y-axis out to the pool's rated
  // power and squash every trace for nothing. The bounds forwarder
  // republishes only when the envelope moves, so on an idle site
  // that is exactly what the window holds.
  const bandCfg = active.find((s) => s.band);
  const bandPoints = (b) => metricsStore.series(b, secs).ys.filter((v) => v != null).length;
  const hasBand = bandCfg?.band.every((b) => bandPoints(b) >= 2) === true;
  const withPf = card.pfOverlay === true && pfOn();
  const assemble = () => {
    const data = [metricsStore.series(active[0].stream, secs).xs];
    for (const s of active) data.push(scaled(s.stream));
    if (hasBand) {
      data.push(scaled(bandCfg.band[1]), scaled(bandCfg.band[0]));
    }
    if (withPf) {
      for (const s of active) {
        const p = metricsStore.series(s.p, secs).ys;
        const q = metricsStore.series(s.stream, secs).ys;
        data.push(q.map((qv, i) => pfValue(p[i], qv)));
      }
    }
    return data;
  };
  const series = [
    {},
    ...active.map((s) => ({
      stroke: cssColor(s.color),
      width: 1.5,
      points: { show: false },
      spanGaps: false,
    })),
  ];
  const bands = [];
  if (hasBand) {
    // spanGaps on the edges only: a bound holds until it is
    // republished, so a gap between two bound samples is the same
    // envelope, not missing data — unlike the traces, where a gap
    // means no sample arrived and has to read as one.
    series.push(
      { stroke: "transparent", points: { show: false }, spanGaps: true },
      { stroke: "transparent", points: { show: false }, spanGaps: true },
    );
    // data indices: 0 = xs, 1..active.length = traces, then hi, lo.
    // uPlot fills a band from series[0] down to series[1] (its
    // default dir), so the upper bound has to be listed first.
    bands.push({
      series: [active.length + 1, active.length + 2],
      fill: "rgba(241, 149, 91, 0.10)",
    });
  }
  // PF overlay: dashed per-source PF on a right-hand 0.85–1.02
  // scale, derived at draw time from the matching P and Q rings.
  const axes = [
    { stroke: "#7d848e", grid: { stroke: "#353a45", width: 0.5 } },
    {
      stroke: "#7d848e",
      grid: { stroke: "#353a45", width: 0.5 },
      size: 56,
      label: prefix || shown ? `${prefix}${shown}` : "",
      labelSize: 12,
    },
  ];
  if (withPf) {
    for (const s of active) {
      series.push({
        stroke: cssColor(s.color),
        width: 1,
        dash: [3, 3],
        points: { show: false },
        scale: "pf",
      });
    }
    axes.push({
      scale: "pf",
      side: 1,
      stroke: "#7d848e",
      grid: { show: false },
      size: 44,
    });
  }
  const opts = {
    width: slot.clientWidth || 380,
    height: 150,
    cursor: { drag: { x: false, y: false } },
    legend: { show: false },
    scales: { x: { time: true }, pf: { range: [0.85, 1.02] } },
    axes,
    series,
    bands,
    hooks: { draw: [drawZeroLine] },
  };
  plots.set(card.key, { plot: new uPlot(opts, assemble(), slot), assemble });
}

// Chips + fold summaries repaint on every store notify (rAF-
// coalesced); charts re-data on the same tick.
function repaint(contentEl) {
  for (const card of CARDS) {
    const summary = contentEl.querySelector(`[data-summary="${card.key}"]`);
    if (summary) {
      const head = metricsStore.latest(card.headline);
      let text = head ? fmtValue(head.quantity, head.unit, head.value) : "—";
      if (card.key === "reactive") {
        const p = metricsStore.latest("grid_power")?.value;
        text += ` · ${pfText(p, head?.value ?? null)}`;
      }
      if (card.key === "power") text = `grid ${text}`;
      summary.textContent = text;
    }
    for (const s of card.series) {
      const v = contentEl.querySelector(`[data-chip-value="${s.stream}"]`);
      if (!v) continue;
      const snap = metricsStore.latest(s.stream);
      v.textContent = snap ? fmtValue(snap.quantity, snap.unit, snap.value) : "—";
      if (s.p) {
        const pf = contentEl.querySelector(`[data-chip-pf="${s.stream}"]`);
        if (pf) pf.textContent = pfText(metricsStore.latest(s.p)?.value, snap?.value ?? null);
      }
    }
  }
  // Charts re-data in place: assemble() regenerates the exact data
  // shape each plot was built with, so no shape checks are needed.
  // Scale (kW vs MW) and band presence refresh on the next rebuild
  // (window / toggle / unfold), not per tick.
  for (const { plot, assemble } of plots.values()) plot.setData(assemble());
}

function scheduleRepaint(contentEl) {
  if (repaintQueued) return;
  repaintQueued = true;
  requestAnimationFrame(() => {
    repaintQueued = false;
    if (isPanelOpen(PANEL)) repaint(contentEl);
  });
}

function rebuildCard(key) {
  const entry = plots.get(key);
  const card = CARDS.find((c) => c.key === key);
  const slot = document.querySelector(`#panel-${PANEL} [data-chart="${key}"]`);
  if (entry) {
    entry.plot.destroy();
    plots.delete(key);
  }
  if (card && slot && cardOpen(card)) {
    slot.innerHTML = "";
    buildChart(card, slot);
  }
}

function chipHtml(s) {
  return `
    <button type="button" class="mchip${seriesOn(s) ? "" : " off"}" data-chip="${s.stream}"
            style="--chip-color: var(${s.color})">
      <span class="mchip-dot"></span>
      <span class="mchip-name">${s.label}</span>
      <span class="mchip-value" data-chip-value="${s.stream}">—</span>
      ${s.p ? `<span class="mchip-pf" data-chip-pf="${s.stream}">PF —</span>` : ""}
    </button>`;
}

function cardHtml(card) {
  const open = cardOpen(card);
  const pfChip = card.pfOverlay
    ? `<button type="button" class="mchip mchip-pf-toggle${pfOn() ? "" : " off"}" data-pf-toggle>
         <span class="mchip-name">PF overlay</span>
       </button>`
    : "";
  return `
    <section class="mcard${open ? " open" : ""}" data-card="${card.key}">
      <h3 class="fold-toggle" data-fold-toggle>${card.title}
        <span class="fold-summary"><span data-summary="${card.key}">—</span><span class="fold-chevron">▾</span></span>
      </h3>
      <div class="fold-body">
        <div class="mchart" data-chart="${card.key}"></div>
        <div class="mchips">${card.series.map(chipHtml).join("")}${pfChip}</div>
      </div>
    </section>`;
}

function render(contentEl) {
  const winKey = loadKey("sw-metrics-window", "5m");
  contentEl.innerHTML = `
    <div class="metrics-panel">
      <div class="metrics-head">
        <h2>Metrics</h2>
        <span class="ctl-label">window</span>
        ${WINDOWS.map(
          (w) =>
            `<button type="button" class="pill win-pill${w.key === winKey ? " active" : ""}" data-window="${w.key}">${w.key}</button>`,
        ).join("")}
      </div>
      ${CARDS.map(cardHtml).join("")}
    </div>`;

  contentEl.querySelector(".metrics-panel").addEventListener("click", (ev) => {
    const win = ev.target.closest("[data-window]");
    if (win) {
      saveKey("sw-metrics-window", win.dataset.window);
      for (const b of contentEl.querySelectorAll("[data-window]")) {
        b.classList.toggle("active", b === win);
      }
      for (const c of CARDS) rebuildCard(c.key);
      return;
    }
    const pfToggle = ev.target.closest("[data-pf-toggle]");
    if (pfToggle) {
      saveKey("sw-metrics-pf", pfOn() ? "0" : "1");
      pfToggle.classList.toggle("off", !pfOn());
      rebuildCard("reactive");
      return;
    }
    const chip = ev.target.closest("[data-chip]");
    if (chip) {
      const stream = chip.dataset.chip;
      const card = CARDS.find((c) => c.series.some((s) => s.stream === stream));
      saveKey(`sw-metrics-series-${stream}`, seriesOn({ stream }) ? "0" : "1");
      chip.classList.toggle("off", !seriesOn({ stream }));
      if (card) rebuildCard(card.key);
      return;
    }
    const foldToggle = ev.target.closest("[data-fold-toggle]");
    if (foldToggle) {
      const cardEl = foldToggle.closest("[data-card]");
      const card = CARDS.find((c) => c.key === cardEl.dataset.card);
      const nowOpen = !cardEl.classList.contains("open");
      cardEl.classList.toggle("open", nowOpen);
      saveKey(`sw-metrics-card-${card.key}`, nowOpen ? "1" : "0");
      rebuildCard(card.key);
    }
  });

  for (const card of CARDS) rebuildCard(card.key);
  unsubscribe = metricsStore.subscribe(() => scheduleRepaint(contentEl));
  // The dropped-frame safety net only earns its poll while the panel
  // is on screen, so it runs on the panel's own lifetime rather than
  // the document's — a process-lifetime interval also keeps the
  // headless boot smoke's event loop alive forever.
  metricsStore.startAutoReseed();
  metricsStore.backfill().then(() => {
    if (!isPanelOpen(PANEL)) return;
    for (const c of CARDS) rebuildCard(c.key);
    repaint(contentEl);
  });
  gridFrequency.backfill();
}

function teardown() {
  unsubscribe?.();
  unsubscribe = null;
  metricsStore.stopAutoReseed();
  destroyPlots();
}

// Re-backfill on topology mutations while open — app.js's debounced
// topology-backfill hook calls this; closed, the next open backfills
// anyway.
export function metricsTopologyRefresh() {
  if (isPanelOpen(PANEL)) metricsStore.backfill();
}

export function setupMetricsPanel() {
  makeSidePanelToggle(PANEL, render, teardown);
}
