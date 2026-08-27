// The metrics panel's sample store: one 900-slot × 1 Hz ring per
// microgrid_sample stream (15 min — the server history cap), plus
// the latest sample per stream and a subscriber list the panel
// renderer hangs off. DOM-free: the renderer (metrics-panel.js)
// subscribes and reads; nothing here touches elements.

import { mgPath } from "./routing.js";
import { isPanelOpen } from "./side-panel.js";

const PANEL = "metrics-btn";
const SPARK_LEN = 900;

// Power auto-scale: W → kW → MW etc. on the same ladder as live.js
// formatScaled, which cross-references this copy from its own header
// — the two have to move together. Copied rather than imported for
// the signature, not for load order: live.js imports nothing, so
// reaching for it here would be cycle-free, but its helpers take a
// display unit while every caller of this one holds a raw sample's
// quantity + wire unit (SI "var", which every readout spells "VAr")
// and non-power quantities that skip the ladder entirely.
export function fmtValue(quantity, unit, value) {
  if (value == null || !Number.isFinite(value)) return "—";
  const shown = unit === "var" ? "VAr" : unit;
  if (quantity === "Power" || quantity === "ReactivePower" || unit === "W" || unit === "var") {
    const a = Math.abs(value);
    if (a >= 1e6) return `${(value / 1e6).toFixed(2)} M${shown}`;
    if (a >= 1e3) return `${(value / 1e3).toFixed(2)} k${shown}`;
    return `${value.toFixed(1)} ${shown}`;
  }
  return `${value.toFixed(2)} ${shown}`;
}

// Power factor from matching P and Q samples: |P| / hypot(P, Q).
// null when either side is missing or both are zero (PF undefined).
export function pfValue(p, q) {
  if (!Number.isFinite(p) || !Number.isFinite(q) || (p === 0 && q === 0)) return null;
  return Math.abs(p) / Math.hypot(p, q);
}

// Chip readout. Sign convention as the old site-PF tile: opposite
// signs on P and Q read as leading, same as lagging, and the
// qualifier drops once PF rounds to unity (>= 0.995) so a clean
// reading doesn't flicker between the two on noise.
export function pfText(p, q) {
  const pf = pfValue(p, q);
  if (pf == null) return "PF —";
  const tag = pf >= 0.995 ? "" : p * q < 0 ? " lead" : " lag";
  return `PF ${pf.toFixed(2)}${tag}`;
}

export const metricsStore = (() => {
  const sparkBuf = new Map(); // stream -> { values: Float32Array, cursor }
  const latestMap = new Map(); // stream -> { value, quantity, unit }
  const listeners = new Set();
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
  function notify() {
    for (const cb of listeners) cb();
  }

  return {
    subscribe(cb) {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    latest(stream) {
      return latestMap.get(stream) ?? null;
    },
    // Last `windowS` slots, oldest first, xs synthesized at 1 Hz
    // ending now — the ring is positional (one slot per second), so
    // per-slot timestamps are honest without storing them. NaN slots
    // (no sample) come back as null, which uPlot renders as a gap.
    series(stream, windowS) {
      const b = buf(stream);
      const n = Math.min(windowS, SPARK_LEN);
      const now = Date.now() / 1000;
      const xs = new Array(n);
      const ys = new Array(n);
      for (let i = 0; i < n; i++) {
        const v = b.values[(b.cursor - n + i + SPARK_LEN * 2) % SPARK_LEN];
        xs[i] = now - (n - 1 - i);
        ys[i] = Number.isNaN(v) ? null : v;
      }
      return { xs, ys };
    },
    applySample(ev) {
      const b = buf(ev.stream);
      b.values[b.cursor] = ev.value == null ? NaN : ev.value;
      b.cursor = (b.cursor + 1) % SPARK_LEN;
      latestMap.set(ev.stream, {
        value: ev.value ?? null,
        quantity: ev.quantity,
        unit: ev.unit,
      });
      notify();
    },
    // Clear one stream's ring (the grid-frequency feeder re-backfills
    // from outside the /microgrid/history map; without the reset each
    // panel re-open would append the same history again). The latest
    // entry stays — the value is still the latest known.
    resetStream(stream) {
      const b = buf(stream);
      b.values.fill(NaN);
      b.cursor = 0;
    },
    async backfill() {
      // Past 15 min per stream, server-side, so charts show the
      // trend immediately on panel open instead of growing from
      // empty. Best-effort: a 503 mid-rebuild leaves the old rings;
      // WS frames fill forward from here.
      try {
        const hres = await fetch(mgPath("microgrid/history"));
        if (hres.ok) {
          const hmap = await hres.json();
          for (const [stream, samples] of Object.entries(hmap)) {
            const b = buf(stream);
            b.values.fill(NaN);
            const slice = samples.slice(-SPARK_LEN);
            const start = SPARK_LEN - slice.length;
            for (let i = 0; i < slice.length; i++) {
              const v = slice[i]?.value;
              b.values[start + i] = v == null ? NaN : v;
            }
            b.cursor = 0;
          }
        }
      } catch (_) {
        // Loopback not up yet — the reseed below may still land.
      }
      await this.reseedLatest();
      notify();
    },
    // Value-only refresh from the server's cached latest sample —
    // the WS Sample stream drops frames on lag and a backgrounded
    // tab throttles its receiver, so chips could otherwise freeze on
    // a stale number. No ring push: the ring stays aligned to the
    // WS/backfill sample flow.
    async reseedLatest() {
      try {
        const res = await fetch(mgPath("microgrid/latest"));
        if (!res.ok) return;
        const map = await res.json();
        for (const [stream, snap] of Object.entries(map)) {
          latestMap.set(stream, {
            value: snap.value ?? null,
            quantity: snap.quantity,
            unit: snap.unit,
          });
        }
        notify();
      } catch (_) {
        // Best-effort.
      }
    },
    // Safety net against dropped WS frames while the panel is open:
    // slow-poll the latest snapshot, and refresh immediately when
    // the tab returns to the foreground. Idempotent — a second call
    // replaces the timer instead of stacking one.
    startAutoReseed(periodMs = 5000) {
      this.stopAutoReseed();
      reseedTimer = setInterval(() => {
        if (isPanelOpen(PANEL)) this.reseedLatest();
      }, periodMs);
      reseedVisHandler = () => {
        if (!document.hidden && isPanelOpen(PANEL)) this.reseedLatest();
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
  };
})();
