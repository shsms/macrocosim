// The events socket and the Logs panel: the WebSocket pump that fans
// out /ws/events frames to liveCharts / the metrics store / pulseBar /
// the log panel, the Logs panel's head, and its load-time backfill.

import { dispatchesPanel, setStatus } from "./app.js";
import { onScriptStatusEvent, pulseBar } from "./chrome.js";
import { onReachedAgain } from "./connection.js";
import { getJson } from "./http.js";
import { inspectorLive, liveCharts } from "./inspect.js";
import { appendLog } from "./logs.js";
import { metricsStore } from "./metrics-store.js";
import { logUi } from "./notices.js";
import { readSelectedMg, readSubview } from "./routing.js";
import { readStorage, writeStorage } from "./storage.js";
import { topology } from "./topology.js";

// The Logs panel's tail: /api/logs gives the load-time backfill (a ring of
// recent records); /ws/events kind:"log" appends each new record live.
export async function backfillLogs() {
  try {
    const lines = await getJson("/api/logs");
    for (const ln of lines) appendLog(ln);
  } catch (_) {
    // A one-off read at boot: a server that cannot be reached shows in the
    // banner, through the polls.
  }
}

// The Logs panel head: the minimum level goes on #logs as a class the
// stylesheet reads, and persists; clear empties the tail (new lines
// keep arriving).
const LOGS_LEVEL_KEY = "mc-logs-level";

export function setupLogsPanel() {
  const box = document.getElementById("logs");
  const level = document.getElementById("logs-level");
  // The select's own options are the list of levels: a level added
  // there gets its min-* class handled here without a second edit.
  const LOG_LEVELS = [...level.options].map((o) => o.value);
  const apply = () => {
    for (const l of LOG_LEVELS) box.classList.toggle(`min-${l}`, l === level.value);
    writeStorage(LOGS_LEVEL_KEY, level.value);
  };
  const stored = readStorage(LOGS_LEVEL_KEY);
  level.value = LOG_LEVELS.includes(stored) ? stored : "info";
  level.addEventListener("change", apply);
  apply();
  document.getElementById("logs-clear").addEventListener("click", () => box.replaceChildren());
}

