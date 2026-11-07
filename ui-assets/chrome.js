// Chrome around the SPA's main views: the configurable-zone clock,
// the script chip and the system pulse in the top bar.

import { escapeHtml } from "./html.js";
import { getJson } from "./http.js";
import { mgFetch, setupDensityToggle } from "./routing.js";
import * as theme from "./theme.js";
import * as zone from "./zone.js";

// ─── Clock + zone chip ─────────────────────────────────────────────────────
//
// The chip switches every displayed time between this browser's zone and
// UTC (zone.js); the clock shows the time now in the zone in use.
export function setupZoneChip() {
  const chip = document.getElementById("tz-toggle");
  if (!chip) return;
  const paint = () => {
    chip.textContent = zone.isUtc() ? "UTC" : zone.label().toLowerCase();
    chip.classList.toggle("active", zone.isUtc());
    renderClock();
  };
  chip.addEventListener("click", () => zone.chooseUtc(!zone.isUtc()));
  zone.onChange(paint);
  paint();
}

function renderClock() {
  const el = document.getElementById("pulse-clock");
  if (el) el.textContent = zone.fmtTime(Date.now());
}

// ─── Script chip ───────────────────────────────────────────────────────────
//
// The engine's script (`macrocosim plant.py`): its child's state on the
// chip, and the last reload and the child's recent output in a popover
// the chip opens. Hidden when the engine runs no script. Filled from
// GET /api/runner at boot and on every open of the events socket (which
// sends only changes), then from every `script_status` event.
const SCRIPT_BAD = new Set(["crashed"]);

// Every `script_status` event and every GET of the status takes the next
// number. A GET answer paints only if nothing numbered after its read has
// painted.
let scriptSeq = 0;
// The number of the last event or GET answer painted.
let paintedSeq = 0;

// Handle a `script_status` event from the events socket: paint it.
export function onScriptStatusEvent(status) {
  paintedSeq = ++scriptSeq;
  paintScriptStatus(status);
}

function paintScriptStatus(status) {
  const chip = document.getElementById("script-chip");
  const pop = document.getElementById("script-popover");
  if (!chip || !pop) return;
  chip.hidden = status.script == null;
  if (chip.hidden) return;
  const state = status.child?.state ?? "stopped";
  const failed = status.last_reload?.result === "failed_to_load";
  chip.textContent = `script ${state}${failed ? " · reload failed" : ""}`;
  chip.dataset.state = state;
  chip.classList.toggle("bad", SCRIPT_BAD.has(state) || failed);
  chip.title = `${status.script}: ${state}${failed ? ", the last reload failed to load" : ""}`;
  const exit = status.child?.exit_code != null ? ` (exit ${status.child.exit_code})` : "";
  const reload = status.last_reload
    ? `<p>last reload: ${escapeHtml(status.last_reload.result.replaceAll("_", " "))} ${zone.timeHtml(status.last_reload.updated_at, "hms")}</p>${
        failed ? `<pre class="script-output">${escapeHtml(status.last_reload.output.join("\n"))}</pre>` : ""
      }`
    : "";
  pop.innerHTML = `
    <h3>${escapeHtml(status.script)}</h3>
    <p>${escapeHtml(state)}${escapeHtml(exit)}</p>
    ${reload}
    <h4>output</h4>
    <pre class="script-output">${status.output.length ? escapeHtml(status.output.join("\n")) : "(none yet)"}</pre>`;
}

export async function setupScriptChip() {
  const chip = document.getElementById("script-chip");
  // The popover opens under the chip, wherever the top bar put it.
  document.getElementById("script-popover")?.addEventListener("beforetoggle", (e) => {
    if (e.newState === "open" && chip) e.target.style.top = `${chip.getBoundingClientRect().bottom + 4}px`;
  });
  await refreshScriptStatus();
}

// Read the script's status and paint the chip with it, unless a
// `script_status` event painted since the read started, or a newer read
// painted first.
export async function refreshScriptStatus() {
  const mine = ++scriptSeq;
  try {
    const status = await getJson("/api/runner");
    if (mine > paintedSeq) {
      paintedSeq = mine;
      paintScriptStatus(status);
    }
  } catch (_) {
    // The events socket's next open, or its next change, reads it again.
  }
}

// ─── Theme chip ────────────────────────────────────────────────────────────
//
// Cycles the theme preference auto → light → dark (theme.js).
const THEME_LABEL = { auto: "◐ auto", light: "☀ light", dark: "☾ dark" };

export function setupThemeChip() {
  const chip = document.getElementById("theme-toggle");
  if (!chip) return;
  const paint = () => {
    chip.textContent = THEME_LABEL[theme.preference()];
  };
  chip.addEventListener("click", () => {
    theme.choose(theme.nextPreference(theme.preference()));
    paint();
  });
  paint();
}

