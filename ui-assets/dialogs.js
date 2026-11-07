// Top-bar dialogs and side-panel toggles:
// - the Help dialog;
// - side-panel toggles for Defaults and the recording Report.

import { escapeHtml } from "./app.js";
import { bgFetch, getJson } from "./http.js";
import { notify } from "./notices.js";
import { reportMgId } from "./routing.js";
import { makeSidePanelToggle } from "./side-panel.js";
import * as zone from "./zone.js";

export function setupHelpButton() {
  const dlg = document.getElementById("help-dialog");
  document
    .getElementById("help-btn")
    .addEventListener("click", () => dlg.showModal());
  document
    .getElementById("help-dialog-close")
    .addEventListener("click", () => dlg.close());
  // Click-outside-to-dismiss.
  dlg.addEventListener("click", (e) => {
    if (e.target === dlg) dlg.close();
  });
}

// Both panel toggles use the same chrome-button + floating-panel
// pattern. The render functions below own the actual content.
export const setupDefaultsToggle = () => makeSidePanelToggle("defaults-btn", renderDefaults);
export const setupReportToggle = () => makeSidePanelToggle("report-btn", renderReport, stopReportLoop);

// The Report panel's poll handle. side-panel.js runs `stopReportLoop`
// as this panel's teardown on re-render/close, so a stale interval
// can't keep firing into a torn-down panel.
let reportTimer = null;
// How many of the newest events the panel lists.
const REPORT_EVENT_LIMIT = 20;
// Counts the panel's renders, so a render that finishes after a newer
// one started (the panel closed and opened again meanwhile) starts no
// poll of its own.
let reportRenders = 0;
function stopReportLoop() {
  reportRenders += 1;
  if (reportTimer != null) {
    clearInterval(reportTimer);
    reportTimer = null;
  }
}

// The recording of one microgrid (reportMgId): its state from
// `/recording`, the report as it stands from `/recording/report`, the
// recent events from `/recording/events` and the CSV files it wrote
// from `/recording/csv`.
async function renderReport(contentEl) {
  const render = ++reportRenders;
  contentEl.innerHTML = `
    <h2>Recording report</h2>
    <p class="hint">The selected microgrid's recording, or the lowest-numbered
       microgrid's while none is selected. A script records with
       <code>Microgrid.record()</code>. Polls every 2 s while this panel is open.</p>
    <p id="report-state" class="report-state"><span class="hint">loading…</span></p>
    <div id="report-card"></div>
    <h3>Recent events</h3>
    <ul id="report-events" class="report-events"><li class="hint">—</li></ul>
    <h3>CSV files</h3>
    <div id="report-csv"><span class="hint">—</span></div>
  `;
  // Initial paint, then start polling.
  await refreshReport();
  // If the panel moved on during the await (closed, or rendered
  // again), don't start a poll loop into a dead panel, or a second
  // one: our teardown can't cancel a timer that didn't exist yet.
  if (render !== reportRenders || !document.getElementById("report-card")) return;
  reportTimer = setInterval(refreshReport, 2000);
}

// Fills the element `id` with `html`, when the panel still has it.
function paint(id, html) {
  const el = document.getElementById(id);
  if (el) el.innerHTML = html;
}

async function refreshReport() {
  try {
    const mg = await reportMgId();
    if (mg == null) {
      paint("report-state", `<span class="hint">no microgrid loaded</span>`);
      paint("report-card", "");
      paint("report-events", `<li class="hint">—</li>`);
      paint("report-csv", `<span class="hint">—</span>`);
      return;
    }
    const base = `/api/mg/${mg}/recording`;
    const res = await Promise.all([
      bgFetch("recording", base),
      bgFetch("report", `${base}/report`),
      bgFetch("recording csv", `${base}/csv`),
    ]);
    if (res.some((r) => !r.ok)) return;
    const [summary, report, csv] = await Promise.all(res.map((r) => r.json()));
    // The newest events: the ring keeps them from its oldest one.
    const since = Math.max(summary.earliest_event_id, summary.next_event_id - REPORT_EVENT_LIMIT);
    const eventsRes = await bgFetch("report events", `${base}/events?since=${since}`);
    if (!eventsRes.ok) return;
    const events = await eventsRes.json();
    paint("report-state", reportStateHtml(mg, summary, report));
    paint("report-card", reportCardHtml(report));
    paint("report-events", reportEventsHtml(events.events));
    paint("report-csv", reportCsvHtml(mg, csv));
  } catch (_e) {
    // Network blip; let the next tick try again. Don't tear down
    // the panel — the user can read the previous values until the
    // server is back.
  }
}

