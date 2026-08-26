import { notify } from "./app.js";
import { mgPath } from "./routing.js";

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
