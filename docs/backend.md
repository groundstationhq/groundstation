# The backend

What `gsd` uploads to when `transport.mode = "cloud"`: three services in this repository, ClickHouse for events, and Postgres for everything else. The hosted service and a self-hosted instance run the same code.

```text
 gsd ──POST /v1/events──┐                      ┌──────────┐  insert  ┌────────────┐
                        ├── edge ─────────────►│ ingest   │─────────►│ ClickHouse │
 browser ──/, /v1, /auth┘   (one origin)  │    └──────────┘          │ events     │
                                          │    ┌──────────┐  read    │ traj_index │
                                          └───►│ api      │◄─────────┤            │
                                               │ + ui/dist│          └─────▲──────┘
                                               └────┬─────┘                │ read
                                                    │      ┌──────────┐    │
                                          Postgres ◄┴──────│ worker   │────┘
                                          tenants, users,  └──────────┘
                                          tokens, insights
```

- **ingest** (`crates/ingest`) accepts batches from `gsd` on `POST /v1/events`, resolves the bearer token to a tenant, and writes events to ClickHouse.
- **api** (`crates/api`) serves the built UI and the same `/v1` read contract `gsd` serves, for the signed-in tenant, plus sign-in (`/auth/*`), ingest tokens (`/v1/tokens`) and findings (`/v1/insights`). It migrates both databases when it starts.
- **worker** (`crates/worker`) runs the detectors over each tenant's recent events and stores findings.
- **edge** routes `POST /v1/events` to ingest and everything else to api, so the UI and `gsd` see one origin. Locally it's Caddy (`docker/caddy/Caddyfile`).

Ingest can also have an origin of its own, so that uploads and the web are scaled, limited and taken down separately: route `POST /v1/events` at that origin to ingest, and set `GROUNDSTATION_INGEST_URL` on the api. People still give `groundstation cloud login` the address they sign in at. The CLI asks the api where uploads go (`GET /auth/cli/endpoints`, no sign-in) before it says what will leave the computer, and saves that address as `transport.endpoint`. A backend that doesn't answer takes uploads where it is.

Shared code lives in `crates/backend` (storage, configuration, startup), `crates/wire` (the HTTP types the UI and CLI also use) and `crates/ui-embed` (the embedded UI `gsd` serves too).

## Running it locally

The services run on your machine; Docker runs what they connect to.

```sh
make stack    # everything at once: the containers, then api, ingest and worker in one terminal
```

Each service's log lines are prefixed with its name, Ctrl-C stops the three, and if one stops the others stop with it. To run them separately, each in its own terminal:

```sh
make deps     # ClickHouse, Postgres, Dex and the edge on http://127.0.0.1:8080
make api      # builds the UI, runs api behind the edge
make ingest
make worker
```

Open http://127.0.0.1:8080 and sign in with the admin password `local development`, or with Dex as `dev@groundstation.test` / `password`. Then send this machine's events there:

```sh
groundstation cloud login --endpoint http://127.0.0.1:8080
```

`cloud login` says what will be uploaded under your `[redaction]` settings, opens the browser to approve the computer, writes the token into `config.toml` and restarts `gsd`. The browser carries only a one-time code back to the CLI, which collects the token from the api with a secret it never shows the browser, so no token is left in the browser's history. `groundstation cloud logout` goes back to local-only; revoke the token on the Tokens page.

On a computer you reach over SSH, the browser that's signed in is somewhere else. Create a token on the Tokens page and paste it instead: `groundstation cloud login --endpoint … --token-stdin`.

On Linux the edge container reaches the services through the Docker bridge, so run them listening on its address, which `ip -4 addr show docker0` shows (usually `172.17.0.1`): `GROUNDSTATION_API_LISTEN=172.17.0.1:4321` and `GROUNDSTATION_INGEST_LISTEN=172.17.0.1:4320`. Not `0.0.0.0`: that puts them on your network, the api behind the development admin password printed above.

## Ingest

`POST /v1/events` takes a gzip-compressed `groundstation.telemetry.v0` batch with `authorization: Bearer <ingest token>`. Problems with the whole request get a non-2xx status and change nothing; problems with single events are refused inside a 200, and the rest of the batch is stored.

