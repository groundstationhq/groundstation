/**
 * Wire types. Mirror `crates/schema` (groundstation.telemetry.v0) and
 * `crates/api` exactly. Change them in the same PR as the Rust side.
 */

export const SCHEMA = "groundstation.telemetry.v0";

export type EventKind =
  | "agent.started"
  | "agent.resumed"
  | "agent.paused"
  | "agent.completed"
  | "agent.failed"
  | "agent.cancelled"
  | "agent.notification"
  | "turn.user"
  | "turn.completed"
  | "model.started"
  | "model.completed"
  | "tool.started"
  | "tool.completed"
  | "tool.failed"
  | "subagent.started"
  | "subagent.completed"
  | "context.compacted"
  | (string & {});

export type ToolCategory = "shell" | "file_read" | "file_write" | "search" | "http" | "browser" | "subagent" | "mcp" | "planning" | "custom";

export interface Agent {
  name: string;
  version?: string;
}

export interface Event {
  id: string;
  trajectory_id: string;
  kind: EventKind;
  timestamp: string; // RFC 3339
  agent: Agent;
  span_id?: string;
  parent_span_id?: string;
  attributes: Record<string, unknown>;
}

/** Attribute keys, same strings as `groundstation_schema::attr`. */
export const attr = {
  GEN_AI_PROVIDER: "gen_ai.provider.name",
  GEN_AI_RESPONSE_MODEL: "gen_ai.response.model",
  GEN_AI_RESPONSE_ID: "gen_ai.response.id",
  GEN_AI_FINISH_REASONS: "gen_ai.response.finish_reasons",
  GEN_AI_INPUT_TOKENS: "gen_ai.usage.input_tokens",
  GEN_AI_OUTPUT_TOKENS: "gen_ai.usage.output_tokens",
  GEN_AI_TOOL_NAME: "gen_ai.tool.name",
  GEN_AI_TOOL_CALL_ID: "gen_ai.tool.call.id",
  SESSION_ID: "gs.session.id",
  SESSION_SOURCE: "gs.session.source",
  END_REASON: "gs.end.reason",
  CWD: "gs.cwd",
  PERMISSION_MODE: "gs.permission_mode",
  HOST_NAME: "gs.host.name",
  VCS_REPOSITORY: "gs.vcs.repository",
  VCS_BRANCH: "gs.vcs.branch",
  VCS_REVISION: "gs.vcs.revision",
  TRANSCRIPT_PATH: "gs.transcript.path",
  SIDECHAIN: "gs.sidechain",
  PROMPT_TEXT: "gs.prompt.text",
  PROMPT_BYTES: "gs.prompt.bytes",
  CACHE_READ_TOKENS: "gs.usage.cache_read_input_tokens",
  CACHE_CREATION_TOKENS: "gs.usage.cache_creation_input_tokens",
  REQUEST_ID: "gs.request.id",
  MODEL_TOOL_USES: "gs.model.tool_uses",
  TOOL_CATEGORY: "gs.tool.category",
  TOOL_INPUT: "gs.tool.input.body",
  TOOL_OUTPUT: "gs.tool.output.body",
  TOOL_INPUT_BYTES: "gs.tool.input.bytes",
  TOOL_OUTPUT_BYTES: "gs.tool.output.bytes",
  SHELL_COMMAND: "gs.shell.command",
  SHELL_EXIT_CODE: "gs.shell.exit_code",
  SHELL_STDOUT_BYTES: "gs.shell.stdout_bytes",
  SHELL_STDERR_BYTES: "gs.shell.stderr_bytes",
  SHELL_INTERRUPTED: "gs.shell.interrupted",
  FILE_PATH: "gs.file.path",
  FILE_OPERATION: "gs.file.operation",
  SEARCH_PATTERN: "gs.search.pattern",
  HTTP_URL: "gs.http.url",
  DURATION_MS: "gs.duration_ms",
  ERROR_CATEGORY: "gs.error.category",
  ERROR_MESSAGE: "gs.error.message",
  SUBAGENT_ID: "gs.subagent.id",
  SUBAGENT_TYPE: "gs.subagent.type",
  COMPACTION_TRIGGER: "gs.compaction.trigger",
  NOTIFICATION_MESSAGE: "gs.notification.message",
} as const;

/** `GET /v1/health`. Mirrors `Health` in crates/api. Fields that describe one machine's daemon are absent on the hosted backend. */
export interface Health {
  status: string;
  /** Which host is answering; absent from gsd builds older than this field. */
  deployment?: "local" | "hosted";
  /** Optional endpoint groups this host serves, e.g. `insights`. */
  features?: string[];
  /** Whether this host serves the UI at `/`. */
  ui?: boolean;
  /** Adapters this host accepts at `/v1/adapters/{name}`. */
  adapters: string[];
  version: string;
  schema: string;
  trajectories: number;
  events: number;
  pid?: number;
  data_dir?: string;
  spool_pending?: number;
  upload?: UploadStatus;
}

export interface UploadStatus {
  mode: string;
  endpoint: string | null;
  pending: number;
  /** Events given up on after repeated rejection by the backend. */
  dropped: number;
  /** The most recent failed upload, cleared by the next success. */
  last_error?: { message: string; at: string };
}

/** `GET /v1/stats/tools?days=N`. Mirrors `ToolStats` in crates/api. */
export interface ToolStats {
  days: number;
  since: string;
  tools: ToolLatency[];
}
export interface ToolLatency {
  category: string;
  calls: number;
  failed: number;
  p50_ms: number;
  p95_ms: number;
  max_ms: number;
}

export type TrajectoryStatus = "running" | "idle" | "completed" | "failed" | "cancelled" | (string & {});

export interface TrajectorySummary {
  id: string;
  agent: string;
  agent_version: string | null;
  title: string | null;
  status: TrajectoryStatus;
  cwd: string | null;
  repository: string | null;
  branch: string | null;
  /** Git commit checked out when the trajectory started. */
  revision: string | null;
  host: string | null;
  started_at: string;
  updated_at: string;
  ended_at: string | null;
  duration_ms: number;
  event_count: number;
  user_turns: number;
  model_calls: number;
  tool_calls: number;
  tool_errors: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  /** Sum of `gs.cost.usd`; 0 when the agent doesn't report cost. */
  cost_usd: number;
}

export interface TrajectoryDetail extends TrajectorySummary {
  events: Event[];
}

export function totalTokens(t: TrajectorySummary): number {
  return t.input_tokens + t.output_tokens + t.cache_read_tokens + t.cache_creation_tokens;
}

export function str(e: Event, key: string): string | undefined {
  const v = e.attributes[key];
  return typeof v === "string" ? v : undefined;
}
export function num(e: Event, key: string): number | undefined {
  const v = e.attributes[key];
  return typeof v === "number" ? v : undefined;
}

/** Share of prompt tokens served from the prompt cache. null when nothing was sent. */
export function cacheHit(inTok: number, cacheWrite: number, cacheRead: number): number | null {
  const prompt = inTok + cacheWrite + cacheRead;
  return prompt > 0 ? cacheRead / prompt : null;
}

/** Everything the model read: uncached input + cache write + cache read. */
export function promptTokens(inTok: number, cacheWrite: number, cacheRead: number): number {
  return inTok + cacheWrite + cacheRead;
}
