# UI test tooling. The UI is plain ES modules with its libraries
# vendored under ui-assets/vendor, so the project has no node
# dependencies: node is only a test runner, and the two tools the
# tests need are pinned here and installed on demand into the
# gitignored node_modules, like a venv. ci.yml calls these targets so
# the pins and the recipes live in one place (its Playwright cache
# key hashes this file).

BIOME_VERSION := 2.4.14
PLAYWRIGHT_VERSION := 1.62.1
TARGET_DIR := $(or $(CARGO_TARGET_DIR),target)

# Every unit test under tools/, then the module-graph boot smoke.
UI_UNIT_TESTS := $(sort $(wildcard tools/*-test.mjs)) tools/boot-smoke.mjs

.PHONY: ui-lint ui-test ui-e2e-deps ui-e2e

# Lint + import ordering over ui-assets/ (the formatter is off in
# biome.json). `npx biome` alone resolves to an unrelated package, so
# the scope is spelled out.
ui-lint:
	npx --yes @biomejs/biome@$(BIOME_VERSION) check ui-assets

# The node-only gates: no browser, no running server.
ui-test: ui-lint
	@for t in $(UI_UNIT_TESTS); do echo "== $$t"; node $$t || exit 1; done

# Playwright + its matching Chromium build, for the browser smoke.
# `--no-save` keeps package.json out of the tree; the stamp carries
# the version, so a bumped pin or a half-done install reinstalls.
# Chromium's OS libraries are the OS's job (`npx playwright
# install-deps chromium`).
PLAYWRIGHT_STAMP := node_modules/.playwright-$(PLAYWRIGHT_VERSION)
ui-e2e-deps: $(PLAYWRIGHT_STAMP)
$(PLAYWRIGHT_STAMP):
	npm install --no-save playwright@$(PLAYWRIGHT_VERSION)
	npx playwright install chromium
	@touch $@

# The browser smoke against a scratch server on OS-chosen ports. The
# server log is printed when the boot or the smoke fails; the scratch
# state dir goes with the server.
ui-e2e: ui-e2e-deps
	cargo build --bin macrocosim
	@set -e; sd=$$(mktemp -d); mkdir "$$sd/microgrids"; \
	  cp examples/berlin-demo.lisp "$$sd/microgrids/2200.lisp"; \
	  $(TARGET_DIR)/debug/macrocosim --state-dir "$$sd" --ephemeral-ports \
	      --emit-endpoints="$$sd/endpoints.json" "$$sd/microgrids/2200.lisp" \
	      > "$$sd/server.log" 2>&1 & pid=$$!; \
	  trap 'kill $$pid 2>/dev/null || :; rm -rf "$$sd"' EXIT; \
	  for _ in $$(seq 1 30); do [ -s "$$sd/endpoints.json" ] && break; sleep 1; done; \
	  [ -s "$$sd/endpoints.json" ] || { echo "server failed to boot:"; cat "$$sd/server.log"; exit 1; }; \
	  ui=$$(sed -n 's/.*"ui":"\([^"]*\)".*/\1/p' "$$sd/endpoints.json"); \
	  MACROCOSIM_UI="http://$$ui" node tools/ui-smoke/live-topology.mjs \
	    || { echo "--- server log:"; cat "$$sd/server.log"; exit 1; }
