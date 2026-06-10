//! Filesystem helpers shared across scans.
//!
//! Leaf utilities with no knowledge of rules or the IPC layer: root expansion,
//! sizing, timestamps, name extraction, and ignore-file / git-ignore queries.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use walkdir::WalkDir;

/// Expand a leading `~` to the user's home directory.
pub(crate) fn expand_root(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            let rest = rest.trim_start_matches(['/', '\\']);
            return if rest.is_empty() {
                home
            } else {
                home.join(rest)
            };
        }
    }
    PathBuf::from(input)
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The artifact's own leaf name, prefixed with `/` (the UI's display form).
pub(crate) fn leaf_artifact(p: &Path) -> String {
    format!(
        "/{}",
        p.file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default()
    )
}

/// The name of the directory that contains `p` (the artifact's project folder).
pub(crate) fn parent_name(p: &Path) -> String {
    p.parent()
        .and_then(|d| d.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The result of walking one artifact tree in a single pass.
#[derive(Default, Clone, Copy)]
pub(crate) struct Measured {
    /// Apparent size in bytes (sum of file lengths). On Unix, a file reachable by
    /// several hard links is counted once. NOTE: this is the logical size, not the
    /// on-disk allocation — actual reclaim is a little higher (block rounding) and,
    /// for reflink/CoW clones, can be lower. Good enough for "how big is this".
    pub size: u64,
    /// Newest modification time anywhere in the tree (unix secs), so "untouched for
    /// N days" reflects the most recent file change — not just the top dir's mtime,
    /// which often goes stale while files beneath it are rebuilt.
    pub newest_mtime: u64,
    /// Count of entries that couldn't be read/stat'd (permissions, races). Surfaced
    /// rather than silently dropped, so a wrong total doesn't look authoritative.
    pub errors: u64,
}

/// Walk `path` once, accumulating size, the newest mtime, and a read-error count.
/// Replaces a plain size sum: folding mtime + error tracking into the same walk is
/// free (we already stat every file) and gives honest age and error reporting.
pub(crate) fn measure_tree(path: &Path) -> Measured {
    // The dir's own mtime is the floor: an empty or fully-deleted tree still has one.
    let mut m = Measured {
        newest_mtime: mtime_secs(path),
        ..Measured::default()
    };
    #[cfg(unix)]
    let mut seen_links: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();

    for entry in WalkDir::new(path).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                m.errors += 1;
                continue;
            }
        };
        // file_type() is free (cached from readdir); dirs/symlinks contribute no
        // size, and the root mtime floor already covers structural changes.
        if !entry.file_type().is_file() {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => {
                m.errors += 1;
                continue;
            }
        };
        if let Ok(modified) = meta.modified() {
            if let Ok(d) = modified.duration_since(UNIX_EPOCH) {
                m.newest_mtime = m.newest_mtime.max(d.as_secs());
            }
        }
        // Count a hard-linked file only once (pnpm-style stores hard-link heavily).
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.nlink() > 1 && !seen_links.insert((meta.dev(), meta.ino())) {
                continue;
            }
        }
        m.size += meta.len();
    }
    m
}

pub(crate) fn mtime_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn load_prunignore(root: &Path) -> Option<Gitignore> {
    let file = root.join(".prunignore");
    if !file.exists() {
        return None;
    }
    let mut b = GitignoreBuilder::new(root);
    b.add(file);
    b.build().ok()
}

/// Whether a path is ignored by the git repo that contains it.
/// Repositories are discovered lazily and cached by their working dir.
pub(crate) fn is_git_ignored(
    path: &Path,
    cache: &mut HashMap<PathBuf, Option<git2::Repository>>,
) -> bool {
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d.join(".git").exists() {
            let repo = cache
                .entry(d.to_path_buf())
                .or_insert_with(|| git2::Repository::open(d).ok());
            if let Some(repo) = repo {
                return repo.is_path_ignored(path).unwrap_or(false);
            }
            return false;
        }
        dir = d.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsupport::{fresh_tmp, touch};

    #[test]
    fn measure_tree_sums_size_and_tracks_recency() {
        let root = fresh_tmp("measure");
        touch(&root.join("a.bin")); // 1 byte ("x")
        touch(&root.join("sub").join("b.bin")); // 1 byte, nested
        let m = measure_tree(&root);
        let now = now_secs();
        let _ = fs::remove_dir_all(&root);

        assert_eq!(m.size, 2, "two 1-byte files summed across the tree");
        assert_eq!(m.errors, 0, "a clean tree reports no read errors");
        assert!(m.newest_mtime > 0, "newest mtime is populated");
        assert!(
            now.saturating_sub(m.newest_mtime) < 3600,
            "just-written files read as recently touched"
        );
    }

    #[test]
    fn measure_tree_of_empty_dir_is_zero_size_with_a_floor_mtime() {
        let root = fresh_tmp("measure_empty");
        let m = measure_tree(&root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(m.size, 0);
        assert!(
            m.newest_mtime > 0,
            "an empty dir still has its own mtime as a floor"
        );
    }
}
