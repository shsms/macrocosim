// Shared parser + renderer for graph-crate-rendered formula strings
// like `MAX(#2 - COALESCE(#1002, #1001, 0.0), 0.0)`. Used by the
// dashboard's per-stream formula tree and the formula explorer
// panel. DOM-free on purpose: tools/formula-ast-test.mjs imports it
// under plain node, so it must not touch document/window (that is
// also why it carries its own escapeHtml instead of app.js's).
const escapeHtml = (s) =>
  String(s).replace(
    /[<>&"']/g,
    (c) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;", '"': "&quot;", "'": "&#39;" })[c],
  );

// Parses a graph-crate-rendered formula like
//   MAX(#2 - COALESCE(#1002, #1001, 0.0), 0.0)
// into an AST: { kind: "op" | "call" | "ref" | "num", ... }. Used by
// the formula inspector (F4 stage 2) to pretty-print the formula
// with each #N as a clickable link to the topology canvas. Hand-
// rolled recursive descent — the grammar is tiny (numbers, refs,
// + - * /, function calls) and a parser library would dwarf it.
export function parseFormula(src) {
  let i = 0;
  const skipWs = () => {
    while (i < src.length && /\s/.test(src[i])) i++;
  };
  const peek = () => {
    skipWs();
    return src[i];
  };
  const match = (re) => {
    skipWs();
    const m = src.slice(i).match(re);
    if (m && m.index === 0) {
      i += m[0].length;
      return m[0];
    }
    return null;
  };
  function expr() {
    let left = mul();
    while (peek() === "+" || peek() === "-") {
      const op = src[i++];
      left = { kind: "op", op, left, right: mul() };
    }
    return left;
  }
  function mul() {
    let left = atom();
    while (peek() === "*" || peek() === "/") {
      const op = src[i++];
      left = { kind: "op", op, left, right: atom() };
    }
    return left;
  }
  function atom() {
    skipWs();
    if (src[i] === "(") {
      i++;
      const e = expr();
      skipWs();
      if (src[i] === ")") i++;
      return { kind: "paren", inner: e };
    }
    if (src[i] === "#") {
      i++;
      const m = match(/^\d+/);
      return { kind: "ref", id: Number(m) };
    }
    const num = match(/^-?\d+(\.\d+)?([eE][-+]?\d+)?/);
    if (num != null) return { kind: "num", value: Number(num) };
    if (src[i] === "-") {
      i++;
      return { kind: "neg", inner: atom() };
    }
    const ident = match(/^[A-Za-z_][A-Za-z0-9_]*/);
    if (ident) {
      if (ident === "None") return { kind: "none" };
      skipWs();
      if (src[i] === "(") {
        i++;
        const args = [];
        skipWs();
        while (src[i] != null && src[i] !== ")") {
          args.push(expr());
          skipWs();
          if (src[i] === ",") {
            i++;
            continue;
          }
          break;
        }
        if (src[i] === ")") i++;
        return { kind: "call", name: ident, args };
      }
      return { kind: "ident", name: ident };
    }
    return { kind: "unknown", text: src.slice(i) };
  }
  return expr();
}

// Renders the AST back to text, byte-for-byte identical to the input
// string (the parser keeps paren nodes, so no re-grouping logic is
// needed). refreshFormula uses this to detect grammar drift between
// the crate's renderer and this parser.
export function formulaToText(node) {
  switch (node.kind) {
    case "ref":
      return `#${node.id}`;
    case "num":
      return renderNumber(node.value);
    case "none":
      return "None";
    case "ident":
      return node.name;
    case "neg":
      return `-${formulaToText(node.inner)}`;
    case "paren":
      return `(${formulaToText(node.inner)})`;
    case "op":
      return `${formulaToText(node.left)} ${node.op} ${formulaToText(node.right)}`;
    case "call":
      return `${node.name}(${node.args.map(formulaToText).join(", ")})`;
    default:
      return node.text || "";
  }
}

// Renders a number like the Rust side: whole numbers get one decimal
// place ("0.0"), everything else prints plainly. -0 keeps its sign.
function renderNumber(value) {
  if (Object.is(value, -0)) return "-0.0";
  return Number.isInteger(value) ? value.toFixed(1) : String(value);
}

// Render the AST as nested HTML:
// - every compound sub-expression wraps in a .formula-node span so a
//   hover handler can highlight exactly the part under the cursor;
// - every #N ref is a .formula-ref span carrying data-id;
// - sign is tracked as a PARITY, not a depth: the right operand of a
//   binary `-` and a negated subtree each flip it, so in
//   `#2 - (#3 - #4)` the #4 is added back (a − (b − c) = a − b + c).
//   A wrapper span is emitted at each flip point only — even→odd is
//   .formula-subtracted (taken OUT of the measurement), odd→even is
//   .formula-unsubtracted. The NEAREST wrapper therefore carries the
//   true sign for every ref beneath it, which is exactly what a
//   closest() reader resolves to;
// - calls with long arg lists break onto their own lines.
export function formulaToHtml(node) {
  const wrap = (inner) => `<span class="formula-node">${inner}</span>`;
  const isAtom = (n) => n.kind === "ref" || n.kind === "num";
  // Wrap a subtree whose sign just flipped to `sub`, naming the parity
  // it flipped INTO so nested readers stop at the innermost flip.
  const flip = (inner, sub) =>
    `<span class="${sub ? "formula-subtracted" : "formula-unsubtracted"}">${inner}</span>`;
  function rec(n, sub) {
    switch (n.kind) {
      case "ref":
        return `<span class="formula-ref formula-node" data-id="${n.id}" title="select component ${n.id}">#${n.id}</span>`;
      case "num":
        return `<span class="formula-num">${renderNumber(n.value)}</span>`;
      case "none":
        return `<span class="formula-num">None</span>`;
      case "ident":
        return `<span class="formula-ident">${escapeHtml(n.name)}</span>`;
      case "paren":
        return wrap(`(${rec(n.inner, sub)})`);
      case "neg":
        return wrap(
          `<span class="formula-op">-</span>${flip(rec(n.inner, !sub), !sub)}`,
        );
      case "op": {
        const right =
          n.op === "-" ? flip(rec(n.right, !sub), !sub) : rec(n.right, sub);
        return wrap(
          `${rec(n.left, sub)} <span class="formula-op">${n.op}</span> ${right}`,
        );
      }
      case "call": {
        const head = `<span class="formula-call">${escapeHtml(n.name)}</span>`;
        const args = n.args.map((a) => rec(a, sub));
        if (n.args.length <= 2 && n.args.every(isAtom)) {
          return wrap(`${head}(${args.join(", ")})`);
        }
        const indented = args
          .map((a) => `<div class="formula-arg">${a},</div>`)
          .join("");
        return wrap(`${head}(${indented})`);
      }
      default:
        return `<span class="formula-raw">${escapeHtml(n.text || "")}</span>`;
    }
  }
  return rec(node, false);
}
