// Round-trip and rendering tests for ui-assets/formula-ast.js.
// Run: node tools/formula-ast-test.mjs   (exits non-zero on failure)
import {
  formulaToHtml,
  formulaToText,
  parseFormula,
} from "../ui-assets/formula-ast.js";

let failures = 0;
function check(name, cond, detail) {
  if (!cond) {
    failures++;
    console.error(`FAIL ${name}${detail ? `: ${detail}` : ""}`);
  }
}

// 1. Round-trip: parse + formulaToText must reproduce the crate's
// rendering byte-for-byte. Cases mirror the 0.6.2 Expr::render tests.
const ROUND_TRIP = [
  "#1",
  "0.0",
  "-0.0",
  "#10 + #11 + #12 + #13",
  "#11 - #10",
  "#11 + #12 - #10",
  "#11 - #12 - #10",
  "-(#10 + #11 + #12)",
  "-#10",
  "None",
  "COALESCE(#1002, #1001, 0.0)",
  "MAX(#2 - COALESCE(#1002, #1001, 0.0), 0.0)",
  "MIN(#1, #2)",
  "COALESCE(#8, #9) + COALESCE(#12, #13)",
  "#2 - (#3 - #4)",
  "#11 + #12 - (#10 + #13)",
  "#11 + #12 - (#10 - #13)",
  "#11 - #12 + #10",
  "#13 - #10 + #11 + #12",
  "COALESCE(#5, #7 + #6)",
  "22.44",
  "MIN(0.0, #5, #7 + #6) - MAX(COALESCE(#5, #7 + #6), #7, 22.44)",
  "-COALESCE(#5, 0.0)",
];
for (const src of ROUND_TRIP) {
  const out = formulaToText(parseFormula(src));
  check(`round-trip ${src}`, out === src, `got ${out}`);
}

// 2. Subtracted tinting: every ref that is taken OUT of the
// measurement sits inside a .formula-subtracted wrapper, and the
// renderer emits a wrapper at each SIGN FLIP, so the NEAREST one
// decides — a .formula-unsubtracted inside a .formula-subtracted
// means the terms below it are added back. DOM-free scan mirroring
// what the panel's closest() reader does: walk the tags keeping a
// stack of the flip wrappers still open; a ref under a subtracted
// innermost flip records positive, otherwise negative (added).
function subtractedIds(html) {
  const ids = [];
  const tokens = html.split(/(<[^>]+>)/);
  const stack = [];
  const flips = [];
  for (const t of tokens) {
    if (t.startsWith("<span")) {
      let flip = null;
      if (t.includes("formula-subtracted")) flip = true;
      else if (t.includes("formula-unsubtracted")) flip = false;
      stack.push(flip);
      if (flip !== null) flips.push(flip);
      const m = /data-id="(\d+)"/.exec(t);
      const sub = flips.length > 0 && flips[flips.length - 1];
      if (m) ids.push(sub ? Number(m[1]) : -Number(m[1])); // negative = added
    } else if (t === "</span>") {
      if (stack.pop() !== null) flips.pop();
    }
  }
  return ids;
}
{
  const html = formulaToHtml(parseFormula("#11 + #12 - #10"));
  const ids = subtractedIds(html);
  check("sub tint: #10 red", ids.includes(10), JSON.stringify(ids));
  check("sub tint: #11 not red", ids.includes(-11), JSON.stringify(ids));
  check("sub tint: #12 not red", ids.includes(-12), JSON.stringify(ids));
}
{
  const html = formulaToHtml(parseFormula("-(#10 + #11)"));
  const ids = subtractedIds(html);
  check("neg tint: #10 red", ids.includes(10), JSON.stringify(ids));
  check("neg tint: #11 red", ids.includes(11), JSON.stringify(ids));
}
// A subtraction inside a subtraction ADDS: a − (b − c) = a − b + c,
// so the inner right operand must come back out of the red.
{
  const html = formulaToHtml(parseFormula("#2 - (#3 - #4)"));
  const ids = subtractedIds(html);
  check("nested tint: #3 red", ids.includes(3), JSON.stringify(ids));
  check("nested tint: #4 not red", ids.includes(-4), JSON.stringify(ids));
}
{
  const html = formulaToHtml(parseFormula("#11 + #12 - (#10 - #13)"));
  const ids = subtractedIds(html);
  check("nested tint: #10 red", ids.includes(10), JSON.stringify(ids));
  check("nested tint: #13 not red", ids.includes(-13), JSON.stringify(ids));
}
{
  const html = formulaToHtml(parseFormula("-(#10 - #11)"));
  const ids = subtractedIds(html);
  check("neg nested tint: #10 red", ids.includes(10), JSON.stringify(ids));
  check("neg nested tint: #11 not red", ids.includes(-11), JSON.stringify(ids));
}
{
  const html = formulaToHtml(parseFormula("MAX(#2 - COALESCE(#1002, 0.0), 0.0)"));
  check("ref link", html.includes('data-id="1002"'), html);
  check("call span", html.includes('class="formula-call"'), html);
  check(
    "hover spans",
    (html.match(/class="formula-node/g) || []).length >= 3,
    html,
  );
}
{
  const html = formulaToHtml(parseFormula("None"));
  check("None renders as value", html.includes("formula-num"), html);
}

if (failures) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log("formula-ast: all tests passed");
