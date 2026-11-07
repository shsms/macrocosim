// Structural edits of the selected microgrid: read its stored spec,
// change it here, and PUT it back, which rebuilds the microgrid with
// carry and is one step on the server's undo stack. New components
// take ids from nextFreeIds; when the PUT answers 409 (another change
// took an id first), the edit reads the specs again, picks again and
// retries once.

import { errorText, getJson, jsonInit } from "./http.js";
import { nextFreeIds } from "./ids.js";
import { notify } from "./notices.js";
import { readSelectedMg } from "./routing.js";

// Every microgrid's stored spec: new component ids must be free
// across all of them.
async function allSpecs() {
  const list = await getJson("/api/microgrids");
  return Promise.all(list.map((m) => getJson(`/api/mg/${m.id}/spec`)));
}

// Apply `change(spec, ids)` to the selected microgrid's spec and PUT
// it. `ids` holds `newIds` fresh component ids. `change` returns false
// to drop the edit, and an edit that leaves the spec as it was is
// dropped too: a PUT rebuilds the microgrid, which ends its streams and
// a recording. A failure is toasted as `${label}: ${error}`. Resolves
// to whether the edit landed.
export async function editSpec(label, change, newIds = 0) {
  const mg = readSelectedMg();
  if (mg == null) {
    notify(`${label}: no microgrid selected`);
    return false;
  }
  for (let attempt = 0; ; attempt++) {
    let res;
    try {
      let spec;
      let ids = [];
      if (newIds) {
        // One read of every spec gives both the free ids and this one.
        const specs = await allSpecs();
        spec = specs.find((s) => s.id === mg);
        if (spec === undefined) throw new Error(`microgrid ${mg} is gone`);
        ids = nextFreeIds(specs, newIds);
      } else {
        spec = await getJson(`/api/mg/${mg}/spec`);
      }
      const before = JSON.stringify(spec);
      if (change(spec, ids) === false || JSON.stringify(spec) === before) return false;
      res = await fetch(`/api/mg/${mg}/spec`, jsonInit("PUT", spec));
    } catch (err) {
      notify(`${label}: ${err.message}`);
      return false;
    }
    if (res.ok) return true;
    if (res.status === 409 && newIds && attempt === 0) continue;
    notify(`${label}: ${await errorText(res)}`);
    return false;
  }
}

// The spec edits the editor makes, each one PUT.

// Add one component of spec type `type`, unconnected.
export function addComponent(type, extra = {}) {
  return editSpec(
    "Create failed",
    (spec, [id]) => {
      spec.components.push({ type, id, ...extra });
    },
    1,
  );
}

// Remove the components `ids` and every connection that touches one.
export function removeComponents(ids) {
  const gone = new Set(ids);
  return editSpec("Delete failed", (spec) => {
    spec.components = spec.components.filter((c) => !gone.has(c.id));
    spec.connections = spec.connections.filter(([from, to]) => !gone.has(from) && !gone.has(to));
  });
}

// Connect parent `from` to child `to`; a connection the spec already
// has is a no-op.
export function connect(from, to) {
  return editSpec("Connect failed", (spec) => {
    if (spec.connections.some(([f, t]) => f === from && t === to)) return false;
    spec.connections.push([from, to]);
  });
}

// Drop the connection from parent `from` to child `to`.
export function disconnect(from, to) {
  return editSpec("Disconnect failed", (spec) => {
    spec.connections = spec.connections.filter(([f, t]) => !(f === from && t === to));
  });
}

// Rename component `id`.
export function rename(id, name) {
  return editSpec("Rename failed", (spec) => {
    const c = spec.components.find((x) => x.id === id);
    if (!c) return false;
    c.name = name;
  });
}
