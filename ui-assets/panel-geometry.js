// The floating panels' placement arithmetic, kept apart from the
// shell that runs it: pure functions over plain values — no document,
// no storage — so side-panel.js reads what it needs, hands it in,
// and applies what comes back, and tools/panel-geometry-test.mjs can
// check the model with no DOM at all.

// The cascade slot for a card nobody has placed: the first
// `base + step * k` no open card sits on. `taken` holds the dy of
// every open card still in the cascade column; a card within half a
// step of a slot holds that slot, so closing the card at the top hands
// its slot to the next card opened instead of stacking that card on
// the one below.
export function cascadeSlot(taken, base, step) {
  for (let k = 0; ; k++) {
    const slot = base + step * k;
    if (!taken.some((t) => Math.abs(t - slot) < step / 2)) return slot;
  }
}

// The dy every card in `cards` claims in the top-right cascade column,
// for `cascadeSlot`. A card is `{ pos, dock }`: a docked card is a
// tile in a strip, not a column card; a placement anchored to the
// bottom edge, or dragged sideways off the column's x by `drift` or
// more, is out of the column.
export function cascadeColumn(cards, drift) {
  const taken = [];
  for (const c of cards) {
    if (!c || c.dock) continue;
    const at = c.pos;
    if (at.bottom || Math.abs(at.dx) >= drift) continue;
    taken.push(at.dy);
  }
  return taken;
}
