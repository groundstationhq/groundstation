# Ground Station cloud architecture

| | |
|:--|:--|
| Status | Draft. §11 is implemented except the insights view and `groundstation cloud login`. |
| Date | 2026-09-29, revised 2026-09-30 |
| Author | akiokio |
| Scope | The backend: ingest, storage, insights and the hosted `/v1` API. We run it as the hosted service, and anyone can self-host it. Changes this requires in `gsd`, `crates/schema` and `ui/`. |

## 1. Summary

`gsd` already knows how to upload. With `transport.mode = "cloud"` it POSTs gzip-compressed `groundstation.telemetry.v0` batches to `{endpoint}/v1/events` with a bearer token, and its SQLite store is the queue. This document designs what sits on the other end of that request.

Four services and two databases:

- **ingest** accepts batches from `gsd`, resolves the token to a tenant, and writes events to ClickHouse.
- **ClickHouse** holds events. It answers every trajectory and statistics query.
- **insights worker** runs detectors over each tenant's recent events and writes findings to Postgres.
- **Postgres** holds tenants, users, ingest tokens, and insights.
- **api** serves the built UI (`ui/dist`) and the same `/v1/*` read contract `gsd` serves locally, plus hosted-only endpoints for insights.
- **edge** puts everything under one origin, so the UI stays a pure client of relative `/v1/*` paths.

The design keeps one promise from `AGENTS.md`: the hosted UI is the same build as the local one, talking to the same API contract. The SaaS version is a change of host, not a rewrite.

The backend is open source and lives in this repository. We run it as the hosted service, and anyone can run the same code on their own infrastructure (§16). The core is open; a small set of enterprise features lives in `ee/` under a commercial license, and the hosted service and those features are how Ground Station makes money (§17).

## 2. Context

What exists today, and constrains this design:

- **Upload path.** `crates/gsd/src/uploader.rs` reads up to `transport.batch_size` (default 500) pending events in `seq` order, gzips a JSON `Batch`, and POSTs it. On 2xx it marks the accepted events uploaded. Events the backend refuses go back in the queue and are dropped from it after 3 refusals (§6.1). Other failures back off exponentially to 5 minutes and retry the same batch. On shutdown it makes one last attempt.
- **Redaction happens before upload.** Secrets, env values, excluded content fields and hashed paths are handled in `gsd` before anything is stored, so the cloud only ever receives what the user's `[redaction]` config allows. Raw agent payloads are never uploaded; only normalized `Event`s are.
- **Event ids are stable.** They are derived from the hook invocation or the transcript message id. The same event is redelivered with the same id.
- **Some events change.** `model.completed` events are re-read from growing transcripts. When their attributes change, `gsd` updates the row and resets `uploaded = 0`, so the same event id is uploaded again with newer content. Every other kind is write-once.
- **Git checkout is on every event.** Each event records the commit and branch checked out when it happened (`gs.vcs.revision`, `gs.vcs.branch`), so a long trajectory that spans commits records each one. The repository doesn't change within a trajectory: it lives on the local `trajectories` table, and the uploader adds its name to each event it sends (§11.1). The hostname never leaves the machine.
- **The UI.** `ui/src/lib/api.ts` fetches relative paths (`BASE = ""`). A 401 from any `/v1` call shows a sign-in screen linking to `/auth/login`; any other failure of `/v1/health` shows an error screen. There is no demo fallback.

## 3. Goals and non-goals

Goals:

1. Accept uploads from any number of `gsd` installs without losing or double-counting events.
2. Serve the exact local `/v1` read contract, so `ui/dist` runs unmodified against the hosted API.
3. Produce findings and recommendations from a tenant's events, each linked to the trajectories and events that support it.
4. Multi-tenant from day one. Isolation is enforced by the server, never trusted from the request.
5. Never let a cloud outage or a bad batch slow down an agent or silently drop data on the machine.
6. Self-hostable with the same code we run. One `docker compose up` gives a working backend, and nothing requires a managed service (§16).

Non-goals, for now:

- Streaming or live tail from the cloud. The UI polls, as it does locally.
- Accepting raw hook payloads in the cloud. Translation stays in `gsd` and the adapters.
- LLM-generated insights. They come later, opt-in per tenant (§9.4).

## 4. Architecture

