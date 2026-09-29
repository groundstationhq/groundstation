# Ground Station — Agent Platform Monitoring

## Executive Summary

**Ground Station** (category: Agent Platform Monitoring) is an open-source observability platform built specifically for AI agents.

Traditional observability platforms such as Datadog, Grafana, and OpenTelemetry help engineering teams understand distributed software systems: requests, services, traces, logs, errors, latency, resource utilization, and infrastructure.

Agents introduce a new execution model.

Instead of a predictable request moving through a fixed collection of services, an agent executes a **trajectory**:

**Prompt → reasoning/context → model call → tool call → observation → model call → tool call → … → completion**

These trajectories can last seconds, minutes, or hours. They may consume millions of tokens, execute hundreds of tools, modify files, interact with browsers, call APIs, spawn subagents, fail silently, repeat work, or become stuck in loops.

Existing infrastructure monitoring can show that a process used CPU or that an API returned HTTP 200.

It cannot easily answer:

- Why did this agent take 18 minutes?
- Which tool call caused the slowdown?
- What context did the model have when it made a decision?
- Why did the agent call the same tool 47 times?
- Which model consumed the most tokens?
- How much did this agent run cost?
- Which agents are failing most frequently?
- Which repositories create the longest trajectories?
- Where are agents getting stuck?
- Which prompts lead to excessive tool usage?
- Which agent versions became slower after deployment?
- What happened immediately before an agent failed?
- Are agents behaving differently after a model upgrade?

Ground Station provides the missing observability layer.

The product captures the complete execution trajectory of agents and transforms raw execution data into structured telemetry, operational understanding, and eventually automated recommendations.

The long-term product vision is simple:

> **Datadog for agents.**

---

# The Ground Station Intelligence Pyramid

A core part of Ground Station's product philosophy is inspired by the **DIKW hierarchy**:

**Data → Information → Knowledge → Wisdom**

Most observability systems stop somewhere between data and information.

Ground Station's ambition is to climb the entire hierarchy.

For agents, that progression looks like this:

```text
                    WISDOM
              What should we do?
                     ▲
                     │
                   KNOWLEDGE
            Why is this happening?
                     ▲
                     │
                  INFORMATION
             What is happening?
                     ▲
                     │
                     DATA
             What actually happened?
```

Ground Station starts by capturing every meaningful part of an agent trajectory and progressively transforms that telemetry into increasingly useful operational intelligence.

---

## Level 1 — Data

At the bottom of the pyramid is raw agent execution.

Ground Station captures facts.

```text
model.started
model.completed
tool.started
tool.completed
file.read
file.write
shell.executed
agent.started
agent.completed
agent.failed
```

And associated measurements:

```text
timestamp
duration
input tokens
output tokens
context size
tool input
tool output
exit code
response size
cost
error
```

Example:

```text
14:03:14.821 tool.started
tool = shell
command = cargo test

14:04:02.014 tool.completed
duration = 47.193s
exit_code = 1
stdout_bytes = 18,421
stderr_bytes = 2,814
```

This is **data**.

It tells us what happened.

But raw events alone do not give the user understanding.

---

# Level 2 — Information

Ground Station organizes those events into a coherent **trajectory**.

Instead of presenting a developer with thousands of disconnected log lines, Ground Station reconstructs the execution:

```text
User asked:
"Fix the flaky checkout tests."

        ↓

Model analyzed repository

        ↓

Read checkout_test.rs

        ↓

Searched for timeout usage

        ↓

Edited checkout_test.rs

        ↓

Ran cargo test
47.2 seconds
FAILED

        ↓

Model inspected failure

        ↓

Ran cargo test again
46.9 seconds

        ↓

Task completed
```

Now the telemetry has context.

Ground Station can tell the user:

```text
Trajectory duration:     4m 18s

Model calls:             8
Tool calls:              34

Tokens:                  84,214
Cost:                    $0.47

Longest operation:
cargo test               47.2s

Total test runtime:
141.3s
```

This is **information**.

It answers:

> **What happened during this agent execution?**

---

# Level 3 — Knowledge

The next layer identifies patterns across trajectories.

Ground Station does not merely show that `cargo test` took 47 seconds.

