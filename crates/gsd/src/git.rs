//! Reads a repository's state straight from its files: the top-level
//! directory, the checked-out branch and commit, and the `origin` remote.
//! gsd never runs `git`, so a repository's own config (`core.fsmonitor`,
//! hooks, pagers) can't make the daemon execute anything.
//!
//! Handles ordinary repositories, linked worktrees and submodules (where
//! `.git` is a file pointing elsewhere), loose and packed refs, and a detached
//! HEAD. Repositories using the reftable ref format report a branch but no
//! commit.

use std::path::{Path, PathBuf};

/// A repository found from a directory inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The working tree's top-level directory.
    pub root: PathBuf,
    /// This working tree's git directory (`HEAD` lives here).
    git_dir: PathBuf,
    /// Shared by all worktrees: refs, packed-refs and config live here.
    common_dir: PathBuf,
}

/// What a working tree has checked out.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Head {
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    /// `None` before the first commit.
    pub revision: Option<String>,
}

impl Repo {
    /// Finds the repository containing `dir` by walking up to a `.git`.
    pub fn discover(dir: &Path) -> Option<Repo> {
        for root in dir.ancestors() {
            let dot_git = root.join(".git");
            let git_dir = if dot_git.is_dir() {
                dot_git
            } else if dot_git.is_file() {
                // Worktrees and submodules: `gitdir: <path>`, relative to the file.
                let text = std::fs::read_to_string(&dot_git).ok()?;
                let target = text.strip_prefix("gitdir:")?.trim();
                root.join(target)
            } else {
                continue;
            };
            if !git_dir.join("HEAD").is_file() {
                return None;
            }
            let common_dir = std::fs::read_to_string(git_dir.join("commondir"))
                .ok()
                .map(|c| git_dir.join(c.trim()))
                .unwrap_or_else(|| git_dir.clone());
            return Some(Repo {
                root: root.to_path_buf(),
                git_dir,
                common_dir,
            });
        }
        None
    }

    pub fn head(&self) -> Head {
        let Ok(text) = std::fs::read_to_string(self.git_dir.join("HEAD")) else {
            return Head::default();
        };
        let text = text.trim();
        match text.strip_prefix("ref:") {
            Some(name) => {
                let name = name.trim();
                Head {
                    branch: name.strip_prefix("refs/heads/").map(str::to_string),
                    revision: self.resolve(name),
                }
            }
            None => Head {
                branch: None,
                revision: is_object_id(text).then(|| text.to_string()),
            },
        }
    }

    /// The `origin` remote as a host and path, without scheme, credentials or
    /// `.git`: `github.com/acme/shop`. `None` without a network remote.
    pub fn origin(&self) -> Option<String> {
        let config = std::fs::read_to_string(self.common_dir.join("config")).ok()?;
        normalize_remote(&remote_url(&config, "origin")?)
    }

    /// Follows a ref to a commit: loose ref files first, then packed-refs.
    fn resolve(&self, name: &str) -> Option<String> {
        let mut name = name.to_string();
        // Symbolic refs can point at other refs; don't follow a cycle forever.
        for _ in 0..5 {
            if !is_safe_ref(&name) {
                return None;
            }
            let loose = [&self.git_dir, &self.common_dir]
                .iter()
                .find_map(|dir| std::fs::read_to_string(dir.join(&name)).ok());
            match loose {
                Some(text) => {
                    let text = text.trim();
                    match text.strip_prefix("ref:") {
                        Some(next) => name = next.trim().to_string(),
                        None => return is_object_id(text).then(|| text.to_string()),
                    }
                }
                None => return self.packed(&name),
            }
        }
        None
    }

    fn packed(&self, name: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.common_dir.join("packed-refs")).ok()?;
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.starts_with('^'))
            .find_map(|l| {
                let (id, refname) = l.split_once(' ')?;
                (refname.trim() == name && is_object_id(id)).then(|| id.to_string())
            })
    }
}

/// A ref name read from the repository is only ever looked up under `refs/`,
/// so a crafted HEAD can't point gsd at files outside the git directory.
fn is_safe_ref(name: &str) -> bool {
    name.starts_with("refs/")
        && !name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

/// A SHA-1 or SHA-256 object id.
fn is_object_id(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The `url` of `[remote "<name>"]` in a git config file. Comments and
/// quoting are handled; includes are not followed.
fn remote_url(config: &str, name: &str) -> Option<String> {
    let header = format!("[remote \"{name}\"]");
    let mut in_section = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line.eq_ignore_ascii_case(&header);
            continue;
        }
        if !in_section || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("url") {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            return Some(value.to_string());
        }
    }
    None
}