```text
 DEVELOPER MACHINE          ┆  CLOUD  (one origin, e.g. app.groundstation.sh)
                            ┆
 agent hooks / plugins      ┆
         │                  ┆
         ▼                  ┆   POST /v1/events
 ┌──────────────────┐       ┆   gzip JSON batch, bearer     ┌──────────┐   async insert   ┌──────────────┐
 │ gsd              │───────┆──────────────────────────────►│ ingest   │─────────────────►│ ClickHouse   │
 │ redact → SQLite  │       ┆                               └────┬─────┘                  │ events       │
 │ (upload queue)   │       ┆                                    │ token → tenant         │ traj_index   │
 └──────────────────┘       ┆                                    ▼                        └───┬──────▲───┘
                            ┆                               ┌──────────┐    findings    ┌─────▼────┐ │
                            ┆                               │ Postgres │◄───────────────│ insights │ │
                            ┆                               │ tenants  │                │ worker   │ │
                            ┆                               │ tokens   │                └──────────┘ │
                            ┆                               │ insights │                             │
                            ┆                               └────▲─────┘                             │
                            ┆                                    │ tenants, insights                 │
 ┌──────────────────┐       ┆   GET /, /v1/*, /auth/*       ┌────┴─────┐   trajectories, stats       │
 │ browser          │───────┆──────────────────────────────►│ api      │─────────────────────────────┘
 │ ui/dist          │       ┆   session cookie              │ + ui/dist│
 └──────────────────┘       ┆                               └──────────┘
```

| Component | Kind | Owns | Reads | Writes |
|:--|:--|:--|:--|:--|
| edge | load balancer / reverse proxy | TLS, routing by path, request size limits | | |
| ingest | stateless service | `POST /v1/events` | Postgres (tokens, cached) | ClickHouse `events` |
| api | stateless service | `GET /`, `/v1/*` reads, `/v1/insights*`, `/auth/*` | ClickHouse, Postgres | Postgres (sessions, insight state) |
| insights worker | scheduled job | detectors | ClickHouse | Postgres `insights` |
| ClickHouse | managed database | events, trajectory index | | |
| Postgres | managed database | tenants, users, tokens, sessions, insights | | |

Routing at the edge: `POST /v1/events` goes to ingest. Everything else goes to api. Ingest and api start as one binary with two route groups and split when their load profiles diverge. The edge routing stays the same either way.

All services are written in Rust and live in this repository (§17). They depend on `groundstation-schema` for `Event`/`Batch` and on the API types (§11.4) as path dependencies, so the wire types have exactly one definition and change in the same pull request on both sides.

## 5. Data flow

### 5.1 Write path

1. `gsd` POSTs a batch. Headers: `content-type: application/json`, `content-encoding: gzip`, `authorization: Bearer <ingest token>`.
2. The edge rejects compressed bodies over 8 MiB with 413.
3. ingest hashes the token, looks it up (cached for 60 s), and gets `tenant_id` and `token_id`. Unknown or revoked tokens get 401.
4. ingest decompresses with a 64 MiB limit on output (a gzip bomb gets 413), parses the `Batch`, and checks `schema`.
5. Each event is validated on its own (§6.3). Valid events become ClickHouse rows stamped with `tenant_id`, `token_id`, `received_at` and `retain_until`.
6. ingest inserts with `async_insert = 1, wait_for_async_insert = 1`. The request doesn't return until ClickHouse has committed the rows.
7. ingest returns 200 with the accepted and rejected counts. `gsd` marks the batch uploaded.

If step 6 fails, ingest returns 503 and `gsd` retries the whole batch. The resent rows that were already committed deduplicate (§7.1).

### 5.2 Read path

1. The browser loads `/` from api, which serves the embedded `ui/dist`.
2. The UI calls `/v1/health`, then `/v1/trajectories`, `/v1/trajectories/{id}` and `/v1/stats/tools`, same as locally. The session cookie goes along automatically because the origin is the same.
3. api resolves the session to a tenant and runs ClickHouse queries scoped to that tenant (§7.3).
4. If `/v1/health` advertises `insights`, the UI also calls `/v1/insights`, which api answers from Postgres.

### 5.3 Insight path

1. Every 5 minutes the worker picks each tenant with new events since its last watermark.
2. It runs each detector over that tenant's recent window in ClickHouse.
3. Detectors emit findings with a stable fingerprint. The worker upserts them into Postgres. A finding that is already known updates its evidence and `last_seen_at` instead of creating a duplicate.
4. The watermark advances.

## 6. Ingest

### 6.1 Contract

`POST /v1/events` is the same route `gsd` serves locally for SDKs. The body is a `groundstation_schema::Batch`.

Response on 200:

```json
{ "stored": 498, "rejected": [ { "id": "0192…", "error": "timestamp is more than 24h in the future" } ] }
```

