//! The full test suite run after most edits in a turn, when a scoped run (one
//! crate, package or file) would have told the agent the same thing sooner.
//! Needs `gs.shell.command`, so it finds nothing when `[redaction]` drops it.
//! The command is read to tell a full run from a scoped one and never copied
//! into the finding: content stays in the events, which the finding links to.
//! Turns count per agent context (`gs.session.id`), so a subagent's edits and
//! runs aren't mixed with the agent's that started it.

use std::collections::BTreeMap;

use clickhouse::Row;
use groundstation_backend::Result;
use groundstation_backend::ch::{TenantScope, deduplicated};
use groundstation_backend::pg::insights::Finding;
use groundstation_schema::attr;
use groundstation_wire::insights::{Evidence, Severity};
use serde::Deserialize;

use super::{Found, Strongest, Window, arrived, ids, query};
use crate::format::{count, plural, short};

pub const NAME: &str = "test_rerun_loop";
const VERSION: i32 = 1;
const MIN_RUNS: usize = 3;
const MAX_EVIDENCE: usize = 20;
/// How much of a command is read. Enough for any test command; a longer
/// line is a script, not a test run.
const MAX_COMMAND: usize = 8_192;

#[derive(Debug, Row, Deserialize)]
pub struct Step {
    pub trajectory_id: String,
    pub session: String,
    pub event_id: String,
    pub kind: String,
    pub category: String,
    pub command: String,
    /// `received_at`, in milliseconds.
    pub received: i64,
}

pub async fn run(scope: &TenantScope, window: Window) -> Result<Found> {
    let steps = deduplicated(
        &format!(
            "trajectory_id, toString(event_id) AS id, ts, kind, tool_category,
             JSONExtractString(attributes, '{session}') AS session,
             if(tool_category = 'shell',
                substringUTF8(JSONExtractString(attributes, '{command}'), 1, {MAX_COMMAND}),
                '') AS command,
             toUnixTimestamp64Milli(received_at) AS received",
            session = attr::SESSION_ID,
            command = attr::SHELL_COMMAND,
        ),
        "(kind = 'turn.user' OR kind = 'tool.started' AND tool_category IN ('file_write', 'shell'))
         AND ts >= {since:DateTime64(9, 'UTC')} AND ts < {until:DateTime64(9, 'UTC')}",
    );
    let mut rows = query(
        scope,
        &format!(
            "SELECT trajectory_id, session, id, kind, tool_category, command, received
             FROM ({steps}) ORDER BY trajectory_id, session, ts, id"
        ),
        window,
    )?
    .fetch::<Step>()?;
    let mut detect = Detect::default();
    while let Some(row) = rows.next().await? {
        detect.push(row);
    }
    Ok(detect.finish())
}

/// [`Detect`] over rows already in memory.
#[cfg(test)]
fn detect(rows: impl IntoIterator<Item = Step>) -> Found {
    let mut detect = Detect::default();
    for row in rows {
        detect.push(row);
    }
    detect.finish()
}

/// One turn's edits and the full test runs that followed an edit.
#[derive(Default)]
struct Turn {
    edits: usize,
    runs: usize,
    ids: Vec<String>,
    received: i64,
    edited_since_run: bool,
}

/// Takes rows ordered by trajectory, context, `ts` and event id.
#[derive(Default)]
pub struct Detect {
    trajectory: String,
    session: String,
    turn: Turn,
    /// The trajectory's turn with the most full runs, of those that count.
    worst: Option<Turn>,
    found: Strongest,
}

impl Detect {
    pub fn push(&mut self, step: Step) {
        if step.trajectory_id != self.trajectory {
            self.end_turn();
            self.end_trajectory();
            self.trajectory = step.trajectory_id;
            self.session = step.session;
        } else if step.session != self.session {
            self.end_turn();
            self.session = step.session;
        }
        if step.kind == "turn.user" {
            self.end_turn();
            return;
        }
        let turn = &mut self.turn;
        if step.category == "file_write" {
            turn.edits += 1;
            turn.edited_since_run = true;
        } else if turn.edited_since_run && is_full_test_run(&step.command) {
            turn.runs += 1;
            turn.received = turn.received.max(step.received);
            if turn.ids.len() < MAX_EVIDENCE {
                turn.ids.push(step.event_id);
            }
            turn.edited_since_run = false;
        }
    }

