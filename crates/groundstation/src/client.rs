//! Talks to the local gsd daemon.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use gsd::api::{ErrorBody, Health, IngestResponse, TrajectoryDetail, TrajectorySummary};
use gsd::config::Config;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub struct Client {
    http: reqwest::Client,
    base: String,
}

impl Client {
    pub fn new(config: &Config) -> Result<Self> {
        Self::with_timeouts(config, Duration::from_secs(1), Duration::from_secs(30))
    }

    /// Hooks sit in the agent's critical path, so they give up fast and spool.
    /// Codex kills `SessionEnd` and `Interrupt` hooks after 3 seconds.
    pub fn for_hooks(config: &Config) -> Result<Self> {
        Self::with_timeouts(config, Duration::from_millis(250), Duration::from_secs(2))
    }

    fn with_timeouts(config: &Config, connect: Duration, total: Duration) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(connect)
            .timeout(total)
            .no_proxy()
            .build()?;
        Ok(Self {
            http,
            base: config.base_url(),
        })
    }

    pub async fn health(&self) -> Result<Health> {
        self.get("/v1/health").await
    }

    pub async fn trajectories(&self, limit: usize) -> Result<Vec<TrajectorySummary>> {
        self.get(&format!("/v1/trajectories?limit={limit}")).await
    }

    pub async fn trajectory(&self, id: &str) -> Result<TrajectoryDetail> {
        self.get(&format!("/v1/trajectories/{}", encode(id))).await
    }

    pub async fn post<B: Serialize>(&self, path: &str, body: &B) -> Result<IngestResponse> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await;
        decode(resp.with_context(|| self.unreachable())?).await
    }

    pub async fn shutdown(&self) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/v1/shutdown", self.base))
            .json(&serde_json::json!({}))
            .send()
            .await;
        let resp = resp.with_context(|| self.unreachable())?;
        if !resp.status().is_success() {
            bail!("gsd refused to shut down: {}", resp.status());
        }
        Ok(())
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self.http.get(format!("{}{path}", self.base)).send().await;
        decode(resp.with_context(|| self.unreachable())?).await
    }

    fn unreachable(&self) -> String {
        format!(
            "gsd is not reachable at {} (start it with `groundstation daemon start`)",
            self.base
        )
    }
}

async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp.json().await?);
    }
    let message = match resp.json::<ErrorBody>().await {
        Ok(body) => body.error,
        Err(_) => status.to_string(),
    };
    bail!("{message}")
}

fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