`stored` is the same field the local `IngestResponse` returns. `rejected` is new and optional, so old clients ignore it.

| Status | Meaning | `gsd` behavior |
|:--|:--|:--|
| 200 | Batch processed; some events may be in `rejected` | Mark the rest uploaded. Count a refusal against each rejected event and keep it queued; after 3 refusals, drop it from the queue (it stays in the local store). Log ids and errors, never content. |
| 400 | The body isn't a `Batch`, or `schema` is unknown | Back off and retry. Counts against no event. |
| 401 / 403 | Token unknown, revoked, or tenant suspended | Back off and retry; surface in `/v1/health` |
| 413 | Batch too large | Halve the batch and retry. For a single event, count a refusal against it. |
| 429 | Rate limited; `retry-after` set | Wait `retry-after` |
| 5xx | Storage unavailable | Back off and retry |

### 6.2 Why per-event rejection

Today the uploader treats every non-2xx as retryable and resends the same batch forever. If ingest rejected a whole batch because one event was invalid, that event would block the machine's upload queue permanently. So ingest rejects individual events inside a 200 and reserves non-2xx for failures that affect the whole request and can clear up on their own.

`gsd` keeps a refused event queued and drops it after 3 refusals, in case the refusal was a backend bug that a deploy fixes. That makes `rejected` a destructive signal: ingest must use it only for problems with that specific event, never for a broken backend. A backend that is broken returns 5xx, and nothing gets dropped.

### 6.3 Validation

Per event:

- `id` is a UUID; `trajectory_id` is non-empty and at most 256 bytes.
- `kind` is a known `EventKind`. Unknown kinds from a newer `gsd` are **accepted** and stored as-is, so an old backend never rejects a newer client's events. They just don't show up in aggregates until the backend learns them.
- `timestamp` is no more than 24 hours in the future. Old timestamps are fine: machines upload after being offline.
- Serialized attributes are at most 1 MiB.

ingest doesn't redact. It trusts `gsd`'s redaction and never re-derives content. It never logs attribute values either.

### 6.4 Schema versions

`Batch.schema` is `groundstation.telemetry.v0`. ingest accepts every schema version any released `gsd` has sent, for as long as that release is supported, because `gsd` installs lag behind. When there is a `v1`, ingest translates `v0` batches to `v1` on the way in. ClickHouse stores one shape.

### 6.5 Limits

- 8 MiB compressed and 64 MiB decompressed per request. At the default 500 events per batch and the 64 KiB per-string capture limit, a normal batch is well under both.
- Rate limited per token and per tenant (token bucket, in memory per instance, and approximate across instances). The defaults are set generously enough that a flush after a week offline drains in minutes.

## 7. Storage

### 7.1 ClickHouse: `events`

```sql
CREATE TABLE events
(
    tenant_id       UUID,
    trajectory_id   String,
    event_id        UUID,
    kind            LowCardinality(String),
    ts              DateTime64(6, 'UTC'),
    span_id         Nullable(String),
    parent_span_id  Nullable(String),
    agent           LowCardinality(String),
    agent_version   LowCardinality(Nullable(String)),
    attributes      String,                         -- JSON object as sent
    schema          LowCardinality(String),
    token_id        UUID,
    received_at     DateTime64(3, 'UTC'),
    retain_until    DateTime('UTC'),

    -- Hot attributes, extracted once at insert for aggregation.
    repository      String                 MATERIALIZED JSONExtractString(attributes, 'gs.vcs.repository'),
    branch          String                 MATERIALIZED JSONExtractString(attributes, 'gs.vcs.branch'),
    revision        String                 MATERIALIZED JSONExtractString(attributes, 'gs.vcs.revision'),
    tool_category   LowCardinality(String) MATERIALIZED JSONExtractString(attributes, 'gs.tool.category'),
    tool_name       LowCardinality(String) MATERIALIZED JSONExtractString(attributes, 'gen_ai.tool.name'),
    duration_ms     Nullable(Int64)        MATERIALIZED if(JSONHas(attributes, 'gs.duration_ms'), JSONExtractInt(attributes, 'gs.duration_ms'), NULL),
    input_tokens    UInt64                 MATERIALIZED JSONExtractUInt(attributes, 'gen_ai.usage.input_tokens'),
    output_tokens   UInt64                 MATERIALIZED JSONExtractUInt(attributes, 'gen_ai.usage.output_tokens'),
    cache_read      UInt64                 MATERIALIZED JSONExtractUInt(attributes, 'gs.usage.cache_read_input_tokens'),
    cache_creation  UInt64                 MATERIALIZED JSONExtractUInt(attributes, 'gs.usage.cache_creation_input_tokens'),
    cost_usd        Float64                MATERIALIZED JSONExtractFloat(attributes, 'gs.cost.usd'),
    model           LowCardinality(String) MATERIALIZED JSONExtractString(attributes, 'gen_ai.response.model')
)
ENGINE = ReplacingMergeTree(received_at)
PARTITION BY toYYYYMM(received_at)
ORDER BY (tenant_id, trajectory_id, event_id)
TTL retain_until DELETE;
```

