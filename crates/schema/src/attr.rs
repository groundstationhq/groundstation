//! Attribute keys. `gen_ai.*` follows the OpenTelemetry GenAI semantic
//! conventions; `gs.*` covers agent-specific fields that have no convention.
//!
//! Keys listed in [`CONTENT`] may carry user content (prompts, source code,
//! command output) and can be dropped with `[redaction] exclude`. Keys in
//! [`PATHS`] are local filesystem paths, subject to `[redaction] paths`.
//! Sizes, counts and durations are never content, so excluding a field keeps
//! its measurements (`exclude = ["tool.output.body"]` keeps `gs.tool.output.bytes`).

// OpenTelemetry GenAI conventions.
pub const GEN_AI_PROVIDER: &str = "gen_ai.provider.name";
pub const GEN_AI_RESPONSE_MODEL: &str = "gen_ai.response.model";
pub const GEN_AI_RESPONSE_ID: &str = "gen_ai.response.id";
pub const GEN_AI_FINISH_REASONS: &str = "gen_ai.response.finish_reasons";
pub const GEN_AI_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
pub const GEN_AI_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";
pub const GEN_AI_TOOL_NAME: &str = "gen_ai.tool.name";
pub const GEN_AI_TOOL_CALL_ID: &str = "gen_ai.tool.call.id";
pub const GEN_AI_REQUEST_MODEL: &str = "gen_ai.request.model";

// Trajectory / session context.
pub const SESSION_ID: &str = "gs.session.id";
pub const TURN_ID: &str = "gs.turn.id";
pub const SESSION_SOURCE: &str = "gs.session.source";
pub const END_REASON: &str = "gs.end.reason";
pub const CWD: &str = "gs.cwd";
pub const PERMISSION_MODE: &str = "gs.permission_mode";
pub const TRANSCRIPT_PATH: &str = "gs.transcript.path";
pub const SIDECHAIN: &str = "gs.sidechain";

// Turns.
pub const PROMPT_TEXT: &str = "gs.prompt.text";
pub const PROMPT_BYTES: &str = "gs.prompt.bytes";

// Model calls.
pub const CACHE_READ_TOKENS: &str = "gs.usage.cache_read_input_tokens";
pub const CACHE_CREATION_TOKENS: &str = "gs.usage.cache_creation_input_tokens";
/// Part of `gen_ai.usage.output_tokens` spent on reasoning, when reported separately.
pub const REASONING_TOKENS: &str = "gs.usage.reasoning_output_tokens";
pub const REQUEST_ID: &str = "gs.request.id";
pub const MODEL_TOOL_USES: &str = "gs.model.tool_uses";
/// Why a model call happened when it isn't the agent's own turn (e.g. `compaction`).
pub const MODEL_PURPOSE: &str = "gs.model.purpose";
/// Cost of a model call in US dollars, as reported by the agent.
pub const COST_USD: &str = "gs.cost.usd";

// Tool calls.
pub const TOOL_CATEGORY: &str = "gs.tool.category";
pub const TOOL_INPUT: &str = "gs.tool.input.body";
pub const TOOL_OUTPUT: &str = "gs.tool.output.body";
pub const TOOL_INPUT_BYTES: &str = "gs.tool.input.bytes";
pub const TOOL_OUTPUT_BYTES: &str = "gs.tool.output.bytes";
pub const SHELL_COMMAND: &str = "gs.shell.command";
pub const SHELL_EXIT_CODE: &str = "gs.shell.exit_code";
pub const SHELL_STDOUT_BYTES: &str = "gs.shell.stdout_bytes";
pub const SHELL_STDERR_BYTES: &str = "gs.shell.stderr_bytes";
pub const SHELL_INTERRUPTED: &str = "gs.shell.interrupted";
pub const FILE_PATH: &str = "gs.file.path";
pub const FILE_OPERATION: &str = "gs.file.operation";
pub const SEARCH_PATTERN: &str = "gs.search.pattern";
pub const HTTP_URL: &str = "gs.http.url";

// Spans.
/// Filled in by the daemon on the event that closes a span.
pub const DURATION_MS: &str = "gs.duration_ms";

// Errors.
pub const ERROR_CATEGORY: &str = "gs.error.category";
pub const ERROR_MESSAGE: &str = "gs.error.message";

// Subagents, compaction, notifications.
pub const SUBAGENT_ID: &str = "gs.subagent.id";
pub const SUBAGENT_TYPE: &str = "gs.subagent.type";
pub const COMPACTION_TRIGGER: &str = "gs.compaction.trigger";
pub const NOTIFICATION_MESSAGE: &str = "gs.notification.message";
pub const NOTIFICATION_TYPE: &str = "gs.notification.type";

// Patches (a single edit that touches several files).
pub const PATCH_FILES: &str = "gs.patch.files";

/// Attributes that can contain user content.
pub const CONTENT: &[&str] = &[
    PROMPT_TEXT,
    TOOL_INPUT,
    TOOL_OUTPUT,
    SHELL_COMMAND,
    SEARCH_PATTERN,
    ERROR_MESSAGE,
    NOTIFICATION_MESSAGE,
];

/// Attributes holding local filesystem paths.
pub const PATHS: &[&str] = &[CWD, FILE_PATH, TRANSCRIPT_PATH];

/// Keys inside a tool's input body that hold filesystem paths.
pub const TOOL_INPUT_PATH_KEYS: &[&str] = &["file_path", "notebook_path", "path", "cwd"];