// Which recording the panel shows and where it stands: running, or
// how it ended.
function reportStateHtml(mg, summary, report) {
  if (summary.name == null) {
    return `<span class="hint">Microgrid #${mg} has no recording yet.</span>`;
  }
  let state = ["running", "running"];
  if (summary.ended_at != null) {
    state = report.ended_by === "rebuild" ? ["rebuild", "ended by a rebuild"] : ["", "stopped"];
  }
  return `<strong>${escapeHtml(summary.name)}</strong> on microgrid #${mg}:
    <span class="report-badge ${state[0]}">${state[1]}</span>
    · started ${zone.timeHtml(summary.started_at, "hms")}
    · ${summary.elapsed_s.toFixed(1)} s · ${summary.event_count} events`;
}

function reportCardHtml(r) {
  const fmt = (v, unit = "W") =>
    v == null ? "—" : `${(v / 1000).toFixed(2)} k${unit}`;
  const soc = r.soc_stats
    ? `${r.soc_stats.mean_pct.toFixed(1)} % mean ·
       ${r.soc_stats.median_pct.toFixed(1)} % median ·
       ${r.soc_stats.mode_pct ?? "—"} % mode`
    : "—";
  const avgRows = r.grid_window_averages.length
    ? r.grid_window_averages
        .slice(-6)
        .map((w) => {
          return `<tr><td>${zone.timeHtml(w.window_start, "hm")}</td><td>${(w.avg_w / 1000).toFixed(2)} kW</td></tr>`;
        })
        .join("")
    : `<tr><td colspan="2" class="hint">no windows yet</td></tr>`;
  return `
    <dl class="report-dl">
      <dt>elapsed</dt><dd>${r.elapsed_s.toFixed(1)} s</dd>
      <dt>grid peak</dt><dd>${fmt(r.peak_grid_w)}</dd>
      <dt>grid Q peak</dt><dd>${fmt(r.peak_grid_var, "VAr")}</dd>
      <dt>site PF at Q peak</dt><dd>${r.site_pf_at_reactive_peak == null ? "—" : r.site_pf_at_reactive_peak.toFixed(2)}</dd>
      <dt>battery charge</dt><dd>${fmt(r.total_battery_charged_wh, "Wh")}</dd>
      <dt>battery discharge</dt><dd>${fmt(r.total_battery_discharged_wh, "Wh")}</dd>
      <dt>PV produced</dt><dd>${fmt(r.total_pv_produced_wh, "Wh")}</dd>
      <dt>battery SoC</dt><dd>${soc}</dd>
    </dl>
    <h3>15-min grid averages (last 6)</h3>
    <table class="report-tbl">
      <thead><tr><th>window</th><th>avg</th></tr></thead>
      <tbody>${avgRows}</tbody>
    </table>
  `;
}

function reportEventsHtml(events) {
  if (!events.length) {
    return '<li class="hint">no events yet</li>';
  }
  return events
    .slice(-REPORT_EVENT_LIMIT)
    .reverse()
    .map((e) => {
      return `<li><code>${zone.timeHtml(e.ts, "hms")}</code> <strong>${escapeHtml(e.kind)}</strong>
              ${escapeHtml(e.payload)}</li>`;
    })
    .join("");
}

// Download links for the CSV files of the running or most recent
// recording, under the directory they were written to.
function reportCsvHtml(mg, csv) {
  if (!csv.files.length) return `<span class="hint">none</span>`;
  const links = csv.files
    .map((f) => {
      const href = `/api/mg/${mg}/recording/csv/${encodeURIComponent(f)}`;
      return `<a class="report-csv-link" href="${href}" download>${escapeHtml(f)}</a>`;
    })
    .join("");
  const dir = csv.dir ? `<p class="hint report-csv-dir">${escapeHtml(csv.dir)}</p>` : "";
  return `${dir}${links}`;
}

// The defaults the engine fills into each component type's spec,
// read-only: one block per type, one row per field it fills.
async function renderDefaults(contentEl) {
  let data;
  try {
    data = await getJson("/api/defaults");
  } catch (err) {
    notify(`Defaults unavailable: ${err.message}`);
    return;
  }
  const blocks = Object.entries(data)
    .map(([type, fields]) => {
      const rows = Object.entries(fields)
        .map(([k, v]) => `<dt>${escapeHtml(k)}</dt><dd>${escapeHtml(JSON.stringify(v))}</dd>`)
        .join("");
      return `<div class="defaults-entry"><h3>${escapeHtml(type)}</h3><dl>${rows}</dl></div>`;
    })
    .join("");
  contentEl.innerHTML = `
    <h2>Per-type defaults</h2>
    <p class="hint">
      What the engine fills into a component's spec for each field the
      spec leaves out.
    </p>
    <div id="defaults-list">${blocks}</div>
  `;
}