| Status | Meaning | What `gsd` does |
|:--|:--|:--|
| 200 | `{"stored": N, "rejected": [{"id", "error"}]}` | Marks the stored events uploaded; counts a refusal against each rejected one and drops it after 3 |
| 400 | Not a batch, unknown schema, or an event without a valid id | Backs off and retries |
| 401 / 403 | Unknown or revoked token / suspended tenant | Backs off and retries; shows the error in `groundstation status` |
| 408 | The batch took over 30 seconds to arrive | Backs off and retries |
| 413 | Over 8 MiB compressed, 64 MiB inflated or 10,000 events | Halves the batch |
| 415 | A `content-encoding` other than gzip (or none) | Backs off and retries |
| 429 | Rate limited, too many token lookups at once, or too many batches in flight; `retry-after` says when to come back | Waits |
| 503 | Storage unavailable | Backs off and retries |

An event is refused when its `trajectory_id` is empty or longer than 256 bytes, its timestamp is more than 24 hours ahead or before 1900 (a time the client never set), or its attributes are over 1 MiB. Unknown kinds are stored. Ingest never redacts (that happens in `gsd`) and never logs attribute values or tokens: one log line per request with tenant, token id, bytes, counts, status and time. A request refused before its token resolves is logged only at debug level, so a flood of them can't fill the disk.

Tokens resolve through a one-minute cache, so a revoked token stops working within a minute. A token that resolved before is looked up again without waiting behind unknown ones. Rate limits are token buckets per token and per tenant, in memory per instance.

A request without a valid token can't be limited by token, so its cost is bounded another way: a token that isn't shaped like one is refused without a lookup, an unknown one is remembered for a minute like a known one, and lookups in Postgres are limited across all clients (`GROUNDSTATION_INGEST_LOOKUP_RATE`). Batches are inflated, parsed and stored off the request threads, `GROUNDSTATION_INGEST_DECODING` at a time, which also bounds memory: each can hold 64 MiB or more. Four times as many may wait with their bodies read; past that, ingest answers 429. On shutdown, requests in flight get 25 seconds to finish.

## Storage

**ClickHouse.** `events` is a `ReplacingMergeTree` ordered by `(tenant_id, trajectory_id, event_id)`, versioned by `received_at`: a redelivered event, or a newer version of an updated `model.completed`, replaces the old one. Background merges are eventual and never cross the monthly partitions, so no read relies on them: every read goes through `ch::deduplicated`, which reads with `FINAL`, so ClickHouse merges the copies as it reads, across partitions too, and filters see only the newest copy. Hot attributes are materialized columns whose names come from `groundstation_schema::attr`. `traj_index` keeps first and last timestamps per trajectory for listing. Rows expire at `retain_until`, set from the tenant's retention when they arrive.

Each service connects as its own user, with its own password. `gs_ingest` has the `gs_writer` role and can only insert. `gs_api` and `gs_worker` have `gs_reader`: they can only read, and a row policy shows them only the tenant named by the `SQL_tenant_id` setting. Every read goes through `ch::TenantScope`, which binds the tenant as a parameter and sets that setting; a query that forgets the filter is refused before it's sent, and one that slipped through would return nothing. Each such read may take 30 seconds and 2 GiB of ClickHouse memory (sorts and groupings past 1 GiB spill to disk), and the api runs at most four at once per tenant, so one tenant can't take the server every tenant shares. Two kinds of read get 10 minutes, with the same memory, because they go at their reader's pace: a trajectory's timeline, which a browser downloads as fast as its connection does, and the worker's detectors, which nobody waits for. The trajectory list is kept for 5 seconds per tenant, so every open tab asking for it costs one read, and a new upload shows in the list within that long. `/v1/health` reads no database. A trajectory's events are streamed to the browser as they are read.

**Postgres** holds tenants, users and their sign-in identities, memberships, sessions, ingest tokens and findings. Tokens and session ids are stored only as SHA-256 hashes. The api connects as the owner. Ingest and the worker can each connect as their own role (`gs_ingest`, `gs_worker`), created by `docker/postgres/roles.sh`: ingest can resolve tokens and nothing else, the worker can read tenants and write findings. The api grants them that at every start. When self-hosting with the compose file they do; run with `cargo`, every service uses the owner unless you give it another `GROUNDSTATION_POSTGRES_URL`.

**Migrations** run when api starts. Postgres migrations (`crates/backend/src/pg/migrations`) are applied in order under an advisory lock; never edit one that has shipped. ClickHouse statements are all idempotent and run at every start.

