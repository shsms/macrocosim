// Screenshots of the main views in both themes, for looking at, not asserting.
// Run: MACROCOSIM_UI=http://127.0.0.1:PORT node tools/ui-smoke/shots.mjs
// OUT_DIR
import { chromium } from "playwright";

const BASE = process.env.MACROCOSIM_UI;
const out = process.argv[2];
if (!BASE || !out) throw new Error("usage: MACROCOSIM_UI=… node tools/ui-smoke/shots.mjs OUT_DIR");

const browser = await chromium.launch({ args: ["--no-sandbox"] });
for (const scheme of ["dark", "light"]) {
  for (const density of ["compact", "comfortable"]) {
    const ctx = await browser.newContext({ viewport: { width: 1600, height: 950 }, locale: "en-US", colorScheme: scheme });
    const page = await ctx.newPage();
    await page.addInitScript((d) => localStorage.setItem("macrocosim-density", d), density);
    await page.goto(`${BASE}/#microgrids/2200/topology`, { waitUntil: "networkidle" });
    await page.waitForTimeout(3000);
    await page.screenshot({ path: `${out}/topology-${scheme}-${density}.png` });
    await page.click("#metrics-btn");
    await page.waitForTimeout(3000);
    await page.screenshot({ path: `${out}/metrics-${scheme}-${density}.png` });
    await page.click("#metrics-btn");
    await page.click("#weather-btn");
    await page.waitForTimeout(2000);
    await page.screenshot({ path: `${out}/weather-${scheme}-${density}.png` });
    await page.click("#weather-btn");
    const battery = await page.evaluate(async () => (await import("/assets/topology.js")).topology.debugNodeScreenRect(1000));
    await page.mouse.click(battery.x + battery.width / 2, battery.y + battery.height / 2);
    await page.waitForTimeout(2000);
    await page.screenshot({ path: `${out}/inspector-${scheme}-${density}.png` });
    await page.keyboard.press("Escape");
    await page.keyboard.press("`");
    await page.waitForTimeout(500);
    await page.fill("#repl-input", '(let ((x 1)) (message "hi %d" (+ x 2))) ; note');
    await page.screenshot({ path: `${out}/repl-${scheme}-${density}.png` });
    await ctx.close();
  }
}
await browser.close();
