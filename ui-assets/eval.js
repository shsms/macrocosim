import { errorText } from "./http.js";
import { notify } from "./notices.js";
import { mgPath } from "./routing.js";

export function jsToLispString(s) {
  return s.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

// POST one Lisp expression to the selected microgrid's eval, or the
// whole-site /api/eval with none selected. Returns { ok: true, value } on
// success, or { ok: false, error } when the evaluation failed, the server
// refused the request, its answer was not JSON or the fetch failed. Undo
// history is the server's: a structural eval stacks the microgrid file's
// previous generated block by itself, so there is nothing to record here.
export async function evalText(expr) {
  let res;
  try {
    res = await fetch(mgPath("eval") ?? "/api/eval", { method: "POST", body: expr });
  } catch (err) {
    return { ok: false, error: err.message };
  }
  if (!res.ok) return { ok: false, error: await errorText(res) };
  try {
    return { ok: true, value: (await res.json()).value };
  } catch (_) {
    return { ok: false, error: "the server's answer was not JSON" };
  }
}

// evalText, with a failure toasted as `${label}: ${error}`. `label`
// defaults to the expression itself — pass something short like "Paste
// failed" when the expression would be unreadable in a toast.
export async function evalQuoted(expr, label = expr) {
  const result = await evalText(expr);
  if (!result.ok) notify(`${label}: ${result.error}`);
  return result;
}
