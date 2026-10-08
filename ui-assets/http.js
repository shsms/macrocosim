// Reading failed API responses. Every route fails with JSON
// `{"error": "..."}`; anything else (a proxy page, an empty body)
// falls back to its text or the status line.

import { reportReachable } from "./connection.js";
import { logUi } from "./notices.js";

// The message of a failed response.
export async function errorText(res) {
  const text = await res.text().catch(() => "");
  try {
    const body = JSON.parse(text);
    if (body && typeof body.error === "string") return body.error;
  } catch (_e) {
    // Not JSON: fall through to the raw text.
  }
  return text.trim() || `HTTP ${res.status}`;
}

// The JSON body of a GET of `path`; a failed response throws its
// message. With a `source`, it is a background loop's request (bgFetch).
export async function getJson(path, source = null) {
  const res = await (source ? bgFetch(source, path) : fetch(path));
  if (!res.ok) throw new Error(await errorText(res));
  return res.json();
}

// Sources whose HTTP error is already in the logs panel; a source leaves
// when it succeeds again.
const loggedHttp = new Set();

// fetch for the background loop named `source`: it reports whether the
// server was reached, logs an HTTP error once per failure streak, and
// returns the response (rethrowing a network error), so the loop keeps its
// own fallback.
export async function bgFetch(source, path, init) {
  let res;
  try {
    res = await fetch(path, init);
  } catch (e) {
    reportReachable(false);
    throw e;
  }
  reportReachable(true);
  if (res.ok) loggedHttp.delete(source);
  else if (!loggedHttp.has(source)) {
    loggedHttp.add(source);
    logUi("warn", `${source}: ${await errorText(res.clone())}`);
  }
  return res;
}
