// The stylesheet's custom properties and colours: every variable the UI
// reads is defined somewhere (an undefined one is not an error to the
// browser, it silently drops the declaration or takes the fallback for
// ever), every colour in style.css is a token from the theme blocks, and
// spacing, radii and z-index come from --space-*, --radius-* and --z-*.
// Run: node tools/css-vars-test.mjs   (exits non-zero on failure)
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";

const dir = new URL("../ui-assets/", import.meta.url);
const sources = readdirSync(dir)
  .filter((f) => /\.(css|js|html)$/.test(f))
  .map((f) => readFileSync(new URL(f, dir), "utf8"))
  .join("\n");

const defined = new Set([...sources.matchAll(/(--[\w-]+)\s*:/g)].map((m) => m[1]));
// Every `var(--name …)`, with or without a fallback; a name built in a template
// literal (`--cat-${…}`) ends in "-" and is checked where it is spelled out.
const used = new Set(
  [...sources.matchAll(/var\(\s*(--[\w-]+)\s*[,)]/g)].map((m) => m[1]).filter((name) => !name.endsWith("-")),
);
const undefinedVars = [...used].filter((name) => !defined.has(name)).sort();
assert.deepEqual(undefinedVars, [], `used but never defined: ${undefinedVars.join(", ")}`);

// Colours in style.css come from the theme blocks: outside them no hex, rgb(),
// rgba() or hsl() literal.
const css = readFileSync(new URL("style.css", dir), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
// The theme blocks, by selector: dark on :root, light under data-theme.
const themeBlock = /(:root(?:\[data-theme="light"\])?)\s*\{([^}]*)\}/g;
const outside = css.replace(themeBlock, "");
const themes = new Map([...css.matchAll(themeBlock)].map((m) => [m[1], m[2]]));

// A hex colour is not followed by a name character, which tells it from an id
// selector like `#add-panel`.
const colour = /#[0-9a-fA-F]{3,8}(?![\w-])|\b(?:rgba?|hsla?)\(/g;
const literals = [...outside.matchAll(colour)].map((m) => m[0]);
assert.deepEqual(literals, [], `colour literals outside the theme blocks: ${literals.join(" ")}`);

// Every colour token on :root has a light value, so the light theme never shows
// a dark colour by omission.
const tokensIn = (text = "") => new Map([...text.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()]));
const dark = tokensIn(themes.get(":root"));
const light = tokensIn(themes.get(':root[data-theme="light"]'));
assert.ok(dark.size > 0 && light.size > 0, "the dark and light theme blocks were not found");
const colourTokens = [...dark].filter(([, v]) => v.match(colour)).map(([k]) => k);
const missing = colourTokens.filter((k) => !light.has(k)).sort();
assert.deepEqual(missing, [], `colour tokens with no light value: ${missing.join(", ")}`);

// Spacing, radius and layers come from tokens too. A value is split into its
// parts at the top level, so a calc() is one part.
const parts = (value) => value.replace(/!important/, "").trim().match(/(?:[^\s(]+|\([^()]*(?:\([^()]*\)[^()]*)*\))+/g) ?? [];
const declarations = (props) =>
  [...outside.matchAll(new RegExp(`(?<![\\w-])(${props})\\s*:\\s*([^;}]+)`, "g"))].map((m) => [m[1], m[2]]);
const offending = (props, ok) =>
  declarations(props)
    .filter(([, v]) => !parts(v).every(ok))
    .map(([p, v]) => `${p}: ${v.trim()}`);
// A viewport unit places a box on the page; it is layout, not spacing. A calc
// may also make room for controls by their size token.
const space = /^var\(--space-[0-5]\)$/;
const spaceCalc = /^calc\((?:[\s\d.*+-]|var\(--space-[0-5]\)|var\(--icon-btn-size\))*\)$/;
const spacing = offending(
  "(?:padding|margin)(?:-(?:top|right|bottom|left|inline|block)(?:-(?:start|end))?)?|(?:row-|column-)?gap",
  (p) => ["0", "1px", "-1px", "auto"].includes(p) || /^\d+v[hw]$/.test(p) || space.test(p) || spaceCalc.test(p),
);
assert.deepEqual(spacing, [], `spacing not from --space-*:\n  ${spacing.join("\n  ")}`);
const radius = offending("border(?:-(?:top|bottom)-(?:left|right))?-radius", (p) => p === "0" || p === "50%" || /^var\(--radius-(?:sm|md|lg|full)\)$/.test(p));
assert.deepEqual(radius, [], `radius not from --radius-*:\n  ${radius.join("\n  ")}`);
const layers = offending("z-index", (p) => p === "0" || /^var\(--z-[\w-]+\)$/.test(p));
assert.deepEqual(layers, [], `z-index not from --z-*:\n  ${layers.join("\n  ")}`);

console.log("css-vars-test: all assertions passed");