// Self-reconnecting WS with exponential backoff. Starts at 1 s,
// doubles on each close, caps at 30 s, resets to 1 s on a clean
// onopen. A laptop returning from sleep, a server bounce, or a
// notify-reload that briefly drops connections all heal without
// a manual page refresh — important for an overnight soak run.
// While it is closed the header status says so; the banner is for the
// background requests (connection.js).
//
// An open after a close also nudges a topology refresh, because samples may
// have moved while we were away and the status line was overwritten. A first
// open that went through is a no-op there because init() already awaited
// refreshTopology before opening the WS.
//
// `onMicrogridUnloaded(id)` runs when the server unloaded microgrid `id`.
// `onOpen()` runs on every open, the first included, to read again what
// the socket sends only as changes (the script's status).
export function openWebSocket(onTopologyChanged, onMicrogridUnloaded, onOpen = () => {}) {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const url = `${proto}//${location.host}/ws/events`;
  const MIN_DELAY = 1000;
  const MAX_DELAY = 30000;
  let delay = MIN_DELAY;
  // The pending reconnect while the socket is closed, else null.
  let retry = null;
  // Whether the socket has closed since it last opened.
  let dropped = false;
  // The server is back after the background requests failed to reach it: try
  // now instead of waiting out the backoff.
  onReachedAgain(() => {
    if (retry == null) return;
    clearTimeout(retry);
    retry = null;
    connect();
  });
  function connect() {
    const ws = new WebSocket(url);
    ws.onopen = () => {
      delay = MIN_DELAY;
      onOpen();
      if (dropped) {
        dropped = false;
        setStatus("", "");
        logUi("info", "live updates reconnected");
        // Catch up state the canvas and inspector cached from
        // before the drop. Loopback pill + metrics panel also
        // self-heal via their next poll / WS frame.
        onTopologyChanged(0);
      }
    };
    ws.onmessage = (msg) => {
      // Defensive: vis-network and other libs sometimes pump non-string
      // frames (binary, blob) through fetch / WS pipelines that look
      // identical from a try/catch perspective. Surface the actual
      // payload type to console so a "JSON.parse undefined" surprise
      // points straight at the offending frame.
      if (typeof msg.data !== "string") {
        console.warn("WS: non-string payload, skipping:", msg.data);
        return;
      }
      let ev;
      try {
        ev = JSON.parse(msg.data);
      } catch (e) {
        console.warn("WS: JSON parse failed:", e.message, "payload was:", msg.data);
        return;
      }
      // Per-microgrid events carry microgrid_id; we filter out
      // anything from a microgrid other than the currently-selected
      // one, and all of them with nothing selected, so the metrics
      // panel doesn't paint with samples from a neighbour. The one
      // exception is topology_changed with nothing selected: the list
      // view refreshes its microgrid cards on it. Enterprise-scoped
      // events (log, lagged) ship microgrid_id = undefined and pass through
      // regardless, and so does microgrid_unloaded, which every view
      // needs.
      const selectedMg = readSelectedMg();
      const perMg = ev.kind === "sample" || ev.kind === "microgrid_sample"
                 || ev.kind === "topology_changed" || ev.kind === "setpoint"
                 || ev.kind === "dispatch_changed" || ev.kind === "knob_changed";
      const forList = ev.kind === "topology_changed" && selectedMg == null;
      if (perMg && !forList && ev.microgrid_id != null && ev.microgrid_id !== selectedMg) {
        return;
      }
      if (ev.kind === "sample") {
        liveCharts.pushSample(ev.component_id, ev.metric, Date.parse(ev.ts) / 1000, ev.value);
        topology.applySample(ev);
        inspectorLive.applySample(ev);
      } else if (ev.kind === "microgrid_sample") {
        metricsStore.applySample({ ...ev, t_s: Date.parse(ev.ts) / 1000 });
        topology.applyMicrogridSample(ev);
      } else if (ev.kind === "topology_changed") {
        onTopologyChanged(ev.version);
      } else if (ev.kind === "setpoint") {
        topology.noteSetpoint(ev);
        pulseBar.recordSetpoint();
        inspectorLive.applySetpoint(ev);
      } else if (ev.kind === "knob_changed") {
        inspectorLive.applyKnob(ev);
        // Some knobs also move the canvas — the plug knob is the only
        // signal an unplug gives the live overlay, since an empty
        // charger stops sending soc_pct rather than sending a null
        // one. topology ignores the tokens it does not draw.
        topology.applyKnob(ev);
      } else if (ev.kind === "log") {
        appendLog(ev);
      } else if (ev.kind === "script_status") {
        onScriptStatusEvent(ev);
      } else if (ev.kind === "microgrid_unloaded") {
        onMicrogridUnloaded(ev.microgrid_id);
      } else if (ev.kind === "dispatch_changed") {
        // The dispatch store changed for ev.microgrid_id; refetch only if
        // we're actually looking at that microgrid's Dispatches tab.
        if (selectedMg != null && readSubview() === "dispatches") {
          dispatchesPanel.render(selectedMg);
        }
      }
    };
    ws.onclose = () => {
      if (!dropped) logUi("warn", "live updates disconnected");
      dropped = true;
      const secs = Math.round(delay / 1000);
      setStatus(`disconnected — retry in ${secs}s`, "error");
      retry = setTimeout(() => {
        retry = null;
        connect();
      }, delay);
      delay = Math.min(delay * 2, MAX_DELAY);
    };
    // onerror fires alongside onclose; setStatus message stays as
    // the "retry in Xs" we just set so the user sees the recovery
    // plan rather than an opaque "ws error".
    ws.onerror = () => {};
  }
  connect();
}