## Signing in

Browsers get an `HttpOnly`, `SameSite=Lax` session cookie. Behind HTTPS it is `Secure` and named `__Host-gs_session`, so no other site or path can plant one in its place, and every answer carries `Strict-Transport-Security`. Every `/v1` request needs a live session; a 401 sends the UI to its sign-in screen, and a suspended workspace answers 403. Signing in ends the session the browser had. A sign-out link followed from another site only asks to confirm; signing out lands on a page that doesn't send you back through the provider. Requests that change anything must carry an `Origin` header matching `GROUNDSTATION_PUBLIC_URL`, and `/v1` writes must be JSON, which browsers can't send cross-origin without a preflight the api never grants. The pages the api renders itself (sign-in, approving a computer) are served with `Referrer-Policy: same-origin`, not `no-referrer`: browsers send `Origin: null` with forms posted from a `no-referrer` page.

Two ways in, either or both:

- **Admin password** (`GROUNDSTATION_ADMIN_PASSWORD`, at least 12 characters): one admin user, for a single-person instance. Wrong passwords are limited across all clients: ten at once, then one every ten seconds. A flood of wrong guesses therefore locks password sign-in until it stops; signing in with a provider keeps working.
- **OpenID Connect** (`GROUNDSTATION_OIDC_ISSUER`, `_CLIENT_ID`, `_CLIENT_SECRET`): any provider (Google, Okta, Entra ID, Keycloak). The redirect URI to register is `{GROUNDSTATION_PUBLIC_URL}/auth/callback`. People are matched by issuer and subject, never by email. `GROUNDSTATION_OIDC_ALLOWED_DOMAINS` admits only verified emails at those domains; it applies when someone signs in, so people already in keep their sessions until they expire and their ingest tokens until revoked. A sign-in can only be finished by the browser that started it (a short-lived `gs_login` cookie holds its `state`), so a link to someone else's finished sign-in signs nobody in.

The first time someone signs in, `GROUNDSTATION_NEW_USERS=personal` gives them their own tenant; `shared` adds everyone to one tenant, administered by whoever signed in first.

## Findings

Every few minutes the worker runs each detector whose tenant has events it hasn't seen, over the last 7 days (or as many as the tenant keeps, if fewer), including events stamped up to a day ahead of its clock, as ingest accepts them. Events stamped in the two minutes before a run are looked at again in the next one, since a batch can commit after a later one. A finding has a fingerprint (detector plus what it's about), so a rerun updates it instead of adding another. A finding someone dismissed or resolved stays that way unless it gets worse.

Open findings are what the detectors see in that window now. One a run no longer finds (the failures stopped, the trajectory is older than the window) is resolved by the worker, and opens again if it comes back; a tenant that stopped uploading has its open findings resolved once they are older than the window. A finding is deleted once its tenant no longer keeps the newest event it describes, so none outlives its events. A detector keeps at most 200 findings per tenant, the most severe; while it has more, it leaves the rest as they were.

Findings count and link; they don't quote. Content attributes (prompts, tool bodies, shell commands) stay in the events.

Worker instances can overlap (a rolling deploy, a scheduled `--once` beside a running worker): they take each tenant in turn, holding a Postgres advisory lock. `--once` exits non-zero if any part of its pass failed.

| Detector | Finds |
|:--|:--|
| `repeated_read` | The same file (or the same lines of it) read 3+ times by one agent in a trajectory with no write in between |
| `test_rerun_loop` | The full test suite run after most edits in a turn (needs `gs.shell.command`) |
| `tool_failure_hotspot` | A tool category failing more than 20% of at least 20 calls |
| `slow_tool` | A tool category whose p95 more than doubled against the previous 7 days |
| `cache_miss` | A trajectory reading under a third of the tenant's median share of input from the cache |
| `cost_outlier` | A trajectory above the 95th percentile and twice the median cost for its agent and model; only agents that report cost (OpenCode, pi) have any |
| `permission_wait` | Long stops after a permission prompt or question: 3+ waits over a minute, or 10+ minutes in all |

Each detector is a ClickHouse query and a pure function over its rows (`crates/worker/src/detectors`), tested on rows alone.

## Configuration

Environment variables only (every one is also a flag; `--help` lists them).

