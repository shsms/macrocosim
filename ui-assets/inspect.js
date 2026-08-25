// Side-panel inspector: live charts per metric, per-category
// knobs / inputs, setpoint event log, and the small utility
// chooseScale / liveCharts machinery the charts route through.
// `showComponent` renders the whole side panel for a selected
// node; `clearSide` tears it back down to the empty hint.

import {
  escapeHtml,
  inspectEl,
  inspectorEl,
  notify,
  openInspector,
} from "./app.js";
import { mgPath, READ_ONLY_TITLE, structureEditable } from "./routing.js";
import { topology } from "./topology.js";

const CHARTS_BY_CATEGORY = {
  grid: ["frequency_hz"],
  meter: ["active_power_w", "reactive_power_var"],
  inverter: ["active_power_w", "reactive_power_var"],
  battery: ["soc_pct", "dc_power_w"],
  "ev-charger": ["soc_pct", "dc_power_w"],
  chp: ["active_power_w"],
};

// Display-only labels per metric. Scaling, units, and the
// "is this a power-family quantity?" decision now come off the
// /api/history response's `quantity` + `unit` fields — see
// chooseScale below. Anything not in this table falls back to the
// raw metric name as the chart title.
const METRIC_TITLES = {
  active_power_w:     "Active Power",
  reactive_power_var: "Reactive Power",
  frequency_hz:       "Frequency",
  soc_pct:            "SoC",
  dc_power_w:         "DC Power",
};

// Pick a display scale from a typed quantity + base unit. Power-
// family quantities autoscale W → kW → MW based on the data range;
// everything else uses the base unit verbatim. The `quantity` /
// `unit` arguments mirror the `Sample<Q>` / `Q.base_unit()` shape
// upstream in frequenz-microgrid, so the same code can serve any
// `Power` / `ReactivePower` / `Frequency` / `Percentage` payload.
function chooseScale(quantity, unit, values) {
  const isPower = quantity === "Power" || quantity === "ReactivePower";
  if (isPower && values.length) {
    const max = Math.max(...values.map((v) => Math.abs(v)));
    if (max >= 1e6) return { div: 1e6, unit: `M${unit}` };
    if (max >= 1e3) return { div: 1e3, unit: `k${unit}` };
    return { div: 1, unit };
  }
  return { div: 1, unit: unit || "" };
}

// Live-chart state for whichever component the user has selected.
// Replaced wholesale on every selection change; the previous uPlots
// get destroyed in clear(). All access to the per-selection chart
// session goes through this module so the surrounding code never
// has to spell out the "is the right component selected, has the
// metric been wired" preconditions for the live push paths.
export const refitCharts = () => liveCharts.refit();

export const liveCharts = (() => {
  let active = null; // { id, charts: Map<metric, {plot, xs, ys, scale}> }
  return {
    set(id, charts) {
      active = { id, charts };
    },
    clear() {
      if (!active) return;
      for (const ch of active.charts.values()) ch.plot.destroy();
      active = null;
    },
    pushSample(id, metric, ts_ms, value) {
      if (!active || active.id !== Number(id)) return;
      const series = active.charts.get(metric);
      if (!series) return;
      series.xs.push(ts_ms / 1000);
      // Apply the chart's chosen unit scale so live samples stay
      // consistent with the backfilled ones.
      series.ys.push(value / series.scale.div);
      // Cap to 5-minute window so the chart doesn't grow forever.
      const cutoff = Date.now() / 1000 - 300;
      while (series.xs.length && series.xs[0] < cutoff) {
        series.xs.shift();
        series.ys.shift();
      }
      series.plot.setData([series.xs, series.ys]);
    },
    pushSetpoint(ev) {
      // Only render if the event is for the currently-inspected
      // component; otherwise it'll be picked up next time the user
      // selects that node (the server's per-component log is the
      // source of truth).
      if (!active || active.id !== Number(ev.id)) return;
      const list = inspectEl.querySelector(".sp-list");
      if (!list) return;
      // Drop the "none" placeholder if it's still showing.
      const empty = list.querySelector(".sp-empty");
      if (empty) empty.remove();
      // The WS event carries the setpoint kind on `setpoint_kind`
      // to dodge collision with the SiteEvent discriminator (also
      // called `kind`).
      list.prepend(setpointEventLi(ev.ts_ms, ev.setpoint_kind, ev.value, ev.accepted, ev.reason));
      // Trim if the list is getting long — match the 600s window
      // used by the initial fetch.
      while (list.children.length > 100) list.removeChild(list.lastChild);
    },
    refit() {
      if (!active) return;
      for (const series of active.charts.values()) {
        const parent = series.plot.root.parentElement;
        if (!parent) continue;
        series.plot.setSize({
          width: parent.clientWidth,
          height: 140,
        });
      }
    },
  };
})();

