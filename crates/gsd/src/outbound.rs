//! What an event looks like when it leaves the machine. The local store keeps
//! full paths for the local UI and CLI; an uploaded event says where things
//! are within the repository, never where the repository is on disk.
//!
//! - `gs.host.name` never leaves: hostnames often carry a person's name.
//! - `gs.vcs.repository` is the repository's name outside this machine: its
//!   origin remote (`github.com/acme/shop`) or its directory name.
//! - `gs.file.path` and the paths in tool input and output become relative to
//!   the repository root (`src/checkout.rs`). Absolute paths outside it keep
//!   only their file name. Relative paths are left alone.
//! - `gs.cwd` becomes relative to the root, and is dropped outside one.
//! - `gs.transcript.path` is dropped: it only means something to this gsd.
//!
//! With `paths = "hash"` every path leaves as a keyed hash, and so does the
//! repository's name.
//!
//! `[redaction]` as it is now applies too, whatever was in force when the
//! event was stored: a backlog kept while gsd was local-only leaves without
//! the fields `exclude` now names, with secrets redacted if `secrets` is now
//! on, and with its paths hashed if `paths` is now `"hash"`.
//!
//! An event whose attributes are larger than the backend stores
//! ([`MAX_EVENT_ATTRIBUTES`]) leaves with its content shortened to fit.

use std::path::Path;

use groundstation_schema::{Event, attr};
use groundstation_wire::MAX_EVENT_ATTRIBUTES;
use serde_json::{Map, Value};

use crate::config::PathPolicy;
use crate::privacy::{Privacy, truncate};
use crate::store::Place;

pub fn prepare(event: &mut Event, place: &Place, privacy: &Privacy) {
    privacy.apply(event);
    relocate(event, place, privacy);
    fit(&mut event.attributes);
}

fn relocate(event: &mut Event, place: &Place, privacy: &Privacy) {
    let attrs = &mut event.attributes;
    attrs.remove(attr::HOST_NAME);
    // Whatever the event carried here may be a local path; the trajectory's
    // name outside this machine replaces it.
    match &place.origin {
        Some(origin) => {
            attrs.insert(
                attr::VCS_REPOSITORY.into(),
                Value::String(privacy.path(origin)),
            );
        }
        None => {
            attrs.remove(attr::VCS_REPOSITORY);
        }
    }
    attrs.remove(attr::TRANSCRIPT_PATH);
    if privacy.paths() == PathPolicy::Hash {
        // `apply` hashed every path.
        return;
    }

    let root = place.repository.as_deref().map(Path::new);
    if let Some(cwd) = attrs
        .get(attr::CWD)
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        match within(root, &cwd) {
            Some(rel) => {
                attrs.insert(attr::CWD.into(), Value::String(rel));
            }
            None if Path::new(&cwd).is_absolute() => {
                attrs.remove(attr::CWD);
            }
            None => {}
        }
    }
    if let Some(Value::String(path)) = attrs.get_mut(attr::FILE_PATH) {
        *path = outbound_path(root, path);
    }
    if let Some(Value::Object(input)) = attrs.get_mut(attr::TOOL_INPUT) {
        for key in attr::TOOL_INPUT_PATH_KEYS {
            if let Some(Value::String(path)) = input.get_mut(*key) {
                *path = outbound_path(root, path);
            }
        }
    }
    if let Some(output) = attrs.get_mut(attr::TOOL_OUTPUT) {
        for pointer in attr::TOOL_OUTPUT_PATHS {
            if let Some(Value::String(path)) = output.pointer_mut(pointer) {
                *path = outbound_path(root, path);
            }
        }
    }
}

