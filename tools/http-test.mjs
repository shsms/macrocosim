// The error reader returns the server's `error` field, the raw text
// for a non-JSON body, and the status line for an empty one. bgFetch
// raises and clears the banner, logs an HTTP error once per failure
// streak, and tells the reconnect when the server is back.
// Run: node tools/http-test.mjs   (exits non-zero on failure)
import assert from "node:assert/strict";
import { onReachedAgain } from "../ui-assets/connection.js";
import { bgFetch, errorText } from "../ui-assets/http.js";

assert.equal(
  await errorText(new Response('{"error":"microgrid 9 not registered"}', { status: 404 })),
  "microgrid 9 not registered",
);
assert.equal(await errorText(new Response("<html>bad gateway</html>", { status: 502 })), "<html>bad gateway</html>");
assert.equal(await errorText(new Response("", { status: 502 })), "HTTP 502");
assert.equal(await errorText(new Response('{"other":1}', { status: 500 })), '{"other":1}');

// The two elements bgFetch reaches: the banner, and the logs panel through
// logUi.
const banner = { hidden: true };
const logs = {
  children: [],
  scrollHeight: 0,
  scrollTop: 0,
  clientHeight: 0,
  appendChild(row) {
    this.children.push(row);
  },
};
globalThis.document = {
  getElementById: (id) => ({ "server-banner": banner, logs })[id] ?? null,
  createElement: () => ({
    append(...cells) {
      this.cells = cells;
    },
  }),
};
const logged = () => logs.children.map((row) => row.cells[2].textContent);
// fetch answers from this queue; an Error is a network failure.
const replies = [];
globalThis.fetch = async () => {
  const next = replies.shift();
  if (next instanceof Error) throw next;
  return next;
};
const boom = () => new Response('{"error":"boom"}', { status: 500 });
let reachedAgain = 0;
onReachedAgain(() => reachedAgain++);

replies.push(new TypeError("Failed to fetch"));
await assert.rejects(bgFetch("src", "/x"), TypeError);
assert.equal(banner.hidden, false);
replies.push(new Response("{}"), new Response("{}"));
await bgFetch("src", "/x");
await bgFetch("src", "/x");
assert.equal(banner.hidden, true);
assert.equal(reachedAgain, 1);
replies.push(boom(), boom(), new Response("{}"), boom());
for (let i = 0; i < 4; i++) await bgFetch("src", "/x");
assert.equal(banner.hidden, true);
assert.deepEqual(logged(), ["ui: server unreachable", "ui: server reachable again", "ui: src: boom", "ui: src: boom"]);

console.log("http-test: all assertions passed");