Decisions:

- **The dedup key is `(tenant_id, trajectory_id, event_id)`**, and the version is `received_at`. A redelivered event collapses into one row. For an updated `model.completed`, the newest upload wins, matching `gsd`'s local rule. Each `gsd` uploads in `seq` order, one batch at a time, so a newer version of an event always arrives after the older one.
- **Merges are eventual, so every read deduplicates**, with `ORDER BY received_at DESC LIMIT 1 BY event_id` or `argMax(…, received_at)`. Reads never trust that merges have run. This also covers the rare case where an event's two versions land in different monthly partitions.
- **`trajectory_id` is in the sort key**, so reading one trajectory's events is a range scan.
- **Attribute names are the same strings as in `groundstation_schema::attr`.** The DDL is generated from those constants in a build step, so the names are never written twice by hand. `gs.vcs.repository`, `gs.vcs.branch` and `gs.vcs.revision` were added for this (§11.1).
- **Retention is per tenant.** ingest computes `retain_until` from the tenant's plan at insert time. A plan change applies to new events. Shortening retention for existing events is an `ALTER TABLE … UPDATE`, run as a rare admin operation.
- **Attributes stay a JSON string.** ClickHouse's `JSON` type would help, but the attribute set is open-ended and changes with adapters. Hot fields are materialized, and everything else is extracted at query time. Revisit once the `JSON` type has settled in production for us.

### 7.2 ClickHouse: `traj_index`

Listing "the 50 most recently updated trajectories" needs an index. Summing tokens in a materialized view would double-count, because materialized views fire on every insert, before duplicates merge. So the index keeps only aggregates that give the same answer however many times an event is inserted:

```sql
CREATE TABLE traj_index
(
    tenant_id      UUID,
    trajectory_id  String,
    agent          SimpleAggregateFunction(any, LowCardinality(String)),
    started_at     SimpleAggregateFunction(min, DateTime64(6, 'UTC')),
    updated_at     SimpleAggregateFunction(max, DateTime64(6, 'UTC')),
    retain_until   SimpleAggregateFunction(max, DateTime('UTC'))
)
ENGINE = AggregatingMergeTree
ORDER BY (tenant_id, trajectory_id)
TTL retain_until DELETE;

CREATE MATERIALIZED VIEW traj_index_mv TO traj_index AS
SELECT tenant_id, trajectory_id, any(agent) AS agent, min(ts) AS started_at, max(ts) AS updated_at, max(retain_until) AS retain_until
FROM events GROUP BY tenant_id, trajectory_id;
```

With the revision on every event, "did p95 tool latency or cost change after commit X" is a `GROUP BY revision` over `events`, with no join.

`GET /v1/trajectories?limit=N` then takes two steps:

1. Pick the top N `trajectory_id`s by `max(updated_at)` from `traj_index` for the tenant.
2. Compute the full `TrajectorySummary` for just those N from deduplicated `events`: counts, token sums, cost, `status`, `title`, `ended_at`, repository from any non-empty value, and branch and revision from the latest event that has them.

Status, title and `ended_at` follow the rules in `upsert_trajectory` (`crates/gsd/src/store.rs`): status follows the latest status-bearing event, late events don't rewind it, and a resume reopens a completed trajectory. To keep the two hosts in agreement, the local store tests and the hosted query tests run the same fixture trajectories and must produce identical summaries (§12).

### 7.3 Tenant isolation

- Every query goes through one query-builder module in api and the worker. It takes the `tenant_id` from the authenticated context and injects it as a bound parameter. No handler writes SQL that touches `events` directly.
- As a second line of defense, a ClickHouse row policy on `events` and `traj_index` checks `tenant_id` against a per-query setting (`SQL_tenant_id`) that the query builder sets. A query that forgets the setting returns nothing instead of every tenant's rows.
- ingest's ClickHouse user can only `INSERT`. The api and worker users can only `SELECT`.

### 7.4 Postgres

