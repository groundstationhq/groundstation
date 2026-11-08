//! `ui/src/lib/types.ts` mirrors these types by hand, and AGENTS.md asks for
//! an exact mirror. This holds the two together: TypeScript declares every key
//! a host sends, marks optional every key a host can leave out, and its unions
//! name every value the Rust side produces. The unions are open-ended
//! (`string & {}`), so the compiler can't notice a missing value.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{TimeZone, Utc};
use groundstation_schema::{Agent, Event, EventKind, SCHEMA, ToolCategory};
use groundstation_wire::insights::{Evidence, Insight, InsightState, Severity, StateChange};
use groundstation_wire::summary::{STATUS_BY_KIND, STATUS_RUNNING};
use groundstation_wire::{
    CreatedToken, Deployment, ErrorBody, Health, IngestToken, NewToken, ToolLatency, ToolStats,
    TrajectoryDetail, TrajectorySummary, UploadError, UploadStatus,
};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

const TYPES_TS: &str = include_str!("../../../ui/src/lib/types.ts");

/// A field of a TypeScript interface: whether it's optional, and its type.
struct Field {
    optional: bool,
    ty: String,
}

/// The fields of `export interface name`, including those of the interface it
/// extends.
fn interface(name: &str) -> BTreeMap<String, Field> {
    let start = format!("export interface {name} ");
    let from = TYPES_TS
        .find(&start)
        .unwrap_or_else(|| panic!("types.ts has no `interface {name}`"));
    let (head, rest) = TYPES_TS[from + start.len()..].split_once('{').unwrap();
    let body = &rest[..rest.find("\n}").unwrap()];
    let mut fields = match head.trim().strip_prefix("extends ") {
        Some(base) => interface(base.trim()),
        None => BTreeMap::new(),
    };
    for line in body.lines().map(str::trim) {
        if line.starts_with("/*") || line.starts_with('*') || line.starts_with("//") {
            continue;
        }
        let Some((key, ty)) = line.split_once(':') else {
            continue;
        };
        let (key, optional) = match key.strip_suffix('?') {
            Some(key) => (key, true),
            None => (key, false),
        };
        let ty = ty.trim().trim_end_matches(';').to_string();
        fields.insert(key.to_string(), Field { optional, ty });
    }
    fields
}