It can recognize that:

```text
cargo test

executed:          18 times
cumulative time:   14m 42s
percentage run:    61%
```

Or:

```text
read_file(src/auth.rs)

31 executions

File changed once.

27 reads returned identical content.
```

Or:

```text
Agent v42

Average tokens/task:
+81%

Average runtime:
+47%

Success rate:
unchanged
```

Ground Station now understands relationships within the telemetry.

The product can explain:

> The trajectory was slow because the agent repeatedly executed the same expensive test suite.

or:

> Token usage increased because tool output grew the context from 32k to 178k tokens.

or:

> The new agent version performs substantially more repository searches before editing files.

This is **knowledge**.

It answers:

> **Why is this happening?**

---

# Level 4 — Wisdom

The top layer transforms that knowledge into action.

Instead of merely identifying a problem, Ground Station can recommend what to change.

For example:

```text
⚡ Optimization Opportunity

61% of this trajectory was spent executing:

cargo test

The agent executed the complete test suite 18 times.

Recommendation:

Run the affected package tests during iteration
and execute the full test suite only before completion.

Estimated impact:

Runtime     -48%
Tool calls  -17
Compute     -43%
```

Or:

```text
⚡ Context Optimization

96k tokens of the final context originated
from repeated test output.

Recommendation:

Truncate successful test output to the final
20 lines before adding it to agent context.

Estimated impact:

Context size   -53%
Model cost     -31%
```

Or across an entire organization:

```text
Ground Station Recommendation

Agents working in monorepo repositories consume
2.4x more tokens than agents working in smaller repositories.

Primary contributor:

repository search output

Recommendation:

Introduce repository-aware search limits and
exclude generated directories.
```

This is **wisdom**.

It answers:

> **What should we do about it?**

---

# The Product Journey

The DIKW pyramid also provides a natural product roadmap.

```text
V0
DATA

Capture everything.
      ↓

V1
INFORMATION

Make trajectories understandable.
      ↓

V2
KNOWLEDGE

Detect patterns and explain behavior.
      ↓

V3
WISDOM

Recommend how agents should be improved.
```

The beta therefore does not need sophisticated AI recommendations.

The first job is to create the best possible foundation:

> **Perfect data → exceptional information.**

Once enough high-quality trajectories have been collected, knowledge and wisdom become possible.

---

# Marketing Narrative

This framework can become one of the primary narratives on the Ground Station website.

### Hero

> **Turn agent data into understanding.**

Capture every trajectory, model call, tool call, error, token, and millisecond—and understand exactly what your agents are doing.

---

### Supporting Message

> **Data → Information → Knowledge → Wisdom**

Your agents generate enormous amounts of execution data.

Ground Station transforms it into information you can explore, knowledge you can act on, and eventually intelligence that helps you build better agents.

---

### Expanded Version

> **Transform agent data into information.  
> Transform information into knowledge.  
> Transform knowledge into action.**

Ground Station captures complete agent trajectories and turns them into the insights engineering teams need to operate autonomous software reliably.

For external messaging, **“action”** may sometimes work better than the academic term **“wisdom”**, while the underlying product framework remains DIKW.

---

# Ground Station's Value Ladder

The same idea can be expressed directly through the product.

```text
DATA

"We captured 18 tool calls."
        ↓

INFORMATION

"These 18 calls consumed
14 minutes of the trajectory."
        ↓

KNOWLEDGE

"The agent repeatedly ran
the full test suite unnecessarily."
        ↓

WISDOM

"Run targeted tests during iteration.
This would reduce runtime by ~48%."
```

This is the core transformation Ground Station provides.

It is not merely:

> Store more logs.

It is:

> **Turn agent execution into understanding.**

---

# Product Vision

Ground Station becomes the standard observability infrastructure for agentic software.

Any agent should be able to emit telemetry into Ground Station regardless of whether it is:

- Claude Code
- Codex
- OpenCode
- Pi
- Hermes
- OpenClaw
- an internal coding agent
- a customer-support agent
- a browser agent
- a research agent
- an autonomous production workflow
- a custom agent framework

Ground Station should eventually become the place where an engineering organization can answer:

> **What are my agents doing right now?**

then:

> **Why are they behaving this way?**

and eventually:

> **How should I improve them?**

---

# The Core Abstraction: The Agent Trajectory

Traditional APM revolves around requests and distributed traces.

Agent Platform Monitoring revolves around the **trajectory**.

A trajectory represents one complete agent execution.

```text
Trajectory
│
├── User Turn
│   └── "Refactor authentication to use OAuth"
│
├── Model Turn
│   ├── model
│   ├── input_tokens
│   ├── output_tokens
│   └── latency
│
├── Tool Call
│   ├── read_file
│   ├── src/auth.ts
│   └── 34ms
│
├── Tool Call
│   ├── grep
│   └── 87ms
│
├── Model Turn
│
├── Tool Call
│   ├── edit_file
│   └── src/auth.ts
│
├── Tool Call
│   ├── shell
│   ├── npm test
│   └── 41.2s
│
├── Model Turn
│
└── Completion
```

Ground Station stores the complete timeline.

Every event becomes searchable and measurable.

---

# What Ground Station Captures

Ground Station captures several classes of telemetry.

## Agent Lifecycle

```text
agent.started
agent.resumed
agent.paused
agent.completed
agent.failed
agent.cancelled
```

Metadata can include:

- agent type
- agent version
- model
- repository
- branch
- working directory
- host
- environment
- organization
- user
- session ID
- trajectory ID

---

## Turns

Capture every conversational turn when available.

```text
user_turn
assistant_turn
system_turn
tool_result
subagent_turn
```

Each turn may contain:

- timestamp
- sequence number
- role
- content size
- token count
- model
- latency
- context size

Depending on privacy configuration, the actual content can be retained, redacted, hashed, sampled, or excluded.

---

# Model Calls

Each LLM invocation becomes a first-class span.

```text
model_call

model: <model>
provider: <provider>

input_tokens: 82,431
output_tokens: 4,231

latency: 7.81s
estimated_cost: $0.42
context_utilization: 71%
```

This enables dashboards for:

- tokens per trajectory
- model latency
- cost per trajectory
- context-window utilization
- cache hit rate
- output tokens
- retries
- provider errors
- model comparisons
- cost by team
- cost by repository

---

# Tool Calls

Tool calls are another first-class primitive.

Examples:

```text
shell
read_file
write_file
edit_file
grep
search
browser
http_request
git
database
MCP tool
subagent
custom tool
```

Every tool call can include:

```text
tool.name
tool.category
started_at
completed_at
duration
success
error
input_size
output_size
exit_code
metadata
```

Tool-specific telemetry can then extend the schema.

For a shell call:

```text
command
exit_code
stdout_bytes
stderr_bytes
duration
```

For a file operation:

```text
path
operation
bytes_read
bytes_written
```

For HTTP:

```text
host
method
status
latency
request_bytes
response_bytes
```

---

# Agent Errors

Errors should be normalized into categories.

Examples:

```text
model_error
tool_error
timeout
rate_limit
authentication_error
context_overflow
agent_crash
permission_error
loop_detected
user_cancelled
provider_error
```

Ground Station can automatically aggregate these:

```text
Top Agent Errors — Last 24 Hours

Tool timeout                 38%
Model rate limit             21%
Command failure              17%
Context overflow              9%
Agent crash                   8%
Other                         7%
```

---

# Agent-Specific Metrics

Agents create entirely new observability metrics.

### Trajectory

```text
trajectory.duration
trajectory.turns
trajectory.tool_calls
trajectory.model_calls
trajectory.tokens
trajectory.cost
trajectory.errors
```

### Efficiency

```text
tokens / successful task
tool calls / successful task
time / successful task
cost / successful task
```

### Behavioral

```text
repeated_tool_calls
duplicate_file_reads
failed_tool_ratio
retry_count
context_growth_rate
subagent_count
```

These enable Ground Station to move upward through the intelligence pyramid from raw telemetry toward automated understanding.

---

# Agent Trace Viewer

The flagship experience is a visual **trajectory explorer**.

Instead of looking at JSON logs, users see an interactive execution timeline.

