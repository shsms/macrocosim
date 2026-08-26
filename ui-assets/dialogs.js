// Top-bar dialogs and side-panel toggles:
// - Help, Snapshots dialogs.
// - Side-panel toggles for Defaults and the live Scenario report.

import {
  escapeHtml,
  inspectEl,
  inspectorEl,
  mutate,
  notify,
  openInspector,
} from "./app.js";
import { evalQuoted } from "./eval.js";
import { clearSide, startScenarioReportLoop } from "./inspect.js";
import { currentMgEntry, readSelectedMg } from "./routing.js";

export function setupHelpButton() {
  const dlg = document.getElementById("help-dialog");
  document
    .getElementById("help-btn")
    .addEventListener("click", () => dlg.showModal());
  document
    .getElementById("help-dialog-close")
    .addEventListener("click", () => dlg.close());
  // Click-outside-to-dismiss, mirroring the snapshots dialog.
  dlg.addEventListener("click", (e) => {
    if (e.target === dlg) dlg.close();
  });
}

// Snapshots are copies of one microgrid's managed file, so the
// dialog only works on a selected, managed microgrid. With none, it
// still opens (the button is always in the chrome) but says why it
// can do nothing rather than firing at an /api/mg/null/… URL.
export function setupSnapshotsDialog() {
  const dlg = document.getElementById("snapshots-dialog");
  const btn = document.getElementById("snapshots-btn");
  const close = document.getElementById("snapshots-dialog-close");
  const list = document.getElementById("snapshots-list");
  const input = document.getElementById("snapshot-name-input");
  const form = document.getElementById("snapshot-save-form");
  const hint = document.getElementById("snapshots-blocked");
  if (!dlg || !btn) return;

  // Why the dialog can't act, or null when it can.
  function blockedReason() {
    const id = readSelectedMg();
    if (id == null) return "Select a microgrid first — snapshots are per microgrid.";
    const entry = currentMgEntry();
    if (entry && !entry.managed) {
      return `Microgrid #${id} is an unmanaged file — Adopt it to snapshot its structure.`;
    }
    return null;
  }

  async function refresh() {
    const blocked = blockedReason();
    hint.textContent = blocked || "";
    hint.hidden = !blocked;
    form.hidden = Boolean(blocked);
    list.innerHTML = "";
    if (blocked) return;
    const id = readSelectedMg();
    try {
      const res = await fetch(`/api/mg/${id}/snapshots`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const names = (await res.json()).snapshots || [];
      if (names.length === 0) {
        list.innerHTML = '<li class="hint">No snapshots yet.</li>';
        return;
      }
      for (const name of names) {
        const li = document.createElement("li");
        li.className = "snapshot-row";
        li.innerHTML = `
          <span class="snapshot-name">${escapeHtml(name)}</span>
          <button class="hdr-btn snapshot-load" type="button">Load</button>
        `;
        li.querySelector(".snapshot-load").addEventListener("click", async () => {
          if (!confirm(`Load snapshot "${name}"? Microgrid #${id}'s current file will be replaced.`)) return;
          try {
            await mutate("POST", `/api/mg/${id}/snapshots/load`, { name });
          } catch (err) {
            notify(`Load failed: ${err.message}`);
            return;
          }
          dlg.close();
        });
        list.appendChild(li);
      }
    } catch (err) {
      list.innerHTML = `<li class="hint">error: ${escapeHtml(err.message)}</li>`;
    }
  }

  btn.addEventListener("click", () => {
    refresh();
    dlg.showModal();
  });
  close.addEventListener("click", () => dlg.close());
  dlg.addEventListener("click", (e) => {
    if (e.target === dlg) dlg.close();
  });
  form.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const id = readSelectedMg();
    const name = input.value.trim();
    if (!name || id == null) return;
    try {
      await mutate("POST", `/api/mg/${id}/snapshots/save`, { name });
    } catch (err) {
      notify(`Save failed: ${err.message}`);
      return;
    }
    input.value = "";
    await refresh();
  });
}

/// Generic inspector toggle: a chrome button (Defaults / Report) that
/// opens the floating inspector with some custom render. Clicking it
/// again closes the inspector.
function makeSidePanelToggle(btnId, render) {
  const btn = document.getElementById(btnId);
  btn.addEventListener("click", async () => {
    // Clicking the lit button (its panel is the one showing) closes;
    // otherwise render this panel and open — even if the inspector is
    // already up showing something else, which just swaps the content.
    if (document.body.classList.contains("inspector-open") && inspectorEl.dataset.panel === btnId) {
      clearSide();
      return;
    }
    await render();
    openInspector(btnId);
  });
}

// Both side-panel toggles use the same chrome-button + swap-side-
// panel pattern. The render functions below own the actual content.
export const setupDefaultsToggle = () => makeSidePanelToggle("defaults-btn", renderDefaults);
export const setupScenarioReportToggle = () =>
  makeSidePanelToggle("scenario-report-btn", renderScenarioReport);

