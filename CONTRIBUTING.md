# Contributing to Ground Station

Thanks for taking a look. Ground Station is pre-alpha, so the most useful contributions right now are bug reports with a reproducible trajectory, adapter fixes when an agent changes its payloads, and small focused pull requests.

Read [`AGENTS.md`](./AGENTS.md) first: it holds the rules that keep the product's shape (the UI never reaches past the HTTP API, redaction can't be bypassed, event ids are stable). [`STYLEGUIDE.md`](./STYLEGUIDE.md) covers anything visual, number formatting, copy voice and code conventions.

## Setup

Rust 1.88 or newer and, for the UI, Node 24.

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets   # warnings fail CI
cargo fmt --all --check

cd ui && npm ci && npm run typecheck && npm run build
```

To try a change end to end without touching your real install:

```sh
export GROUNDSTATION_CONFIG_DIR=/tmp/gs-config GROUNDSTATION_DATA_DIR=/tmp/gs-data
cargo run -p groundstation -- connect claude-code --no-start
RUST_LOG=gsd=debug cargo run -p gsd
```

## Pull requests

- One change per PR. Keep refactors separate from behaviour changes.
- Add or update a test. Adapter changes need a fixture in `adapters/<agent>/tests/fixtures/` showing the real payload.
- If you change the wire format, change `crates/schema`, `crates/gsd/src/api.rs` and `ui/src/lib/types.ts` in the same PR.
- Never log or persist content attributes outside the store, and never log tokens.
- Commit subjects are turned into the changelog by `cliff.toml`, so write them as `UI: …`, `CLI: …`, `gsd: …`, `Add …`, `Fix …` or `Docs: …`. Don't edit `CHANGELOG.md` by hand.

CI runs fmt, clippy, the test suite and the UI build on Linux and macOS. Green CI and one maintainer review are all it takes to merge.

## Adding an agent

Add an `adapters/<agent>` crate implementing the `Adapter` trait from `crates/schema/src/adapter.rs` (pure translation, no I/O), register it in `builtin_adapters()` in `crates/gsd/src/ingest.rs`, and add it to the CLI's `connect` targets in `crates/groundstation/src/main.rs`. `docs/gsd.md` explains how the existing four work.

## Reporting bugs

Open an issue with the output of `groundstation status`, `groundstation config`, the agent and its version, and if possible `groundstation show <id> --json` for the affected trajectory. Redact anything you'd rather not share; the daemon already strips known secret formats, but prompts and paths are yours.

Security problems go through [`SECURITY.md`](./SECURITY.md), not the issue tracker.

## License

By contributing you agree that your contributions are licensed under the Apache License 2.0, like the rest of the project.
