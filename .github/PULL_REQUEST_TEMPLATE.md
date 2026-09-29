## What

<!-- One or two sentences. The commit subject should follow cliff.toml: "UI: …", "CLI: …", "gsd: …", "Add …", "Fix …", "Docs: …". -->

## Why

<!-- The problem this solves, or the issue it closes. -->

## Checklist

- [ ] `cargo test --workspace`, `cargo clippy --workspace --all-targets` and `cargo fmt --all --check` pass
- [ ] Adapter changes come with a fixture in `adapters/<agent>/tests/fixtures/`
- [ ] Wire format changes update `crates/schema`, `crates/gsd/src/api.rs` and `ui/src/lib/types.ts` together
- [ ] No content attributes or tokens are logged or persisted outside the store
- [ ] Visual changes follow `STYLEGUIDE.md`
