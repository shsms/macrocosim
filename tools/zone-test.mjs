// The display zone's formatters and wall-time conversions, in the sim zone and
// in UTC, across a DST change.
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

// Wall time -> UTC in the sim zone.
assert.equal(zone.wallToUtcMs("2026-07-01T14:34"), Date.UTC(2026, 6, 1, 12, 34));
assert.equal(zone.wallToUtcMs("2026-01-01T13:00"), Date.UTC(2026, 0, 1, 12, 0));
// Spring forward (29 Mar 2026, 02:00 -> 03:00): 02:30 does not exist and maps
// forward to 03:30 CEST, i.e. 01:30 UTC.
assert.equal(zone.wallToUtcMs("2026-03-29T02:30"), Date.UTC(2026, 2, 29, 1, 30));
// Fall back (25 Oct 2026, 03:00 -> 02:00): 02:30 happens twice and reads as
// the first, 02:30 CEST, i.e. 00:30 UTC.
assert.equal(zone.wallToUtcMs("2026-10-25T02:30"), Date.UTC(2026, 9, 25, 0, 30));
assert.equal(zone.wallToUtcMs("not a time"), null);

// Midnight of the zone's day; on the spring-forward day the day starts at 23:00
// UTC the evening before.
assert.equal(zone.zonedDayStartMs(Date.UTC(2026, 2, 29, 12)), Date.UTC(2026, 2, 28, 23));
assert.equal(zone.zonedDayStartMs(summer), Date.UTC(2026, 5, 30, 22));

// UTC mode: the same instants, in UTC; listeners hear the flip.
let heard = 0;
const off = zone.onChange(() => heard++);
zone.setUtc(true);
assert.equal(heard, 1);
assert.equal(zone.tz(), "UTC");
assert.equal(zone.label(), "UTC");
assert.equal(zone.fmtTime(summer), "12:34:56");
assert.equal(zone.tzDate(summer / 1000).getHours(), 12);
assert.equal(zone.wallToUtcMs("2026-07-01T12:34"), Date.UTC(2026, 6, 1, 12, 34));
assert.equal(zone.zonedDayStartMs(summer), Date.UTC(2026, 6, 1));
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

// West of UTC the skipped hour also moves forward: 02:30 on New York's
// spring-forward day (8 Mar 2026) is 03:30 EDT, 07:30 UTC.
assert.equal(zone.wallToUtcMs("2026-03-08T02:30"), Date.UTC(2026, 2, 8, 7, 30));
// An ordinary time just after that change is read at the new offset.
assert.equal(zone.wallToUtcMs("2026-03-08T04:30"), Date.UTC(2026, 2, 8, 8, 30));
// A repeated (fall-back) wall time takes its first occurrence: 01:30 EDT on
// 1 Nov 2026.
assert.equal(zone.wallToUtcMs("2026-11-01T01:30"), Date.UTC(2026, 10, 1, 5, 30));
// Santiago skips its own midnight (6 Sep 2026, 00:00 -> 01:00 -03), so that
// day starts at 01:00 local, 04:00 UTC.
zone.setSimZone("America/Santiago");
assert.equal(zone.zonedDayStartMs(Date.UTC(2026, 8, 6, 12)), Date.UTC(2026, 8, 6, 4));

// A zone the server knows but this Intl does not falls back to UTC instead of
// throwing out of every formatter, whether set directly or read by init.
zone.setSimZone("Not/A_Zone");
assert.equal(zone.tz(), "UTC");
assert.equal(zone.fmtTime(summer), "12:34:56");
globalThis.fetch = async () => new Response(JSON.stringify({ tz: "Not/A_Zone" }));
zone.setSimZone("Europe/Berlin");
await zone.init();
assert.equal(zone.tz(), "UTC");
delete globalThis.fetch;
zone.setSimZone("Europe/Berlin");

console.log("zone-test: all assertions passed");
