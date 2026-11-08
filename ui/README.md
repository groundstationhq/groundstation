# Ground Station UI

The trajectory viewer. A pure client of the `/v1` API on the origin that served it: `gsd` on your machine, or the hosted api. A 401 shows a sign-in screen; any other failure of `/v1/health` shows an error screen.

```sh
npm install
npm run dev        # http://localhost:5180, proxies /v1 and /auth to http://127.0.0.1:4318 (override with GSD_URL)
npm run build      # dist/, embedded and served by gsd and the hosted api
npm run typecheck
```

Screens

- `#/` overview: live totals, active agents, token composition and recent runs; findings on the hosted api. The version comparison, and the findings panel on gsd, are mocked and marked "preview · v2"
- `#/trajectories` the full list, refreshed every 5s
- `#/t/:id` trajectory: minimap of the whole run, where the time went, longest tool calls, every event with an inspector, refreshed every 3s while running
- `#/findings` and `#/tokens` when `/v1/health` lists `insights` and `tokens` in `features` (the hosted api)

Conventions are in [`../STYLEGUIDE.md`](../STYLEGUIDE.md). Wire types in `src/lib/types.ts` mirror `crates/schema` and `crates/wire`; change both sides in the same PR. `crates/wire/tests/ui_mirror.rs` checks that they match.
