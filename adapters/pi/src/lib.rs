//! Ground Station adapter for [pi](https://pi.dev), the minimal agent harness.
//!
//! Two halves:
//!
//! * **Connecting** ([`settings`]): pi has no command hooks; it runs
//!   TypeScript/JavaScript extensions. `groundstation connect pi` installs a
//!   small extension (`extensions/groundstation.js`) that listens to pi's
//!   lifecycle events and forwards them to gsd.
//! * **Normalizing** ([`Pi`]): turns those events into
//!   `groundstation.telemetry.v0` events. Assistant messages carry pi's own
//!   usage accounting, including cost, so there is no transcript to read.
//!
//! Sample payloads live in `tests/fixtures/`. When pi changes an event, add a
//! fixture first.

use groundstation_schema::{Adapter, HookEnvelope, Normalized};

mod events;
pub mod settings;

pub use events::tool_category;

/// Adapter name: `agent.name`, the CLI argument and the ingest URL segment.
pub const NAME: &str = "pi";

pub struct Pi;

impl Adapter for Pi {
    fn name(&self) -> &'static str {
        NAME
    }

    fn normalize(&self, envelope: &HookEnvelope) -> Normalized {
        events::normalize(envelope)
    }
}
