# gsd and the `groundstation` CLI

The local half of Ground Station: the `gsd` daemon, the `groundstation` CLI, and the first adapter (Claude Code).

| Crate | Binary | What it does |
|---|---|---|
| [`crates/schema`](../crates/schema) | | `groundstation.telemetry.v0`: event types and attribute names shared by adapters, SDKs, `gsd` and the backend |
| [`crates/gsd`](../crates/gsd) | `gsd` | Local daemon. Ingests, redacts, stores and (optionally) uploads agent telemetry, translating hook payloads with the matching adapter |
| [`crates/groundstation`](../crates/groundstation) | `groundstation` | CLI. Connects agents, runs their hooks, manages `gsd`, and shows trajectories |
| [`adapters/claude-code`](../adapters/claude-code) | | Claude Code: installs hooks in `settings.json`, turns hook payloads and transcript lines into events. Recorded payloads in `tests/fixtures/` |

Each adapter implements the `Adapter` trait from [`crates/schema/src/adapter.rs`](../crates/schema/src/adapter.rs): pure translation from an agent's native payloads to events, with no I/O. `gsd` owns storage, privacy and transcript reading, and keeps raw payloads so they can be re-normalized when an adapter improves. Adding an agent means adding an `adapters/<agent>` crate, registering it in `gsd`'s `builtin_adapters()`, and adding it to the CLI's `connect` targets.

## Quick start

```sh
cargo install --path crates/gsd
cargo install --path crates/groundstation

groundstation connect claude-code   # installs hooks in ~/.claude/settings.json and starts gsd
claude                              # use Claude Code as usual

groundstation trajectories          # recent runs
groundstation show <id-prefix>      # one run as a timeline
```

```text
Fix the flaky checkout tests
3f2a9c1e-…  claude-code 2.1.3  idle
~/src/shop (fix/checkout)
started 2026-09-28 14:03:11   duration 4m18s
turns 1   model calls 8   tool calls 34 (2 failed)
tokens 84.2k   input 1.2k · cache read 78.1k · cache write 2.4k · output 2.5k

   +0:00.000  ▸ user         Fix the flaky checkout tests
   +0:02.113  ◆ model        20.1k → 2.1k  claude-opus-5-5
   +0:04.020  ● Read         src/checkout_test.rs                   12ms
   +0:09.500  ● Bash         cargo test                            47.2s  ✕ tool_error
   ...
longest tool calls
     47.2s  Bash cargo test
```

Use `--scope project` to commit the hooks to a repository's `.claude/settings.json`, or `--scope local` for `.claude/settings.local.json`. `groundstation disconnect claude-code` removes only Ground Station's hooks.

## How it works

```text
Claude Code ──hook (stdin JSON)──► groundstation hook claude-code ──HTTP──► gsd ──► SQLite
                                               │                              │
                                               └── gsd down? spool to disk ───┘ (drained every 2s)
                                                                              │
                                                    transcript .jsonl ────────┘ (model + token usage)
                                                                              │
                                                                              ▼
                                                           backend (mode = "cloud", gzip batches)
```

- **Hooks** give lifecycle, user turns and tool calls, timestamped when they fire. The hook never writes to stdout, never fails the agent, and gives up on the daemon after 250 ms, spooling the payload to disk instead.
- **Transcripts.** Hooks don't expose model usage, so `gsd` reads new lines from the session transcript (only under `~/.claude` or `$CLAUDE_CONFIG_DIR`) to record each model response with its model, input, output and cache tokens.
- **Idempotent everywhere.** Event ids are derived from the hook invocation or the transcript message id, so retries, spool replays and transcript re-reads never double-count.
- **Spans.** `tool.started` and `tool.completed`/`tool.failed` share a `span_id`, and `gsd` computes `gs.duration_ms` on the closing event.

## gsd

`gsd` listens on `127.0.0.1:4318` (override with `--listen` or `GROUNDSTATION_LISTEN`):

| Endpoint | |
|---|---|
| `POST /v1/events` | A `groundstation.telemetry.v0` batch, for SDKs and custom agents |
| `POST /v1/adapters/{name}` | An agent's hook payload wrapped in a `HookEnvelope`, translated by that adapter (`claude-code`) |
| `GET /v1/trajectories?limit=N` | Trajectory summaries, most recent first |
| `GET /v1/trajectories/{id}` | One trajectory and its events (unique id prefixes work) |
| `GET /v1/health` | Status, counters and transport mode |
| `POST /v1/shutdown` | Stop the daemon |

