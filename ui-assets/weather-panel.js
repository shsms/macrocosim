// The weather panel: the site's sky as one card — a day curve, the
// live sunlight readout, the config knobs, and a "pass a cloud"
// trigger. It is the UI's face on `GET/POST /api/weather`
// (src/ui/handlers/weather.rs), which mirrors the Lisp
// `(make-weather)` / `(set-weather)` / `(pass-cloud)` doors for
// people who aren't driving the site from the console.
//
// Everything here is UTC: the server prints `"HH:MM"` times and
// RFC 3339 instants, the panel reads and writes exactly those. There
// is no timezone conversion anywhere in this module, on purpose — a
// sunrise the user typed has to come back as the sunrise they typed.
//
// The day curve is computed client-side rather than sampled off the
// server: the clear-sky sine and the clouds' trapezoids are four
// lines of arithmetic each (mirrored from `Weather::clear_sky_pct` /
// `CloudEvent::attenuation_at`), and 145 samples an endpoint call
// apiece would be a poll-rate stampede for a curve that only moves
// when the config does.

import { mgPath } from "./routing.js";
import { isPanelOpen, makeSidePanelToggle } from "./side-panel.js";

const PANEL = "weather-btn";
// The scenarios panel's cadence (panels.js:945): weather has no WS
// push of its own, and the readout has to track a passing cloud.
const POLL_MS = 3000;
// The curve's sampling grid: the whole UTC day, every 10 minutes.
// Fine enough that the sine reads as a smooth arch and a multi-minute
// cloud shows as a visible notch, coarse enough to stay 145 points.
const SAMPLE_S = 600;
const DAY_S = 86400;

// contentEl lives here rather than being threaded through every
// helper: there is exactly one weather panel, and teardown() clears
// it (which is also how an in-flight refresh knows to drop its
// result).
let contentEl = null;
let plot = null;
let pollTimer = 0;
// Which skeleton is currently painted — "" (nothing yet), "empty"
// (404, the create prompt) or "live". A refresh only re-paints the
// skeleton when this changes; otherwise it writes into the DOM that
// is already there, which is what keeps a focused field's text from
// being blown away every 3 s.
let skeleton = "";
// Where the now-marker goes, in hours-of-day — read by the draw hook,
// which uPlot calls on its own schedule and can't be handed an
// argument. See nowHours() for why the browser's clock is the right
// one to ask.
let markerHour = 0;

const escapeHtml = (s) =>
  String(s).replace(
    /[&<>"']/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c],
  );

const cssColor = (v) => getComputedStyle(document.documentElement).getPropertyValue(v).trim();

// ── the sky, in arithmetic ──────────────────────────────────────────

// `"HH:MM"` → seconds from UTC midnight; null when malformed. The
// same spelling `parse_time_of_day` accepts, checked here too so a
// typo reverts the field locally instead of round-tripping a 400.
function hhmmToSecs(text) {
  const m = /^(\d{1,2}):([0-5]\d)$/.exec(String(text).trim());
  if (!m) return null;
  const h = Number(m[1]);
  return h > 23 ? null : h * 3600 + Number(m[2]) * 60;
}

const pad2 = (n) => String(Math.floor(n)).padStart(2, "0");
const secsToHhmm = (s) => `${pad2(s / 3600)}:${pad2((s % 3600) / 60)}`;
// An absolute instant as its UTC wall time — never the viewer's zone.
const utcHhmm = (ms) => `${pad2(new Date(ms).getUTCHours())}:${pad2(new Date(ms).getUTCMinutes())}`;

// `Weather::clear_sky_pct`, transcribed: zero outside the window,
// else a sine arch peaking at `peak` at the window's midpoint.
function clearSkyPct(secs, sunrise, sunset, peak) {
  if (sunset <= sunrise || secs < sunrise || secs > sunset) return 0;
  return peak * Math.sin(Math.PI * ((secs - sunrise) / (sunset - sunrise)));
}

// `CloudEvent::attenuation_at`, transcribed: a trapezoid ramping to
// `depth` over `ramp`, holding, then ramping back down.
//
// The GET payload gives each event a start, an end and a depth but
// not its ramp split, so `ev.ramp` is an estimate (see daySeries).
// A cloud's ramps are seconds-to-a-minute against a 10-minute
// sampling grid, so the estimate moves the drawn curve by less than
// a pixel; it is the plateau depth and the event's span, both exact,
// that the notch is actually read off.
function attenuationAt(ev, tMs) {
  if (tMs < ev.start || tMs >= ev.end) return 0;
  const depth = Math.min(1, Math.max(0, ev.depth / 100));
  const span = ev.end - ev.start;
  const ramp = Math.min(ev.ramp, span / 2);
  if (!(ramp > 0)) return depth;
  const elapsed = tMs - ev.start;
  if (elapsed < ramp) return depth * (elapsed / ramp);
  if (elapsed < span - ramp) return depth;
  return depth * ((span - elapsed) / ramp);
}

// Midnight UTC of the day the marker sits in — the curve's origin.
function dayStartMs(nowMs) {
  const d = new Date(nowMs);
  return Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate());
}

