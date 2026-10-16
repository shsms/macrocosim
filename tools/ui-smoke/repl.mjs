// REPL smoke: history, the Emacs keys and the terminal layout, driven through
// real key presses against a running macrocosim.
// Run: MACROCOSIM_UI=http://127.0.0.1:PORT node tools/ui-smoke/repl.mjs [chromium|firefox]
// Firefox needs its own Playwright build first: `npx playwright install firefox`.
//
// The key logic itself is unit-tested in tools/repl-keys-test.mjs; this checks
// what only a browser can: that the keys reach the input, that edits stay on
// the undo stack, and where the prompt and the completion popup land.
import { chromium, firefox } from "playwright";

const BASE = process.env.MACROCOSIM_UI;
if (!BASE) throw new Error("set MACROCOSIM_UI to a running macrocosim UI, e.g. http://127.0.0.1:8801");
const which = process.argv[2] ?? "chromium";
const engine = { chromium, firefox }[which];
if (!engine) throw new Error(`unknown browser ${which}: chromium or firefox`);

const browser = await engine.launch();
const page = await browser.newPage();
let failures = 0;
const check = (name, got, want) => {
  const ok = JSON.stringify(got) === JSON.stringify(want);
  console.log(`${ok ? "PASS" : "FAIL"} repl [${which}]: ${name}${ok ? "" : ` — got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`}`);
  if (!ok) failures++;
};
page.on("pageerror", (e) => {
  console.log(`FAIL repl [${which}]: page error: ${e.message}`);
  failures++;
});

const val = () => page.$eval("#repl-input", (t) => t.value);
const caret = () => page.$eval("#repl-input", (t) => [t.selectionStart, t.selectionEnd]);
// Sets the input without an `input` event, so no completion popup opens.
const setVal = (v, c = v.length) =>
  page.$eval("#repl-input", (t, [v, c]) => { t.value = v; t.setSelectionRange(c, c); }, [v, c]);
// A full reload, then the REPL open with the focus in its input.
async function openRepl() {
  await page.goto(`${BASE}/#/microgrids/2200`);
  await page.reload();
  await page.waitForSelector("#repl-btn");
  if (!(await page.isVisible("#repl-input"))) await page.click("#repl-btn");
  await page.waitForSelector("#repl-input", { state: "visible" });
  await page.focus("#repl-input");
}
async function evalSrc(src) {
  await setVal(src);
  const n = await page.$$eval(".repl-value, .repl-error", (e) => e.length);
  await page.keyboard.press("Control+Enter");
  await page.waitForFunction((n) => document.querySelectorAll(".repl-value, .repl-error").length > n, n);
}

await page.goto(BASE);
await page.evaluate(() => localStorage.clear());
await openRepl();

// ── history ─────────────────────────────────────────────────────
await evalSrc("(+ 1 2)");
await evalSrc("(* 2 3)");
check("the input empties after an eval", await val(), "");
check("history is stored", await page.evaluate(() => JSON.parse(localStorage.getItem("mc-repl-history"))), ["(+ 1 2)", "(* 2 3)"]);
await setVal("draft");
await page.keyboard.press("ArrowUp");
check("Up brings back the newest command", await val(), "(* 2 3)");
await page.keyboard.press("ArrowUp");
check("Up again, the one before", await val(), "(+ 1 2)");
await page.keyboard.press("ArrowDown");
check("Down, the newer one", await val(), "(* 2 3)");
await page.keyboard.press("ArrowDown");
check("Down past the newest brings back the draft", await val(), "draft");
await setVal("(a\n b)");
await page.keyboard.press("ArrowUp");
check("Up on a second line moves within the input", [await val(), (await caret())[0] <= 2], ["(a\n b)", true]);
await page.keyboard.press("ArrowUp");
check("Up on the first line brings back a command", await val(), "(* 2 3)");
await page.keyboard.press("ArrowDown");
check("Down brings back the multi-line draft", await val(), "(a\n b)");
await openRepl();
await page.keyboard.press("ArrowUp");
check("history survives a reload", await val(), "(* 2 3)");
await evalSrc("(list 1 2)");
await page.locator(".repl-prompt").last().click();
check("a click on a command in the output puts it back", await val(), "(list 1 2)");