```sql
tenants        (id uuid pk, name text, plan text, retention_days int, created_at timestamptz, suspended_at timestamptz)
users          (id uuid pk, email text unique, github_id bigint unique, created_at timestamptz)
memberships    (tenant_id uuid, user_id uuid, role text, primary key (tenant_id, user_id))
sessions       (id_hash bytea pk, user_id uuid, tenant_id uuid, expires_at timestamptz)
ingest_tokens  (id uuid pk, tenant_id uuid, token_hash bytea unique, label text, created_by uuid,
                created_at timestamptz, last_used_at timestamptz, revoked_at timestamptz)
insights       (id uuid pk, tenant_id uuid, detector text, detector_version int, fingerprint text,
                severity text, title text, body text, recommendation text, evidence jsonb, metrics jsonb,
                first_seen_at timestamptz, last_seen_at timestamptz, state text, state_changed_by uuid,
                unique (tenant_id, fingerprint))
insight_runs   (tenant_id uuid, detector text, watermark timestamptz, last_run_at timestamptz,
                primary key (tenant_id, detector))
```

Tokens and session ids are stored only as SHA-256 hashes. A token is shown once, at creation. `last_used_at` is updated at most once a minute per token.

## 8. Hosted API

### 8.1 Parity with `gsd`

api serves, with identical paths, query parameters and response shapes:

| Endpoint | Source |
|:--|:--|
| `GET /v1/health` | api itself (§8.2) |
| `GET /v1/trajectories?limit=N` | `traj_index` + `events` |
| `GET /v1/trajectories/{id}` | `events`. Unique id prefixes resolve within the tenant, as locally. |
| `GET /v1/stats/tools?days=N` | `events`, deduplicated, with `quantileExact` so p50 and p95 match the local computation |

Local-only endpoints (`/v1/adapters/*`, `/v1/shutdown`) are not served. The UI never calls them. The CLI does, and it only ever talks to the local `gsd`.

### 8.2 `/v1/health`

`Health` has fields that only make sense on a machine: `pid`, `data_dir`, `spool_pending`, `upload`. The UI reads only `upload.endpoint`, to label the connection (`ui/src/components/Shell.tsx`). Proposed change, made on both sides in one commit:

- Make `pid`, `data_dir`, `spool_pending` and `upload` optional in `api.rs` and `types.ts`. `gsd` keeps sending them; api omits them.
- Add `deployment: "local" | "hosted"` for the connection label.
- Add `features: string[]`, which lists optional endpoint groups. The first is `"insights"`. `gsd` sends `[]`.

The UI feature-detects insights from `features`, the same way it already treats `adapters` and `ui`.

### 8.3 Hosted-only endpoints

| Endpoint | |
|:--|:--|
| `GET /v1/insights?state=open&limit=N` | Findings, most severe then most recent first |
| `GET /v1/insights/{id}` | One finding with its evidence |
| `POST /v1/insights/{id}/state` | `{ "state": "dismissed" \| "open" \| "resolved" }` |
| `GET /auth/*` | Login, callback, logout. Not under `/v1`, because it isn't part of the data contract. |

Evidence is a list of `{ trajectory_id, event_ids[], note }`, so the UI links each finding straight to the trajectory view it already has.

### 8.4 Authentication

- Browser: OpenID Connect, then an `HttpOnly; Secure; SameSite=Lax` session cookie on the app origin. Because the origin is the same, `fetch` sends it with no change to `api.ts`. The hosted service offers GitHub and Google sign-in; a self-hosted instance points at any OIDC provider (Okta, Entra ID, Keycloak, Google Workspace) through configuration. A single-user self-hosted instance can skip sign-in by setting an admin password instead. SAML is an enterprise feature (§17).
- CSRF: the one mutating browser endpoint (`POST /v1/insights/{id}/state`) requires `content-type: application/json`, which a cross-origin form can't send without a preflight, and api answers no preflights. This is the same defense `gsd` uses locally.
- `gsd`: bearer ingest token, accepted only on `POST /v1/events`. An ingest token can't read anything.

## 9. Insights

### 9.1 Terms

| Term | What it is | Where it lives |
|:--|:--|:--|
| Aggregate | A number over events: tokens, cost, p95 tool latency | Computed at read time from ClickHouse |
| Finding | Something specific that happened, with evidence: "The agent re-ran `cargo test` after each of 11 edits in 3f2a9c1e" | `insights` row |
| Recommendation | A concrete change attached to a finding: "Scope the test command to the edited crate" | `insights.recommendation` |

### 9.2 Detectors

A detector is a pure function over the results of a ClickHouse query: `fn detect(rows) -> Vec<Finding>`. It has a name, a version, and a query window. Pure functions make detectors testable against fixture trajectories, the same fixtures the adapters already use.

