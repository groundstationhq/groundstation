//! Terminal rendering for trajectories.

use std::collections::HashMap;
use std::fmt::Write;

use chrono::{DateTime, Local, Utc};
use groundstation_schema::{Event, EventKind, attr};
use gsd::api::{TrajectoryDetail, TrajectorySummary};
use serde_json::Value;

pub fn trajectory_table(rows: &[TrajectorySummary]) -> String {
    let ids = short_ids(&rows.iter().map(|t| t.id.as_str()).collect::<Vec<_>>());
    let width = ids
        .iter()
        .map(|id| id.chars().count())
        .max()
        .unwrap_or(2)
        .max(2);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<width$}  {:<11}  {:<9}  {:<11}  {:>8}  {:>5}  {:>9}  {:>5}  {:>6}  {:>7}  {:>7}  {:>6}  TASK",
        "ID",
        "AGENT",
        "STATUS",
        "STARTED",
        "DURATION",
        "TURNS",
        "TOOLS",
        "MODEL",
        "IN",
        "CACHE-W",
        "CACHE-R",
        "OUT"
    );
    for (t, id) in rows.iter().zip(&ids) {
        let tools = if t.tool_errors > 0 {
            format!("{} ({}✕)", t.tool_calls, t.tool_errors)
        } else {
            t.tool_calls.to_string()
        };
        let task = t.title.as_deref().unwrap_or("—");
        let place = t.repository.as_deref().or(t.cwd.as_deref()).map(basename);
        let task = match place {
            Some(p) => format!("{p}: {task}"),
            None => task.to_string(),
        };
        let _ = writeln!(
            out,
            "{:<width$}  {:<11}  {:<9}  {:<11}  {:>8}  {:>5}  {:>9}  {:>5}  {:>6}  {:>7}  {:>7}  {:>6}  {}",
            id,
            truncate(&t.agent, 11),
            t.status,
            t.started_at.with_timezone(&Local).format("%m-%d %H:%M"),
            duration(t.duration_ms),
            t.user_turns,
            tools,
            t.model_calls,
            tokens(t.input_tokens),
            tokens(t.cache_creation_tokens),
            tokens(t.cache_read_tokens),
            tokens(t.output_tokens),
            truncate(&task, 60),
        );
    }
    out
}

