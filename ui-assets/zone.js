// The display zone, which every displayed time is shown in: the simulation's
// (enterprise) zone, from /api/clock, or UTC when the top bar's zone chip
// says so. Times on the wire stay UTC; only display goes through here.

import { getJson } from "./http.js";
import { readStorage, writeStorage } from "./storage.js";

// localStorage: "utc", or "local" for the sim zone.
const TZ_PREF_KEY = "macrocosim-tz";
let simTz = "Europe/Berlin";
let utc = false;
const listeners = new Set();
// The Intl formatters for the display zone, by kind; emptied when it changes.
let formatters = new Map();

export const tz = () => (utc ? "UTC" : simTz);
export const isUtc = () => utc;

// Sets the sim zone. A name this browser's Intl does not know (its zone data
// can be older than the server's) falls back to UTC.
export function setSimZone(name) {
  try {
    new Intl.DateTimeFormat("en-GB", { timeZone: name });
    simTz = name;
  } catch (_) {
    simTz = "UTC";
  }
  formatters = new Map();
}

// Switches between the sim zone and UTC, re-formats the times already painted,
// and tells the listeners.
export function setUtc(on) {
  utc = Boolean(on);
  formatters = new Map();
  if (typeof document !== "undefined") refreshTimes();
  for (const fn of listeners) fn();
}

// `fn` runs after every switch; the returned function unsubscribes.
export function onChange(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

// Reads the sim zone and the chip's remembered choice, then tells the listeners
// as a switch would. Without an answer from the server the zone stays
// Europe/Berlin.
export async function init() {
  try {
    const j = await getJson("/api/clock");
    if (j.tz) setSimZone(j.tz);
  } catch (_) {
    // Keep the fallback zone.
  }
  setUtc(readStorage(TZ_PREF_KEY) === "utc");
}

// The chip's choice: remembered, then applied.
export function chooseUtc(on) {
  writeStorage(TZ_PREF_KEY, on ? "utc" : "local");
  setUtc(on);
}

// The short name of the display zone at instant `ms`: CEST, EST, UTC. Some
// browsers answer with an offset ("GMT+2") or several words; the city part of
// the IANA name reads better in a chip.
export function label(ms = Date.now()) {
  const zone = tz();
  for (const kind of ["short", "shortGeneric"]) {
    try {
      const tag = new Intl.DateTimeFormat("en-US", { timeZone: zone, timeZoneName: kind })
        .formatToParts(ms)
        .find((p) => p.type === "timeZoneName");
      if (tag && !/^GMT[+-]/i.test(tag.value) && !/\s/.test(tag.value)) return tag.value;
    } catch (_) {
      // Try the next kind.
    }
  }
  const city = zone.split("/").pop();
  return city ? city.replace(/_/g, " ") : zone;
}

const OPTIONS = {
  hms: { hour: "2-digit", minute: "2-digit", second: "2-digit" },
  hm: { hour: "2-digit", minute: "2-digit" },
  datetime: { year: "numeric", month: "short", day: "2-digit", hour: "2-digit", minute: "2-digit" },
  parts: { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit" },
};

// The formatter for `kind` in the display zone, built once per zone.
function formatter(kind) {
  let f = formatters.get(kind);
  if (!f) {
    f = new Intl.DateTimeFormat("en-GB", { hourCycle: "h23", timeZone: tz(), ...OPTIONS[kind] });
    formatters.set(kind, f);
  }
  return f;
}

export const fmtTime = (ms) => formatter("hms").format(ms);
export const fmtHm = (ms) => formatter("hm").format(ms);

// A `<time>` element for instant `t` (epoch ms, or an RFC 3339 string as the
// server sends it), formatted as `kind` (hms, hm or datetime), or a dash when
// `t` is not a time. A switch re-formats it.
export function timeHtml(t, kind) {
  const ms = t == null ? Number.NaN : new Date(t).getTime();
  if (!Number.isFinite(ms)) return "—";
  return `<time datetime="${new Date(ms).toISOString()}" data-kind="${kind}">${formatter(kind).format(ms)}</time>`;
}

function refreshTimes() {
  for (const el of document.querySelectorAll("time[data-kind]")) {
    el.textContent = formatter(el.dataset.kind).format(Date.parse(el.dateTime));
  }
}

// The zone's wall-clock fields at instant `ms`.
function wallParts(ms) {
  return Object.fromEntries(
    formatter("parts")
      .formatToParts(ms)
      .map((p) => [p.type, Number(p.value)]),
  );
}

// uPlot's `tzDate` option: a Date whose local fields read as the wall time, in
// the display zone, at epoch second `ts`. It reads the zone on every call, so a
// chart redrawn after a switch follows it.
export function tzDate(ts) {
  const p = wallParts(ts * 1000);
  return new Date(p.year, p.month - 1, p.day, p.hour, p.minute, p.second);
}

// How far the display zone is ahead of UTC at instant `ms`, in milliseconds.
function offsetMs(ms) {
  const p = wallParts(ms);
  return Date.UTC(p.year, p.month - 1, p.day, p.hour, p.minute, p.second) - ms;
}

// The instant whose wall time in the display zone is `naive`, a wall time
// written as if it were UTC. The zone's offsets a day before and a day after
// give the candidates; a wall time it repeats (fall-back) takes its first
// occurrence, and one it skips (spring-forward) moves forward by the size of
// the gap.
function fromWall(naive) {
  const before = naive - offsetMs(naive - 86400000);
  const after = naive - offsetMs(naive + 86400000);
  const fits = [before, after].filter((t) => t + offsetMs(t) === naive);
  return fits.length ? Math.min(...fits) : before;
}

// A `datetime-local` value read as a wall time in the display zone, or null
// when it is not one.
export function wallToUtcMs(text) {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(String(text));
  return m ? fromWall(Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5])) : null;
}

// Midnight, in the display zone, of the day instant `ms` falls in.
export function zonedDayStartMs(ms) {
  const p = wallParts(ms);
  return fromWall(Date.UTC(p.year, p.month - 1, p.day));
}
