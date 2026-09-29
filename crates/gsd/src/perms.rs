//! Owner-only files and directories. Everything gsd writes (the database,
//! spooled payloads, the log) can hold prompts, tool output and source code,
//! so on Unix directories are created `0700` and files `0600`, and paths
//! created by an older gsd are tightened on the next start. Windows inherits
//! the user profile's ACLs.

use std::fs::{DirBuilder, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

/// `create_dir_all` with owner-only permissions on every directory it
/// creates and on `dir` itself.
pub fn create_dir_all(dir: &Path) -> Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, DIR_MODE);
    builder
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    restrict(dir, DIR_MODE)
}

/// `OpenOptions` that create files readable and writable by the owner only.
pub fn open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, FILE_MODE);
    options
}

/// Writes `bytes` to an owner-only file, replacing any existing content.
pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = open_options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing {}", path.display()))?;
    restrict(path, FILE_MODE)
}

/// Creates `path` as an empty owner-only file if it doesn't exist, and
/// tightens its permissions if it does. Content is left alone.
pub fn touch(path: &Path) -> Result<()> {
    open_options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    restrict(path, FILE_MODE)
}

/// Sets `mode` on an existing path. A no-op outside Unix.
pub fn restrict(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("restricting permissions of {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a/b");
        create_dir_all(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&tmp.path().join("a")), 0o700);

        let file = dir.join("f");
        write(&file, b"x").unwrap();
        assert_eq!(mode(&file), 0o600);

        // Paths left by an older gsd are tightened without losing content.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        touch(&file).unwrap();
        create_dir_all(&dir).unwrap();
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(std::fs::read(&file).unwrap(), b"x");
    }
}