// The operational modes a component can be declared with, plus a
// hover hint each — shared with the topology context menu's bulk
// set so the two surfaces offer the same vocabulary. The tokens
// round-trip through (set-component-operational-mode …) verbatim.
export const OPERATIONAL_MODES = [
  { value: "unspecified", hint: "not explicitly set; treated as full capability" },
  { value: "inactive", hint: "no telemetry, no control" },
  { value: "telemetry-only", hint: "streams telemetry, rejects commands" },
  { value: "control-only", hint: "accepts commands, streams no telemetry" },
  { value: "control-and-telemetry", hint: "full capability, explicitly declared" },
];

// Categories that the gRPC server actually accepts setpoints on.
// command-mode (timeout / error fault simulation) only makes sense
// for these — grids and meters have no setpoint surface, so we hide
// the dropdown rather than offering a knob that does nothing.
// Categories whose components accept setpoint commands. Shared with
// the dispatch dialog's target pickers, so "controllable" means the
// same thing in both places.
export const ACCEPTS_SETPOINTS = new Set(["battery", "inverter", "ev-charger", "chp"]);

// Per-category runtime knobs the inspector exposes as numeric
// inputs. Each one binds to an existing Lisp setter — so this is
// just UI sugar over what the REPL could already do. Construction-
// time args (capacity, rated bounds, …) aren't here because most
// aren't runtime-mutable on the underlying component yet.
// `dynamic: true` knobs accept either a numeric literal or a Lisp
// expression (lambda, quoted symbol, …) — the underlying defun
// dispatches on input kind. Inputs with `dynamic` render as text,
// everything else as numeric. See the renderInspect Knobs block.
const KNOBS_BY_CATEGORY = {
  meter: [
    { label: "power (W or expr)", defun: "set-meter-power", dynamic: true },
    {
      label: "reactive power (VAr or expr)",
      defun: "set-meter-reactive-power",
      dynamic: true,
    },
    {
      label: "power factor (0–1]",
      defun: "set-meter-power-factor",
      flag: "leading",
    },
  ],
  inverter: [
    { label: "reactive power (VAr)", defun: "set-reactive-power" },
    { label: "reactive PF limit", defun: "set-reactive-pf-limit" },
    { label: "reactive apparent (VA)", defun: "set-reactive-apparent-va" },
  ],
};

function knobsFor(d) {
  const knobs = [...(KNOBS_BY_CATEGORY[d.category] || [])];
  // Solar inverters also get a sunlight knob — driven by the same
  // (set-solar-sunlight ID PCT) defun the cloud-curve timer uses.
  if (d.category === "inverter" && d.subtype === "solar") {
    knobs.unshift({
      label: "sunlight (% or expr)",
      defun: "set-solar-sunlight",
      dynamic: true,
    });
  }
  return knobs;
}