| Variable | Service | Default |
|:--|:--|:--|
| `GROUNDSTATION_CLICKHOUSE_URL` | all | `http://127.0.0.1:8123`; `https://` for TLS (ClickHouse Cloud), verified against the system's certificate authorities |
| `GROUNDSTATION_CLICKHOUSE_USER` | all | the service's own: `gs_ingest`, `gs_api`, `gs_worker` |
| `GROUNDSTATION_CLICKHOUSE_PASSWORD` | all | `groundstation` |
| `GROUNDSTATION_CLICKHOUSE_DATABASE` | all | `groundstation` |
| `GROUNDSTATION_POSTGRES_MAX_CONNECTIONS` | all | 10 per service (the worker uses at least 2) |
| `RUST_LOG` | all | the service's own `info` filter; `RUST_LOG` replaces it |
| `GROUNDSTATION_POSTGRES_URL` | all | `postgres://groundstation:groundstation@127.0.0.1:5432/groundstation`; `sslmode=require` for TLS, which verifies the certificate and host name (`verify-ca` and `verify-full` mean the same), against the system's certificate authorities or the one in `sslrootcert=/path/to/ca.pem` (refused without `sslmode=require`, rather than connecting unencrypted) |
| `GROUNDSTATION_INGEST_LISTEN` | ingest | `127.0.0.1:4320` |
| `GROUNDSTATION_INGEST_TOKEN_RATE`, `_TOKEN_BURST` | ingest | 20 requests/s, bursts of 200 |
| `GROUNDSTATION_INGEST_TENANT_RATE`, `_TENANT_BURST` | ingest | 200 requests/s, bursts of 2,000 |
| `GROUNDSTATION_INGEST_LOOKUP_RATE` | ingest | 500 token lookups/s in Postgres across all clients; a token is looked up once a minute |
| `GROUNDSTATION_INGEST_DECODING` | ingest | 16 batches inflated, parsed and stored at once, and 64 held |
| `GROUNDSTATION_API_LISTEN` | api | `127.0.0.1:4321` |
| `GROUNDSTATION_PUBLIC_URL` | api | `http://127.0.0.1:4321`; an origin only (scheme, host, optional port): Ground Station is served at the root |
| `GROUNDSTATION_INGEST_URL` | api | none: uploads go to the public URL, through the edge. Set it when ingest has an origin of its own; the api tells `groundstation cloud login` to upload there |
| `GROUNDSTATION_CLICKHOUSE_ADMIN_USER`, `_ADMIN_PASSWORD` | api | `groundstation` / `groundstation`; used only to migrate |
| `GROUNDSTATION_ADMIN_PASSWORD`, `GROUNDSTATION_OIDC_*` | api | none: set at least one |
| `GROUNDSTATION_NEW_USERS` | api | `personal` |
| `GROUNDSTATION_RETENTION_DAYS` | api | 90, for new tenants |
| `GROUNDSTATION_SESSION_DAYS` | api | 30 (1 to 400) |
| `GROUNDSTATION_WORKER_INTERVAL`, `_WINDOW_DAYS` | worker | 300 s, 7 days (1 to 365) |

Ingest and api answer `GET /healthz` with whether their databases respond, and why not when they don't. It's for whoever runs the services: the edge answers 404 for it. Self-hosting, ask from the compose network: `docker run --rm --network groundstation-selfhost_default curlimages/curl -s http://api:4321/healthz` (or `ingest:4320`). The worker has no HTTP server: its log says when a pass fails.

## Self-hosting

```sh
docker compose -p groundstation-selfhost --profile selfhost up -d --build   # start
docker compose -p groundstation-selfhost --profile selfhost down            # stop; the data stays
docker compose -p groundstation-selfhost --profile selfhost down -v         # stop and delete the data
```

That runs everything: the databases, the three services from the `Dockerfile` (one image, the service chosen by its command) and the edge on `127.0.0.1:8080`. The project name (`-p`) keeps it apart from a development stack on the same machine, whose databases have the development passwords; every `docker compose` command for it needs the same `-p` and `--profile`.

Put your TLS-terminating proxy in front of the edge, serving only https at `GROUNDSTATION_PUBLIC_URL` (the api then sends HSTS and host-only cookies). It must pass `Origin` and `Cookie` through unchanged, accept request bodies of at least 8 MiB on `POST /v1/events`, and wait at least 60 seconds for one to arrive.