/// Shortens the content attributes (prompt, tool bodies, shell command) until
/// the attributes fit what the backend stores. gsd limits each string, not an
/// event's total, and the backend refuses an event that is too large, which
/// gsd drops after three refusals. Measurements and paths stay as they are.
fn fit(attrs: &mut Map<String, Value>) {
    // Most events fit even at their longest, which costs no serializing.
    if map_bound(attrs) <= MAX_EVENT_ATTRIBUTES {
        return;
    }
    let size = |v: &Map<String, Value>| serde_json::to_vec(v).map_or(0, |b| b.len());
    let mut cap = 64 * 1024;
    let mut replaced = Vec::new();
    while size(attrs) > MAX_EVENT_ATTRIBUTES {
        if cap > 256 {
            cap /= 2;
            for key in attr::CONTENT {
                if let Some(value) = attrs.get_mut(*key) {
                    shorten(value, cap);
                }
            }
            continue;
        }
        // Many short strings: give up the largest content attribute whole.
        let largest = attr::CONTENT
            .iter()
            .filter(|key| !replaced.contains(*key))
            .filter_map(|key| Some((*key, serde_json::to_vec(attrs.get(*key)?).ok()?.len())))
            .max_by_key(|(_, bytes)| *bytes);
        let Some((key, bytes)) = largest else {
            return;
        };
        attrs.insert(
            key.into(),
            Value::String(format!("…[truncated {bytes} bytes]")),
        );
        replaced.push(key);
    }
}

/// The most bytes `value` can take as JSON: a string's byte escapes to at
/// most six (`\u001f`), and a number takes at most 24 characters.
fn json_bound(value: &Value) -> usize {
    match value {
        Value::Null | Value::Bool(_) => 5,
        Value::Number(_) => 24,
        Value::String(s) => string_bound(s),
        Value::Array(items) => 2 + items.iter().map(|v| json_bound(v) + 1).sum::<usize>(),
        Value::Object(map) => map_bound(map),
    }
}

fn map_bound(map: &Map<String, Value>) -> usize {
    2 + map
        .iter()
        .map(|(key, value)| string_bound(key) + 1 + json_bound(value) + 1)
        .sum::<usize>()
}

fn string_bound(s: &str) -> usize {
    2 + 6 * s.len()
}

fn shorten(value: &mut Value, cap: usize) {
    match value {
        Value::String(s) => truncate(s, cap),
        Value::Array(items) => items.iter_mut().for_each(|v| shorten(v, cap)),
        Value::Object(map) => map.values_mut().for_each(|v| shorten(v, cap)),
        _ => {}
    }
}

/// `path` relative to `root`, or `None` if it isn't inside it.
fn within(root: Option<&Path>, path: &str) -> Option<String> {
    let rel = Path::new(path).strip_prefix(root?).ok()?;
    Some(match rel.as_os_str().is_empty() {
        true => ".".into(),
        false => rel.to_string_lossy().into_owned(),
    })
}