function renderInspect(d, parentIds, childIds) {
  // Rename and disconnect rewrite the microgrid's file; on an
  // unmanaged one they are shown but inert, with the reason on
  // hover. Everything below them — modes, health, knobs — is
  // runtime state and stays live either way.
  const locked = !structureEditable();
  const lockAttrs = locked ? ` disabled title="${escapeHtml(READ_ONLY_TITLE)}"` : "";
  const renderEdgeRow = (id, dataAttr) => {
    const c = topology.get(id);
    const label = c ? c.name : `id ${id}`;
    return `<li>${escapeHtml(label)} <button class="link-btn" ${dataAttr}="${id}"${lockAttrs}>✕</button></li>`;
  };
  const parentList = parentIds.length
    ? parentIds.map((id) => renderEdgeRow(id, "data-disconnect-from")).join("")
    : '<li class="hint">none</li>';
  const childList = childIds.length
    ? childIds.map((id) => renderEdgeRow(id, "data-disconnect-to")).join("")
    : '<li class="hint">none</li>';

  inspectEl.innerHTML = `
    <h2><input id="rename" class="name-input" value="${escapeHtml(d.name)}"${lockAttrs} /></h2>
    <dl>
      <dt>id</dt><dd>${d.id}</dd>
      <dt>category</dt><dd>${d.category}</dd>
      <dt>subtype</dt><dd>${d.subtype || "—"}</dd>
    </dl>
    <h3>Config</h3>
    <dl>
      <dt>mode</dt><dd>${selectField("operational-mode", d.operational_mode, OPERATIONAL_MODES.map((m) => m.value))}</dd>
    </dl>
    <h3>Runtime</h3>
    <dl>
      <dt>health</dt><dd>${selectField("health", d.health, ["ok", "error", "standby"])}</dd>
      <dt>telemetry</dt><dd>${selectField("telemetry-mode", d.telemetry_mode, ["normal", "silent", "closed", "error-empty", "not-found"], d.provides_telemetry === false ? `operational mode ${d.operational_mode} streams no telemetry` : null)}</dd>
      ${ACCEPTS_SETPOINTS.has(d.category)
        ? `<dt>commands</dt><dd>${selectField("command-mode", d.command_mode, ["normal", "timeout", "error", "over-bound"], d.accepts_control === false ? `operational mode ${d.operational_mode} accepts no commands` : null)}</dd>`
        : ""}
    </dl>
    ${(() => {
      const knobs = knobsFor(d);
      if (!knobs.length) return "";
      return `<h3>Knobs</h3><dl>${knobs
        .map((k) => {
          const inputAttrs = k.dynamic
            ? `type="text" placeholder="value or (lambda () ...)"`
            : `type="number" step="any" placeholder="value"`;
          // `k.flag`, when set, renders a checkbox alongside the
          // input for an optional boolean arg (e.g. power factor's
          // LEADING). The checkbox by itself never submits anything
          // — it only qualifies whatever value is next entered into
          // the input, at which point the change handler below reads
          // it and appends it to the eval'd expression.
          const flagHtml = k.flag
            ? `<label class="knob-flag"><input type="checkbox" class="knob-flag-input" /> ${escapeHtml(k.flag)}</label>`
            : "";
          return `<dt>${escapeHtml(k.label)}</dt><dd>
            <input ${inputAttrs} class="knob-input"
                   data-defun="${k.defun}" />${flagHtml}
          </dd>`;
        })
        .join("")}</dl>`;
    })()}
    <h3>Connections</h3>
    <div class="conns">
      <div><strong>parents</strong><ul>${parentList}</ul></div>
      <div><strong>children</strong><ul>${childList}</ul></div>
    </div>
    <div id="charts"></div>
  `;

  // Wire form callbacks. Every action POSTs to /api/eval; the WS
  // TopologyChanged refresh re-reads the form state from the server
  // and re-renders this panel automatically.
  document.getElementById("rename").addEventListener("change", (e) => {
    const name = e.target.value.trim();
    if (!name) return;
    evalQuoted(`(rename-component ${d.id} "${jsToLispString(name)}")`);
  });
  for (const [key, defun] of [
    ["operational-mode", "set-component-operational-mode"],
    ["health", "set-component-health"],
    ["telemetry-mode", "set-component-telemetry-mode"],
    ["command-mode", "set-component-command-mode"],
  ]) {
    const sel = inspectEl.querySelector(`select[data-knob="${key}"]`);
    if (!sel) continue; // dropdown hidden for this category
    sel.addEventListener("change", (e) => {
      evalQuoted(`(${defun} ${d.id} '${e.target.value})`);
    });
  }
  // Numeric knob inputs: change (or Enter then blur) → eval the
  // setter with the typed value; then clear so the field reads as
  // "what would you set it to next" rather than "what's it set to
  // now" (we don't have getters for most of these, and stale values
  // would mislead).
  for (const inp of inspectEl.querySelectorAll(".knob-input")) {
    inp.addEventListener("change", (e) => {
      const v = e.target.value.trim();
      if (v === "") return;
      // A sibling `.knob-flag-input`, if this knob has one, qualifies
      // the value as an optional trailing `t` arg (e.g. power
      // factor's LEADING) — the checkbox itself never triggers an
      // eval on its own; it only takes effect once a value is typed.
      const flag = e.target.closest("dd")?.querySelector(".knob-flag-input");
      evalQuoted(
        `(${e.target.dataset.defun} ${d.id} ${v}${flag?.checked ? " t" : ""})`,
      );
      e.target.value = "";
    });
  }
  for (const btn of inspectEl.querySelectorAll("[data-disconnect-from]")) {
    btn.addEventListener("click", () =>
      evalQuoted(`(disconnect ${btn.dataset.disconnectFrom} ${d.id})`),
    );
  }
  for (const btn of inspectEl.querySelectorAll("[data-disconnect-to]")) {
    btn.addEventListener("click", () =>
      evalQuoted(`(disconnect ${d.id} ${btn.dataset.disconnectTo})`),
    );
  }
}

