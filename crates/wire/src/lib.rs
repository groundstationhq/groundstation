//! Types exchanged over the Ground Station HTTP API: gsd's local `/v1` and the
//! hosted backend serve the same shapes. No I/O, so any host can depend on it.

use chrono::{DateTime, Utc};
use groundstation_schema::{Batch, Event};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use groundstation_schema::HookEnvelope;

/// What the hook writes to the spool directory when the daemon is unreachable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "kebab-case")]
pub enum SpoolItem {
    /// A hook payload for the named adapter.
    Hook {
        adapter: String,
        envelope: HookEnvelope,
    },
    Batch(Batch),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestResponse {
    /// Events produced by this request that were new or changed.
    pub stored: usize,
    /// Events the receiver refused, each with the reason. The rest of the
    /// batch was accepted. gsd never rejects; the hosted backend may.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<Rejection>,
}

/// One event a receiver refused (`IngestResponse::rejected`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rejection {
    pub id: Uuid,
    pub error: String,
}

/// Result of re-reading an adapter's transcripts (`POST /v1/adapters/{name}/resync`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResyncResponse {
    /// Transcripts read again from the start.
    pub transcripts: usize,
    /// Events that were new or changed.
    pub events: usize,
    /// Transcripts that could not be read, e.g. deleted since.
    pub failed: Vec<ResyncFailure>,
    /// Transcripts under the adapter's directories that no stored hook links
    /// to a trajectory, so they were skipped.
    pub unmatched: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResyncFailure {
    pub path: String,
    pub error: String,
}

/// `GET /v1/health`. Both hosts serve it; the fields that only describe a
/// daemon on one machine are `None` on the hosted backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    /// Which host is answering.
    #[serde(default)]
    pub deployment: Deployment,
    /// Optional endpoint groups this host serves (e.g. `insights`), for the UI
    /// to feature-detect. Empty for gsd.
    #[serde(default)]
    pub features: Vec<String>,
    /// Whether this host serves the UI at `/` (false for builds without `ui/dist`).
    #[serde(default)]
    pub ui: bool,
    /// Adapters this host accepts at `/v1/adapters/{name}`.
    pub adapters: Vec<String>,
    pub version: String,
    pub schema: String,
    pub trajectories: u64,
    pub events: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spool_pending: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload: Option<UploadStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Deployment {
    /// gsd on the user's machine.
    #[default]
    Local,
    /// The hosted backend.
    Hosted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadStatus {
    /// `local-only` or `cloud`.
    pub mode: String,
    /// `None` in local-only mode.
    pub endpoint: Option<String>,
    pub pending: u64,
    /// Events dropped from the upload queue after the backend rejected them
    /// repeatedly. They stay in the local store.
    #[serde(default)]
    pub dropped: u64,
    /// The most recent failed upload, cleared by the next success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<UploadError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadError {
    pub message: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectorySummary {
    pub id: String,
    pub agent: String,
    pub agent_version: Option<String>,
    pub title: Option<String>,
    pub status: String,
    pub cwd: Option<String>,
    pub repository: Option<String>,
    pub branch: Option<String>,
    /// The commit checked out at the trajectory's latest event. Each event
    /// carries its own in `gs.vcs.revision`.
    #[serde(default)]
    pub revision: Option<String>,
    pub host: Option<String>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: i64,
    pub event_count: u64,
    pub user_turns: u64,
    pub model_calls: u64,
    pub tool_calls: u64,
    pub tool_errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// Sum of `gs.cost.usd`; 0 when the agent doesn't report cost.
    #[serde(default)]
    pub cost_usd: f64,
}

impl TrajectorySummary {
    /// Every token the model processed or produced, cached or not.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryDetail {
    #[serde(flatten)]
    pub summary: TrajectorySummary,
    pub events: Vec<Event>,
}

/// Tool latency by category over a window (`GET /v1/stats/tools?days=N`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStats {
    pub days: u32,
    pub since: DateTime<Utc>,
    /// Most-called first.
    pub tools: Vec<ToolLatency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolLatency {
    /// `gs.tool.category`, or the tool name when the adapter set none.
    pub category: String,
    pub calls: u64,
    pub failed: u64,
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub max_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_reads_older_daemons_and_the_hosted_backend() {
        // A gsd from before `deployment` and `features` existed.
        let old: Health = serde_json::from_str(
            r#"{"status":"ok","adapters":[],"version":"0.1.2","schema":"groundstation.telemetry.v0",
                "pid":7,"data_dir":"/d","trajectories":1,"events":2,"spool_pending":0,
                "upload":{"mode":"local-only","endpoint":null,"pending":0}}"#,
        )
        .unwrap();
        assert_eq!(old.deployment, Deployment::Local);
        assert_eq!(old.pid, Some(7));
        assert_eq!(old.upload.unwrap().last_error, None);

        // The hosted backend leaves out what only describes one machine.
        let hosted: Health = serde_json::from_str(
            r#"{"status":"ok","deployment":"hosted","features":["insights"],"adapters":[],
                "version":"1.0.0","schema":"groundstation.telemetry.v0","trajectories":1,"events":2}"#,
        )
        .unwrap();
        assert_eq!(hosted.deployment, Deployment::Hosted);
        assert_eq!(hosted.features, ["insights"]);
        assert_eq!((hosted.pid, hosted.upload.is_none()), (None, true));
        let json = serde_json::to_value(&hosted).unwrap();
        assert!(json.get("pid").is_none() && json.get("upload").is_none());
    }
}