First set, all rules-based:

| Detector | Signal |
|:--|:--|
| `repeated_read` | The same `gs.file.path` read 3 or more times in a trajectory with no write in between |
| `test_rerun_loop` | A full test command run after most edits in a turn |
| `tool_failure_hotspot` | A tool category whose failure rate this week is above 20% with at least 20 calls |
| `slow_tool` | A tool category whose p95 rose by more than 2× against the previous window |
| `cache_miss` | A trajectory whose cache-read share of input tokens is far below the tenant's median |
| `cost_outlier` | A trajectory above the tenant's p95 cost for the same agent and model |
| `permission_wait` | Long gaps after `agent.notification` events, meaning the agent sat waiting for a human |

### 9.3 Fingerprints and state

A finding's fingerprint is a hash of `(detector, scope key)`, where the scope key is what makes it that finding (for `repeated_read`, the trajectory id and file path). Re-running a detector updates the existing row: new evidence, `last_seen_at`, metrics. It never duplicates. A dismissed finding stays dismissed unless its severity rises. Bumping `detector_version` lets a detector change its logic without resurrecting old dismissals.

### 9.4 LLM-written findings, later

A model can write better narrative findings than rules, but it needs content: prompts, commands, tool output. Sending that to a model provider is a new data flow. It will be opt-in per tenant, shown in settings with the provider named, and it will run in the worker only. Out of scope for the first release.

## 10. Privacy and security

- **What the cloud stores** is exactly what `gsd` uploads after redaction, with paths made relative to the repository (§11.1). That can include prompt text, tool bodies and shell commands unless the user excludes them. The docs and the `groundstation` CLI must say this plainly before cloud mode is turned on.
- **The logging rule applies in the cloud too.** No cloud service logs content attributes (`attr::CONTENT`) or tokens. Request logs record tenant, token id, byte counts, event counts, status and latency. Error messages that could echo input are truncated and scrubbed.
- **Encryption:** TLS to the edge and between services and databases, and encryption at rest from the managed providers.
- **Deletion:** deleting a tenant deletes its Postgres rows immediately and its ClickHouse rows with `ALTER TABLE … DELETE WHERE tenant_id = …`. Deleting a single trajectory is supported the same way.
- **Tokens:** hashed at rest, revocable, and scoped to ingest. A leaked ingest token lets someone write junk into a tenant, never read from it. Junk shows up per token and is removable by `token_id`.

## 11. Changes in this repository

### 11.1 Git checkout and trajectory context on events (done)

- **Per event, at ingest:** `gs.vcs.revision` and `gs.vcs.branch`, the checkout when the event happened. `gsd` reads them from the repository's files on every hook (`.git/HEAD`, loose and packed refs, worktree links) and never runs `git`, so a repository's config can't execute anything in the daemon. Model calls read from transcripts take the checkout of the latest event before them, so re-reading a transcript doesn't change them. Stamping at ingest rather than at upload means a backlog uploaded later still carries the right commit.
- **Per trajectory, at upload:** each event gets `gs.vcs.repository`, the repository's name outside the machine: its `origin` remote without scheme or credentials (`github.com/acme/shop`), or its directory name. It's the same on every teammate's machine, so the backend can group by repository. The hostname is not uploaded: hostnames often carry a person's name, so the hosted `TrajectorySummary.host` is always empty.
- **Paths leave relative to the repository.** The local store keeps full paths for the local UI. Uploads carry `gs.file.path` and tool-input paths relative to the repository root (`src/checkout.rs`), absolute paths outside it as just a file name, `gs.cwd` relative to the root (dropped outside one), and no `gs.transcript.path`. No home directory or username leaves the machine through a path field. With `paths = "hash"`, paths are HMAC-SHA256 hashes under a per-install key and leave as stored: they can't be reversed by guessing likely paths, and don't match across machines.
- `TrajectorySummary.revision` and `branch` are the latest checkout. The UI shows the short revision, a commit count, and a "HEAD moved" divider in the event list.

### 11.2 Uploader error handling (done)

- Parse `rejected` on 200. Accepted events are marked uploaded. Each rejected event gets a refusal counted against it (`events.upload_attempts`) and stays queued. After 3 refusals it is marked dropped (`uploaded = 2`) and logged by id. An event whose content changes later (an updated `model.completed`) is queued again with a fresh count. `upload.dropped` in `/v1/health` and `groundstation status` report how many were dropped.
- On 413, halve the batch size for the next attempt, then grow it back after successes. A single event that still gets 413 counts as a refusal.
- On 429, wait for `retry-after`.
- Record the last error and when it happened, and expose them as `upload.last_error` in `/v1/health` and `groundstation status`, so a revoked token is visible instead of silent. It clears on the next success.