/// The quoted values in a union such as `"a" | "b" | (string & {})`.
fn quoted(union: &str) -> BTreeSet<String> {
    union
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// The quoted values of `export type name = ...;`.
fn union(name: &str) -> BTreeSet<String> {
    let start = format!("export type {name} =");
    let from = TYPES_TS
        .find(&start)
        .unwrap_or_else(|| panic!("types.ts has no `type {name}`"));
    let rest = &TYPES_TS[from + start.len()..];
    quoted(&rest[..rest.find(';').unwrap()])
}

fn keys(value: &impl Serialize) -> BTreeSet<String> {
    match serde_json::to_value(value).unwrap() {
        Value::Object(map) => map.keys().cloned().collect(),
        other => panic!("not an object: {other}"),
    }
}

/// Holds `interface name` to what a host sends: `bare` with every optional
/// value unset, `full` with every one set.
fn mirrors(name: &str, bare: &impl Serialize, full: &impl Serialize) {
    let (bare, full) = (keys(bare), keys(full));
    let fields = interface(name);
    for key in &full {
        assert!(
            fields.contains_key(key),
            "hosts send {name}.{key}, but types.ts doesn't declare it"
        );
    }
    for (key, field) in &fields {
        assert!(
            full.contains(key),
            "types.ts declares {name}.{key}, which no host sends"
        );
        assert!(
            field.optional || bare.contains(key),
            "hosts can leave out {name}.{key}, so types.ts must mark it optional"
        );
    }
}

fn names<T: Serialize>(values: &[T]) -> BTreeSet<String> {
    values
        .iter()
        .map(|v| {
            serde_json::to_value(v)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

fn at() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap()
}

fn upload(full: bool) -> UploadStatus {
    UploadStatus {
        mode: "cloud".into(),
        endpoint: full.then(|| "https://gs.example.com".into()),
        pending: 1,
        dropped: 0,
        last_error: full.then(|| UploadError {
            message: "HTTP 401".into(),
            at: at(),
        }),
    }
}

fn health(full: bool) -> Health {
    Health {
        status: "ok".into(),
        deployment: Deployment::Local,
        features: Vec::new(),
        ui: true,
        adapters: Vec::new(),
        version: "1.0.0".into(),
        schema: SCHEMA.into(),
        trajectories: 0,
        events: 0,
        pid: full.then_some(7),
        data_dir: full.then(|| "/data".into()),
        spool_pending: full.then_some(0),
        upload: full.then(|| upload(true)),
    }
}

fn event(full: bool) -> Event {
    let agent = Agent {
        name: "claude-code".into(),
        version: full.then(|| "2.1.0".into()),
    };
    let mut e = Event::new(Uuid::nil(), "t", EventKind::TurnUser, at(), agent);
    if full {
        e.span_id = Some("s".into());
        e.parent_span_id = Some("p".into());
        e.set("gs.prompt.text", "hi");
    }
    e
}

fn summary(full: bool) -> TrajectorySummary {
    let some = |s: &str| full.then(|| s.to_string());
    TrajectorySummary {
        id: "t".into(),
        agent: "claude-code".into(),
        agent_version: some("2.1.0"),
        title: some("Fix the tests"),
        status: STATUS_RUNNING.into(),
        cwd: some("web"),
        repository: some("github.com/acme/shop"),
        branch: some("main"),
        revision: some("3f2a9c1e"),
        host: some("laptop"),
        started_at: at(),
        updated_at: at(),
        ended_at: full.then(at),
        duration_ms: 0,
        event_count: 0,
        user_turns: 0,
        model_calls: 0,
        tool_calls: 0,
        tool_errors: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        cost_usd: 0.0,
    }
}

fn token(full: bool) -> IngestToken {
    IngestToken {
        id: Uuid::nil(),
        label: "laptop".into(),
        created_at: at(),
        last_used_at: full.then(at),
        revoked_at: full.then(at),
    }
}

fn insight() -> Insight {
    Insight {
        id: Uuid::nil(),
        detector: "repeated_read".into(),
        severity: Severity::Low,
        title: "title".into(),
        body: "body".into(),
        recommendation: "recommendation".into(),
        evidence: vec![evidence()],
        metrics: BTreeMap::from([("reads".into(), 3.0)]),
        first_seen_at: at(),
        last_seen_at: at(),
        state: InsightState::Open,
    }
}

fn evidence() -> Evidence {
    Evidence {
        trajectory_id: "t".into(),
        event_ids: vec![Uuid::nil()],
        note: "note".into(),
    }
}

#[test]
fn interfaces_declare_what_hosts_send() {
    mirrors("Health", &health(false), &health(true));
    mirrors("UploadStatus", &upload(false), &upload(true));
    mirrors("Event", &event(false), &event(true));
    mirrors("Agent", &event(false).agent, &event(true).agent);
    mirrors("TrajectorySummary", &summary(false), &summary(true));
    let detail = |full| TrajectoryDetail {
        summary: summary(full),
        events: vec![event(full)],
    };
    mirrors("TrajectoryDetail", &detail(false), &detail(true));
    let latency = ToolLatency {
        category: "shell".into(),
        calls: 1,
        failed: 0,
        p50_ms: 1,
        p95_ms: 1,
        max_ms: 1,
    };
    let stats = ToolStats {
        days: 14,
        since: at(),
        tools: vec![latency.clone()],
    };
    mirrors("ToolStats", &stats, &stats);
    mirrors("ToolLatency", &latency, &latency);
    mirrors("IngestToken", &token(false), &token(true));
    let created = |full| CreatedToken {
        token: token(full),
        secret: "gsi_".into(),
    };
    mirrors("CreatedToken", &created(false), &created(true));
    let new_token = NewToken {
        label: "laptop".into(),
    };
    mirrors("NewToken", &new_token, &new_token);
    mirrors("Insight", &insight(), &insight());
    mirrors("Evidence", &evidence(), &evidence());
    let change = StateChange {
        state: InsightState::Dismissed,
    };
    mirrors("StateChange", &change, &change);
    let error = ErrorBody {
        error: "trajectory not found".into(),
    };
    mirrors("ErrorBody", &error, &error);
}

#[test]
fn unions_name_every_value() {
    let kinds: BTreeSet<String> = EventKind::KNOWN.iter().map(|k| k.to_string()).collect();
    assert_eq!(union("EventKind"), kinds);

    let mut statuses: BTreeSet<String> =
        STATUS_BY_KIND.iter().map(|(_, s)| s.to_string()).collect();
    statuses.insert(STATUS_RUNNING.into());
    assert_eq!(union("TrajectoryStatus"), statuses);

    // Each list fails to compile when its enum gains a variant.
    let severity = |s| match s {
        Severity::Low | Severity::Medium | Severity::High => s,
    };
    assert_eq!(
        union("Severity"),
        names(&[Severity::Low, Severity::Medium, Severity::High].map(severity))
    );
    let state = |s| match s {
        InsightState::Open | InsightState::Dismissed | InsightState::Resolved => s,
    };
    assert_eq!(
        union("InsightState"),
        names(
            &[
                InsightState::Open,
                InsightState::Dismissed,
                InsightState::Resolved
            ]
            .map(state)
        )
    );
    let deployment = |d| match d {
        Deployment::Local | Deployment::Hosted => d,
    };
    assert_eq!(
        quoted(&interface("Health")["deployment"].ty),
        names(&[Deployment::Local, Deployment::Hosted].map(deployment))
    );
    let category = |c| match c {
        ToolCategory::Shell
        | ToolCategory::FileRead
        | ToolCategory::FileWrite
        | ToolCategory::Search
        | ToolCategory::Http
        | ToolCategory::Browser
        | ToolCategory::Subagent
        | ToolCategory::Mcp
        | ToolCategory::Planning
        | ToolCategory::Custom => c,
    };
    assert_eq!(
        union("ToolCategory"),
        names(
            &[
                ToolCategory::Shell,
                ToolCategory::FileRead,
                ToolCategory::FileWrite,
                ToolCategory::Search,
                ToolCategory::Http,
                ToolCategory::Browser,
                ToolCategory::Subagent,
                ToolCategory::Mcp,
                ToolCategory::Planning,
                ToolCategory::Custom,
            ]
            .map(category)
        )
    );
}
