// The stylesheet's custom properties and colours: every variable the UI
// reads is defined somewhere (an undefined one is not an error to the
// browser, it silently drops the declaration or takes the fallback for
// ever), and every colour in style.css is a token from the theme blocks.
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

console.log("css-vars-test: all assertions passed");