    /// A turn counts when a full run followed most of its edits.
    fn end_turn(&mut self) {
        let turn = std::mem::take(&mut self.turn);
        let counts = turn.runs >= MIN_RUNS && turn.runs * 2 > turn.edits;
        if counts && self.worst.as_ref().is_none_or(|w| turn.runs > w.runs) {
            self.worst = Some(turn);
        }
    }

    fn end_trajectory(&mut self) {
        if let Some(turn) = self.worst.take() {
            let size = turn.runs as f64;
            self.found.push(finding(&self.trajectory, &turn), size);
        }
    }

    pub fn finish(mut self) -> Found {
        self.end_turn();
        self.end_trajectory();
        self.found.finish()
    }
}

fn finding(trajectory: &str, turn: &Turn) -> Finding {
    let (runs, edits) = (count(turn.runs as u64), count(turn.edits as u64));
    Finding {
        detector: NAME,
        version: VERSION,
        scope: trajectory.to_string(),
        severity: if turn.runs >= 10 {
            Severity::High
        } else {
            Severity::Medium
        },
        title: format!("Ran the full test suite after {runs} of {edits} edits"),
        body: format!(
            "In one turn of trajectory {}, the agent ran the full test suite after {runs} of its {edits} edits.",
            short(trajectory)
        ),
        recommendation:
            "Run the tests for what changed (one crate, package or file) after each edit, \
            and the full suite once at the end of the turn. Say so in the agent's instructions."
                .into(),
        evidence: vec![Evidence {
            trajectory_id: trajectory.to_string(),
            event_ids: ids(&turn.ids),
            note: plural(turn.runs as u64, "full test run", "full test runs"),
        }],
        metrics: BTreeMap::from([
            ("edits".to_string(), turn.edits as f64),
            ("full_runs".to_string(), turn.runs as f64),
        ]),
        evidence_received_at: arrived(turn.received),
    }
}

/// Test runners, as the words a command starts with.
const RUNNERS: &[&[&str]] = &[
    &["cargo", "test"],
    &["cargo", "nextest", "run"],
    &["npm", "test"],
    &["npm", "run", "test"],
    &["pnpm", "test"],
    &["pnpm", "run", "test"],
    &["yarn", "test"],
    &["bun", "test"],
    &["npx", "jest"],
    &["npx", "vitest"],
    &["jest"],
    &["vitest"],
    &["pytest"],
    &["python", "-m", "pytest"],
    &["python3", "-m", "pytest"],
    &["go", "test"],
    &["make", "test"],
    &["mvn", "test"],
    &["gradle", "test"],
    &["./gradlew", "test"],
    &["rspec"],
    &["bundle", "exec", "rspec"],
];

/// Flags that pick which tests run, whether written `--flag value`,
/// `--flag=value` or, for one letter, `-fvalue`.
const NARROWING: &[&str] = &[
    // cargo
    "-p",
    "--package",
    "--test",
    "--bin",
    "--example",
    "--bench",
    "--lib",
    "--doc",
    "--manifest-path",
    // go
    "-run",
    // pytest
    "-k",
    "-m",
    "--lf",
    "--last-failed",
    "--deselect",
    "--sw",
    "--stepwise",
    // jest, vitest
    "-t",
    "--testNamePattern",
    "--testPathPattern",
    "--testPathPatterns",
    "-o",
    "--onlyChanged",
    "--changed",
    "--changedSince",
    "--findRelatedTests",
    "--related",
    "--project",
    "--selectProjects",
    // npm, pnpm
    "--filter",
    // gradle, maven
    "--tests",
    "-Dtest",
];

/// A shell line without its redirections (`2>&1`, `> out.txt`, `2>/dev/null`),
/// which say where output goes, not what runs. Their `&` and file names
/// would otherwise read as a command separator and an argument.
fn without_redirections(line: &str) -> String {
    let mut kept = Vec::new();
    for command in line.split('\n') {
        let mut words = command.split_whitespace();
        while let Some(word) = words.next() {
            if !word.contains(['<', '>']) {
                kept.push(word);
            } else if word.ends_with(['<', '>']) {
                // The target is the next word.
                words.next();
            }
        }
        kept.push("\n");
    }
    kept.join(" ")
}