// The now-marker's hour-of-day. The server evaluates the sky at the
// weather's own anchor, which `advance` re-stamps from the wall clock
// every tick — so the browser's clock is the same clock, and asking
// it here costs nothing. Clock skew between the two would show as the
// marker sitting a little off the readout; nothing else depends on it.
const nowHours = (nowMs) => (nowMs - dayStartMs(nowMs)) / 3600000;

// The two plotted series over one UTC day: clear-sky, and clear-sky
// attenuated by every tracked cloud (Π (1 − attenuation_i), so
// overlapping clouds compound — `Weather::pct_at`'s rule).
function daySeries(w, nowMs) {
  const sunrise = hhmmToSecs(w.sunrise) ?? 0;
  const sunset = hhmmToSecs(w.sunset) ?? 0;
  const peak = Number(w.peak_pct) || 0;
  // Ramp estimate: the midpoint of the configured ramp range, which
  // is what an ambient cloud draws from. See attenuationAt.
  const rampMs = (((w.cloud_ramp?.[0] ?? 0) + (w.cloud_ramp?.[1] ?? 0)) / 2) * 1000;
  const events = (w.events ?? [])
    .map((e) => ({
      start: Date.parse(e.start),
      end: Date.parse(e.end),
      depth: Number(e.depth_pct) || 0,
      ramp: rampMs,
    }))
    .filter((e) => Number.isFinite(e.start) && Number.isFinite(e.end));
  const origin = dayStartMs(nowMs);
  const xs = [];
  const clear = [];
  const atten = [];
  for (let s = 0; s <= DAY_S; s += SAMPLE_S) {
    const cs = clearSkyPct(s, sunrise, sunset, peak);
    let transmission = 1;
    for (const e of events) transmission *= 1 - attenuationAt(e, origin + s * 1000);
    xs.push(s / 3600);
    clear.push(cs);
    atten.push(cs * transmission);
  }
  return [xs, clear, atten];
}

// ── the chart ───────────────────────────────────────────────────────

// The vertical "you are here" line — the metrics panel's drawZeroLine
// (metrics-panel.js:135) turned on its side. Same device-pixel care:
// uPlot hands canvas hooks a DPR-scaled context, so the width, the
// dashes and the half-pixel snap all scale by the canvas's own ratio
// or the line lands as a grey smear on a HiDPI screen.
function drawNowMarker(u) {
  const { min, max } = u.scales.x;
  if (min == null || max == null || markerHour < min || markerHour > max) return;
  const dpr = u.ctx.canvas.width / (u.width || 1);
  const { left, top, width, height } = u.bbox;
  const x = Math.round(u.valToPos(markerHour, "x", true) - dpr / 2) + dpr / 2;
  const ctx = u.ctx;
  ctx.save();
  ctx.beginPath();
  ctx.rect(left, top, width, height);
  ctx.clip();
  ctx.strokeStyle = "#7d848e";
  ctx.lineWidth = dpr;
  ctx.setLineDash([3 * dpr, 3 * dpr]);
  ctx.beginPath();
  ctx.moveTo(x, top);
  ctx.lineTo(x, top + height);
  ctx.stroke();
  ctx.restore();
}

