//! On-disk fallback for when the daemon is unreachable. Hooks drop one file
//! per payload here; the daemon drains the directory at startup and on an
//! interval. Files are written to a temp name and renamed, so the daemon never
//! sees a partial write.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::api::SpoolItem;
use crate::ingest::Ingestor;

pub fn write(dir: &Path, item: &SpoolItem) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = format!(
        "{:020}-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or(0),
        Uuid::now_v7()
    );
    let tmp = dir.join(format!(".{name}.tmp"));
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&tmp, serde_json::to_vec(item)?)?;
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

/// Ingests and removes every spooled file. Unreadable files are moved to
/// `rejected/` rather than retried forever.
pub fn drain(dir: &Path, ingestor: &Ingestor) -> usize {
    let mut stored = 0;
    for path in pending(dir) {
        let item = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(serde_json::from_slice::<SpoolItem>(&bytes)?));
        let result = item.and_then(|item| ingestor.ingest_spooled(item));
        match result {
            Ok(n) => {
                stored += n;
                let _ = std::fs::remove_file(&path);
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), "rejecting spooled payload: {e:#}");
                let rejected = dir.join("rejected");
                let _ = std::fs::create_dir_all(&rejected);
                let _ = std::fs::rename(&path, rejected.join(path.file_name().unwrap_or_default()));
            }
        }
    }
    stored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HookEnvelope;
    use crate::config::Config;
    use crate::privacy::Privacy;
    use crate::store::Store;
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
            write(dir.path(), &SpoolItem::ClaudeCode(envelope)).unwrap();
        }
        std::fs::write(dir.path().join("zzz.json"), b"{nope").unwrap();

        assert_eq!(pending(dir.path()).len(), 3);
        assert_eq!(drain(dir.path(), &ingestor), 2);
        assert!(pending(dir.path()).is_empty());
        assert!(dir.path().join("rejected/zzz.json").exists());
        let (summary, _) = store.trajectory("s").unwrap().unwrap();
        assert_eq!(summary.status, "idle");
    }
}
