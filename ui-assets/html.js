// HTML helpers with no DOM, so the node tests can import them.

// `s` as text that is safe inside HTML, in an element or a quoted attribute.
export function escapeHtml(s) {
  return String(s).replace(
    /[<>&"']/g,
    (c) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;", '"': "&quot;", "'": "&#39;" })[c],
  );
}
