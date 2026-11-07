// The component ids the UI gives new components: the lowest free ones
// of UI_FIRST_ID or more. Component ids are unique across the engine,
// so "free" means no microgrid's spec uses it. Scripts number their
// automatic ids from 1000, so the two ranges meet only in a script with
// more than 4000 of them. The server checks again, and answers a
// collision with 409. DOM-free so tools/ids-test.mjs can import it.

export const UI_FIRST_ID = 5000;

// The lowest `n` ids of UI_FIRST_ID or more that none of `specs` (each
// with a `components` list of `{id}`) uses, ascending.
export function nextFreeIds(specs, n) {
  const used = new Set();
  for (const spec of specs) {
    for (const c of spec.components ?? []) used.add(c.id);
  }
  const out = [];
  for (let id = UI_FIRST_ID; out.length < n; id++) {
    if (!used.has(id)) out.push(id);
  }
  return out;
}