function buildChart(slot, data) {
  const solar = cssColor("--cat-inverter-solar") || "#a8d35a";
  const opts = {
    width: slot.clientWidth || 380,
    height: 140,
    cursor: { drag: { x: false, y: false } },
    legend: { show: false },
    scales: {
      x: { time: false, range: [0, 24] },
      // Pinned to zero at the bottom so a cloudy day doesn't rescale
      // into looking like a clear one; the top follows the peak.
      y: { range: (_u, _min, max) => [0, Math.max(5, max * 1.1)] },
    },
    axes: [
      {
        stroke: "#7d848e",
        grid: { stroke: "#353a45", width: 0.5 },
        splits: [0, 4, 8, 12, 16, 20, 24],
        values: (_u, splits) => splits.map((h) => `${pad2(h)}:00`),
      },
      {
        stroke: "#7d848e",
        grid: { stroke: "#353a45", width: 0.5 },
        size: 42,
        label: "%",
        labelSize: 12,
      },
    ],
    series: [
      {},
      // Clear sky dashed, the actual sky solid, the gap between them
      // filled — the same translucent-band idiom the metrics panel's
      // battery envelope uses (metrics-panel.js:309). uPlot fills
      // from the first listed series down to the second, and
      // clear-sky is by construction the upper of the two.
      { stroke: solar, width: 1, dash: [3, 3], points: { show: false } },
      { stroke: solar, width: 1.5, points: { show: false } },
    ],
    bands: [{ series: [1, 2], fill: "rgba(168, 211, 90, 0.14)" }],
    hooks: { draw: [drawNowMarker] },
  };
  plot = new uPlot(opts, data, slot);
}

// ── HTTP ────────────────────────────────────────────────────────────

// Both doors answer with a body either way — the weather shape on
// success, `{"error": …}` on a 4xx — so one envelope covers both and
// callers branch on `ok` alone. A dead fetch (server gone) lands here
// as ok:false with a message rather than an unhandled rejection.
async function call(init) {
  try {
    // Weather is site data, so it routes through `mgPath` like every
    // sibling data panel: a hardcoded `/api/weather` would read and
    // write the FIRST registered microgrid's sky no matter which one
    // the user has selected.
    const r = await fetch(mgPath("weather"), init);
    const body = await r.json().catch(() => null);
    return { ok: r.ok, status: r.status, body };
  } catch (e) {
    return { ok: false, status: 0, body: { error: String(e) } };
  }
}

const getWeather = () => call(undefined);
const postWeather = (payload) =>
  call({
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });

const errorOf = (res) => res.body?.error ?? `request failed (${res.status || "no response"})`;

function showError(text) {
  const el = document.getElementById("weather-error");
  if (!el) return;
  el.textContent = text ?? "";
  el.hidden = !text;
}

// ── editable fields ─────────────────────────────────────────────────

// The scalar knobs. `kind` decides both the input type and how the
// typed text becomes a POST value; `key` is the payload field, spelled
// exactly as WeatherPostRequest deserializes it.
const FIELDS = [
  { id: "weather-sunrise", key: "sunrise", label: "sunrise", kind: "time" },
  { id: "weather-sunset", key: "sunset", label: "sunset", kind: "time" },
  { id: "weather-peak-pct", key: "peak_pct", label: "peak %", kind: "num" },
  { id: "weather-cloud-rate", key: "cloud_rate_per_h", label: "clouds/h", kind: "num" },
];

