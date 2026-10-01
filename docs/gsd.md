# gsd and the `groundstation` CLI

The local half of Ground Station: the `gsd` daemon, the `groundstation` CLI, and the agent adapters (Claude Code, Codex, OpenCode and pi).

| Crate | Binary | What it does |
|---|---|---|
| [`crates/schema`](../crates/schema) | | `groundstation.telemetry.v0`: event types and attribute names shared by adapters, SDKs, `gsd` and the backend |
| [`crates/api`](../crates/api) | | Wire types of the HTTP API (`Health`, `TrajectorySummary`, `IngestResponse` …), shared by `gsd`, the CLI and the hosted backend |
| [`crates/gsd`](../crates/gsd) | `gsd` | Local daemon. Ingests, redacts, stores and (optionally) uploads agent telemetry, translating hook payloads with the matching adapter |
| [`crates/groundstation`](../crates/groundstation) | `groundstation` | CLI. Connects agents, runs their hooks, manages `gsd`, and shows trajectories |
| [`adapters/claude-code`](../adapters/claude-code) | | Claude Code: installs hooks in `settings.json`, turns hook payloads and transcript lines into events. Sample payloads in `tests/fixtures/` |
| [`adapters/codex`](../adapters/codex) | | OpenAI Codex (CLI, IDE and desktop app): installs hooks in `hooks.json`, turns hook payloads and rollout lines into events. Sample payloads in `tests/fixtures/` |
| [`adapters/opencode`](../adapters/opencode) | | OpenCode v2+: generates a plugin (`plugins/groundstation.js`) that forwards OpenCode's event stream, and turns those events into Ground Station events. Sample payloads in `tests/fixtures/` |
| [`adapters/pi`](../adapters/pi) | | pi ([pi.dev](https://pi.dev)): generates an extension (`extensions/groundstation.js`) that forwards pi's lifecycle events, and turns those events into Ground Station events. Sample payloads in `tests/fixtures/` |
| [`crates/hooks-json`](../crates/hooks-json) | | Surgical install/uninstall of Ground Station hooks in the `hooks.json` shape Claude Code and Codex share, and of the generated OpenCode plugin and pi extension files (`managed`) |

Each adapter implements the `Adapter` trait from [`crates/schema/src/adapter.rs`](../crates/schema/src/adapter.rs): pure translation from an agent's native payloads to events, with no I/O. `gsd` owns storage, privacy and transcript reading, and keeps raw payloads so they can be re-normalized when an adapter improves. Adding an agent means adding an `adapters/<agent>` crate, registering it in `gsd`'s `builtin_adapters()`, and adding it to the CLI's `connect` targets.

## Quick start

```sh
cargo install --path crates/gsd
cargo install --path crates/groundstation

groundstation connect claude-code   # installs hooks in ~/.claude/settings.json and starts gsd
groundstation connect codex         # installs hooks in ~/.codex/hooks.json (then trust them in Codex: /hooks)
groundstation connect opencode      # installs a plugin in ~/.config/opencode/plugins/ (OpenCode 2+)
groundstation connect pi            # installs an extension in ~/.pi/agent/extensions/
claude                              # use your agents as usual

groundstation trajectories          # recent runs
groundstation show <id-prefix>      # one run as a timeline
groundstation resync claude-code    # re-read its transcripts, e.g. after upgrading
groundstation update                # install the latest release over an installer-made install and restart gsd
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

Use `--scope project` to put the hooks in the repository (`.claude/settings.json`, `.codex/hooks.json`, `.opencode/plugins/`, `.pi/extensions/`), or `--scope local` for Claude Code's `.claude/settings.local.json`. `groundstation disconnect <agent>` removes only Ground Station's hooks.

**Codex trusts hooks explicitly.** Codex runs hooks it doesn't manage only after you review them: after `connect codex`, open Codex, run `/hooks` and trust the `groundstation hook codex` entries. Reconnecting changes the hooks' hash, so trust them again after that. Project hooks load only once the project's `.codex/` layer is trusted.

**OpenCode 2+ uses a plugin, not hooks.** OpenCode has no command hooks; `connect opencode` checks that `opencode --version` is 2 or newer (v1 plugins don't load in v2 and vice versa) and writes `groundstation.js`, generated with gsd's address and the path of `groundstation` baked in. The plugin subscribes to OpenCode's event stream and `fetch`es the relevant events to gsd; when gsd is down it hands them to `groundstation hook opencode`, which spools them. Restart OpenCode (or `opencode service restart`) to load it. `disconnect opencode` deletes the file, and neither command touches a `groundstation.js` that Ground Station didn't write.

**pi uses an extension.** pi has no command hooks either; `connect pi` writes `groundstation.js` to `~/.pi/agent/extensions/` (or `$PI_CODING_AGENT_DIR/extensions/`, or `.pi/extensions/` with `--scope project`, which pi loads only for trusted projects). It listens to `session_start`/`session_shutdown`, `before_agent_start` (the prompt), `message_end` (assistant messages carry pi's own usage and cost accounting), `tool_execution_start`/`end`, `agent_end` and `session_compact`, and forwards them like the OpenCode plugin, with the same spool fallback. Restart pi or run `/reload` to load it.

## How it works

```text
Claude Code / Codex ──hook (stdin JSON)──► groundstation hook <agent> ──HTTP──► gsd ──► SQLite
                                                       │                              │
                                                       └── gsd down? spool to disk ───┘ (drained every 2s)
                                                                                      │
                                                   transcript / rollout .jsonl ───────┘ (model + token usage)
                                                                                      │
                                                                                      ▼
                                                                   backend (mode = "cloud", gzip batches)
```

- **Hooks** give lifecycle, user turns and tool calls, timestamped when they fire. The hook never writes to stdout, never fails the agent, and gives up on the daemon quickly (250 ms to connect, 2 s total; Codex kills `SessionEnd` and `Interrupt` hooks at 3 s), spooling the payload to disk instead. `gsd` replies as soon as the hook's events are stored and reads transcripts afterwards, so a long transcript never slows the agent.
- **Transcripts.** Claude Code and Codex hooks don't expose model usage, so `gsd` reads new lines from the session transcript to record each model response with its model, input, output and cache tokens: Claude Code's transcript (only under `~/.claude` or `$CLAUDE_CONFIG_DIR`) and Codex's rollout (`token_usage_record` lines, only under `~/.codex` or `$CODEX_HOME`). Codex counts cached tokens inside `input_tokens` and reasoning inside `output_tokens`; the adapter splits them so `in` always means uncached input, and keeps reasoning as `gs.usage.reasoning_output_tokens`. The read offset and any adapter state (Codex's current model) are saved per file, so each read continues where the last stopped. OpenCode and pi need no transcript: its `session.step.ended` events carry each model call's tokens, dollar cost and dispatch time (so latency too), and tool events carry OpenCode's own timings. pi's assistant messages carry the same (usage, cost, stop reason), and the extension measures each call's streaming time.
- **Idempotent everywhere.** Event ids are derived from the hook invocation or the transcript's message/response id, so retries, spool replays and transcript re-reads never double-count.
- **Git checkout.** Each event records the commit and branch checked out when it happened (`gs.vcs.revision`, `gs.vcs.branch`), so a trajectory open across several commits shows each one. `gsd` asks git when a hook arrives, at most every 2 s per trajectory, and always on a new user turn and after a shell command, where commits and checkouts happen. Model calls read from transcripts take the checkout of the latest event before them. The trajectory shows the latest.
- **Spans.** `tool.started` and `tool.completed`/`tool.failed` share a `span_id`, and `gsd` computes `gs.duration_ms` on the closing event.

## gsd

`gsd` listens on `127.0.0.1:4318` (override with `--listen` or `GROUNDSTATION_LISTEN`):

| Endpoint | |
|---|---|
| `POST /v1/events` | A `groundstation.telemetry.v0` batch, for SDKs and custom agents |
| `POST /v1/adapters/{name}` | An agent's hook payload wrapped in a `HookEnvelope`, translated by that adapter (`claude-code`, `codex`, `opencode`, `pi`) |
| `POST /v1/adapters/{name}/resync` | Re-read that adapter's transcripts from the start, so stored trajectories pick up what this version derives from them |
| `GET /v1/trajectories?limit=N` | Trajectory summaries, most recent first |
| `GET /v1/trajectories/{id}` | One trajectory and its events (unique id prefixes work) |
| `GET /v1/stats/tools?days=N` | Tool latency per category over the last N days (default 14): calls, failures, p50, p95 and max of `gs.duration_ms` |
| `GET /v1/health` | Status, counters, transport mode and the last upload failure. `deployment` is `local` and `features` is empty; the hosted backend serves the same shape without the fields that describe one machine (`pid`, `data_dir`, `spool_pending`, `upload`) |
| `POST /v1/shutdown` | Stop the daemon |

Because the daemon holds prompts and code, it only answers requests whose `Host` is a loopback name (which defeats DNS rebinding) and only accepts `application/json` bodies (browsers can't send those cross-origin without a preflight, and gsd never answers one).

Run it with `groundstation daemon start|stop|status`, or in the foreground with `gsd` or `groundstation daemon run`. `groundstation update` installs the latest GitHub Release into the installer's layout (`~/.local/share/groundstation/releases/`, linked from `~/.local/bin`) and restarts a running gsd; `--check` only reports, `--to 0.1.1` picks a version. Installs made with cargo or a package manager are left alone with a hint. `groundstation status` warns when the running gsd's version differs from the CLI's. Data lives in `~/.local/share/groundstation` (`gsd.db`, `spool/`, `gsd.log`), created readable by your user only.

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

**Uploading.** With `mode = "cloud"`, gsd sends queued events to `{endpoint}/v1/events` in gzip batches of up to `batch_size`, in the order they were stored. Each uploaded event also carries its trajectory's `gs.host.name` and `gs.vcs.repository` (under the path policy); the stored events don't change. Network errors, auth failures, rate limits (`retry-after` is honored) and server errors leave the queue untouched and retry with backoff up to 5 minutes. A 413 halves the batch. The backend can refuse single events while accepting the rest (`rejected` in the response). A refused event stays queued and is dropped from the queue after 3 refusals, so one bad event can't block the ones behind it. Dropped events stay in the local store, and `groundstation status` counts them. The most recent failure (a revoked token, a backend that's down) is reported as `upload.last_error` in `/v1/health` and by `groundstation status` until the next upload succeeds, so a growing queue comes with a reason.

## Telemetry schema

Events carry an `id`, `trajectory_id`, `kind`, `timestamp`, `agent`, an optional `span_id`, and `attributes`. Attributes use OpenTelemetry `gen_ai.*` names where a convention exists and `gs.*` for everything else (see [`crates/schema/src/attr.rs`](../crates/schema/src/attr.rs)).

| Kind | Claude Code | Codex | OpenCode (event) | pi (event) |
|---|---|---|---|---|
| `agent.started` / `agent.resumed` | `SessionStart` | `SessionStart` | `session.created` | `session_start` |
| `agent.completed` | `SessionEnd` | `SessionEnd` | `session.deleted` | `session_shutdown` |
| `agent.failed` | | | `session.execution.failed` | `agent_end` (stop reason `error`) |
| `turn.user` | `UserPromptSubmit` | `UserPromptSubmit` | `session.inbox.enqueued` (user item) | `before_agent_start` |
| `turn.completed` | `Stop` | `Stop` | `session.execution.succeeded` | `agent_end` |
| `turn.interrupted` | | `Interrupt` | `session.execution.interrupted` | `agent_end` (stop reason `aborted`) |
| `tool.started` | `PreToolUse` | `PreToolUse` | `session.tool.called` | `tool_execution_start` |
| `tool.completed` / `tool.failed` | `PostToolUse` / `PostToolUseFailure` | `PostToolUse` (failed when the exit code is non-zero) | `session.tool.success` / `session.tool.failed` (or non-zero exit) | `tool_execution_end` (`isError` or non-zero exit) |
| `subagent.started` / `subagent.completed` | `SubagentStart` / `SubagentStop` | `SubagentStart` / `SubagentStop` | child `session.created` / child `session.execution.*` |  |
| `context.compacted` | `PreCompact` | `PreCompact` | `session.compaction.started` | `session_compact` |
| `agent.notification` | `Notification` | `PermissionRequest` | `permission.asked` |  |
| `model.completed` | session transcript | rollout `token_usage_record` | `session.step.ended` / `step.failed` / `compaction.ended` | `message_end` (assistant) |

OpenCode subagents run as child sessions; the plugin tracks each session's parent and gsd folds child sessions into the root session's trajectory, marking their model and tool calls as `gs.sidechain`. OpenCode and pi report cost, so `gs.cost.usd` (and `cost_usd` on trajectory summaries) is filled for those two agents; Claude Code and Codex report tokens only.

Codex fires tool hooks for shell commands, `apply_patch` (reported with the patched files), MCP tools and local function tools; hosted tools such as web search fire none, so they don't appear as tool calls.

Unknown hook events are kept as `claude_code.<name>`, `codex.<name>`, `opencode.<event>` or `pi.<event>`, and unknown kinds round-trip, so older daemons accept newer producers.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
RUST_LOG=gsd=debug cargo run -p gsd -- --listen 127.0.0.1:14318
```

Set `GROUNDSTATION_DEBUG=1` to make the hook report errors on stderr. Set `GROUNDSTATION_CONFIG_DIR` and `GROUNDSTATION_DATA_DIR` to isolate a test install.