/// `https://user:token@github.com/acme/shop.git`, `ssh://git@github.com:22/acme/shop`
/// and `git@github.com:acme/shop.git` all become `github.com/acme/shop`.
/// Local paths and `file://` remotes give `None`: they'd leak a path.
fn normalize_remote(url: &str) -> Option<String> {
    let (host, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            if scheme.eq_ignore_ascii_case("file") {
                return None;
            }
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next()?;
            let host = host.split(':').next()?;
            (host, path)
        }
        // scp-like `user@host:path`, as long as the colon comes before any slash.
        None => {
            let (authority, path) = url.split_once(':')?;
            if authority.contains('/') {
                return None;
            }
            (authority.rsplit('@').next()?, path)
        }
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{path}", host.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    /// A minimal repository on disk, without git.
    fn repo(dir: &Path, head: &str) -> PathBuf {
        let git = dir.join(".git");
        fs::create_dir_all(git.join("refs/heads")).unwrap();
        fs::write(git.join("HEAD"), format!("{head}\n")).unwrap();
        git
    }

    #[test]
    fn reads_branch_and_commit_from_loose_and_packed_refs() {
        let dir = tempfile::tempdir().unwrap();
        let git = repo(dir.path(), "ref: refs/heads/main");
        fs::write(git.join("refs/heads/main"), format!("{A}\n")).unwrap();
        fs::create_dir_all(dir.path().join("src/deep")).unwrap();

        let r = Repo::discover(&dir.path().join("src/deep")).unwrap();
        assert_eq!(r.root, dir.path());
        let head = r.head();
        assert_eq!(head.branch.as_deref(), Some("main"));
        assert_eq!(head.revision.as_deref(), Some(A));

        // After `git gc` the loose ref is gone and lives in packed-refs.
        fs::remove_file(git.join("refs/heads/main")).unwrap();
        fs::write(
            git.join("packed-refs"),
            format!("# pack-refs with: peeled\n{B} refs/heads/other\n{A} refs/heads/main\n^{B}\n"),
        )
        .unwrap();
        assert_eq!(r.head().revision.as_deref(), Some(A));
    }

    #[test]
    fn detached_and_unborn_heads() {
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path(), A);
        let head = Repo::discover(dir.path()).unwrap().head();
        assert_eq!((head.branch, head.revision.as_deref()), (None, Some(A)));

        let dir = tempfile::tempdir().unwrap();
        repo(dir.path(), "ref: refs/heads/main");
        let head = Repo::discover(dir.path()).unwrap().head();
        assert_eq!(
            (head.branch.as_deref(), head.revision),
            (Some("main"), None)
        );
    }

    #[test]
    fn follows_worktree_gitdir_files_to_shared_refs() {
        let main = tempfile::tempdir().unwrap();
        let git = repo(main.path(), "ref: refs/heads/main");
        fs::write(git.join("refs/heads/feature"), format!("{B}\n")).unwrap();
        let wt_git = git.join("worktrees/wt");
        fs::create_dir_all(&wt_git).unwrap();
        fs::write(wt_git.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
        fs::write(wt_git.join("commondir"), "../..\n").unwrap();

        let wt = tempfile::tempdir().unwrap();
        fs::write(
            wt.path().join(".git"),
            format!("gitdir: {}\n", wt_git.display()),
        )
        .unwrap();
        let r = Repo::discover(wt.path()).unwrap();
        assert_eq!(r.root, wt.path());
        let head = r.head();
        assert_eq!(head.branch.as_deref(), Some("feature"));
        assert_eq!(head.revision.as_deref(), Some(B));
    }

    #[test]
    fn a_crafted_head_cannot_reach_outside_refs() {
        let dir = tempfile::tempdir().unwrap();
        let git = repo(dir.path(), "ref: refs/../../secret");
        fs::write(dir.path().join("secret"), format!("{A}\n")).unwrap();
        fs::write(git.join("refs/heads/x"), "ref: ../../../secret\n").unwrap();
        let r = Repo::discover(dir.path()).unwrap();
        assert_eq!(r.head().revision, None);
        assert_eq!(r.resolve("refs/heads/x"), None);
    }

    #[test]
    fn outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Repo::discover(dir.path()), None);
    }

    #[test]
    fn origin_is_host_and_path_without_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let git = repo(dir.path(), "ref: refs/heads/main");
        fs::write(
            git.join("config"),
            "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = git@github.com:other/x.git\n\
             [remote \"origin\"]\n\t# a comment\n\turl = https://bot:ghp_secret@GitHub.com/acme/shop.git\n",
        )
        .unwrap();
        let r = Repo::discover(dir.path()).unwrap();
        assert_eq!(r.origin().as_deref(), Some("github.com/acme/shop"));

        for (url, want) in [
            ("git@github.com:acme/shop.git", Some("github.com/acme/shop")),
            (
                "ssh://git@gitlab.example.com:2222/team/api/",
                Some("gitlab.example.com/team/api"),
            ),
            (
                "https://dev.azure.com/org/proj/_git/repo",
                Some("dev.azure.com/org/proj/_git/repo"),
            ),
            ("/srv/git/shop.git", None),
            ("file:///srv/git/shop.git", None),
            ("../shop", None),
        ] {
            assert_eq!(normalize_remote(url).as_deref(), want, "{url}");
        }
    }

    #[test]
    fn agrees_with_git_on_a_real_repository() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        };
        if git(&["init", "-q", "-b", "main"]).is_none() {
            return; // git unavailable
        }
        git(&["commit", "-q", "--allow-empty", "-m", "one"]).unwrap();
        git(&["pack-refs", "--all"]).unwrap();
        git(&["checkout", "-q", "-b", "feat/x"]).unwrap();
        git(&["commit", "-q", "--allow-empty", "-m", "two"]).unwrap();

        let head = Repo::discover(dir.path()).unwrap().head();
        assert_eq!(head.branch.as_deref(), Some("feat/x"));
        assert_eq!(head.revision, git(&["rev-parse", "HEAD"]));
    }
}