### 11.3 `Health` (done)

Done: the field changes in §8.2, applied to `crates/api`, `ui/src/lib/types.ts` and `Shell.tsx`. The UI labels the connection from `deployment`, and shows "upload failing" with the error when `upload.last_error` is set.

### 11.4 An API types crate (done)

The hosted services need `TrajectorySummary`, `TrajectoryDetail`, `ToolStats`, `Health` and the rest without depending on `gsd` (and pulling in rusqlite, axum and the adapters). The wire types moved from `crates/gsd/src/api.rs` into `crates/api` (`groundstation-api`), which has no I/O dependencies. `gsd` and the CLI depend on it directly. `IngestResponse` gained `rejected`. The insights wire types live there too, even though only the hosted API serves them.

### 11.5 UI

- Done: the demo fallback and fixtures are gone. A 401 from any `/v1` call replaces the app with a sign-in screen linking to `/auth/login`. When `/v1/health` fails, a full-page error screen says either that gsd can't be reached or which HTTP status came back, and the app recovers on its own once health succeeds.
- To do: an insights view, shown only when `features` includes `insights`.
- Done: the connection label reads `deployment` instead of guessing from `upload.endpoint`.

### 11.6 CLI

`groundstation cloud login` opens the browser, creates an ingest token for the chosen tenant, and writes `transport.mode = "cloud"`, `endpoint` and `token` into `config.toml`, then restarts `gsd`. Before it enables upload, it states what will be uploaded under the current `[redaction]` settings.

## 12. Testing

- **Contract tests.** A shared suite of HTTP requests and expected responses for `/v1/health`, `/v1/trajectories`, `/v1/trajectories/{id}` and `/v1/stats/tools`, run against a local `gsd` and against a hosted api loaded with the same fixture events. The two hosts must return identical bodies, apart from the fields §8.2 lets differ.
- **Summary parity.** The fixture trajectories used by `store.rs` tests (redelivery, model updates, late events, resume) are replayed through ingest into ClickHouse, and the resulting summaries must match.
- **Idempotency.** Upload the same batch twice, and upload a batch with an updated `model.completed`. The counts must not change, and the newer attributes must win.
- **Poison batch.** A batch containing one invalid event stores the rest, returns the bad event in `rejected`, and the uploader moves on.
- **Isolation.** Queries with a missing or wrong tenant setting return no rows.

## 13. Operations

- **Hosting.** Stateless containers for ingest, api and the worker, on whatever container platform we pick. Managed ClickHouse (ClickHouse Cloud) and managed Postgres, so we don't run stateful infrastructure at this stage. The hosted service runs the same images self-hosters run; nothing in the code depends on the managed providers.
- **Deploys.** api embeds a pinned `ui/dist` release, the same way `gsd` does. A hosted deploy and a `gsd` release that change the contract ship in the order: backend first (accepting both shapes), then clients.
- **Metrics.** Per service: request rate, status mix, and p50/p95 latency. For ingest: events accepted and rejected per second, ClickHouse insert latency, and ingest lag (`now − max(received_at)`). For the worker: run duration per detector and findings upserted. For the fleet: `gsd` versions seen, taken from the `user-agent` header.
- **Alerts.** Ingest 5xx rate, insert latency, and worker runs failing or falling behind.
- **Backups.** Provider-managed for both databases. ClickHouse data is also recoverable from the machines, because each `gsd` keeps its store; the cloud is not the only copy.

## 14. Rollout

| Milestone | Delivers |
|:--|:--|
| M0 | Changes in this repository: §11.1–11.5, the `groundstation-api` crate, and contract tests against `gsd`. Done, except the contract tests. |
| M1 | ingest, ClickHouse, and api with read parity, single tenant, internal use only; hosted `ui/dist` unmodified. The `docker compose` setup from §16 is the development environment from day one, so self-hosting works from the first commit. |
| M2 | Tenancy, OIDC sign-in, ingest tokens, retention, `groundstation cloud login` (which also targets a self-hosted endpoint); self-hosting guide; private beta |
| M3 | Insights worker with the first detectors, and the insights view in the UI |
| M4 | Team views across machines and users; opt-in LLM findings |

## 15. Alternatives considered