```text
00:00      User Prompt
           "Fix flaky checkout tests"

00:02      Model
           18k → 2.1k tokens
           1.8s

00:04      read_file
           checkout_test.rs
           12ms

00:04      grep
           "timeout"
           31ms

00:06      Model
           21k → 1.4k
           1.2s

00:08      edit_file
           checkout_test.rs
           +14 -7

00:09      shell
           cargo test
           ████████████████████
           47.2s

00:57      Model

01:01      shell
           cargo test
           46.9s

01:49      Complete
```

Users should instantly see where time, tokens, tools, and money were spent.

Selecting any span opens contextual information:

```text
Tool Call

cargo test

Duration
47.2 seconds

Exit Code
1

stdout
...

stderr
...

Previous model turn
...

Next model turn
...
```

The experience should feel closer to a modern performance profiler than a raw log viewer.

---

# Sessions and Live Monitoring

Ground Station should provide a real-time view.

```text
LIVE AGENTS

Agent        Repository       Duration   Tools   Tokens     Status

Claude       payments         03:21      14      84k        Running
Codex        web              00:41       7      23k        Running
Claude       backend          18:43      91     418k        ⚠ Slow
OpenCode     infra            04:11      22      67k        Running
```

An engineer can click a running agent and watch its trajectory evolve in real time.

That capability makes Ground Station useful not only for postmortems but also as an **operations console for fleets of agents**.

---

# Automatic Agent Intelligence

Over time, Ground Station analyzes trajectories automatically.

Examples include:

### Slow Agent

```text
⚠ Slow trajectory detected

Duration: 24m 32s
Typical duration: 6m 18s

Primary contributor:

cargo test
18 executions
14m 42s cumulative runtime
```

### Tool Loop

```text
⚠ Potential agent loop detected

read_file(src/api.rs)

Executed 31 times within 4 minutes.
File changed only once.
```

### Cost Regression

```text
⚠ Cost regression

Agent version 1.42

Average task cost
$0.41 → $0.87

+112%
```

### Context Explosion

```text
⚠ Context growth anomaly

Context:
24k → 181k tokens

Largest contributor:
tool output

96k tokens
```

This is where Ground Station evolves from telemetry infrastructure into an **agent reliability platform**: the transition from **information** to **knowledge**.

Recommendations on how to remediate these behaviors represent the final **wisdom/action** layer.

---

# Alerts

Organizations eventually define monitors similar to Datadog.

```text
trajectory.duration > 10m

trajectory.cost > $5

tool.error_rate > 5%

provider.rate_limit > 10/min

context_utilization > 90%

loop_detected == true

agent.failure_rate > 2%
```

Notifications can go to:

- Slack
- PagerDuty
- email
- webhooks
- Microsoft Teams

Example:

```text
🚨 Ground Station Alert

Coding Agent Failure Rate

Repository:
payments-api

Failure rate exceeded 10%

Current:
17.8%

Baseline:
2.1%

View affected trajectories →
```

---

# Integrations

Ground Station supports agents through two mechanisms.

## Native Agent Adapters

Popular agents receive zero-code integrations.

```text
groundstation connect claude-code
groundstation connect codex
groundstation connect opencode
```

Depending on what the agent exposes, adapters can use:

- hooks
- plugins
- callbacks
- logs
- event streams
- wrappers
- MCP
- local IPC

The experience should approach:

```text
install → run agent → telemetry appears
```

Ideally setup requires less than one minute.

---

# Ground Station SDK

Custom agents use an instrumentation SDK.

Potential languages:

```text
Rust
Python
TypeScript
Go
```

Conceptually:

```python
import groundstation as gs

with gs.trajectory("fix-bug"):
    response = agent.run(...)

    with gs.tool("database_query"):
        ...
```

The long-term goal is automatic instrumentation rather than forcing users to manually wrap every operation.

---

# Local Daemon: gsd

The centerpiece of the collection architecture is a small Rust daemon:

```text
gsd
```

Architecture:

```text
Claude Code
     │
Codex
     │
OpenCode
     │
Custom Agent
     │
     ▼
┌──────────────┐
│     gsd      │
│ Rust daemon  │
└──────┬───────┘
       │
       │ batch / compress
       ▼
┌──────────────┐
│  GS backend  │
└──────────────┘
```

The daemon handles:

