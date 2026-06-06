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
            return if rest.is_empty() { home } else { home.join(rest) };
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
        p.file_name().map(|s| s.to_string_lossy()).unwrap_or_default()
    )
}

/// The name of the directory that contains `p` (the artifact's project folder).
pub(crate) fn parent_name(p: &Path) -> String {
    p.parent()
        .and_then(|d| d.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub(crate) fn dir_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
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
