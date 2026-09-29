# Shortcuts for working on Ground Station. `make` lists them.

PORT ?= 14318
NPM  := npm --prefix ui

.PHONY: help dev ui ui-dev check build

help: ## Show this list
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F ':.*## ' '{ printf "  make %-8s %s\n", $$1, $$2 }'

ui: ## Build the UI, embed it in a debug gsd and serve it at http://127.0.0.1:$(PORT)/ over your real data
	$(NPM) install --no-audit --no-fund
	$(NPM) run build
	cargo build -p gsd
	@echo
	@echo "  UI: http://127.0.0.1:$(PORT)/   (Ctrl-C stops it; the installed gsd on 4318 keeps running)"
	@echo
	@(sleep 1.5; open "http://127.0.0.1:$(PORT)/" 2>/dev/null || xdg-open "http://127.0.0.1:$(PORT)/" 2>/dev/null || true) &
	./target/debug/gsd --listen 127.0.0.1:$(PORT)

dev: ## Build both binaries, swap the dev gsd in on 4318 and run the Vite dev server; Ctrl-C brings the installed daemon back
	cargo build -p groundstation -p gsd
	$(NPM) install --no-audit --no-fund
	@bash -c ' \
	  ./target/debug/groundstation daemon stop >/dev/null 2>&1 || true; \
	  ./target/debug/gsd > target/gsd-dev.log 2>&1 & GSD=$$!; \
	  restore() { kill $$GSD 2>/dev/null; wait $$GSD 2>/dev/null; echo; groundstation daemon start 2>/dev/null || echo "start your daemon again with: groundstation daemon start"; }; \
	  trap restore EXIT; \
	  sleep 1; echo; echo "  dev gsd on 127.0.0.1:4318 (log: target/gsd-dev.log)"; echo "  UI: http://localhost:5180/"; echo; \
	  $(NPM) run dev || true'

ui-dev: ## Vite dev server with hot reload at http://localhost:5180/, proxied to the gsd on 4318
	$(NPM) install --no-audit --no-fund
	$(NPM) run dev

check: ## What CI runs: fmt, clippy, tests, UI typecheck and build
	cargo fmt --all --check
	cargo clippy --workspace --all-targets
	cargo test --workspace
	$(NPM) run typecheck
	$(NPM) run build

build: ## Release build of groundstation and gsd with the UI embedded
	$(NPM) install --no-audit --no-fund
	$(NPM) run build
	cargo build --release -p groundstation -p gsd