- event ingestion
- buffering
- batching
- compression
- retry
- sampling
- redaction
- secrets filtering
- local persistence
- schema normalization
- authentication

Agents should never need to care whether the backend is temporarily unavailable.

---

# OpenTelemetry Compatibility

Ground Station should not attempt to replace OpenTelemetry.

Instead:

```text
trajectory       → trace

model invocation → span

tool invocation  → span

events           → span events

measurements     → metrics
```

Ground Station adds higher-level agent semantics.

This gives customers a path to integrate agent telemetry with the rest of their infrastructure.

Eventually:

```text
User Request

Frontend
   │
API
   │
Agent
   │
Model
   │
Tool
   │
Database
```

can become one continuous distributed trace.

---

# Backend Architecture

The backend should be optimized for extremely high-cardinality event telemetry.

A potential architecture:

```text
                   ┌──────────────┐
Agents ──► gsd ──►│ Ingestion API│
                   └──────┬───────┘
                          │
                    event stream
                          │
              ┌───────────▼───────────┐
              │ Processing / Routing  │
              └───────────┬───────────┘
                          │
                  ┌───────▼───────┐
                  │  ClickHouse   │
                  └───────┬───────┘
                          │
                 ┌────────▼────────┐
                 │ Query API       │
                 │ Rust            │
                 └────────┬────────┘
                          │
                         UI
```

Rust is particularly appropriate for:

- the local collector
- ingestion
- telemetry processing
- high-throughput query services
- low memory overhead
- predictable latency

---

# Storage

**ClickHouse** is a strong candidate for the primary telemetry datastore.

The workload is naturally:

- append-heavy
- high-volume
- time-oriented
- high-cardinality
- analytical
- aggregation-heavy

Potential tables:

```text
trajectories
turns
model_calls
tool_calls
events
errors
metrics
```

Large text payloads may eventually use object storage.

```text
ClickHouse

metadata
indexes
dimensions
metrics
timestamps
references

S3 / object storage

full prompts
tool outputs
large context snapshots
artifacts
```

This keeps expensive analytical queries fast without forcing huge text blobs into the hot path.

---

# Privacy and Security

Agent telemetry can contain:

- source code
- API keys
- credentials
- customer data
- internal documents
- database records
- proprietary prompts

Privacy therefore needs to be architectural.

The local daemon should eventually support:

```text
redact secrets
redact paths
redact environment variables
redact regex matches
exclude tool outputs
exclude prompts
hash user identity
sample trajectories
local-only mode
```

Organizations control what leaves their machines.

---

# Open Source Strategy

The core Ground Station stack should be open source.

Potential components:

```text
gsd
SDKs
agent adapters
telemetry schema
local development UI
basic backend
```

The open-source strategy creates several advantages:

- developers can instrument new agents themselves
- organizations can inspect exactly what is collected
- integrations can be community-maintained
- the telemetry schema can become a standard

The strategic goal is eventually to make:

> **“Send it to Ground Station.”**

a normal sentence in the agent ecosystem.

---

# Commercial Product

The hosted platform monetizes the operational layer.

Possible paid capabilities:

```text
long-term retention
large telemetry volumes
team dashboards
alerting
RBAC
SSO/SAML
audit logs
advanced analytics
anomaly detection
cost analytics
fleet monitoring
enterprise integrations
compliance
private cloud
```

Pricing can eventually be based on:

```text
events ingested
GB ingested
trajectories
retention
active agents
```

Usage-based pricing is probably the most natural long-term model because agent execution volume can vary dramatically between customers.

---

# Target Customer

The first customer should be:

> **A software engineering organization running coding agents extensively.**

Examples:

- AI-native startups
- coding-agent companies
- teams using Claude Code or Codex heavily
- companies building internal coding agents
- companies running coding agents in CI
- companies operating concurrent remote coding environments

These teams experience the observability problem much earlier than normal enterprises.

---

# Initial Beachhead: Coding Agents

Coding agents are an ideal entry point because their execution is highly measurable.

They generate:

```text
prompts
model calls
file reads
file writes
shell commands
git operations
tests
compiler output
subagents
network requests
tool errors
```

And they run tasks long enough for performance problems to matter.