// `disabledReason` (a string) greys the select out and becomes its
// hover tooltip — used when the operational mode forbids the knob,
// where the backend would reject the set with the same message.
function selectField(knob, current, options, disabledReason = null) {
  const opts = options
    .map(
      (o) => `<option value="${o}"${o === current ? " selected" : ""}>${o}</option>`,
    )
    .join("");
  const attrs = disabledReason
    ? ` disabled title="${escapeHtml(disabledReason)}"`
    : "";
  return `<select data-knob="${knob}"${attrs}>${opts}</select>`;
}

export function jsToLispString(s) {
  return s.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

// POST one Lisp expression to /api/eval. `label` is the notify
// prefix on failure (defaults to the expression itself — pass
// something short like "Paste failed" when the expression would be
// unreadable in a toast). Returns the parsed response ({ ok, ... })
// so callers can act on success, or an { ok: false } shape when
// transport / parsing failed. Undo history is the server's: a
// structural eval stacks the microgrid file's previous generated
// block by itself, so there is nothing to record here.
export async function evalQuoted(expr, label = expr) {
  let res;
  try {
    res = await fetch(mgPath("eval"), { method: "POST", body: expr });
  } catch (err) {
    notify(`${label}: ${err.message}`);
    return { ok: false, error: err.message };
  }
  // res.json() can throw "JSON.parse: unexpected character" if the
  // server returned an empty / non-JSON body (e.g. a 5xx with HTML
  // error page, or a connection that died mid-response). Surface the
  // raw text so the actual culprit shows up in the console instead
  // of an opaque parse error.
  const text = await res.text();
  let data;
  try {
    data = JSON.parse(text);
  } catch (_e) {
    console.error(`evalQuoted: bad JSON for ${expr.slice(0, 60)}…`, {
      status: res.status,
      body: text,
    });
    notify(`${label}: server returned non-JSON (HTTP ${res.status})`);
    return { ok: false, error: `non-JSON (HTTP ${res.status})` };
  }
  if (!data.ok) notify(`${label}: ${data.error}`);
  return data;
}

// Bumped on every showComponent call. Rapid node selection races two
// async renders; an await that resolves after a newer call started
// carries a stale generation and bails out (destroying any uPlots it
// already built) — same last-STARTED-wins guard as dispatchesPanel's
// render and explain's refreshFormula.
let showGen = 0;

export async function showComponent(d) {
  if (!d) return;
  const gen = ++showGen;
  openInspector("node");
  liveCharts.clear();

  // vis-network's getConnectedNodes(id, direction) returns the
  // ids on either side of the selected node — cheaper than walking
  // /api/topology for the disconnect buttons. Display labels get
  // resolved by renderInspect via topology.get().
  const parentIds = topology.parentsOf(d.id);
  const childIds = topology.childrenOf(d.id);
  renderInspect(d, parentIds, childIds);

  const metrics = CHARTS_BY_CATEGORY[d.category] || [];
  const container = document.getElementById("charts");
  const charts = new Map(); // metric → { plot, xs, ys }

  // All metric histories fetched concurrently — inspector-open
  // latency is the slowest round-trip, not the sum. Errors settle
  // into the result slot so a stale-generation return can't leak an
  // unhandled rejection.
  const slots = metrics.map((metric) => {
    const slot = document.createElement("div");
    slot.className = "chart";
    container.appendChild(slot);
    return { metric, slot };
  });
  const results = await Promise.all(
    slots.map(({ metric }) =>
      fetch(`${mgPath("history")}?id=${d.id}&metric=${metric}&window_s=300`)
        .then(async (res) => {
          if (!res.ok) throw new Error(`HTTP ${res.status}`);
          return res.json();
        })
        .then(
          (resp) => ({ resp }),
          (err) => ({ err }),
        ),
    ),
  );
  if (gen !== showGen) return;
  for (const [i, { metric, slot }] of slots.entries()) {
    const { resp, err } = results[i];
    if (err) {
      // Same discipline as renderSetpoints below: one failed metric
      // renders an "unavailable" slot instead of breaking the panel.
      slot.innerHTML = `<p class="hint">${escapeHtml(
        METRIC_TITLES[metric] || metric,
      )} unavailable: ${escapeHtml(err.message)}</p>`;
      continue;
    }
    const samples = resp.samples || [];
    const xs = samples.map(([t]) => t / 1000);
    const ys = samples.map(([, v]) => v);
    const { plot, scale } = makePlot(slot, metric, resp.quantity, resp.unit, xs, ys);
    // Stored ys are pre-scaled (already divided by scale.div) so the
    // live push path can append by dividing each new sample once.
    charts.set(metric, { plot, xs, ys: ys.map((y) => y / scale.div), scale });
  }
  liveCharts.set(d.id, charts);

  // Setpoint events: list recent control-app requests + outcome
  // below the charts. Live-overlay markers on the chart are a
  // follow-up; this is the inspector's MVP.
  await renderSetpoints(d.id, container);
}

// One setpoint-event row, shared by the live-WS push and the REST
// backfill so the markup and escape discipline can't drift. Every
// interpolation goes through `escapeHtml` — the server currently
// only emits fixed-shape strings and a numeric `value`, but
// defense-in-depth: anything that lands in `innerHTML` is escaped.
function setpointEventLi(ts, kind, value, accepted, reason) {
  const li = document.createElement("li");
  li.className = `sp-event ${accepted ? "accepted" : "rejected"}`;
  const time = new Date(ts).toLocaleTimeString();
  const tag = escapeHtml(String(kind ?? "").replace("_", " "));
  const head = `<span class="sp-ts">${escapeHtml(time)}</span> <span class="sp-tag">${tag}</span> <span class="sp-val">${escapeHtml(String(value))}</span>`;
  const body = accepted
    ? '<span class="sp-ok">✓ accepted</span>'
    : `<span class="sp-bad">✕ ${escapeHtml(reason || "")}</span>`;
  li.innerHTML = `${head}<br/>${body}`;
  return li;
}

async function renderSetpoints(id, container) {
  const wrap = document.createElement("div");
  wrap.className = "setpoints";
  wrap.innerHTML = "<h3>Recent setpoints</h3>";
  container.appendChild(wrap);
  try {
    const res = await fetch(`${mgPath("setpoints")}?id=${id}&window_s=600`);
    const data = await res.json();
    // Always create the list element, even when empty — pushSetpoint
    // appends to it on incoming WS events. A no-events placeholder
    // hint sits inside the list and gets dropped once the first
    // event lands.
    const list = document.createElement("ol");
    list.className = "sp-list";
    if (!data.events.length) {
      const empty = document.createElement("li");
      empty.className = "hint sp-empty";
      empty.textContent = "none in the last 10 min";
      list.appendChild(empty);
    }
    // Newest first reads better in a chronological log.
    for (const e of data.events.slice().reverse()) {
      const accepted = e.outcome.kind === "accepted";
      list.appendChild(setpointEventLi(e.ts, e.kind, e.value, accepted, e.outcome.reason));
    }
    wrap.appendChild(list);
  } catch (err) {
    wrap.insertAdjacentHTML(
      "beforeend",
      `<p class="hint">setpoints unavailable: ${escapeHtml(err.message)}</p>`,
    );
  }
}

function makePlot(container, metric, quantity, unit, xs, ys) {
  const title = METRIC_TITLES[metric] || metric;
  const scale = chooseScale(quantity, unit, ys);
  const scaledYs = ys.map((y) => y / scale.div);
  const opts = {
    width: container.clientWidth || 280,
    height: 140,
    title: scale.unit ? `${title} (${scale.unit})` : title,
    cursor: { drag: { x: false, y: false } },
    legend: { show: false },
    scales: { x: { time: true } },
    axes: [
      { stroke: "#7d848e", grid: { stroke: "#353a45", width: 0.5 } },
      // size = pixels reserved for the y-axis labels. 60 fits values
      // up to 6 chars (e.g. -32.5 kW) without truncation.
      {
        stroke: "#7d848e",
        grid: { stroke: "#353a45", width: 0.5 },
        size: 60,
      },
    ],
    series: [
      {},
      { stroke: "#79b8ff", width: 1.5, points: { show: false } },
    ],
  };
  return { plot: new uPlot(opts, [xs, scaledYs], container), scale };
}

// Close the floating inspector: stop its live charts / report poll,
// reset its content, and hide the card. Named `clearSide` for the
// callers that predate the float (deselect handler, Esc, panel toggles).
export function clearSide() {
  liveCharts.clear();
  if (scenarioReportTimer != null) {
    clearInterval(scenarioReportTimer);
    scenarioReportTimer = null;
  }
  inspectEl.innerHTML =
    '<p class="hint">Click a node to inspect. Right-click for the context menu.</p>';
  document.body.classList.remove("inspector-open");
  delete inspectorEl.dataset.panel;
  for (const b of document.querySelectorAll("#defaults-btn, #scenario-report-btn")) {
    b.classList.remove("primary");
  }
}

// Side-panel scenario-report poll handle. dialogs/refreshScenarioReport
// calls `startScenarioReportLoop(setInterval(...))` to register the
// id; `clearSide` cancels via the module-private handle so a stale
// interval can't keep firing into a torn-down inspect.
let scenarioReportTimer = null;
export function startScenarioReportLoop(id) {
  if (scenarioReportTimer != null) clearInterval(scenarioReportTimer);
  scenarioReportTimer = id;
}
