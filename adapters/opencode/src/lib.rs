//! Ground Station adapter for [OpenCode](https://opencode.ai) v2 and newer.
//!
//! Two halves:
//!
//! * **Connecting** ([`settings`]): OpenCode has no command hooks, so
//!   `groundstation connect opencode` installs a small plugin
//!   (`plugins/groundstation.js`) that subscribes to OpenCode's event stream
//!   and forwards session events to gsd.
//! * **Normalizing** ([`OpenCode`]): turns those events into
//!   `groundstation.telemetry.v0` events. OpenCode reports everything on the
//!   stream, including per-step tokens, cost and latency, so there is no
//!   transcript to read. Subagents run as child sessions and are folded into
//!   the root session's trajectory.
//!
//! Sample payloads live in `tests/fixtures/`. When OpenCode changes an event,
//! add a fixture first.

use groundstation_schema::{Adapter, HookEnvelope, Normalized};

mod events;
pub mod settings;

pub use events::tool_category;

/// Adapter name: `agent.name`, the CLI argument and the ingest URL segment.
pub const NAME: &str = "opencode";

pub struct OpenCode;

impl Adapter for OpenCode {
    fn name(&self) -> &'static str {
        NAME
    }

    fn normalize(&self, envelope: &HookEnvelope) -> Normalized {
        events::normalize(envelope)
    }
}