// ── highlighting ────────────────────────────────────────────────
await evalSrc('(list 1 "a")');
const lastPrompt = page.locator(".repl-prompt").last();
check("a command in the output is highlighted", (await lastPrompt.locator(".paren-0, .repl-function-head").count()) > 0, true);
check("and reads as typed", await lastPrompt.textContent(), '▸ (list 1 "a")');
check("a value is highlighted", (await page.locator(".repl-value").last().locator(".paren-0, .repl-string").count()) > 0, true);
await evalSrc("(no-such-fn)");
check("an error is not", await page.locator(".repl-error").last().locator("span").count(), 0);

// ── Emacs keys ──────────────────────────────────────────────────
check("Emacs keys are off by default", await page.isChecked("#repl-emacs"), false);
await page.focus("#repl-input");
await setVal("(make-meter :id 1)");
await page.keyboard.press("Control+a");
check("with them off, C-a selects all", await caret(), [0, 18]);
await page.click("#repl-emacs");
check("the head checkbox turns them on", await page.isChecked("#repl-emacs"), true);
check("and gives the focus back to the input", await page.evaluate(() => document.activeElement.id), "repl-input");
await openRepl();
check("the browser remembers them", await page.isChecked("#repl-emacs"), true);
await page.focus("#repl-input");
await setVal("(make-meter :id 1)");
await page.keyboard.press("Control+a");
check("C-a", await caret(), [0, 0]);
await page.keyboard.press("Alt+f");
check("M-f moves a word", await caret(), [5, 5]);
check("M-f types nothing", await val(), "(make-meter :id 1)");
await page.keyboard.press("Control+f");
check("C-f", await caret(), [6, 6]);
await page.keyboard.press("Control+e");
check("C-e", await caret(), [18, 18]);
await page.keyboard.press("Alt+b");
check("M-b", await caret(), [16, 16]);
await page.keyboard.press("Control+b");
check("C-b", await caret(), [15, 15]);
await page.keyboard.press("Control+a");
await page.keyboard.press("Alt+f");
await page.keyboard.press("Control+k");
check("C-k", await val(), "(make");
await page.keyboard.press("Control+y");
check("C-y", await val(), "(make-meter :id 1)");
await page.keyboard.press("Control+a");
await page.keyboard.press("Alt+f");
await page.keyboard.press("Control+k");
await page.keyboard.press("Control+z");
check("C-z undoes a kill", await val(), "(make-meter :id 1)");
await page.keyboard.press("Control+a");
await page.keyboard.press("Control+d");
check("C-d", await val(), "make-meter :id 1)");
await page.keyboard.press("Alt+d");
check("M-d", await val(), "-meter :id 1)");
await page.keyboard.press("Control+e");
await page.keyboard.press("Alt+Backspace");
check("M-DEL", await val(), "-meter :id ");
await page.keyboard.press("Alt+Shift+Comma");
check("M-<", await caret(), [0, 0]);
await page.keyboard.press("Alt+Shift+Period");
check("M->", (await caret())[0], (await val()).length);
await setVal("(a\n b)", 1);
await page.keyboard.press("Alt+p");
check("M-p brings back a command from any line", await val(), "(no-such-fn)");
await page.keyboard.press("Alt+n");
check("M-n goes back to the draft", await val(), "(a\n b)");
await setVal("(a\n b)", 5);
await page.keyboard.press("Control+p");
check("C-p moves up a line", [await val(), (await caret())[0]], ["(a\n b)", 2]);
await page.keyboard.press("Control+p");
check("C-p on the first line brings back a command", await val(), "(no-such-fn)");
await page.keyboard.press("Control+n");
check("C-n goes back", await val(), "(a\n b)");
check("no completion popup came up", await page.$eval("#repl-popup", (u) => u.hidden), true);

