// Playwright for the smoke scripts, from the install `make ui-e2e-deps` makes
// outside the checkout. The Makefile names it in PLAYWRIGHT_DIR; to run a
// script by hand, set it from `make -s playwright-dir`.
import { createRequire } from "node:module";

const dir = process.env.PLAYWRIGHT_DIR;
if (!dir) {
  throw new Error(
    "set PLAYWRIGHT_DIR=$(make -s playwright-dir), after make ui-e2e-deps installs it",
  );
}

export const { chromium } = createRequire(`${dir}/`)("playwright");