Their success is measurable:

```text
Did the agent finish?

How long did it take?

How much did it cost?

How many actions did it perform?

What failed?
```

That makes coding agents an ideal proving ground for agent observability.

---

# Beta

The beta should intentionally be narrow.

Do **not** attempt to support every agent.

Build one integration extremely well.

A strong beta target would be one major coding agent with enough hooks or event surfaces to capture useful lifecycle data.

The beta contains four pieces.

## 1. Rust Collector

```text
gsd
```

Responsibilities:

- receive events
- normalize telemetry
- queue locally
- batch
- compress
- upload
- redact sensitive fields

## 2. One Coding-Agent Integration

Capture:

```text
trajectory start/end
user turns
assistant turns where available
model calls where available
tool calls
tool results
errors
timestamps
token information where available
```

Installation should eventually resemble:

```bash
curl -fsSL https://groundstation.sh/install | sh

groundstation login

groundstation connect <agent>
```

Then the user simply launches the agent normally.

## 3. Rust Backend + ClickHouse

Initial API:

```text
POST /v1/events

GET /v1/trajectories

GET /v1/trajectories/:id

GET /v1/metrics
```

Avoid prematurely building dozens of microservices.

A single well-designed Rust service plus ClickHouse is enough for the beta.

## 4. Web UI

The beta needs several exceptional screens.

### Agent Overview

```text
Agents

1,284 trajectories
92.4% successful

Median runtime     3m 41s
P95 runtime       14m 22s
Tokens             84M
Estimated cost     $742
Tool errors        1,421
```

### Trajectory List

```text
Task                     Duration   Tools   Tokens   Status

Fix OAuth bug              4:31       31     82k     ✓
Refactor payments         18:41      114    298k     ⚠
Update dependencies        1:12        9     21k     ✓
Fix integration tests     42:13      281    710k     ✕
```

### Trajectory Viewer

Interactive visualization of every turn and tool call.

### Basic Analytics

```text
trajectory duration
token consumption
tool latency
tool usage
failure rate
cost
```

---

# What NOT to Build in the Beta

Avoid:

```text
PagerDuty
Slack alerts
enterprise RBAC
SAML
AI anomaly detection
ten agent integrations
complex billing
distributed collectors
Kubernetes operators
custom query languages
mobile apps
hundreds of dashboards
```

The beta needs to answer one question:

> **When an agent behaves badly, does Ground Station make understanding what happened dramatically easier?**

---

# Beta Success Metric

The key metric should be:

> **Percentage of agent failures or performance problems investigated through Ground Station.**

The desired behavior:

```text
Agent behaved strangely
        ↓
Open Ground Station
```

---

# Product Design

The interface should feel significantly more modern than traditional enterprise observability software.

Principles:

### Extremely fast

Navigation and queries should feel nearly instantaneous.

### Dense but readable

Thousands of events without visual overload.

### Visual hierarchy

Failures, expensive model calls, long-running tools, and anomalies immediately stand out.

### Dark-first

Deep neutral surfaces with bright telemetry accents.

### Motion

Subtle animations communicate:

```text
model running
tool executing
subagent spawning
trajectory progressing
```

The product should feel like watching autonomous software execute rather than searching through log files.

---

# Long-Term Product Surface

## Agent Reliability

Detect:

```text
loops
stalls
runaway token consumption
tool failures
context explosion
provider degradation
```

## Agent Economics

Measure:

```text
cost per task
cost per successful task
cost per developer
cost per repository
model efficiency
```

## Agent Evaluation

Compare releases and configurations.

```text
Agent v41

success       81%
median time   8m 11s
cost          $0.82

Agent v42

success       89%
median time   5m 42s
cost          $0.61
```

## Agent Fleet Management

Monitor thousands of concurrently executing agents.

```text
4,192 active agents
184 degraded
23 stuck
18 failed
```

## Agent Security

Detect:

```text
unexpected network access
secret exposure
suspicious commands
permission escalation
unusual file access
```

---

# Strategic Moat

The eventual moat is not simply storing traces.

The strongest defensibility comes from several layers.

### Agent integrations

Deep integrations with popular agent runtimes.

### Agent telemetry schema

