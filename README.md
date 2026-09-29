# Ground Station

**Open-source observability for AI agents.** Agent Platform Monitoring.

Ground Station captures every model call, tool call, token and millisecond of an agent's execution, reconstructs it as a **trajectory**, and tells you why it was slow, what it cost, and what to change.

> Status: pre-alpha. The Cargo workspace is scaffolded (schema, daemon, CLI); there is no backend or UI yet and nothing is released. See [`product.md`](./product.md) for the full product thinking.

Website: [groundstation.sh](https://groundstation.sh) · Org: [github.com/groundstationhq](https://github.com/groundstationhq)

---

## Why

Agents are production systems, but they don't fail like services. A request returns HTTP 200 in 214 ms and the dashboard stays green while the agent behind it re-runs the same test suite 18 times, re-reads an unchanged file 27 times, fills its context window with stale tool output and burns $2.81 on a task that should cost $0.40.

Infrastructure monitoring sees the machine. Ground Station sees the execution.

The core abstraction is not the request. It is the **trajectory**: one complete agent run, from prompt to completion, with every turn, model call, tool call, observation and subagent in order, carrying the dimensions that matter (time, tokens, latency, cost, status).

```text
Trajectory
├── User turn        "Fix the flaky checkout tests."
├── Model call       claude-sonnet   18.2k → 2.1k tokens   1.8s   $0.06
├── Tool call        read_file       tests/checkout_test.rs        12ms
├── Tool call        grep            "timeout"                     31ms
├── Model call       claude-sonnet   21.4k → 1.4k tokens   1.2s
├── Tool call        edit_file       tests/checkout_test.rs  +14 −7
├── Tool call        shell           cargo test           47.2s   exit 101
├── Model call       claude-sonnet   38.7k → 1.9k tokens   2.4s
├── Tool call        shell           cargo test           46.9s   exit 0
└── Complete         1m 43s · 4 model calls · 7 tool calls · 126,467 tokens · $0.39
```

## What it does

The product climbs the DIKW hierarchy, and the hierarchy is also the roadmap:

| Level | Question | Ground Station | Version |
|---|---|---|---|
| **Data** | What happened? | Capture every event: model, tool, file, shell, browser, subagent, lifecycle | v0 |
| **Information** | What is happening? | Reconstruct events into a readable trajectory with metrics | v1 |
| **Knowledge** | Why is it happening? | Detect patterns: repeated commands, duplicate reads, context growth, regressions between agent versions | v2 |
| **Action** | What should change? | Recommend changes with estimated impact, grounded in the trajectory | v3 |

The founding thesis: **perfect data → exceptional information.** v0 does not need AI analysis. It needs to capture everything, losslessly and safely. Knowledge and action become possible once enough high-quality trajectories exist.

## Architecture

```text
Claude Code ─┐
Codex        ├──►  gsd  ──►  Ground Station backend  ──►  UI
OpenCode     │   (local Rust daemon)      (ingest · ClickHouse · query)
Custom / SDK ┘        │
                      └──►  any OTLP backend
```

- **Adapters** attach to an agent runtime through its own hook system, or wrap the process when there is none. No code changes to the agent.
- **`gsd`**, the local Rust daemon, handles ingestion, buffering, batching, compression, retry, sampling, redaction, secret filtering, local persistence, schema normalization and authentication. Redaction happens here, before anything leaves the machine. A local-only mode keeps every byte on the developer's machine.
- **Backend** ingests and stores high-cardinality event telemetry (ClickHouse) and serves the query API.
- **UI** is the trajectory viewer, fleet view, detections and analytics.
- **OpenTelemetry**: Ground Station does not replace OTel, it adds agent semantics. A trajectory is a trace, model and tool invocations are spans, events are span events, measurements are metrics. `gen_ai.*` attributes where a convention exists, `gs.*` for agent-specific fields. OTLP in and out.

## Repository layout

This is a monorepo: a Cargo workspace for everything in Rust, plus (later) the UI. Backend, daemon, CLI and UI share one telemetry schema, so they change together. SDKs for other languages will live in their own repositories once they exist.

```text
groundstationd/
├── Cargo.toml                  workspace (edition 2024, rust 1.88, resolver 3)
├── crates/
│   ├── schema/                 groundstation-schema: event types, identifiers, versioned telemetry schema
│   ├── gsd/                    local daemon (axum HTTP ingest, buffering, compression, redaction)
│   └── groundstation/          the `groundstation` CLI (login, connect, status)
├── product.md                  product brief
└── README.md
```

Planned additions, in order: `crates/gs-ingest` and `crates/gs-query` (backend over ClickHouse), `ui/` (trajectory viewer), `adapters/claude-code/` (first integration), `docs/`.

### Build

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace          # `clippy::all` is warn, `unsafe_code` is forbidden
```

## Planned developer experience

```sh
curl -fsSL https://groundstation.sh/install | sh
groundstation login
groundstation connect claude-code
claude                         # run the agent as usual; the trajectory appears
```

Configuration lives in `~/.config/groundstation/config.toml`:

```toml
[redaction]
secrets  = true                 # stripe, aws, github, openai, jwt, pem …
paths    = "hash"
env      = ["*_KEY", "*_TOKEN", "*_SECRET", "DATABASE_URL"]
exclude  = ["prompt", "tool.output.body"]

[transport]
mode     = "local-only"         # or "cloud"
```

## Identifiers

| | |
|---|---|
| Product | Ground Station |
| Category | Agent Platform Monitoring (APM) |
| CLI | `groundstation` |
| Daemon | `gsd` |
| Config | `~/.config/groundstation/config.toml` |
| Schema | `groundstation.telemetry.v0` |
| Default listen address | `127.0.0.1:4318` |

## Beta thesis

Do not try to support every agent. Instrument one coding agent so well that debugging it without Ground Station feels primitive. The success metric is the share of agent failures and performance problems that get investigated through Ground Station.

## Contributing

Not open for contributions yet. Watch the repository for the first `gsd` release.

## License

Apache-2.0. Declared at the workspace level; a `LICENSE` file will be added with the first release.