/// Whether any command in a shell line runs a whole test suite: a known
/// runner followed only by flags that don't narrow it. A path, a package, a
/// filter, a test name or a flag that picks tests scopes it.
pub fn is_full_test_run(line: &str) -> bool {
    // Quotes go: in `bash -lc 'cd web && npm test'` the script is what runs.
    without_redirections(&line.replace("\\\n", " "))
        .replace(['\'', '"'], "")
        .split(['&', '|', ';', '\n', '(', ')'])
        .any(|command| {
            let words: Vec<&str> = command.split_whitespace().collect();
            let words = unwrapped(&words);
            RUNNERS
                .iter()
                .any(|runner| words.starts_with(runner) && full(runner, &words[runner.len()..]))
        })
}

/// The command without what only wraps it: environment assignments, `env`,
/// `time`, `nice`, `timeout 600`, `nohup`, `command`, `exec`, and a shell
/// running it (`bash -lc cargo test`, as the Codex adapter writes argv).
fn unwrapped<'a>(mut words: &'a [&'a str]) -> &'a [&'a str] {
    loop {
        let Some((first, rest)) = words.split_first() else {
            return words;
        };
        words = match *first {
            w if w.contains('=') && !w.starts_with('-') => rest,
            "env" | "time" | "nice" | "nohup" | "command" | "exec" => {
                without_flags(rest, &["-n", "-u"])
            }
            // Its flags, then how long to wait.
            "timeout" => without_flags(rest, &["-s", "-k"]).get(1..).unwrap_or(&[]),
            "bash" | "sh" | "zsh" | "dash" => without_flags(rest, &["-o", "+o"]),
            _ => return words,
        };
    }
}

/// `words` without the flags they start with; those in `with_value` take
/// the next word along.
fn without_flags<'a>(mut words: &'a [&'a str], with_value: &[&str]) -> &'a [&'a str] {
    while let Some((w, rest)) = words.split_first() {
        if !w.starts_with(['-', '+']) {
            break;
        }
        words = match with_value.contains(w) {
            true => rest.get(1..).unwrap_or(&[]),
            false => rest,
        };
    }
    words
}

/// Whether a runner's arguments leave its whole suite to run.
fn full(runner: &[&str], args: &[&str]) -> bool {
    let go = runner[0] == "go";
    // Without `./...`, `go test` tests one package: its narrowest run.
    if go && !args.contains(&"./...") {
        return false;
    }
    args.iter().all(|w| match *w {
        "./..." => go,
        // `vitest run`; and what ends a runner's own flags.
        "run" | "--" => true,
        w if w.starts_with('-') => !narrows(w, go),
        _ => false,
    })
}

fn narrows(flag: &str, go: bool) -> bool {
    let (name, value) = match flag.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (flag, None),
    };
    // `--workspace` alone is cargo's every crate; npm's `--workspace=web` is one.
    if NARROWING.contains(&name) || (name == "--workspace" && value.is_some()) {
        return true;
    }
    // A one-letter flag with its value attached, as `-pgsd`. Go's flags are
    // words after one dash (`-race`), so never this.
    !go && !flag.starts_with("--")
        && flag.len() > 2
        && flag.get(..2).is_some_and(|f| NARROWING.contains(&f))
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[test]
    fn tells_full_runs_from_scoped_ones() {
        for full in [
            "cargo test",
            "cargo test --workspace -- --nocapture",
            "RUST_LOG=debug cargo test",
            "cd web && npm test",
            "go test ./...",
            "go test -count=1 -race ./...",
            "npx vitest run",
            "pytest -x -q",
            "cargo test 2>&1 | tail -20",
            "cargo test > /tmp/out.txt 2>&1",
            "npm test 2>/dev/null",
            "cargo build &&\ncargo test 2>&1",
            "cargo test --features=serde --release",
            "npm test -- --watch=false",
            // Wrapped.
            "bash -lc cargo test",
            "bash -lc 'cargo test'",
            "sh -c \"npm test\"",
            "bash -o pipefail -c 'cd web && npm test'",
            "timeout 600 cargo test",
            "timeout -s KILL 10m cargo test",
            "nice -n 10 cargo test",
            "time cargo test",
            "env CI=1 cargo test",
            "cargo test \\\n  --workspace",
        ] {
            assert!(is_full_test_run(full), "{full}");
        }
        for scoped in [
            "cargo test -p gsd",
            "cargo test store::tests",
            "go test ./pkg/...",
            "pytest tests/test_api.py",
            "npm test -- src/app.test.ts",
            "cargo build",
            "cargo test -p gsd 2>&1 | tail -5",
            "cargo build 2>&1 | grep test",
            "",
            // Scoping in one word.
            "cargo test --package=gsd",
            "cargo test -pgsd",
            "cargo test --test=integration",
            "go test -run=TestCheckout ./...",
            "npx jest --testPathPattern=src/cart",
            "npm test -- --testPathPattern=checkout",
            "pytest --deselect=tests/test_slow.py",
            "pytest --lf",
            "pytest -kcheckout",
            "npm test --workspace=web",
            "mvn test -Dtest=CheckoutTest",
            // One Go package.
            "go test",
            "go test -v",
            // Wrapped, and scoped.
            "bash -lc 'cargo test -p gsd'",
            "timeout 600 cargo test store::tests",
            "sh script.sh",
        ] {
            assert!(!is_full_test_run(scoped), "{scoped}");
        }
    }

    fn step(trajectory: &str, kind: &str, category: &str, command: &str) -> Step {
        Step {
            trajectory_id: trajectory.into(),
            session: "s".into(),
            event_id: Uuid::now_v7().to_string(),
            kind: kind.into(),
            category: category.into(),
            command: command.into(),
            received: 7_000,
        }
    }

    #[test]
    fn finds_turns_that_rerun_the_suite_after_most_edits() {
        let prompt = || step("t1", "turn.user", "", "");
        let edit = || step("t1", "tool.started", "file_write", "");
        let test = || step("t1", "tool.started", "shell", "cargo test");
        let mut rows = vec![prompt()];
        for _ in 0..4 {
            rows.extend([edit(), test(), test()]); // the second run follows no edit
        }
        rows.extend([prompt(), edit(), test()]); // a later turn with one run
        let found = detect(rows);
        assert_eq!(found.findings.len(), 1);
        let f = &found.findings[0];
        assert_eq!(f.title, "Ran the full test suite after 4 of 4 edits");
        assert_eq!(f.evidence[0].event_ids.len(), 4);
        assert_eq!(f.evidence[0].note, "4 full test runs");
        assert_eq!(f.severity, Severity::Medium);
        assert_eq!(f.evidence_received_at, arrived(7_000));
        // The command stays in the events; the finding only counts.
        assert!(!f.body.contains("cargo test"));
        assert!(
            f.body
                .contains("ran the full test suite after 4 of its 4 edits")
        );
    }

    #[test]
    fn needs_three_runs_after_most_edits() {
        let edit = || step("t1", "tool.started", "file_write", "");
        let test = || step("t1", "tool.started", "shell", "cargo test");
        let two: Vec<_> = (0..2).flat_map(|_| [edit(), test()]).collect();
        assert!(detect(two).findings.is_empty(), "two runs");
        // Three runs after six edits: half, not most.
        let half: Vec<_> = (0..3).flat_map(|_| [edit(), edit(), test()]).collect();
        assert!(detect(half).findings.is_empty(), "half the edits");
        let ten: Vec<_> = (0..10).flat_map(|_| [edit(), test()]).collect();
        assert_eq!(detect(ten).findings[0].severity, Severity::High);
    }

    #[test]
    fn scoped_runs_and_rare_runs_are_fine() {
        let edit = || step("t1", "tool.started", "file_write", "");
        let scoped = || step("t1", "tool.started", "shell", "cargo test -p gsd");
        let full = || step("t1", "tool.started", "shell", "cargo test");
        let mut rows = Vec::new();
        for _ in 0..5 {
            rows.extend([edit(), scoped()]);
        }
        rows.extend([edit(), full()]);
        assert!(detect(rows).findings.is_empty());
    }

    #[test]
    fn a_subagent_has_turns_of_its_own() {
        // Rows come ordered by context. Two runs in one, one in the other:
        // neither turn has three.
        let in_session = |session: &str, category: &str, command: &str| Step {
            session: session.into(),
            ..step("t1", "tool.started", category, command)
        };
        let edit_and_test = |session: &str| {
            [
                in_session(session, "file_write", ""),
                in_session(session, "shell", "cargo test"),
            ]
        };
        let mut rows: Vec<_> = (0..2).flat_map(|_| edit_and_test("a")).collect();
        rows.extend(edit_and_test("b"));
        assert!(detect(rows).findings.is_empty());
    }
}
