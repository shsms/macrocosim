// Every button uses the control kit or a named component of its own, and the
// classes the kit replaced are gone from ui-assets.
// Run: node tools/controls-test.mjs   (exits non-zero on failure)
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";

const dir = new URL("../ui-assets/", import.meta.url);
const sources = new Map(
  readdirSync(dir)
    .filter((f) => /\.(js|html|css)$/.test(f))
    .map((f) => [f, readFileSync(new URL(f, dir), "utf8")]),
);

// Classes the kit replaced; none may appear in markup, JS or CSS.
const RETIRED = [
  "hdr-btn",
  "link-btn",
  "tool-btn",
  "canvas-btn",
  "dd-input",
  "dd-input-inline",
  "dd-input-num",
  "dlg-input",
  "dialog-error",
  "dd-error",
  "weather-err",
  // the lit state of a panel's toggle, now aria-pressed
  "primary",
];
// A button's class list must contain one of these.
const ALLOWED = [
  "btn",
  "pill",
  "pulse-chip",
  "mode-btn",
  "dd-seg-btn",
  "ctx-item",
  "dock-menu-item",
  "script-row",
  "mglist-card",
  "mchip",
  "pal-btn",
  "dd-chip",
  "repl-mg-chip",
  "seg-chip",
];

// The text from `from` up to the first `stop` character outside a template
// `${…}`, which may hold quotes, `>` and nested braces of its own. A `)` stop
// skips the parentheses nested before it.
function scanTo(text, from, stop) {
  let depth = 0;
  let parens = 0;
  for (let i = from; i < text.length; i++) {
    if (text.startsWith("${", i)) {
      depth++;
      i++;
    } else if (depth > 0 && text[i] === "{") depth++;
    else if (depth > 0 && text[i] === "}") depth--;
    else if (depth === 0 && stop === ")" && text[i] === "(") parens++;
    else if (depth === 0 && stop === ")" && text[i] === ")" && parens > 0) parens--;
    else if (depth === 0 && text[i] === stop) return text.slice(from, i);
  }
  return text.slice(from);
}
// The `${…}` parts of a template text, in order, and the text with them cut
// out.
function splitTemplate(text) {
  const parts = [];
  let rest = "";
  for (let i = 0; i < text.length; i++) {
    if (!text.startsWith("${", i)) {
      rest += text[i];
      continue;
    }
    const start = i + 2;
    let depth = 0;
    for (i++; i < text.length; i++) {
      if (text[i] === "{") depth++;
      else if (text[i] === "}" && --depth === 0) break;
    }
    parts.push(text.slice(start, i));
    rest += " ";
  }
  return { parts, rest };
}
// The bodies of the string literals in a piece of code, in order.
function stringsIn(code) {
  const strings = [];
  for (let i = 0; i < code.length; i++) {
    const quote = code[i];
    if (quote !== '"' && quote !== "'" && quote !== "`") continue;
    const body = scanTo(code, i + 1, quote);
    strings.push(body);
    i += body.length + 1;
  }
  return strings;
}
// Every class a class value can produce: its static words and the words of
// the strings inside its `${…}` parts, so both sides of a ternary count.
const classWords = (value) => [
  ...splitTemplate(value).rest.split(/\s+/).filter(Boolean),
  ...stringsIn(value).flatMap(classWords),
];
// The values of the class attributes in markup.
const classAttrs = (text) =>
  [...text.matchAll(/(?<![\w-])class\s*=\s*(["'`])/g)].map((m) => scanTo(text, m.index + m[0].length, m[1]));
// The strings code assigns to className or hands to setAttribute("class", …)
// and the `methods` of classList, on whatever `on` matches before the dot.
function setterStrings(code, on, methods) {
  const setter = new RegExp(
    `${on}(?:\\.className\\s*\\+?=(?!=)|\\.setAttribute\\(\\s*["']class["']\\s*,|\\.classList\\.(?:${methods})\\()`,
    "g",
  );
  return [...code.matchAll(setter)].flatMap((m) =>
    stringsIn(scanTo(code, m.index + m[0].length, m[0].endsWith("(") || m[0].endsWith(",") ? ")" : ";")),
  );
}
// The strings a source compares className with, on either side.
function comparedStrings(text) {
  const strings = [];
  for (const m of text.matchAll(/\.className\s*[!=]==?\s*(["'`])/g)) {
    strings.push(scanTo(text, m.index + m[0].length, m[1]));
  }
  // On the left: the string just before the operator, on its line; a quote
  // string ends at its own quote, a template may hold `${…}`.
  for (const m of text.matchAll(/[!=]==?\s*[\w$.?[\]]*\.className\b/g)) {
    const before = text.slice(text.lastIndexOf("\n", m.index) + 1, m.index).trimEnd();
    const quote = before.at(-1);
    if (quote === "`") strings.push(stringsIn(before).at(-1));
    else if (quote === '"' || quote === "'") strings.push(before.slice(before.lastIndexOf(quote, before.length - 2) + 1, -1));
  }
  return strings;
}
// The class values a source holds: class attributes, what it sets, compares
// className with or passes to classList, and every string inside a template's
// `${…}`, since a class is often built in one and set later.
const classValues = (text) => [
  ...classAttrs(text),
  ...setterStrings(text, "", "\\w+"),
  ...comparedStrings(text),
  ...splitTemplate(text).parts.flatMap(stringsIn),
];
// The classes code gives the variable `owner`: its className assignments,
// setAttribute("class", …) values and classList.add/toggle calls, not those of
// a property with the same name. A later write that drops a class is not
// followed.
const classesGiven = (code, owner) =>
  setterStrings(code, `(?<![\\w.$])${owner}`, "add|toggle").flatMap(classWords);
// A class used as a selector: `.cls` in CSS. Outside CSS, a `.cls` must not
// follow an identifier, `)` or `]`, so a property read such as `cfg.primary`
// is not taken for one; inside a string it may, as in "button.primary". An
// element id of the same name is not a use.
const selectorUse = (cls, css) =>
  new RegExp(
    `${css ? "" : "(?:(?<![\\w$)\\]])(?:[.#][\\w-]+)*|[\"'\`][^\"'\`$\\n]*?)"}\\.${cls}(?![\\w-])`,
  );
const retired = RETIRED.map((cls) => ({ cls, js: selectorUse(cls, false), css: selectorUse(cls, true) }));
// The retired classes a source uses; `kind` is "css" or "js" (markup too).
function retiredIn(text, kind) {
  const used = new Set(kind === "js" ? classValues(text).flatMap(classWords) : []);
  return retired.filter((r) => used.has(r.cls) || r[kind].test(text)).map((r) => r.cls);
}

// The helpers on known inputs, so a broken helper fails here and not only on
// a later slip in ui-assets.
for (const [text, want] of [
  ['<b class="btn primary">', true],
  ["<b class='btn primary'>", true],
  ['<b class="btn${on ? " primary" : ""}">', true],
  ['const cls = `pill ${on ? "primary" : ""}`;', true],
  ['b.className = `x ${c ? `btn ${size} primary` : ""}`;', true],
  ['<b class="x ${a ? `${b ? `c ${d} primary` : ""}` : ""}">', true],
  ['el.className = on\n  ? "btn primary"\n  : "btn";', true],
  ['el.classList.add(fn(a), "primary");', true],
  ['q("button.primary")', true],
  ['if (el.className === "primary") f();', true],
  ['if ("primary" !== el.className) f();', true],
  ['if (`x ${on ? "a" : "b"} primary` === el?.className) f();', true],
  ['if ("primary" == a[0].className) f();', true],
  ["/* don't */ if ('primary' === el.className) f();", true],
  ['if ("primary" === kind && el.className) f();', false],
  ['if (el.className === "x") { t.textContent = "primary"; }', false],
  ['<b data-class="primary" class="btn">', false],
  ['`${n === 1 ? "is" : "are"} primary`', false],
  ["const x = cfg.primary;", false],
  ['"the primary button"', false],
]) {
  assert.equal(retiredIn(text, "js").includes("primary"), want, `retiredIn(${JSON.stringify(text)})`);
}
assert.deepEqual(
  classesGiven(
    'b.className = "x"; b.setAttribute("class", "y"); b.className += " z"; b.classList.remove("v");' +
      ' a.b.className = "w"; if (b.className === "u") f();',
    "b",
  ),
  ["x", "y", "z"],
);

for (const [f, text] of sources) {
  assert.deepEqual(retiredIn(text, f.endsWith(".css") ? "css" : "js"), [], `${f} still uses retired classes`);
}

const offenders = [];
for (const [f, text] of sources) {
  if (f.endsWith(".css")) continue;
  for (const m of text.matchAll(/<button\b/g)) {
    const [cls] = classAttrs(scanTo(text, m.index, ">"));
    if (cls === undefined) {
      offenders.push(`${f}: a <button> with no class`);
      continue;
    }
    if (!classWords(cls).some((c) => ALLOWED.includes(c))) offenders.push(`${f}: ${cls}`);
  }
  // A button built in JS: its class is set somewhere in the rest of the
  // block it is created in (the lines up to the first one indented less).
  for (const m of text.matchAll(/^([ \t]*).*?\b(\w+) = document\.createElement\(["'`]button["'`]\)/gm)) {
    const [, indent, v] = m;
    const rest = text.slice(m.index + m[0].length).split("\n");
    const end = rest.findIndex((l, i) => i > 0 && l.trim() && l.search(/\S/) < indent.length);
    const block = rest.slice(0, end < 0 ? undefined : end).join("\n");
    const classes = classesGiven(block, v);
    if (classes.length === 0) {
      offenders.push(`${f}: a JS-built button with no class (${v})`);
      continue;
    }
    if (!classes.some((c) => ALLOWED.includes(c))) offenders.push(`${f}: ${classes.join(" ")}`);
  }
}
assert.deepEqual(offenders, [], `buttons outside the kit:\n  ${offenders.join("\n  ")}`);

console.log("controls-test: all assertions passed");