// ── the terminal layout ─────────────────────────────────────────
await page.evaluate(() => localStorage.removeItem("mc-repl-emacs"));
await openRepl();
const layout = () =>
  page.evaluate(() => {
    const r = (el) => el.getBoundingClientRect();
    const out = document.getElementById("repl-output");
    const inp = document.getElementById("repl-input");
    const entries = out.querySelectorAll(".repl-entry");
    const pop = document.getElementById("repl-popup");
    return {
      inOutput: out.contains(inp),
      afterLast: entries.length > 0 && r(inp).top >= r(entries[entries.length - 1]).bottom - 1,
      evalButton: document.querySelector('#repl button[type="submit"]') !== null,
      toolsInHead: document.querySelector("#repl .panel-drag #repl-mg-chip") !== null && document.querySelector("#repl .panel-drag #repl-emacs") !== null,
      inputH: r(inp).height,
      popup: pop.hidden
        ? null
        : {
            below: r(pop).top >= r(inp).bottom - 1,
            above: r(pop).bottom <= r(inp).top + 1,
            inOutput: r(pop).top >= r(out).top - 1 && r(pop).bottom <= r(out).bottom + 1,
          },
    };
  });
await evalSrc("(+ 1 2)");
let at = await layout();
check("the prompt is in the output", at.inOutput, true);
check("after the last entry", at.afterLast, true);
check("there is no Eval button", at.evalButton, false);
check("the microgrid chip and the Emacs checkbox are in the head", at.toolsInHead, true);
// The head drags the panel; a press on one of its controls is the control's.
await page.click("#repl-emacs");
check("the head checkbox takes a click", await page.isChecked("#repl-emacs"), true);
await page.click("#repl-emacs");
check("and another", await page.isChecked("#repl-emacs"), false);
await page.focus("#repl-input");
const oneLine = at.inputH;
await setVal("(a\n b\n c)");
await page.$eval("#repl-input", (t) => t.dispatchEvent(new Event("input")));
at = await layout();
check("the input grows to its three lines", at.inputH > oneLine * 1.8, true);
await setVal("");
await page.$eval("#repl-input", (t) => t.dispatchEvent(new Event("input")));
// The popup takes the side of the prompt with more room: below while the output
// is short, above once the prompt has reached the bottom.
await page.keyboard.type("(make-b");
await page.waitForFunction(() => !document.getElementById("repl-popup").hidden);
at = await layout();
check("in a short session the popup opens below the prompt", at.popup?.below, true);
check("within the output", at.popup?.inOutput, true);
await page.keyboard.press("Escape");
for (let i = 0; i < 12; i++) await evalSrc(`(+ ${i} 1)`);
await page.focus("#repl-input");
await page.keyboard.type("(make-b");
await page.waitForFunction(() => !document.getElementById("repl-popup").hidden);
at = await layout();
check("in a long session it opens above", at.popup?.above, true);
check("within the output, again", at.popup?.inOutput, true);
await page.keyboard.press("Escape");
await setVal("");
await page.evaluate(() => document.activeElement.blur());
await page.locator(".repl-value").first().click();
check("a click in the output moves to the prompt", await page.evaluate(() => document.activeElement.id), "repl-input");

// ── a floating REPL keeps the height it is given ────────────────
// Other cards hug their content and take a drag as a cap; a terminal keeps
// its size, its transcript filling it however short it is.
await page.setViewportSize({ width: 1280, height: 900 });
await page.evaluate(() => {
  localStorage.setItem("mc-panel-dock-repl-btn", JSON.stringify({ mode: "float" }));
  localStorage.setItem("mc-panel-size-repl-btn", JSON.stringify({ h: 560 }));
});
await openRepl();
await page.waitForTimeout(500);
const card = () => page.evaluate(() => {
  const el = document.getElementById("repl");
  const r = el.getBoundingClientRect();
  return { floating: !el.classList.contains("docked"), h: Math.round(r.height), right: r.right, bottom: r.bottom };
});
let held = await card();
check("the REPL floats", held.floating, true);
check("a floating REPL is as tall as its stored height, with a short transcript", Math.abs(held.h - 560) <= 2, true);
await page.mouse.move(held.right - 4, held.bottom - 4);
await page.mouse.down();
await page.mouse.move(held.right - 4, held.bottom - 104, { steps: 5 });
await page.mouse.up();
await page.waitForTimeout(300);
held = await card();
check("a gripper drag sets the height it is let go at", Math.abs(held.h - 460) <= 4, true);
await page.evaluate(() => {
  localStorage.removeItem("mc-panel-dock-repl-btn");
  localStorage.removeItem("mc-panel-size-repl-btn");
});

await browser.close();
console.log(failures ? `repl smoke: ${failures} FAILED` : "repl smoke: all passed");
process.exit(failures ? 1 : 0);
