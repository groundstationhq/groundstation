# Shortcuts for working on Ground Station. `make` lists them.

PORT ?= 14318
NPM  := npm --prefix ui

.PHONY: help dev ui ui-dev check build deps deps-down stack ingest api worker backend-test edge-test

help: ## Show this list
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F ':.*## ' '{ printf "  make %-13s %s\n", $$1, $$2 }'

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

check: ## CI's check and ui jobs: the UI built first (and required), then fmt, clippy, tests, UI typecheck
	$(NPM) ci --no-audit --no-fund
	$(NPM) run build
	cargo fmt --all --check
	GS_UI_REQUIRED=1 cargo clippy --workspace --all-targets
	GS_UI_REQUIRED=1 cargo test --workspace
	$(NPM) run typecheck
	@echo "CI also runs make backend-test and make edge-test."

build: ## Release build of groundstation and gsd with the UI embedded
	$(NPM) install --no-audit --no-fund
	$(NPM) run build
	cargo build --release -p groundstation -p gsd

# The backend services run on your machine against the containers from
# `make deps`; open the edge at http://127.0.0.1:8080.
DEV_PUBLIC_URL := http://127.0.0.1:8080
DEV_SIGN_IN := GROUNDSTATION_PUBLIC_URL=$(DEV_PUBLIC_URL) \
	GROUNDSTATION_ADMIN_PASSWORD="local development" \
	GROUNDSTATION_OIDC_ISSUER=http://127.0.0.1:5556/dex \
	GROUNDSTATION_OIDC_CLIENT_ID=groundstation \
	GROUNDSTATION_OIDC_CLIENT_SECRET=groundstation-dev-secret \
	GROUNDSTATION_OIDC_NAME=Dex

deps: ## Start ClickHouse, Postgres, Dex and the edge (http://127.0.0.1:8080) in Docker
	docker compose --profile dev up -d --wait

deps-down: ## Stop the backend's containers; their data stays in Docker volumes
	docker compose --profile dev down

stack: deps ## Start the whole backend at once: the containers, then api, ingest and worker together; Ctrl-C stops the three
	$(NPM) install --no-audit --no-fund
	$(NPM) run build
	cargo build -p groundstation-api -p groundstation-ingest -p groundstation-worker
	@bash -c ' \
	  label() { awk -v l="$$1" "{ print l \$$0; fflush() }"; }; \
	  $(DEV_SIGN_IN) ./target/debug/groundstation-api > >(label "api    | ") 2>&1 & API=$$!; \
	  ./target/debug/groundstation-ingest > >(label "ingest | ") 2>&1 & INGEST=$$!; \
	  ./target/debug/groundstation-worker --interval 30 > >(label "worker | ") 2>&1 & WORKER=$$!; \
	  stop() { kill $$API $$INGEST $$WORKER 2>/dev/null; wait 2>/dev/null; }; \
	  trap stop EXIT; trap "echo; exit 0" INT TERM; \
	  echo; echo "  Ground Station: $(DEV_PUBLIC_URL)/   (sign in with \"local development\" or Dex)"; \
	  echo "  Ctrl-C stops api, ingest and worker; make deps-down stops the containers"; echo; \
	  while kill -0 $$API 2>/dev/null && kill -0 $$INGEST 2>/dev/null && kill -0 $$WORKER 2>/dev/null; do sleep 1; done; \
	  echo "one of the services stopped; stopping the others"; exit 1'

api: ## Build the UI and run the api behind the edge; sign in with "local development" or Dex
	$(NPM) install --no-audit --no-fund
	$(NPM) run build
	$(DEV_SIGN_IN) cargo run -p groundstation-api

ingest: ## Run the ingest service; point gsd at it with `groundstation cloud login --endpoint http://127.0.0.1:8080`
	cargo run -p groundstation-ingest

worker: ## Run the insights worker, passing over every tenant every 30s
	cargo run -p groundstation-worker -- --interval 30

edge-test: ## The edge's routing and limits, against stand-ins for api and ingest (needs Docker)
	docker/caddy/test.sh

backend-test: ## The backend's tests, including those that need `make deps`
	cargo test -p groundstation-backend -p groundstation-ingest -p groundstation-api -p groundstation-worker -- --include-ignored