pub fn trajectory(detail: &TrajectoryDetail) -> String {
    let t = &detail.summary;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}",
        t.title.as_deref().unwrap_or("(no prompt captured)")
    );
    let version = t
        .agent_version
        .as_deref()
        .map(|v| format!(" {v}"))
        .unwrap_or_default();
    let _ = writeln!(out, "{}  {}{}  {}", t.id, t.agent, version, t.status);
    if let Some(place) = t.repository.as_deref().or(t.cwd.as_deref()) {
        let branch = t
            .branch
            .as_deref()
            .map(|b| format!(" ({b})"))
            .unwrap_or_default();
        let _ = writeln!(out, "{}{}", tilde(place), branch);
    }
    let _ = writeln!(
        out,
        "started {}   duration {}",
        t.started_at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S"),
        duration(t.duration_ms)
    );
    let failed = if t.tool_errors > 0 {
        format!(" ({} failed)", t.tool_errors)
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        "turns {}   model calls {}   tool calls {}{}",
        t.user_turns, t.model_calls, t.tool_calls, failed
    );
    if t.total_tokens() > 0 {
        let _ = writeln!(
            out,
            "tokens   in {} · cache write {} · cache read {} · out {}",
            tokens(t.input_tokens),
            tokens(t.cache_creation_tokens),
            tokens(t.cache_read_tokens),
            tokens(t.output_tokens),
        );
    }
    if t.cost_usd > 0.0 {
        let _ = writeln!(out, "cost     {}", usd(t.cost_usd));
    }
    out.push('\n');

    let cwd = t.cwd.as_deref();
    let closing: HashMap<&str, &Event> = detail
        .events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ToolCompleted | EventKind::ToolFailed))
        .filter_map(|e| Some((e.span_id.as_deref()?, e)))
        .collect();
    let opened: std::collections::HashSet<&str> = detail
        .events
        .iter()
        .filter(|e| e.kind == EventKind::ToolStarted)
        .filter_map(|e| e.span_id.as_deref())
        .collect();

    let mut slowest: Vec<(i64, String)> = Vec::new();
    for ev in &detail.events {
        let at = offset(t.started_at, ev.timestamp);
        let line = match &ev.kind {
            EventKind::ToolStarted => {
                let end = ev.span_id.as_deref().and_then(|s| closing.get(s)).copied();
                Some(tool_line(ev, end, cwd, &mut slowest))
            }
            EventKind::ToolCompleted | EventKind::ToolFailed
                if ev.span_id.as_deref().is_none_or(|s| !opened.contains(s)) =>
            {
                Some(tool_line(ev, Some(ev), cwd, &mut slowest))
            }
            EventKind::ToolCompleted | EventKind::ToolFailed => None,
            EventKind::TurnUser => {
                let prompt = ev
                    .get(attr::PROMPT_TEXT)
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Some(format!("▸ {:<12} {}", "user", one_line(prompt, 90)))
            }
            EventKind::ModelCompleted => {
                let n = |k| ev.get(k).and_then(Value::as_u64).unwrap_or(0);
                let model = ev
                    .get(attr::GEN_AI_RESPONSE_MODEL)
                    .and_then(Value::as_str)
                    .unwrap_or("model");
                let side = if ev.get(attr::SIDECHAIN).is_some() {
                    "  (subagent)"
                } else {
                    ""
                };
                Some(format!(
                    "◆ {:<12} in {:>6}  cache-w {:>6}  cache-r {:>6}  out {:>6}  {model}{side}{extra}",
                    "model",
                    tokens(n(attr::GEN_AI_INPUT_TOKENS)),
                    tokens(n(attr::CACHE_CREATION_TOKENS)),
                    tokens(n(attr::CACHE_READ_TOKENS)),
                    tokens(n(attr::GEN_AI_OUTPUT_TOKENS)),
                    extra = model_extra(ev),
                ))
            }
            EventKind::TurnCompleted => Some("■ turn done".to_string()),
            EventKind::TurnInterrupted => Some("■ turn interrupted".to_string()),
            EventKind::AgentStarted | EventKind::AgentResumed => {
                let source = ev
                    .get(attr::SESSION_SOURCE)
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Some(format!("○ {:<12} {source}", ev.kind.as_str()))
            }
            EventKind::AgentCompleted | EventKind::AgentFailed | EventKind::AgentCancelled => {
                let reason = ev
                    .get(attr::END_REASON)
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Some(format!("○ {:<12} {reason}", ev.kind.as_str()))
            }
            EventKind::SubagentStarted | EventKind::SubagentCompleted => {
                let kind = ev
                    .get(attr::SUBAGENT_TYPE)
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let took = ev
                    .get(attr::DURATION_MS)
                    .and_then(Value::as_i64)
                    .map(duration)
                    .unwrap_or_default();
                Some(format!("◇ {:<12} {kind}  {took}", ev.kind.as_str()))
            }
            EventKind::AgentNotification => {
                let s = |k| ev.get(k).and_then(Value::as_str);
                let text = match (s(attr::NOTIFICATION_MESSAGE), s(attr::GEN_AI_TOOL_NAME)) {
                    (Some(msg), _) => one_line(msg, 90),
                    (None, Some(tool)) => format!("permission requested for {tool}"),
                    (None, None) => s(attr::NOTIFICATION_TYPE).unwrap_or("").to_string(),
                };
                Some(format!("! {:<12} {text}", "notification"))
            }
            EventKind::ContextCompacted => {
                let trigger = ev
                    .get(attr::COMPACTION_TRIGGER)
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Some(format!("↺ {:<12} {trigger}", "compacted"))
            }
            other => Some(format!("· {}", other.as_str())),
        };
        if let Some(line) = line {
            let _ = writeln!(out, "  {at:>10}  {line}");
        }
    }

    slowest.sort_by_key(|(ms, _)| std::cmp::Reverse(*ms));
    if slowest.first().is_some_and(|(ms, _)| *ms >= 1000) {
        let _ = writeln!(out, "\nlongest tool calls");
        for (ms, label) in slowest.iter().take(3).filter(|(ms, _)| *ms >= 1000) {
            let _ = writeln!(out, "  {:>8}  {label}", duration(*ms));
        }
    }
    out
}

fn tool_line(
    start: &Event,
    end: Option<&Event>,
    cwd: Option<&str>,
    slowest: &mut Vec<(i64, String)>,
) -> String {
    let name = start
        .get(attr::GEN_AI_TOOL_NAME)
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let detail = tool_detail(start, cwd);
    let took = end
        .and_then(|e| e.get(attr::DURATION_MS))
        .and_then(Value::as_i64);
    let status = match end {
        None => "  …".to_string(),
        Some(e) if e.kind == EventKind::ToolFailed => {
            let category = e
                .get(attr::ERROR_CATEGORY)
                .and_then(Value::as_str)
                .unwrap_or("error");
            format!("  ✕ {category}")
        }
        Some(_) => String::new(),
    };
    if let Some(ms) = took {
        slowest.push((ms, format!("{name} {}", one_line(&detail, 60))));
    }
    format!(
        "● {:<12} {:<60} {:>8}{status}",
        truncate(name, 12),
        one_line(&detail, 60),
        took.map(duration).unwrap_or_default()
    )
}