fn outbound_path(root: Option<&Path>, path: &str) -> String {
    if let Some(rel) = within(root, path) {
        return rel;
    }
    let p = Path::new(path);
    if !p.is_absolute() {
        return path.to_string();
    }
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use chrono::Utc;
    use groundstation_schema::{Agent, EventKind};
    use serde_json::json;
    use uuid::Uuid;

    fn privacy(f: impl FnOnce(&mut Config)) -> Privacy {
        let mut config = Config::default();
        f(&mut config);
        Privacy::with_env(&config, []).unwrap()
    }

    fn keep() -> Privacy {
        privacy(|_| {})
    }

    fn hash() -> Privacy {
        privacy(|c| c.redaction.paths = PathPolicy::Hash)
    }

    fn event() -> Event {
        let mut e = Event::new(
            Uuid::now_v7(),
            "t",
            EventKind::ToolStarted,
            Utc::now(),
            Agent::named("x"),
        );
        e.set(attr::CWD, "/Users/ana/src/shop/web");
        e.set(attr::FILE_PATH, "/Users/ana/src/shop/web/app.ts");
        e.set(attr::TRANSCRIPT_PATH, "/Users/ana/.claude/projects/x.jsonl");
        e.set(attr::HOST_NAME, "Anas-MacBook-Pro.local");
        e.set(
            attr::TOOL_INPUT,
            json!({"file_path": "/Users/ana/.zshrc", "path": "src", "pattern": "/Users/ana"}),
        );
        e
    }

    fn place() -> Place {
        Place {
            repository: Some("/Users/ana/src/shop".into()),
            origin: Some("github.com/acme/shop".into()),
        }
    }

    #[test]
    fn paths_leave_relative_to_the_repository() {
        let mut e = event();
        prepare(&mut e, &place(), &keep());
        let get = |k| e.get(k).cloned();
        assert_eq!(get(attr::HOST_NAME), None);
        assert_eq!(
            get(attr::VCS_REPOSITORY),
            Some(json!("github.com/acme/shop"))
        );
        assert_eq!(get(attr::CWD), Some(json!("web")));
        assert_eq!(get(attr::FILE_PATH), Some(json!("web/app.ts")));
        assert_eq!(get(attr::TRANSCRIPT_PATH), None);
        // Outside the repository only the file name leaves; relative paths
        // stay; keys that aren't paths are content and untouched.
        assert_eq!(
            get(attr::TOOL_INPUT),
            Some(json!({"file_path": ".zshrc", "path": "src", "pattern": "/Users/ana"}))
        );
        assert!(
            !serde_json::to_string(&e.attributes)
                .unwrap()
                .contains("/Users/ana/src")
        );
    }

    #[test]
    fn file_tool_paths_leave_relative_wherever_the_agent_puts_them() {
        let mut e = event();
        // OpenCode names it filePath; Claude Code's results repeat it.
        e.set(
            attr::TOOL_INPUT,
            json!({ "filePath": "/Users/ana/src/shop/web/app.ts" }),
        );
        e.set(
            attr::TOOL_OUTPUT,
            json!({ "filePath": "/Users/ana/src/shop/web/app.ts",
                    "file": { "filePath": "/Users/ana/.zshrc", "content": "x" } }),
        );
        prepare(&mut e, &place(), &keep());
        assert_eq!(
            e.get(attr::TOOL_INPUT),
            Some(&json!({ "filePath": "web/app.ts" }))
        );
        assert_eq!(
            e.get(attr::TOOL_OUTPUT),
            Some(
                &json!({ "filePath": "web/app.ts", "file": { "filePath": ".zshrc", "content": "x" } })
            )
        );
        assert!(
            !serde_json::to_string(&e.attributes)
                .unwrap()
                .contains("/Users/ana")
        );
    }

    #[test]
    fn an_event_leaves_small_enough_for_the_backend_to_store() {
        let size = |e: &Event| serde_json::to_vec(&e.attributes).unwrap().len();
        // Each string within gsd's own limit, the whole far over the backend's.
        let mut e = event();
        let lines: Vec<String> = (0..40)
            .map(|i| format!("{i}{}", "x".repeat(40_000)))
            .collect();
        e.set(attr::TOOL_OUTPUT, json!({ "structuredPatch": lines }));
        e.set(attr::DURATION_MS, 1200);
        prepare(&mut e, &place(), &keep());
        assert!(size(&e) <= MAX_EVENT_ATTRIBUTES, "{}", size(&e));
        assert_eq!(e.get(attr::DURATION_MS), Some(&json!(1200)));
        assert_eq!(e.get(attr::FILE_PATH), Some(&json!("web/app.ts")));
        let patch = &e.get(attr::TOOL_OUTPUT).unwrap()["structuredPatch"];
        assert!(patch[0].as_str().unwrap().contains("…[truncated"));

        // So many short strings that no cap helps: the body gives way.
        let mut e = event();
        e.set(attr::TOOL_OUTPUT, json!(vec!["ab"; 400_000]));
        prepare(&mut e, &place(), &hash());
        assert!(size(&e) <= MAX_EVENT_ATTRIBUTES, "{}", size(&e));
        assert!(
            e.get(attr::TOOL_OUTPUT)
                .unwrap()
                .as_str()
                .unwrap()
                .starts_with("…[truncated")
        );

        // Small events keep their content whole.
        let mut small = event();
        small.set(attr::PROMPT_TEXT, "fix the checkout");
        prepare(&mut small, &place(), &hash());
        assert_eq!(
            small.get(attr::PROMPT_TEXT),
            Some(&json!("fix the checkout"))
        );
        assert_eq!(
            small.get(attr::TOOL_INPUT).unwrap()["pattern"],
            "/Users/ana"
        );
    }

    #[test]
    fn outside_any_repository_no_directory_leaves() {
        let mut e = event();
        prepare(&mut e, &Place::default(), &keep());
        assert_eq!(e.get(attr::CWD), None);
        assert_eq!(e.get(attr::FILE_PATH), Some(&json!("app.ts")));
        assert_eq!(e.get(attr::VCS_REPOSITORY), None);
    }

    #[test]
    fn the_repository_root_itself_is_dot() {
        let mut e = event();
        e.set(attr::CWD, "/Users/ana/src/shop");
        prepare(&mut e, &place(), &keep());
        assert_eq!(e.get(attr::CWD), Some(&json!(".")));
    }

    #[test]
    fn hashed_paths_leave_as_stored() {
        let mut e = event();
        e.set(attr::FILE_PATH, "sha256:0123456789abcdef");
        let mut place = place();
        place.origin = Some("sha256:fedcba9876543210".into());
        prepare(&mut e, &place, &hash());
        assert_eq!(
            e.get(attr::FILE_PATH),
            Some(&json!("sha256:0123456789abcdef"))
        );
        assert_eq!(
            e.get(attr::VCS_REPOSITORY),
            Some(&json!("sha256:fedcba9876543210"))
        );
        assert_eq!(e.get(attr::TRANSCRIPT_PATH), None);
    }

    #[test]
    fn a_backlog_leaves_under_the_settings_in_force_now() {
        // Stored while local-only, under the defaults; then the person
        // excluded prompts and shell commands, and connected.
        let mut e = event();
        e.set(attr::PROMPT_TEXT, "fix the checkout");
        e.set(attr::SHELL_COMMAND, "cargo test");
        e.set(attr::ERROR_MESSAGE, "denied for AKIAABCDEFGHIJKLMNOP");
        let now = privacy(|c| {
            c.redaction.exclude = vec!["prompt".into(), "shell".into()];
            c.redaction.secrets = true;
        });
        prepare(&mut e, &place(), &now);
        assert_eq!(e.get(attr::PROMPT_TEXT), None);
        assert_eq!(e.get(attr::SHELL_COMMAND), None);
        assert_eq!(
            e.get(attr::ERROR_MESSAGE),
            Some(&json!("denied for [REDACTED]"))
        );
    }

    #[test]
    fn the_size_bound_is_never_below_the_json() {
        let worst = "\u{0}\u{1f}\"\\\n\t/é😀\u{7f}\u{2028}";
        let values = [
            json!(null),
            json!(true),
            json!(-9_223_372_036_854_775_808_i64),
            json!(u64::MAX),
            json!(-1.797_693_134_862_315_7e308),
            json!(f64::MIN_POSITIVE),
            json!(worst),
            json!(worst.repeat(1000)),
            json!({ worst: [worst, 1.5e-300, null, {"": [[], {}]}] }),
            json!([[[[[]]]], {"a": {"b": {"c": worst}}}]),
        ];
        for value in values {
            let real = serde_json::to_vec(&value).unwrap().len();
            assert!(
                json_bound(&value) >= real,
                "{value}: {} < {real}",
                json_bound(&value)
            );
            let map = Map::from_iter([(worst.to_string(), value.clone())]);
            let real = serde_json::to_vec(&map).unwrap().len();
            assert!(map_bound(&map) >= real, "{value} in a map");
        }
    }

    #[test]
    fn content_cut_when_stored_leaves_saying_how_much_was_cut() {
        let p = privacy(|c| c.capture.max_content_bytes = 16);
        let mut e = event();
        e.set(attr::TOOL_OUTPUT, "x".repeat(1000));
        p.apply(&mut e);
        let stored = json!(format!("{}…[truncated 984 bytes]", "x".repeat(16)));
        assert_eq!(e.get(attr::TOOL_OUTPUT), Some(&stored));
        prepare(&mut e, &place(), &p);
        assert_eq!(e.get(attr::TOOL_OUTPUT), Some(&stored));
    }

    #[test]
    fn full_paths_stored_before_hashing_was_on_leave_hashed() {
        let mut e = event();
        prepare(&mut e, &place(), &hash());
        let hashed = |v: Option<&Value>| {
            v.and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("sha256:"))
        };
        assert!(hashed(e.get(attr::CWD)), "{:?}", e.get(attr::CWD));
        assert!(hashed(e.get(attr::FILE_PATH)));
        assert!(hashed(e.get(attr::VCS_REPOSITORY)));
        assert!(hashed(
            e.get(attr::TOOL_INPUT).and_then(|i| i.get("file_path"))
        ));
        // Hashing twice gives the same value: the stored hash.
        let mut again = e.clone();
        prepare(&mut again, &place(), &hash());
        assert_eq!(again.get(attr::FILE_PATH), e.get(attr::FILE_PATH));
    }
}
