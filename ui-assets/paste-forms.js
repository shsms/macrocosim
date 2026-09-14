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

// Build the let*-bound eval that pastes `snap` (a clipboard snapshot:
// `components` + the `edges` among them) as a fresh set of components
// and edges. Uses the public make-* wrappers so per-category defaults
// apply, and threads the bindings so the reconnects land atomically —
// one eval, one undo step.
export function pasteSource(snap) {
  const bindings = snap.components
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
  const reconnects = snap.edges.map(([from, to]) => `(connect m${from} m${to})`).join(" ");
  return `(let* (${bindings}) ${reconnects || "t"})`;
}