fn tool_detail(ev: &Event, cwd: Option<&str>) -> String {
    let s = |k| ev.get(k).and_then(Value::as_str);
    if let Some(cmd) = s(attr::SHELL_COMMAND) {
        return cmd.to_string();
    }
    if let Some(path) = s(attr::FILE_PATH) {
        let path = cwd
            .and_then(|c| path.strip_prefix(c))
            .map(|p| p.trim_start_matches('/'))
            .filter(|p| !p.is_empty())
            .unwrap_or(path);
        return match s(attr::SEARCH_PATTERN) {
            Some(pattern) => format!("{pattern}  in {path}"),
            None => path.to_string(),
        };
    }
    s(attr::SEARCH_PATTERN)
        .or_else(|| s(attr::HTTP_URL))
        .or_else(|| s(attr::SUBAGENT_TYPE))
        .unwrap_or("")
        .to_string()
}

fn offset(start: DateTime<Utc>, at: DateTime<Utc>) -> String {
    let ms = (at - start).num_milliseconds().max(0);
    let (m, s, ms) = (ms / 60_000, (ms / 1000) % 60, ms % 1000);
    format!("+{m}:{s:02}.{ms:03}")
}

/// Latency, cost and purpose of a model call, when the agent reports them.
fn model_extra(ev: &Event) -> String {
    let mut parts = Vec::new();
    if let Some(ms) = ev.get(attr::DURATION_MS).and_then(Value::as_i64) {
        parts.push(duration(ms));
    }
    if let Some(cost) = ev.get(attr::COST_USD).and_then(Value::as_f64) {
        parts.push(usd(cost));
    }
    if let Some(purpose) = ev.get(attr::MODEL_PURPOSE).and_then(Value::as_str) {
        parts.push(format!("({purpose})"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  {}", parts.join("  "))
    }
}

pub fn usd(amount: f64) -> String {
    if amount < 0.01 {
        format!("${amount:.4}")
    } else {
        format!("${amount:.2}")
    }
}

pub fn duration(ms: i64) -> String {
    match ms {
        ms if ms < 1000 => format!("{ms}ms"),
        ms if ms < 60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        ms if ms < 3_600_000 => format!("{}m{:02}s", ms / 60_000, (ms / 1000) % 60),
        ms => format!("{}h{:02}m", ms / 3_600_000, (ms / 60_000) % 60),
    }
}

pub fn tokens(n: u64) -> String {
    match n {
        n if n < 1000 => n.to_string(),
        n if n < 1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        n => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// The shortest id prefixes (at least 8 characters after any `xyz_` type
/// prefix, as in OpenCode's `ses_…`) that still tell the rows apart.
fn short_ids(ids: &[&str]) -> Vec<String> {
    let min = |id: &str| {
        let prefix = id.find('_').filter(|&i| i <= 4).map_or(0, |i| i + 1);
        prefix + 8
    };
    let mut extra = 0;
    loop {
        let short: Vec<String> = ids
            .iter()
            .map(|id| id.chars().take(min(id) + extra).collect())
            .collect();
        let unique: std::collections::HashSet<&String> = short.iter().collect();
        let longest = ids.iter().map(|id| id.chars().count()).max().unwrap_or(0);
        if unique.len() == short.len() || min("") + extra >= longest {
            return short;
        }
        extra += 1;
    }
}

pub fn tilde(path: &str) -> String {
    let home = gsd::config::home_dir();
    match path.strip_prefix(&*home.to_string_lossy()) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

fn basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

fn one_line(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let more = s.trim().lines().count() > 1;
    let mut out = truncate(line, max);
    if more && out.chars().count() < max {
        out.push_str(" …");
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_units() {
        assert_eq!(duration(12), "12ms");
        assert_eq!(duration(47_193), "47.2s");
        assert_eq!(duration(258_000), "4m18s");
        assert_eq!(duration(3_900_000), "1h05m");
        assert_eq!(tokens(84_214), "84.2k");
        assert_eq!(
            short_ids(&["ses_f131be2e4ffeS8", "ses_f131be2e5aaaX1", "07404509-96b9"]),
            ["ses_f131be2e4", "ses_f131be2e5", "07404509-"]
        );
        assert_eq!(
            short_ids(&["07404509-96b9", "966a5ec0-1111"]),
            ["07404509", "966a5ec0"]
        );
        assert_eq!(usd(0.4712), "$0.47");
        assert_eq!(usd(0.0031), "$0.0031");
        assert_eq!(tokens(1_250_000), "1.2M");
        assert_eq!(truncate("héllo", 3), "hé…");
        assert_eq!(one_line("\n  cargo test\n  --all", 40), "cargo test …");
    }
}