- **gRPC for ingest.** Rejected. `gsd` sends batches, not streams, so gRPC's streaming buys nothing. It would add a second protocol and break the rule that the local and hosted hosts serve identical paths. OTLP/HTTP is worth adding later as an extra ingest route for SDK users, since the attributes already follow `gen_ai.*` conventions.
- **A queue (Kafka, Redpanda) between ingest and ClickHouse.** Deferred. Every `gsd` already holds a durable queue and retries until it gets a 2xx, and async inserts batch writes on the server. Add a log when a second consumer needs the raw stream, or when replay without asking machines to resend becomes necessary.
- **Postgres only.** Viable for an early product and simpler to run. Rejected because the workload is append-heavy and aggregation-heavy over JSON attributes (percentiles, per-category rollups, cross-trajectory detectors). Moving off Postgres later would cost more than starting on ClickHouse now.
- **Insights in ClickHouse.** Rejected. Findings are few, mutable, and carry per-user state (dismissed, resolved). That is transactional data.
- **Building summaries in ingest.** Rejected. It would require read-modify-write on every event, which is exactly what makes redelivery hard. Computing summaries at read time from deduplicated events stays correct under redelivery and updates.
- **A closed-source backend in a private repository.** Rejected. Ground Station handles prompts and source code, and many teams can't send that to a SaaS without a security review. "Run it yourself, the data never leaves" removes that objection, builds trust in what the hosted service does with data, and turns self-hosters into a path to paying customers. Keeping the backend in this repository also removes the need to publish and version the shared crates separately.

## 16. Self-hosting

Self-hosting runs the same code as the hosted service. A self-hosted instance is the hosted service with one tenant and the operator's own sign-in.

- **What it takes:** ClickHouse, Postgres, and the Ground Station server image. The repository ships a `docker-compose.yml` that starts all three, and the same file is the development environment, so it can't drift from what developers use. A Helm chart comes later, if self-hosters ask for it.
- **Configuration** is environment variables only: database URLs, the public URL, the OIDC provider, and retention. No config files to mount.
- **Migrations** for both databases run when the server starts, so upgrading is a change of image tag. Each release says which versions it can upgrade from.
- **Ingest tokens** are created in the UI or with the CLI. `groundstation cloud login --endpoint https://gs.internal.example` points `gsd` at a self-hosted instance the same way it points at the hosted one.
- **Nothing calls home** by default. An opt-in, anonymous usage report (version, event volume) may come later; it's never on without the operator choosing it.
- **No required managed services:** no Kafka, no object storage, no external queue. Keeping ingest stateless and leaning on `gsd`'s own queue (§5.1) is what makes a three-container deployment enough.

## 17. Repository and licensing

The backend lives in this repository, next to the daemon and the UI it serves:

| Path | | License |
|:--|:--|:--|
| `crates/schema`, `crates/api`, `crates/gsd`, `crates/groundstation`, `adapters/*`, `ui/` | daemon, CLI, wire types, UI | Apache-2.0, as today |
| `crates/server` | the backend: ingest, api, insights worker, in one binary with subcommands | Apache-2.0 |
| `ee/` | enterprise features, compiled into the server behind an `ee` Cargo feature | commercial; free to try, a license key to use in production |

The model is open core, as PostHog and Langfuse run it: everything a team needs to self-host and use Ground Station is open, and the features large organizations pay for are in `ee/`. Revenue comes from the hosted service, from `ee/` licenses for self-hosted enterprises, and from support.

What goes where:

- **Open:** ingest, storage, the full `/v1` API, the UI, tenancy, OIDC sign-in, ingest tokens, retention settings, and all of insights: the rule-based detectors in §9.2 and, when they come, LLM-written findings (§9.4) and benchmarks.
- **`ee/`:** SAML, SCIM provisioning, role-based access control beyond admin and member, audit logs, per-project retention policies, and anything built specifically for very large deployments.

A rule keeps the line honest: a feature goes in `ee/` only if it matters mostly to organizations with a security or procurement team. Anything a small team needs to get value from Ground Station is open.

The core is Apache-2.0, like the rest of the repository, for the widest adoption: no legal review blocks a team from self-hosting it. The tradeoff is accepted knowingly. Anyone may host a copy as a competing service without publishing changes; the hosted service has to win on running Ground Station well, not on the license.

## 18. Open questions

1. Tenancy model: personal workspace by default, with teams as a plan upgrade, or teams from the start?
2. Default retention per plan, and whether it can be shorter for content attributes than for measurements.
3. Region: one region at launch, or EU data residency from M2?
4. Should `gsd` offer a "measurements only" cloud mode that excludes every content attribute when uploading while keeping it locally? It would make cloud mode an easier yes for cautious teams, at the cost of weaker findings.