Because the daemon holds prompts and code, it only answers requests whose `Host` is a loopback name (which defeats DNS rebinding) and only accepts `application/json` bodies (browsers can't send those cross-origin without a preflight, and gsd never answers one).

Run it with `groundstation daemon start|stop|status`, or in the foreground with `gsd` or `groundstation daemon run`. Data lives in `~/.local/share/groundstation` (`gsd.db`, `spool/`, `gsd.log`).

## Configuration

`~/.config/groundstation/config.toml`. Every key is optional, and the values below are the defaults. Unknown keys are an error, so a typo can't silently turn redaction off.

```toml
[redaction]
secrets  = true           # Stripe, AWS, GitHub, OpenAI, Anthropic, Slack, JWT, PEM, passwords, URL credentials
paths    = "keep"         # or "hash": cwd, repository and file paths become stable sha256 prefixes
env      = ["*_KEY", "*_TOKEN", "*_SECRET", "DATABASE_URL"]
exclude  = []             # e.g. ["prompt", "tool.output.body"]
patterns = []             # extra regexes, each match becomes [REDACTED]

[transport]
mode     = "local-only"   # or "cloud" (requires endpoint)
# endpoint = "https://…"
# token    = "…"          # or GROUNDSTATION_TOKEN
batch_size = 500
flush_interval_secs = 5

[capture]
max_content_bytes = 65536 # per string; sizes are always recorded
raw = true                # keep the original payload; only when exclude is empty and paths = "keep"

[daemon]
listen = "127.0.0.1:4318"
# data_dir = "/absolute/path"  # default: ~/.local/share/groundstation
```

All of it runs inside `gsd`, before anything is written to disk or uploaded:

- **`exclude`** names content fields, with or without the `gs.` prefix: `prompt.text`, `tool.input.body`, `tool.output.body`, `shell.command`, `search.pattern`, `error.message` and `notification.message`. A dotted prefix covers several fields (`prompt`, or `tool` for both tool bodies). Measurements such as `gs.tool.output.bytes` are never removed. An entry that matches nothing is an error.
- **`env`** globs match variable names (case-insensitive). `NAME=value` and `"NAME": "value"` assignments for matching names are redacted anywhere they appear. The values those variables hold in gsd's own environment (8+ characters) are redacted literally too, so `DATABASE_URL`'s value is caught even when a command prints it without the name.
- **`paths = "hash"`** hashes path attributes and the path fields of tool input (`file_path`, `path` …). The same file still groups together, so repeated-read detection keeps working. Paths embedded in free text such as shell commands are not rewritten; use `exclude` for those.

## Telemetry schema

Events carry an `id`, `trajectory_id`, `kind`, `timestamp`, `agent`, an optional `span_id`, and `attributes`. Attributes use OpenTelemetry `gen_ai.*` names where a convention exists and `gs.*` for everything else (see [`crates/schema/src/attr.rs`](../crates/schema/src/attr.rs)).

| Kind | Claude Code source |
|---|---|
| `agent.started` / `agent.resumed` | `SessionStart` |
| `agent.completed` | `SessionEnd` |
| `turn.user` | `UserPromptSubmit` |
| `turn.completed` | `Stop` |
| `tool.started` | `PreToolUse` |
| `tool.completed` / `tool.failed` | `PostToolUse` / `PostToolUseFailure` |
| `subagent.started` / `subagent.completed` | `SubagentStart` / `SubagentStop` |
| `context.compacted` | `PreCompact` |
| `agent.notification` | `Notification` |
| `model.completed` | session transcript |

Unknown hook events are kept as `claude_code.<name>`, and unknown kinds round-trip, so older daemons accept newer producers.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
RUST_LOG=gsd=debug cargo run -p gsd -- --listen 127.0.0.1:14318
```

Set `GROUNDSTATION_DEBUG=1` to make the hook report errors on stderr. Set `GROUNDSTATION_CONFIG_DIR` and `GROUNDSTATION_DATA_DIR` to isolate a test install.