Before the first start, set your own values in a `.env` file next to `docker-compose.yml`. Generate each password from letters and digits, for example with `openssl rand -hex 24`, and single-quote every value: compose expands `$` and drops anything after ` #` in unquoted and double-quoted ones.

```sh
GROUNDSTATION_PUBLIC_URL='https://gs.example.com'
GROUNDSTATION_ADMIN_PASSWORD='...'          # and/or GROUNDSTATION_OIDC_*

# ClickHouse: the admin (migrations) and one user per service
GROUNDSTATION_CLICKHOUSE_ADMIN_PASSWORD='...'
GROUNDSTATION_INGEST_CLICKHOUSE_PASSWORD='...'
GROUNDSTATION_API_CLICKHOUSE_PASSWORD='...'
GROUNDSTATION_WORKER_CLICKHOUSE_PASSWORD='...'

# Postgres: the owner (api) and one role each for ingest and the worker
GROUNDSTATION_POSTGRES_PASSWORD='...'
GROUNDSTATION_INGEST_POSTGRES_PASSWORD='...'
GROUNDSTATION_WORKER_POSTGRES_PASSWORD='...'
```

Ingest and the worker get only their own credentials, so if one is broken into it can't act as another. The api holds the ClickHouse admin's and the Postgres owner's, because it migrates both databases when it starts. The ClickHouse users and the Postgres roles are created with these passwords when the data volumes are first created (`docker/clickhouse/users.sh`, `docker/postgres/roles.sh`); changing a value in `.env` later doesn't change the password in the database. The compose file passes the variables above, the sign-in ones and `GROUNDSTATION_RETENTION_DAYS` and `GROUNDSTATION_NEW_USERS`; set others in an override file.

To change a password (all commands with `-p groundstation-selfhost --profile selfhost`):
- Postgres owner: `docker compose exec postgres psql -U groundstation -c "ALTER ROLE groundstation PASSWORD '<new>'"`, then set it in `.env` and `docker compose up -d`.
- `gs_ingest` or `gs_worker`: set it in `.env`, `docker compose up -d postgres` (so the container has it), `docker compose exec postgres bash /docker-entrypoint-initdb.d/roles.sh`, then `docker compose up -d`.
- ClickHouse users: `docker compose exec clickhouse clickhouse-client --user groundstation --password '<admin>' --query "ALTER USER gs_ingest IDENTIFIED BY '<new>'"` (or `gs_api`, `gs_worker`, `groundstation` for the admin), then set it in `.env` and `docker compose up -d`.

The self-hosting profile defaults to `GROUNDSTATION_NEW_USERS=shared`, so your team works in one tenant. With a sign-in provider anyone can get an account at (Google, GitHub), set `GROUNDSTATION_OIDC_ALLOWED_DOMAINS`: otherwise every account there can join your tenant. `GROUNDSTATION_RETENTION_DAYS` applies to a tenant when it's created, so the shared tenant keeps the value it had at the first sign-in, and events keep the retention they arrived with.

There is no command to delete data yet. To delete a trajectory, or a whole tenant's events (leave out the `trajectory_id` condition), run in ClickHouse as the admin:

```sql
ALTER TABLE events DELETE WHERE tenant_id = '<tenant>' AND trajectory_id = '<id>';
ALTER TABLE traj_index DELETE WHERE tenant_id = '<tenant>' AND trajectory_id = '<id>';
```

Deleting a tenant in Postgres (`DELETE FROM tenants WHERE id = '<tenant>'`) removes its members' access, tokens and findings with it.

Upgrading is a new image: the api migrates on start. Nothing calls home.

### On Kubernetes

[`infra/helm/groundstation`](../infra/helm/groundstation) is a Helm chart for the same stack: the three services, the edge with the same Caddyfile, and ClickHouse and Postgres set up by the same scripts, or your own databases in their place. It generates the passwords and keeps them, gives each service only its own, and is exposed through the edge's Service or an Ingress. Its `README.md` says how to install it.

## Tests

The unit tests run with `cargo test`. Tests that need the databases are `#[ignore]`d there and run with `make deps && make backend-test`, as CI does. Among them, `crates/api/tests/parity.rs` loads the same events into `gsd`'s store and into ClickHouse and checks that summaries, timelines, lists and tool stats come out identical.
