//! On-disk fallback for when the daemon is unreachable. Hooks drop one file
//! per payload here; the daemon drains the directory at startup and on an
//! interval. Files are written to a temp name and renamed, so the daemon never
//! sees a partial write.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use groundstation_api::SpoolItem;
use uuid::Uuid;

use crate::ingest::Ingestor;
use crate::perms;

pub fn write(dir: &Path, item: &SpoolItem) -> Result<PathBuf> {
    perms::create_dir_all(dir)?;
    let name = format!(
        "{:020}-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or(0),
        Uuid::now_v7()
    );
    let tmp = dir.join(format!(".{name}.tmp"));
    let path = dir.join(format!("{name}.json"));
    perms::write(&tmp, &serde_json::to_vec(item)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Spooled files, oldest first.
pub fn pending(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    files
}

/// Where payloads for adapters this daemon doesn't have wait for one that does.
const DEFERRED: &str = "deferred";
const REJECTED: &str = "rejected";

/// Ingests and removes every spooled file. Unreadable files are moved to
/// `rejected/` (as `*.rejected`) rather than retried forever. Payloads for an adapter this
/// build of gsd doesn't know (a newer CLI talking to an older daemon) are
/// moved to `deferred/` and retried by the next daemon to start.
pub fn drain(dir: &Path, ingestor: &Ingestor) -> usize {
    let mut stored = 0;
    for path in pending(dir) {
        let item = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(serde_json::from_slice::<SpoolItem>(&bytes)?));
        if let Ok(SpoolItem::Hook { adapter, .. }) = &item
            && !ingestor.has_adapter(adapter)
        {
            tracing::warn!(%adapter, "deferring payload for an adapter this gsd doesn't have; restart gsd after upgrading");
            move_into(dir, DEFERRED, &path);
            continue;
        }
        let result = item.and_then(|item| ingestor.ingest_spooled(item));
        match result {
            Ok(n) => {
                stored += n;
                let _ = std::fs::remove_file(&path);
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), "rejecting spooled payload: {e:#}");
                let _ = perms::create_dir_all(&dir.join(REJECTED));
                let name = path.with_extension("rejected");
                let _ = std::fs::rename(
                    &path,
                    dir.join(REJECTED)
                        .join(name.file_name().unwrap_or_default()),
                );
            }
        }
    }
    stored
}

/// Puts payloads that an earlier daemon couldn't handle back in the spool:
/// everything in `deferred/`, plus hook payloads in `rejected/` still named
/// `*.json` (rejected by a gsd that predates deferral) whose adapter this
/// daemon has. Called at startup, since a daemon's adapters only change when
/// it is replaced. Returns how many were requeued.
pub fn requeue(dir: &Path, ingestor: &Ingestor) -> usize {
    let mut requeued = pending(&dir.join(DEFERRED));
    requeued.extend(pending(&dir.join(REJECTED)).into_iter().filter(|path| {
        let item = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SpoolItem>(&bytes).ok());
        matches!(item, Some(SpoolItem::Hook { adapter, .. }) if ingestor.has_adapter(&adapter))
    }));
    for path in &requeued {
        let _ = std::fs::rename(path, dir.join(path.file_name().unwrap_or_default()));
    }
    requeued.len()
}

fn move_into(dir: &Path, sub: &str, path: &Path) {
    let target = dir.join(sub);
    let _ = perms::create_dir_all(&target);
    let _ = std::fs::rename(path, target.join(path.file_name().unwrap_or_default()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::privacy::Privacy;
    use crate::store::Store;
    use groundstation_api::HookEnvelope;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn drains_in_order_and_rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let ingestor = Ingestor::new(
            store.clone(),
            Privacy::with_env(&Config::default(), []).unwrap(),
        );
        for hook in ["SessionStart", "Stop"] {
            let envelope = HookEnvelope {
                id: Uuid::now_v7(),
                observed_at: Utc::now(),
                payload: json!({"session_id": "s", "hook_event_name": hook}),
            };
            let item = SpoolItem::Hook {
                adapter: "claude-code".into(),
                envelope,
            };
            write(dir.path(), &item).unwrap();
        }
        std::fs::write(dir.path().join("zzz.json"), b"{nope").unwrap();

        assert_eq!(pending(dir.path()).len(), 3);
        assert_eq!(drain(dir.path(), &ingestor), 2);
        assert!(pending(dir.path()).is_empty());
        assert!(dir.path().join("rejected/zzz.rejected").exists());
        // Rejected by this daemon: never requeued.
        assert_eq!(requeue(dir.path(), &ingestor), 0);
        let (summary, _) = store.trajectory("s").unwrap().unwrap();
        assert_eq!(summary.status, "idle");
    }

    #[test]
    fn unknown_adapters_wait_for_a_newer_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let ingestor = Ingestor::new(store, Privacy::with_env(&Config::default(), []).unwrap());
        let item = SpoolItem::Hook {
            adapter: "future-agent".into(),
            envelope: HookEnvelope {
                id: Uuid::now_v7(),
                observed_at: Utc::now(),
                payload: json!({"session_id": "s", "hook_event_name": "Stop"}),
            },
        };
        write(dir.path(), &item).unwrap();
        assert_eq!(drain(dir.path(), &ingestor), 0);
        assert!(pending(dir.path()).is_empty());
        assert_eq!(pending(&dir.path().join(DEFERRED)).len(), 1);
        assert!(!dir.path().join("rejected").exists());

        // An older gsd rejected (rather than deferred) a payload for an adapter we have.
        let legacy = SpoolItem::Hook {
            adapter: "claude-code".into(),
            envelope: HookEnvelope {
                id: Uuid::now_v7(),
                observed_at: Utc::now(),
                payload: json!({"session_id": "s", "hook_event_name": "Stop"}),
            },
        };
        let legacy_path = write(dir.path(), &legacy).unwrap();
        move_into(dir.path(), REJECTED, &legacy_path);

        assert_eq!(requeue(dir.path(), &ingestor), 2);
        assert_eq!(pending(dir.path()).len(), 2);
        assert_eq!(drain(dir.path(), &ingestor), 1, "legacy payload ingested");
        assert_eq!(
            pending(&dir.path().join(DEFERRED)).len(),
            1,
            "future-agent waits again"
        );
    }
}
