// The display zone's formatters, in the sim zone and in UTC, across a DST
// change.
// Run: node tools/zone-test.mjs   (exits non-zero on failure)
import assert from "node:assert/strict";
import * as zone from "../ui-assets/zone.js";

// The text of a `<time>` element, or the dash timeHtml gives for no time.
const text = (t, kind) => zone.timeHtml(t, kind).replace(/<[^>]*>/g, "");

// Before init: the Berlin fallback, sim mode.
assert.equal(zone.tz(), "Europe/Berlin");
assert.equal(zone.isUtc(), false);

const summer = Date.UTC(2026, 6, 1, 12, 34, 56); // 14:34:56 CEST
assert.equal(zone.fmtTime(summer), "14:34:56");
assert.equal(text(summer, "hm"), "14:34");
assert.equal(text(summer, "datetime"), "01 Jul 2026, 14:34");
const winter = Date.UTC(2026, 0, 1, 12, 0, 0); // 13:00 CET
assert.equal(zone.fmtTime(winter), "13:00:00");

// <time> markup carries the instant and its kind, from epoch ms or from the
// RFC 3339 string the server sends.
assert.equal(zone.timeHtml(summer, "hm"), '<time datetime="2026-07-01T12:34:56.000Z" data-kind="hm">14:34</time>');
assert.equal(text("2026-07-01T12:34:56Z", "hms"), "14:34:56");
// No time, or one the server sent unparsable, shows a dash instead of throwing
// out of the row that holds it.
assert.equal(zone.timeHtml(null, "hms"), "—");
assert.equal(zone.timeHtml("soon", "hms"), "—");

// uPlot's tzDate: a Date whose local fields are the zone's wall time.
const axis = zone.tzDate(summer / 1000);
assert.deepEqual([axis.getHours(), axis.getMinutes(), axis.getSeconds()], [14, 34, 56]);

// UTC mode: the same instants, in UTC; listeners hear the flip.
let heard = 0;
const off = zone.onChange(() => heard++);
zone.setUtc(true);
assert.equal(heard, 1);
assert.equal(zone.tz(), "UTC");
assert.equal(zone.label(), "UTC");
assert.equal(zone.fmtTime(summer), "12:34:56");
assert.equal(zone.tzDate(summer / 1000).getHours(), 12);
off();
zone.setUtc(false);
assert.equal(heard, 1, "an unsubscribed listener is not called");

// init tells the listeners, so a label painted before it ran is repainted with
// the zone it read (here: none — no server, no storage — so the zone stays put
// and nothing throws).
let afterInit = 0;
const offInit = zone.onChange(() => afterInit++);
await zone.init();
offInit();
assert.equal(afterInit, 1);
assert.equal(zone.tz(), "Europe/Berlin");

// Another sim zone; its short name follows DST.
zone.setSimZone("America/New_York");
assert.equal(text(summer, "hm"), "08:34");
assert.equal(zone.label(summer), "EDT");
assert.equal(zone.label(winter), "EST");

// A zone the server knows but this Intl does not falls back to UTC instead of
// throwing out of every formatter.
zone.setSimZone("Not/A_Zone");
assert.equal(zone.tz(), "UTC");
assert.equal(zone.fmtTime(summer), "12:34:56");
zone.setSimZone("Europe/Berlin");

console.log("zone-test: all assertions passed");
