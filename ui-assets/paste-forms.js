// The clipboard paste's pure spec splice: which spec type a copied
// component clones as, and how a copied subgraph joins a microgrid's
// spec under fresh ids. DOM-free so tools/paste-forms-test.mjs can pin
// it.

// The topology document's `"type"` for a component of the topology
// payload (`category` + `subtype`), or null for one the spec has no
// type for.
export function specTypeFor(c) {
  if (c.category === "inverter") {
    return c.subtype === "solar" ? "solar_inverter" : "battery_inverter";
  }
  return (
    {
      grid: "grid_connection_point",
      meter: "meter",
      battery: "battery",
      "ev-charger": "ev_charger",
      chp: "chp",
      "wind-turbine": "wind_turbine",
      "steam-boiler": "steam_boiler",
      "power-transformer": "power_transformer",
      breaker: "breaker",
    }[c.category] ?? null
  );
}

// Children before parents, the order an authored spec lists them in.
// A depth-first post-order walk over the parent → child edges gives
// that order. A component reached from two parents comes out once;
// one in no edge is its own whole subtree, emitted where the
// selection-order sweep reaches it — interleaved, not collected at
// the end. A node is marked seen on entry, so the walk is linear.
// `edges` must only join components in `components`.
function childrenFirst(components, edges) {
  const byId = new Map(components.map((c) => [c.id, c]));
  const children = new Map();
  for (const [from, to] of edges) {
    if (!children.has(from)) children.set(from, []);
    children.get(from).push(to);
  }
  const seen = new Set();
  const ordered = [];
  const visit = (id) => {
    if (seen.has(id)) return;
    seen.add(id);
    for (const child of children.get(id) ?? []) visit(child);
    ordered.push(byId.get(id));
  };
  for (const c of components) visit(c.id);
  return ordered;
}

// Splice `snap` (a clipboard snapshot: `components` + the `edges`
// among them) into `spec` as fresh components: the i-th copied
// component, children first, takes `ids[i]`, and the edges between
// copied components are rewritten to the new ids. A clone carries its
// structure only — its type, whether it is hidden and its operational
// mode — so the engine's per-type defaults fill the rest. `ids` must
// hold one id per copied component. Returns the new ids.
export function pasteIntoSpec(spec, snap, ids) {
  // The editor filters the snapshot's edges to the selected ids, but
  // a selected id the topology no longer holds is dropped from
  // `components` while its edges are not. This filter keeps `edges`
  // within `components`, which `childrenFirst` assumes.
  const copied = new Set(snap.components.map((c) => c.id));
  const edges = snap.edges.filter(([from, to]) => copied.has(from) && copied.has(to));
  const newId = new Map();
  for (const c of childrenFirst(snap.components, edges)) {
    const id = ids[newId.size];
    newId.set(c.id, id);
    const clone = { type: specTypeFor(c), id };
    if (c.hidden) clone.hidden = true;
    // The operational mode is config, so a clone keeps it.
    if (c.operational_mode && c.operational_mode !== "unspecified") {
      clone.operational_mode = c.operational_mode;
    }
    spec.components.push(clone);
  }
  for (const [from, to] of edges) spec.connections.push([newId.get(from), newId.get(to)]);
  return [...newId.values()];
}