async function renderScenarioReport() {
  inspectEl.innerHTML = `
    <h2>Scenario report</h2>
    <p class="hint">Live aggregate metrics for the running scenario.
       Polls every 2 s while this panel is open.</p>
    <div id="sc-report-card"><span class="hint">loading…</span></div>
    <h3>Recent events</h3>
    <ul id="sc-report-events" class="sc-events"><li class="hint">—</li></ul>
  `;
  // Initial paint, then start polling. inspect.js owns the timer
  // handle so clearSide can cancel it from the inspect tear-down
  // path without reaching across module boundaries.
  await refreshScenarioReport();
  // If the inspector moved on during the await (closed, or another
  // panel / node view replaced our markup), don't start a poll loop
  // into a dead panel — clearSide can't cancel a timer that didn't
  // exist yet when the panel went away.
  if (!document.getElementById("sc-report-card")) return;
  startScenarioReportLoop(setInterval(refreshScenarioReport, 2000));
}

async function refreshScenarioReport() {
  try {
    const [reportRes, eventsRes] = await Promise.all([
      fetch("/api/scenario/report"),
      fetch("/api/scenario/events?limit=50"),
    ]);
    if (!reportRes.ok || !eventsRes.ok) return;
    const r = await reportRes.json();
    const ev = await eventsRes.json();
    const card = document.getElementById("sc-report-card");
    if (card) card.innerHTML = renderScenarioCard(r);
    const list = document.getElementById("sc-report-events");
    if (list) list.innerHTML = renderScenarioEvents(ev.events);
  } catch (_e) {
    // Network blip; let the next tick try again. Don't tear down
    // the panel — the user can read the previous values until the
    // server is back.
  }
}

function renderScenarioCard(r) {
  const fmt = (v, unit = "W") =>
    v == null ? "—" : `${(v / 1000).toFixed(2)} k${unit}`;
  const soc = r.soc_stats
    ? `${r.soc_stats.mean_pct.toFixed(1)} % mean ·
       ${r.soc_stats.median_pct.toFixed(1)} % median ·
       ${r.soc_stats.mode_pct ?? "—"} % mode`
    : "—";
  const avgRows = r.main_meter_window_averages.length
    ? r.main_meter_window_averages
        .slice(-6)
        .map((w) => {
          const ts = new Date(w.window_start).toISOString().slice(11, 16);
          return `<tr><td>${ts}Z</td><td>${(w.avg_w / 1000).toFixed(2)} kW</td></tr>`;
        })
        .join("")
    : `<tr><td colspan="2" class="hint">no windows yet</td></tr>`;
  return `
    <dl class="sc-report-dl">
      <dt>elapsed</dt><dd>${r.scenario_elapsed_s.toFixed(1)} s</dd>
      <dt>main-meter peak</dt><dd>${fmt(r.peak_main_meter_w)}</dd>
      <dt>main-meter Q peak</dt><dd>${fmt(r.peak_main_meter_var, "VAr")}</dd>
      <dt>site PF at Q peak</dt><dd>${r.site_pf_at_peak_var == null ? "—" : r.site_pf_at_peak_var.toFixed(2)}</dd>
      <dt>battery charge</dt><dd>${fmt(r.total_battery_charged_wh, "Wh")}</dd>
      <dt>battery discharge</dt><dd>${fmt(r.total_battery_discharged_wh, "Wh")}</dd>
      <dt>PV produced</dt><dd>${fmt(r.total_pv_produced_wh, "Wh")}</dd>
      <dt>battery SoC</dt><dd>${soc}</dd>
    </dl>
    <h3>15-min main-meter averages (last 6)</h3>
    <table class="sc-report-tbl">
      <thead><tr><th>window</th><th>avg</th></tr></thead>
      <tbody>${avgRows}</tbody>
    </table>
  `;
}

function renderScenarioEvents(events) {
  if (!events.length) {
    return '<li class="hint">no events yet</li>';
  }
  return events
    .slice(-20)
    .reverse()
    .map((e) => {
      const t = new Date(e.ts).toISOString().slice(11, 19);
      return `<li><code>${t}Z</code> <strong>${escapeHtml(e.kind)}</strong>
              ${escapeHtml(e.payload)}</li>`;
    })
    .join("");
}


async function renderDefaults() {
  let data;
  try {
    const res = await fetch("/api/defaults");
    data = await res.json();
  } catch (err) {
    notify(`Defaults unavailable: ${err.message}`);
    return;
  }
  inspectEl.innerHTML = `
    <h2>Per-category defaults</h2>
    <p class="hint">
      Edit a value (raw Lisp) and click Save to <code>setq</code> the
      variable. Changes apply immediately and persist to
      <code>enterprise.lisp</code>.
    </p>
    <div id="defaults-list"></div>
  `;
  const list = document.getElementById("defaults-list");
  for (const e of data.entries) {
    const block = document.createElement("div");
    block.className = "defaults-entry";
    // Size the textarea to the pair-per-line value and let long
    // lines scroll horizontally instead of soft-wrapping mid-token.
    const rows = Math.min(10, Math.max(3, e.value.split("\n").length + 1));
    block.innerHTML = `
      <label>${e.var_name}</label>
      <textarea rows="${rows}" wrap="off" spellcheck="false">${escapeHtml(e.value)}</textarea>
      <button class="hdr-btn primary">Save</button>
    `;
    const ta = block.querySelector("textarea");
    block.querySelector("button").addEventListener("click", async () => {
      const expr = `(setq ${e.var_name} (quote ${ta.value}))`;
      await evalQuoted(expr);
    });
    list.appendChild(block);
  }
}