A common vocabulary for describing autonomous execution.

### Historical trajectories

Understanding normal and abnormal behavior across huge numbers of agent executions.

### Detection engine

Algorithms that identify loops, waste, regressions, and failures.

### Intelligence layer

Turning those detections into actionable recommendations.

### Developer workflow

Developers instinctively opening Ground Station when an agent behaves unexpectedly.

---

# Positioning

The product should avoid being positioned merely as an LLM tracing platform.

The category is:

> **Agent Observability**

with the product category:

> **Agent Platform Monitoring**

Core message:

> **Your agents are production systems. Monitor them like production systems.**

Alternative:

> **Observability for autonomous software.**

---

# The Name

The product is **Ground Station**. The category is **Agent Platform Monitoring (APM)**.

A ground station is the facility that receives telemetry from an autonomous craft, reconstructs its trajectory, and tells the operators what it is doing. That is a literal description of this product: agents run autonomously, emit telemetry, and Ground Station turns it back into a trajectory the team can read. The name is descriptive without being "agent-something", which keeps it apart from AgentOps, AgentScope and the rest of the crowded "agent + noun" space.

The category name keeps the deliberate collision with the older meaning of Ground Station:

```text
Traditional:   APM  →  Application Performance Monitoring
Agent era:     APM  →  Agent Platform Monitoring
```

```text
Applications became distributed systems.

Distributed systems created Application Performance Monitoring.

Software is becoming agentic.

Agentic systems need Agent Platform Monitoring.
```

Use the collision as a category-defining moment (the landing page morphs one phrase into the other), never as the brand. "Ground Station" alone is unsearchable and belongs to a twenty-year-old market; "Ground Station" is ownable.

## Identifiers

| Thing | Name |
|---|---|
| Product / wordmark | Ground Station |
| Category line | Agent Platform Monitoring |
| Domain | groundstation.sh (landing live; `v2.groundstation.sh` hosts the second landing candidate) |
| GitHub org | `groundstationhq` |
| CLI | `groundstation` (`groundstation login`, `groundstation connect <agent>`) |
| Local daemon | `gsd` (Rust). Not `gs`, which collides with Ghostscript on most machines |
| Install | `curl -fsSL https://groundstation.sh/install \| sh` |
| Config | `~/.config/groundstation/config.toml` |
| Telemetry schema | `groundstation.telemetry.v0` |
| OpenTelemetry attributes | `gen_ai.*` where a semantic convention exists, `gs.*` for agent-specific fields |
| Default listen address | `127.0.0.1:4318` (OTLP-compatible) |

Both npm (`groundstation`) and crates.io (`groundstation`, `gsd`) should be claimed early; check before publishing, availability was only verified for the GitHub org and domain.

---

# One-Sentence Pitch

> **Ground Station is open-source observability for AI agents: it captures every model call, tool call and token of an agent's run, reconstructs it as a trajectory, and tells you why it was slow, what it cost, and what to change.**

---

# Short Pitch

AI agents are becoming production software, but existing observability tools cannot explain their behavior.

Ground Station is an open-source, Rust-based observability platform designed specifically for agents. It captures complete trajectories—including model calls, tool calls, turns, context, errors, latency, tokens, and cost—and transforms that data into understandable traces, metrics, insights, and eventually recommendations.

**Data → Information → Knowledge → Wisdom.**

Think **Datadog for agents**.

Start with coding agents. Expand to every autonomous software system.

---

# Beta Thesis

The first version does not need to prove that Ground Station can monitor every agent.

It needs to prove:

> **Instrument one coding agent so well that debugging that agent without Ground Station feels primitive.**

Then:

```text
one coding agent
        ↓
all coding agents
        ↓
custom engineering agents
        ↓
production agents
        ↓
enterprise agent fleets
```

And simultaneously:

```text
DATA
capture execution
        ↓
INFORMATION
understand trajectories
        ↓
KNOWLEDGE
explain agent behavior
        ↓
WISDOM
improve agent behavior
```

The long-term goal is not simply to observe agents.

It is to build the system that turns **agent execution into understanding**.

The ultimate goal is for agent developers to treat telemetry the same way application developers treat logs, metrics, and traces today:

**something you would never ship without.**