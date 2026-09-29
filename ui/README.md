# Ground Station UI

The trajectory viewer. Talks to a local `gsd` over its HTTP API (`/v1/health`, `/v1/trajectories`, `/v1/trajectories/{id}`) and falls back to built-in demo data when no daemon answers, with a visible notice.

```sh
npm install
npm run dev        # http://localhost:5180, proxies /v1 to http://127.0.0.1:4318 (override with GSD_URL)
npm run build      # dist/, to be embedded and served by gsd
npm run typecheck
```

Screens

- `#/` trajectories: overview strip and the list, refreshed every 5s
- `#/t/:id` trajectory: minimap of the whole run, where the time went, longest tool calls, every event with an inspector, refreshed every 3s while running

Conventions are in [`../STYLEGUIDE.md`](../STYLEGUIDE.md). Wire types in `src/lib/types.ts` mirror `crates/schema` and `crates/gsd/src/api.rs`; change both sides in the same PR.
