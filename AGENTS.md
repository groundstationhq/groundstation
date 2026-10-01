# Working in this repository

Read this before changing anything. It applies to people and agents alike.

## The one rule that protects the product's shape

**The UI never reaches past the HTTP API.** Everything the UI in `ui/` needs comes from `/v1/*` on the origin that served it. It must not read SQLite, the daemon's files, or anything else directly, and no endpoint may exist only to leak internals. If a feature is hard to express through the API, add its types to `crates/api` and its route to `crates/gsd/src/server.rs`; don't add a back door.

Why: the same built UI (`ui/dist`) is served by `gsd` on `127.0.0.1:4318` today and will be served by the hosted backend later. The API contract (`Health`, `TrajectorySummary`, `TrajectoryDetail`, the event schema) is the seam between them. Keep the UI a pure client of that contract and the SaaS version is a change of host, not a rewrite.

Corollaries:

- Keep API paths identical for both hosts. Extra endpoints that only one host offers must be feature-detected from `/v1/health` (as `adapters`, `upload.mode` and `ui` already are), never assumed.
- Authentication stays in one `fetch` wrapper (`ui/src/lib/api.ts`). Local sends nothing; hosted adds a token or relies on a cookie.
- Wire types in `ui/src/lib/types.ts` mirror `crates/schema` and `crates/api` exactly. Change both sides in the same commit.

## Workflow

Every change is a branch and a pull request into `main`, squash-merged once CI is green. Never push to `main` directly.

- Branch names carry a type prefix and say what the change is: `feat/opencode-cost-rollup`, `fix/codex-apply-patch-paths`, `chore/pin-actions`, `docs/config-reference`.
- The PR title becomes the squashed commit subject, and `cliff.toml` turns commit subjects into `CHANGELOG.md`. So the title needs the same prefix and must describe the change for a user reading the changelog: `feat: roll up OpenCode subagent cost`, not `feat: fixes`. `feat:`/`Add` land under Added, `fix:` under Fixed, `UI:`, `CLI:` and `gsd:` under their area, `docs:` under Documentation, anything else under Changed.
- Never edit `CHANGELOG.md` by hand. Releases are cut with the *Prepare release* workflow, which regenerates it.

## Other rules

- `STYLEGUIDE.md` governs anything visual, every number format, copy voice, and code conventions for both TypeScript and Rust. Follow it without being asked.
- Never log or persist content attributes (`gs.prompt.text`, tool bodies, `gs.shell.command`) outside the store, and never log tokens. Redaction runs in `gsd` before storage; nothing may bypass it.
- Event ids must be stable across redelivery. Derive them from the source; never mint one at ingest.
- Attribute keys come from `groundstation_schema::attr`; don't write the string twice.
- `cargo build` must keep working without Node. The UI is embedded by `crates/gsd/build.rs` when `ui/dist` exists and replaced by a placeholder page when it doesn't.

## Map

| Path | |
|:--|:--|
| `crates/schema` | `groundstation.telemetry.v0`: event types, attribute names, the `Adapter` trait |
| `crates/api` | wire types of the HTTP API, shared by `gsd`, the CLI and the hosted backend |
| `crates/gsd` | the daemon: ingest, privacy, store, HTTP API, embedded UI |
| `crates/groundstation` | the CLI |
| `adapters/*` | one crate per agent, pure translation, no I/O |
| `ui/` | the viewer; Vite + React + TypeScript |
| `docs/gsd.md` | daemon and CLI internals; user docs live in the `landing_page` repo under `docs/` |
| `product.md` | the product brief and roadmap (kept locally, not in git) |