// ─── System pulse ──────────────────────────────────────────────────────────
//
// The always-on system pulse group. The live sources:
//   - Setpoint sparkbar: rate of /ws/events kind="setpoint" frames,
//     bucketed into 12 × 5 s windows over the last minute.
//   - Health pill: rolling counters from the topology route's health
//     field — recomputed every refreshTopology() call (WS push on
//     topology_changed already drives this).
//   - Graph pill: the topology route's graph_status — ✓ when the
//     component-graph validator accepted the topology, ⚠ (with the
//     error on click) when it rejected.
//   - Loopback pill: metrics/status polled every 5 s. ✓ when
//     connected, ⚠ when still booting.
//   - Wall clock at the right edge, ticked every second.
//
// All panels are read-only and tolerant of partial data — a
// page loaded before the loopback comes up shows ⚠ and flips to
// ✓ on the next poll. Mirrors tradingsim's `.pulse` shape so the
// developer sees the same "is the sim alive" pattern across both
// simulators.
export const pulseBar = (() => {
  const SPARK_BUCKETS = 12;
  const BUCKET_MS = 5000;
  const buckets = new Array(SPARK_BUCKETS).fill(0);
  let lastSpan = pulseBucketIndex();
  function pulseBucketIndex() {
    // Floor of (now / BUCKET_MS) — when this rolls forward, every
    // bucket between lastSpan and now shifts in as a 0.
    return Math.floor(Date.now() / BUCKET_MS);
  }
  function rotateIfNeeded() {
    const idx = pulseBucketIndex();
    const advance = Math.min(idx - lastSpan, SPARK_BUCKETS);
    for (let i = 0; i < advance; i++) {
      buckets.shift();
      buckets.push(0);
    }
    lastSpan = idx;
  }
  function recordSetpoint() {
    rotateIfNeeded();
    buckets[SPARK_BUCKETS - 1] += 1;
    renderSpark();
  }
  function renderSpark() {
    const svg = document.getElementById("pulse-spark");
    if (!svg) return;
    const max = Math.max(1, ...buckets);
    // SVG viewBox is 0..60 wide × 0..16 tall. 5 px wide per bar
    // with no gap (the trace reads as a continuous histogram). Bar
    // height proportional to bucket / max; minimum 1 px so a single
    // event is still visible.
    const bw = 60 / SPARK_BUCKETS;
    const bars = buckets
      .map((v, i) => {
        const h = v === 0 ? 0 : Math.max(1, (v / max) * 16);
        const x = i * bw;
        const y = 16 - h;
        return `<rect class="bar" x="${x.toFixed(2)}" y="${y.toFixed(2)}" width="${(bw - 0.5).toFixed(2)}" height="${h.toFixed(2)}" />`;
      })
      .join("");
    svg.innerHTML = bars;
  }
  function renderHealth(components) {
    const counts = { ok: 0, standby: 0, error: 0 };
    for (const c of components) {
      const h = (c.health || "ok").toLowerCase();
      if (h in counts) counts[h] += 1;
    }
    const el = document.getElementById("pulse-health");
    if (!el) return;
    el.innerHTML = `
      <span class="health-chip ok"      title="ok components">OK ${counts.ok}</span>
      <span class="health-chip standby" title="standby components">STDBY ${counts.standby}</span>
      <span class="health-chip error"   title="error components">ERR ${counts.error}</span>`;
  }
  function renderGraph(status) {
    const el = document.getElementById("pulse-graph");
    if (!el) return;
    if (status == null) {
      el.textContent = "✓";
      el.className = "pulse-pill ok";
      el.title = "frequenz-microgrid-component-graph accepted the topology";
      el.onclick = null;
    } else {
      // Compact for the pill, full message in the title + alert on
      // click so the dev can read past the truncation.
      el.textContent = "⚠ rejected";
      el.className = "pulse-pill bad";
      el.title = status;
      el.onclick = () => alert(`Graph validator rejected the topology:\n\n${status}`);
    }
  }
  async function refreshLoopback() {
    const el = document.getElementById("pulse-loopback");
    if (!el) return;
    try {
      const res = await mgFetch("metrics/status", undefined, "loopback");
      if (res == null) {
        el.textContent = "…";
        el.className = "pulse-pill";
        return;
      }
      const j = await res.json();
      if (res.ok && j.connected) {
        el.textContent = "✓ connected";
        el.className = "pulse-pill ok";
      } else {
        el.textContent = "⚠ connecting";
        el.className = "pulse-pill warn";
      }
    } catch (_) {
      el.textContent = "✗ unreachable";
      el.className = "pulse-pill bad";
    }
  }
  return {
    setup() {
      renderSpark();
      renderHealth([]);
      renderGraph(null);
      refreshLoopback();
      renderClock();
      setupDensityToggle();
      // Loopback poll: every 5 s while not connected, every 15 s
      // once connected (cheap heartbeat, picks up a server restart
      // within one cycle). Constants kept generous so a slow page
      // doesn't see the pill flicker on a stalled fetch.
      setInterval(refreshLoopback, 5000);
      // 1 Hz clock + spark rotation; the spark rotator also handles
      // the case where no setpoints fire for a while (buckets
      // advance + drop off the left).
      setInterval(() => {
        renderClock();
        rotateIfNeeded();
        renderSpark();
      }, 1000);
    },
    recordSetpoint,
    applyTopology(components, graphStatus) {
      renderHealth(components);
      // `graphStatus === undefined` keeps the existing display
      // (e.g. an older server build without the field); the field
      // is reported as `null` for healthy graphs.
      if (graphStatus !== undefined) renderGraph(graphStatus);
    },
  };
})();