// The `[lo, hi]` pairs the ambient cloud generator draws from. Either
// half commits the whole pair, since the door takes the range as one
// value — the untouched half rides along at whatever it reads.
const RANGES = [
  { key: "cloud_depth", label: "depth %", lo: "weather-depth-lo", hi: "weather-depth-hi" },
  { key: "cloud_duration", label: "duration s", lo: "weather-duration-lo", hi: "weather-duration-hi" },
  { key: "cloud_ramp", label: "ramp s", lo: "weather-ramp-lo", hi: "weather-ramp-hi" },
];

const fieldText = (v) => (v == null ? "" : String(v));

// Write a reading into a field. `dataset.live` is ALWAYS refreshed —
// it is what Esc and blur restore, so freezing it too would revert a
// cancelled edit to a value as stale as focus time. Only the visible
// text is frozen while the field is being edited.
function paintField(inp, text) {
  if (!inp) return;
  inp.dataset.live = text;
  if (inp.dataset.editing) return;
  inp.value = text;
}

// The inspector's edit-in-place contract (inspect.js:515), local to
// this panel because the inspector's is component-bound: focus freezes
// the visible text against the 3 s poll, Enter commits, Esc and blur
// both revert to the last reading. A rejected commit reverts too, so
// nothing the server refused is left sitting in the field looking
// applied.
function wireField(inp, commit) {
  inp.dataset.live = inp.value;
  // The hidden spinner's sibling affordance: in Firefox a wheel over a
  // focused type=number steps it, which is the same uncommitted change
  // an arrow click was — invisible, and reverted by the next poll. Arrow
  // KEYS still step, deliberately: those are keyboard edits, one Enter
  // from a commit. Non-passive so preventDefault holds; number fields
  // only, so a wheel over a text field still scrolls the panel.
  if (inp.type === "number") {
    inp.addEventListener("wheel", (e) => e.preventDefault(), { passive: false });
  }
  inp.addEventListener("focus", () => {
    inp.dataset.editing = "1";
  });
  inp.addEventListener("blur", () => {
    delete inp.dataset.editing;
    inp.value = inp.dataset.live ?? "";
  });
  inp.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      commit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      inp.blur();
    }
  });
}

// Commit one scalar field. A malformed time or a non-number is caught
// here rather than round-tripped: the door would reject it anyway, and
// the local check keeps the error next to the field that caused it.
async function commitField(f) {
  const inp = document.getElementById(f.id);
  const text = inp.value.trim();
  if (text === "") return;
  let value;
  if (f.kind === "time") {
    if (hhmmToSecs(text) === null) {
      showError(`${f.label}: expected "HH:MM"`);
      inp.value = inp.dataset.live ?? "";
      return;
    }
    value = text;
  } else {
    value = Number(text);
    if (!Number.isFinite(value)) {
      showError(`${f.label}: expected a number`);
      inp.value = inp.dataset.live ?? "";
      return;
    }
  }
  const res = await postWeather({ [f.key]: value });
  if (!res.ok) {
    showError(errorOf(res));
    inp.value = inp.dataset.live ?? "";
    return;
  }
  showError(null);
  applyLive(res.body);
}

async function commitRange(r) {
  const loEl = document.getElementById(r.lo);
  const hiEl = document.getElementById(r.hi);
  const lo = Number(loEl.value.trim());
  const hi = Number(hiEl.value.trim());
  if (!Number.isFinite(lo) || !Number.isFinite(hi)) {
    showError(`${r.label}: both bounds must be numbers`);
    loEl.value = loEl.dataset.live ?? "";
    hiEl.value = hiEl.dataset.live ?? "";
    return;
  }
  const res = await postWeather({ [r.key]: [lo, hi] });
  if (!res.ok) {
    showError(errorOf(res));
    loEl.value = loEl.dataset.live ?? "";
    hiEl.value = hiEl.dataset.live ?? "";
    return;
  }
  showError(null);
  applyLive(res.body);
}

// ── rendering ───────────────────────────────────────────────────────

const fieldInput = (f) =>
  f.kind === "time"
    ? `<input id="${f.id}" class="wfield-input" type="text" placeholder="HH:MM" />`
    : `<input id="${f.id}" class="wfield-input" type="number" step="any" placeholder="—" />`;

