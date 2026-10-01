//! What an event looks like when it leaves the machine. The local store keeps
//! full paths for the local UI and CLI; an uploaded event says where things
//! are within the repository, never where the repository is on disk.
//!
//! - `gs.host.name` never leaves: hostnames often carry a person's name.
//! - `gs.vcs.repository` is the repository's name outside this machine: its
//!   origin remote (`github.com/acme/shop`) or its directory name.
//! - `gs.file.path` and the paths in tool input become relative to the
//!   repository root (`src/checkout.rs`). Absolute paths outside it keep only
//!   their file name. Relative paths are left alone.
//! - `gs.cwd` becomes relative to the root, and is dropped outside one.
//! - `gs.transcript.path` is dropped: it only means something to this gsd.
//!
//! With `paths = "hash"` the stored paths are already keyed hashes, so they
//! leave as they are.

use std::path::Path;

use groundstation_schema::{Event, attr};
use serde_json::Value;

use crate::config::PathPolicy;
use crate::store::Place;

pub fn prepare(event: &mut Event, place: &Place, paths: PathPolicy) {
    let attrs = &mut event.attributes;
    attrs.remove(attr::HOST_NAME);
    // Whatever the event carried here may be a local path; the trajectory's
    // name outside this machine replaces it.
    match &place.origin {
        Some(origin) => {
            attrs.insert(attr::VCS_REPOSITORY.into(), Value::String(origin.clone()));
        }
        None => {
            attrs.remove(attr::VCS_REPOSITORY);
        }
    }
    attrs.remove(attr::TRANSCRIPT_PATH);
    if paths == PathPolicy::Hash {
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
    use chrono::Utc;
    use groundstation_schema::{Agent, EventKind};
    use serde_json::json;
    use uuid::Uuid;

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
        prepare(&mut e, &place(), PathPolicy::Keep);
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
    fn outside_any_repository_no_directory_leaves() {
        let mut e = event();
        prepare(&mut e, &Place::default(), PathPolicy::Keep);
        assert_eq!(e.get(attr::CWD), None);
        assert_eq!(e.get(attr::FILE_PATH), Some(&json!("app.ts")));
        assert_eq!(e.get(attr::VCS_REPOSITORY), None);
    }

    #[test]
    fn the_repository_root_itself_is_dot() {
        let mut e = event();
        e.set(attr::CWD, "/Users/ana/src/shop");
        prepare(&mut e, &place(), PathPolicy::Keep);
        assert_eq!(e.get(attr::CWD), Some(&json!(".")));
    }

    #[test]
    fn hashed_paths_leave_as_stored() {
        let mut e = event();
        e.set(attr::FILE_PATH, "sha256:0123456789abcdef");
        let mut place = place();
        place.origin = Some("sha256:fedcba9876543210".into());
        prepare(&mut e, &place, PathPolicy::Hash);
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
}
