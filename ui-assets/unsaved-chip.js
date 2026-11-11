// The `unsaved` chip of a microgrid card and the microgrid bar, with no DOM,
// so the node tests can import it.

import { escapeHtml } from "./html.js";

// The chip for microgrid `m` (an `/api/microgrids` entry): none while its live
// structure matches its source. A microgrid booted from a topology file and
// changed since is "edited here"; one made in the UI or created by a script's
// declaration has no source and is "in memory only". Either way, an export keeps it.
export function unsavedChip(m) {
  if (!m.unsaved) return "";
  if (m.source) {
    const file = m.source.split(/[\\/]/).pop();
    const title = `edited here: ${file} does not have these changes; export to keep them`;
    return `<span class="mg-chip edited" title="${escapeHtml(title)}">edited here</span>`;
  }
  return `<span class="mg-chip unsaved" title="in memory only: export to keep it">in memory only</span>`;
}