function liveHtml() {
  const rows = FIELDS.map(
    (f) => `<div class="wfield"><label for="${f.id}">${escapeHtml(f.label)}</label>
        ${fieldInput(f)}</div>`,
  ).join("");
  const ranges = RANGES.map(
    (r) => `<div class="wfield"><label for="${r.lo}">${escapeHtml(r.label)}</label>
        <span class="wfield-pair">
          <input id="${r.lo}" class="wfield-input" type="number" step="any" placeholder="lo" />
          <span class="wfield-dash">–</span>
          <input id="${r.hi}" class="wfield-input" type="number" step="any" placeholder="hi" />
        </span>
      </div>`,
  ).join("");
  return `
    <div class="weather-panel">
      <div class="weather-head">
        <h2>Weather</h2>
        <span class="weather-readout"><span id="weather-pct">—</span><span class="weather-unit">%</span></span>
      </div>
      <div class="wchart" id="weather-chart"></div>
      <p class="hint weather-sub" id="weather-clear-sky">clear sky —</p>
      <p class="hint weather-err" id="weather-error" hidden></p>
      <div class="wfields">${rows}${ranges}</div>
      <section class="wcloud">
        <h3>Pass a cloud</h3>
        <div class="wcloud-row">
          <label for="weather-cloud-depth">depth %</label>
          <input id="weather-cloud-depth" class="wfield-input wfield-fire" type="number" step="any" value="60" />
          <label for="weather-cloud-duration">for s</label>
          <input id="weather-cloud-duration" class="wfield-input wfield-fire" type="number" step="any" value="600" />
          <label for="weather-cloud-ramp">ramp s</label>
          <input id="weather-cloud-ramp" class="wfield-input wfield-fire" type="number" step="any" value="60" />
          <button type="button" class="pill" id="weather-cloud-fire">fire</button>
        </div>
      </section>
      <section class="wevents">
        <h3>Clouds</h3>
        <ul id="weather-events"><li class="hint">none</li></ul>
      </section>
    </div>`;
}

const EMPTY_HTML = `
  <div class="weather-panel">
    <div class="weather-head"><h2>Weather</h2></div>
    <p class="hint">No weather on this site. Creating it installs the default sky —
      the same one <code>(make-weather)</code> gives, which you can retune here
      afterwards.</p>
    <button type="button" class="pill" id="weather-create">Create weather</button>
    <p class="hint weather-err" id="weather-error" hidden></p>
  </div>`;

function eventsHtml(w) {
  const events = w.events ?? [];
  if (events.length === 0) return '<li class="hint">none</li>';
  return events
    .map((e) => {
      const start = Date.parse(e.start);
      const end = Date.parse(e.end);
      const span =
        Number.isFinite(start) && Number.isFinite(end)
          ? `${utcHhmm(start)}–${utcHhmm(end)}`
          : "—";
      const depth = Number(e.depth_pct) || 0;
      return `<li><span class="wev-span">${escapeHtml(span)}</span><span class="wev-depth">−${depth.toFixed(0)}%</span></li>`;
    })
    .join("");
}

// Paint the live skeleton once, then wire everything in it. Called
// only on the transition into "live" — every later refresh writes
// values into this DOM instead of replacing it, which is what lets a
// focused field survive the poll.
function paintLiveSkeleton() {
  contentEl.innerHTML = liveHtml();
  for (const f of FIELDS) {
    wireField(document.getElementById(f.id), () => commitField(f));
  }
  for (const r of RANGES) {
    wireField(document.getElementById(r.lo), () => commitRange(r));
    wireField(document.getElementById(r.hi), () => commitRange(r));
  }
  document.getElementById("weather-cloud-fire").addEventListener("click", fireCloud);
  plot?.destroy();
  plot = null;
}

