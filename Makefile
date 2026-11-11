# UI test tooling. The UI is plain ES modules with its libraries
# vendored under ui-assets/vendor, so the project has no node
# dependencies: node is only a test runner, and the two tools the
# tests need are pinned here and installed on demand outside the
# checkout (biome in npx's cache, Playwright in PLAYWRIGHT_DIR). ci.yml
# calls these targets so the pins and the recipes live in one place
# (its Playwright cache key is the version `make -s playwright-version`
# prints).

BIOME_VERSION := 2.4.14
PLAYWRIGHT_VERSION := 1.62.1
# One install per version, shared by every checkout. The smoke scripts load
# Playwright from it (tools/ui-smoke/playwright.mjs).
PLAYWRIGHT_DIR ?= $(or $(XDG_CACHE_HOME),$(HOME)/.cache)/macrocosim/playwright-$(PLAYWRIGHT_VERSION)
# Absolute, as the smoke scripts' loader needs. It must hold no spaces: make
# splits on them.
override PLAYWRIGHT_DIR := $(abspath $(PLAYWRIGHT_DIR))
export PLAYWRIGHT_DIR
TARGET_DIR := $(or $(CARGO_TARGET_DIR),target)

# Every unit test under tools/, then the module-graph boot smoke.
UI_UNIT_TESTS := $(sort $(wildcard tools/*-test.mjs)) tools/boot-smoke.mjs

.PHONY: ui-lint ui-test ui-e2e-deps ui-e2e-os-deps ui-e2e playwright-dir playwright-version

# Lint + import ordering over ui-assets/ (the formatter is off in
# biome.json). `npx biome` alone resolves to an unrelated package, so
# the scope is spelled out.
ui-lint:
	npx --yes @biomejs/biome@$(BIOME_VERSION) check ui-assets

# The node-only gates: no browser, no running server.
ui-test: ui-lint
	@for t in $(UI_UNIT_TESTS); do echo "== $$t"; node $$t || exit 1; done

# Playwright + its matching Chromium build, for the browser smoke. The
# package goes into PLAYWRIGHT_DIR, with no package.json; Chromium into
# Playwright's own per-OS cache (on Linux, ms-playwright in $XDG_CACHE_HOME,
# else ~/.cache).
# The stamp is inside PLAYWRIGHT_DIR, so a bumped pin or a half-done
# install reinstalls.
PLAYWRIGHT_CLI := $(PLAYWRIGHT_DIR)/node_modules/.bin/playwright
PLAYWRIGHT_STAMP := $(PLAYWRIGHT_DIR)/.installed
ui-e2e-deps: $(PLAYWRIGHT_STAMP)
$(PLAYWRIGHT_STAMP):
	mkdir -p "$(PLAYWRIGHT_DIR)"
	npm install --prefix "$(PLAYWRIGHT_DIR)" --no-save --no-package-lock \
	    playwright@$(PLAYWRIGHT_VERSION)
	"$(PLAYWRIGHT_CLI)" install chromium
	@touch $@

# Chromium's OS libraries, as CI installs them: through apt-get, so on Debian or
# Ubuntu only, as root (Playwright runs it with sudo, or su without sudo). On
# other systems, install them with the system's package manager.
ui-e2e-os-deps: ui-e2e-deps
	"$(PLAYWRIGHT_CLI)" install-deps chromium

# Where Playwright is installed, for running a smoke script by hand:
# PLAYWRIGHT_DIR=$(make -s playwright-dir) node tools/ui-smoke/shots.mjs OUT
playwright-dir:
	@echo "$(PLAYWRIGHT_DIR)"

# The pinned Playwright version, for CI's cache key.
playwright-version:
	@echo "$(PLAYWRIGHT_VERSION)"

# The browser smokes against a scratch server on OS-chosen ports, with
# tools/ui-smoke/site.py running against it on the venv `uv sync`
# makes in python/. The server's and the script's logs are printed when
# the boot or a smoke fails; the scratch state dir goes with the server.
ui-e2e: ui-e2e-deps
	cargo build --bin macrocosim --bin macroctl
	@set -e; sd=$$(mktemp -d); \
	  $(TARGET_DIR)/debug/macrocosim --state-dir "$$sd" --ephemeral-ports \
	      --emit-endpoints="$$sd/endpoints.json" --python python/.venv/bin/python \
	      > "$$sd/server.log" 2>&1 & pid=$$!; \
	  trap 'kill $$pid $${script:-} 2>/dev/null || :; rm -rf "$$sd"' EXIT; \
	  logs() { echo "--- server log:"; cat "$$sd/server.log"; \
	    echo "--- script log:"; cat "$$sd/script.log" 2>/dev/null || :; }; \
	  for _ in $$(seq 1 30); do [ -s "$$sd/endpoints.json" ] && break; sleep 1; done; \
	  [ -s "$$sd/endpoints.json" ] || { echo "server failed to boot:"; logs; exit 1; }; \
	  ui=$$(sed -n 's/.*"ui":"\([^"]*\)".*/\1/p' "$$sd/endpoints.json"); \
	  MACROCOSIM_URL="http://$$ui" python/.venv/bin/python -u tools/ui-smoke/site.py \
	      > "$$sd/script.log" 2>&1 & script=$$!; \
	  declared=; \
	  for _ in $$(seq 1 30); do \
	    curl -s "http://$$ui/api/microgrids" | grep -q '"id":2200' && { declared=1; break; }; \
	    kill -0 $$script 2>/dev/null || { echo "the smoke site exited:"; logs; exit 1; }; \
	    sleep 1; \
	  done; \
	  [ -n "$$declared" ] || { echo "the smoke site did not declare 2200:"; logs; exit 1; }; \
	  MACROCOSIM_UI="http://$$ui" MACROCTL=$(TARGET_DIR)/debug/macroctl node tools/ui-smoke/live-topology.mjs \
	    || { logs; exit 1; }
