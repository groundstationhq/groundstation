# Shortcuts for working on Ground Station. `make` lists them.

PORT ?= 14318
NPM  := npm --prefix ui

.PHONY: help ui ui-dev check build

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