function paintEmptySkeleton() {
  plot?.destroy();
  plot = null;
  contentEl.innerHTML = EMPTY_HTML;
  document.getElementById("weather-create").addEventListener("click", async () => {
    // An empty body is a no-op partial update, which on a site with
    // no weather is exactly "install the defaults" (apply_weather's
    // `unwrap_or_default` branch) — no field list to keep in sync.
    const res = await postWeather({});
    if (!res.ok) {
      showError(errorOf(res));
      return;
    }
    applyLive(res.body);
  });
}

async function fireCloud() {
  const depth = Number(document.getElementById("weather-cloud-depth").value);
  const duration = Number(document.getElementById("weather-cloud-duration").value);
  const ramp = Number(document.getElementById("weather-cloud-ramp").value);
  if (!Number.isFinite(depth) || !Number.isFinite(duration) || !Number.isFinite(ramp)) {
    showError("pass a cloud: depth, duration and ramp must all be numbers");
    return;
  }
  const res = await postWeather({
    pass_cloud: { depth_pct: depth, duration_s: duration, ramp_s: ramp },
  });
  if (!res.ok) {
    showError(errorOf(res));
    return;
  }
  showError(null);
  applyLive(res.body);
}

// Take a fresh weather payload as the panel's state and repaint from
// it. Both the poll and every successful POST land here, so the two
// paths can't drift — a POST's response is the same shape as a GET's
// precisely so it can be used this way.
function applyLive(w) {
  if (!w || !contentEl) return;
  if (skeleton !== "live") {
    paintLiveSkeleton();
    skeleton = "live";
  }
  const pct = Number(w.pct) || 0;
  const clear = Number(w.clear_sky_pct) || 0;
  document.getElementById("weather-pct").textContent = pct.toFixed(1);
  const sunrise = hhmmToSecs(w.sunrise);
  const sunset = hhmmToSecs(w.sunset);
  const daylight =
    sunrise != null && sunset != null
      ? `${secsToHhmm(sunrise)}–${secsToHhmm(sunset)} UTC`
      : "—";
  document.getElementById("weather-clear-sky").textContent =
    `clear sky ${clear.toFixed(1)}% · daylight ${daylight}`;

  for (const f of FIELDS) paintField(document.getElementById(f.id), fieldText(w[f.key]));
  for (const r of RANGES) {
    const pair = w[r.key] ?? [];
    paintField(document.getElementById(r.lo), fieldText(pair[0]));
    paintField(document.getElementById(r.hi), fieldText(pair[1]));
  }
  document.getElementById("weather-events").innerHTML = eventsHtml(w);

  const nowMs = Date.now();
  markerHour = nowHours(nowMs);
  const data = daySeries(w, nowMs);
  const slot = document.getElementById("weather-chart");
  if (plot) plot.setData(data);
  else if (slot) buildChart(slot, data);
}

async function refresh() {
  const res = await getWeather();
  // The panel may have closed while the request was in flight; its
  // teardown already dropped contentEl, and writing into the detached
  // DOM would resurrect a chart nobody can see.
  if (!isPanelOpen(PANEL) || !contentEl) return;
  if (res.status === 404) {
    if (skeleton !== "empty") {
      paintEmptySkeleton();
      skeleton = "empty";
    }
    return;
  }
  if (!res.ok) {
    showError(errorOf(res));
    return;
  }
  applyLive(res.body);
}

function render(el) {
  contentEl = el;
  skeleton = "";
  el.innerHTML = '<p class="hint">loading weather…</p>';
  refresh();
  // Weather has no WS push — a passing cloud only shows up if the
  // panel asks. The interval lives on the panel's own lifetime, so a
  // closed panel costs nothing (and the headless boot smoke's event
  // loop isn't held open by it).
  pollTimer = setInterval(refresh, POLL_MS);
}

function teardown() {
  clearInterval(pollTimer);
  pollTimer = 0;
  plot?.destroy();
  plot = null;
  contentEl = null;
  skeleton = "";
}

export function setupWeatherPanel() {
  makeSidePanelToggle(PANEL, render, teardown);
}
