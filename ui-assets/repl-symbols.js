// The REPL's knowledge of the names the interpreter defines, from
// /api/symbols. No DOM, so tools/repl-symbols-test.mjs imports it under plain
// node.

// The names from /api/symbols, refreshed by `refresh`. `fetchJson` is (url) =>
// Promise of the parsed body; it throws on a failed request.
export function createSymbolStore(fetchJson) {
  let byName = new Map();
  let reads = 0; // reads started
  let applied = 0; // the newest read whose list is in use
  return {
    // Reads the list again. A failed read keeps the last list.
    async refresh() {
      const read = ++reads;
      try {
        const { symbols } = await fetchJson("/api/symbols");
        // A read older than the list in use is dropped.
        if (read > applied) {
          applied = read;
          byName = new Map(symbols.map((s) => [s.name, s]));
        }
      } catch (_) {
        // The REPL still works without completion; the next focus or eval tries
        // again.
      }
    },
    // Up to `limit` names that start with `prefix`, in name order. A number
    // completes to nothing, so typing one never offers 1+ or 1-.
    complete(prefix, limit = 12) {
      if (!prefix || /^[+-]?(\d+\.?\d*|\.\d+)$/.test(prefix)) return [];
      const out = [];
      for (const name of byName.keys()) {
        if (name.startsWith(prefix)) {
          out.push(name);
          if (out.length === limit) break;
        }
      }
      return out;
    },
    lookup(name) {
      return byName.get(name);
    },
  };
}
