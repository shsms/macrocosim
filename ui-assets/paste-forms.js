// The clipboard paste's pure form builder: which make-* wrapper a
// copied component clones through, and the let* that binds the
// clones and reconnects them. DOM-free so tools/paste-forms-test.mjs
// can pin the order the bindings come out in.

export function makeFnFor(c) {
  if (c.category === "inverter") {
    return c.subtype === "solar" ? "make-solar-inverter" : "make-battery-inverter";
  }
  return {
    grid: "make-grid-connection-point",
    meter: "make-meter",
    battery: "make-battery",
    "ev-charger": "make-ev-charger",
    chp: "make-chp",
    "wind-turbine": "make-wind-turbine",
    "steam-boiler": "make-steam-boiler",
    "power-transformer": "make-power-transformer",
    breaker: "make-breaker",
  }[c.category] ?? null;
}

// Children before parents, as an authored config registers them:
// Lisp evaluates :successors before the surrounding make-*, so a
// pasted inverter must not register — and so tick — before the
// battery it pushes into (the order the server gives imports too).
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

// Build the let*-bound eval that pastes `snap` (a clipboard snapshot:
// `components` + the `edges` among them) as a fresh set of components
// and edges. Uses the public make-* wrappers so per-category defaults
// apply, and threads the bindings so the reconnects land atomically —
// one eval, one undo step.
export function pasteSource(snap) {
  // The editor filters the snapshot's edges to the selected ids, but
  // a selected id the topology no longer holds is dropped from
  // `components` while its edges are not. This filter is what keeps
  // `edges` within `components`, which `childrenFirst` assumes; a
  // stray edge would otherwise name a symbol the let* never bound
  // and abort the whole eval.
  const copied = new Set(snap.components.map((c) => c.id));
  const edges = snap.edges.filter(([from, to]) => copied.has(from) && copied.has(to));
  const bindings = childrenFirst(snap.components, edges)
    .map((c) => {
      const flags = [];
      // `:hidden t` is accepted by make-meter alone, and the make-*
      // plists reject unknown keys outright — so emitting it at any
      // other constructor would be a Lisp error, not a no-op. Safe
      // unconditionally because a meter is the only component that
      // can report hidden, so `c.hidden` already implies one. Emitted
      // when set so the snapshot round-trips: sticky for cut+paste
      // and cross-mg copy+paste.
      if (c.hidden) flags.push(":hidden t");
      // The operational mode is config, so a clone keeps it.
      if (c.operational_mode && c.operational_mode !== "unspecified") {
        flags.push(`:operational-mode '${c.operational_mode}`);
      }
      const args = flags.length ? ` ${flags.join(" ")}` : "";
      return `(m${c.id} (${makeFnFor(c)}${args}))`;
    })
    .join(" ");
  const reconnects = edges.map(([from, to]) => `(connect m${from} m${to})`).join(" ");
  return `(let* (${bindings}) ${reconnects || "t"})`;
}
