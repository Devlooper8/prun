//! The compiled matcher: a ruleset prepared for fast lookup during a walk.
//!
//! `Matcher::compile` turns a [`RuleFile`] into precomputed indexes (name->rule
//! maps, glob sets, reclaim/marker tables). The scan engine reads these fields
//! directly while walking, so they are `pub(crate)` rather than encapsulated.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use super::model::{GlobalCache, RuleFile};

/// A rule prepared for fast matching during the walk.
pub(crate) struct CompiledRule {
    pub(crate) ecosystem: String,
    /// Exact-name markers (e.g. `Cargo.toml`), checked with `join(..).exists()`.
    exact_markers: Vec<String>,
    /// Glob markers (e.g. `*.csproj`), checked against a dir's children.
    marker_glob_set: Option<GlobSet>,
    /// Exact-name anti-markers: their presence suppresses the rule at that dir.
    exact_anti_markers: Vec<String>,
    /// Glob anti-markers, checked against a dir's children.
    anti_marker_glob_set: Option<GlobSet>,
    /// The rule's candidate globs (matched recursively under the project root).
    pub(crate) glob_set: Option<GlobSet>,
    pub(crate) enabled: bool,
}

impl CompiledRule {
    /// Is one of this rule's markers a direct child of `dir`?
    pub(crate) fn marker_in(&self, dir: &Path) -> bool {
        dir_has_any(dir, &self.exact_markers, &self.marker_glob_set)
    }

    /// Is one of this rule's anti-markers a direct child of `dir`? When true the
    /// rule must NOT treat `dir` as a root: a CMake build tree is reclaimed
    /// wholesale, but a dir that *also* holds the source `CMakeLists.txt` is the
    /// project root (an in-source build), so it's left to the per-file globs.
    pub(crate) fn anti_marker_in(&self, dir: &Path) -> bool {
        dir_has_any(dir, &self.exact_anti_markers, &self.anti_marker_glob_set)
    }
}

/// True if `dir` directly contains a name in `exact`, or a child whose name
/// matches `glob_set`. Shared by the marker and anti-marker checks; a single
/// `read_dir` services the glob case.
fn dir_has_any(dir: &Path, exact: &[String], glob_set: &Option<GlobSet>) -> bool {
    for m in exact {
        if dir.join(m).exists() {
            return true;
        }
    }
    if let Some(set) = glob_set {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                if set.is_match(Path::new(&e.file_name())) {
                    return true;
                }
            }
        }
    }
    false
}

pub(crate) struct CompiledJunk {
    pub(crate) ecosystem: String,
    pub(crate) enabled: bool,
}

pub(crate) struct Matcher {
    pub(crate) global_ignore: HashSet<String>,
    /// last path segment -> (rule_idx, full segments) for every rule `dirs` entry.
    pub(crate) dir_index: HashMap<String, Vec<(usize, Vec<String>)>>,
    /// last path segment -> (junk_idx, full segments) for every junk `dirs` entry.
    pub(crate) junk_dir_index: HashMap<String, Vec<(usize, Vec<String>)>>,
    /// Junk file/dir glob patterns, each paired with its owning junk index.
    pub(crate) junk_globs: GlobOwners,
    /// Rules whose marker presence makes the *containing* dir the artifact.
    pub(crate) reclaim_rules: Vec<usize>,
    /// For glob-bearing rules: marker name -> rule indices (root detection).
    pub(crate) glob_marker_exact: HashMap<String, Vec<usize>>,
    /// Glob markers, each paired with its owning rule index (root detection).
    pub(crate) glob_markers: GlobOwners,
    pub(crate) rules: Vec<CompiledRule>,
    pub(crate) junk: Vec<CompiledJunk>,
    pub(crate) global_caches: Vec<GlobalCache>,
}

fn is_glob(s: &str) -> bool {
    s.contains(['*', '?', '[', ']'])
}

fn build_globset(patterns: &[String]) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }
    let mut b = GlobSetBuilder::new();
    let mut any = false;
    for p in patterns {
        if let Ok(g) = GlobBuilder::new(p).literal_separator(true).build() {
            b.add(g);
            any = true;
        }
    }
    if !any {
        return None;
    }
    b.build().ok()
}

/// A compiled glob set whose matches map back to owner indices. Patterns and
/// owners are only ever appended together (via [`GlobOwnersBuilder::add`]), so a
/// GlobSet match index always maps straight back to an owner — callers read owners
/// via [`matches`](GlobOwners::matches) and never touch the parallel indexing.
pub(crate) struct GlobOwners {
    set: GlobSet,
    owners: Vec<usize>,
}

impl GlobOwners {
    /// The owner index of every glob that matches `name`.
    pub(crate) fn matches<'a>(&'a self, name: &Path) -> impl Iterator<Item = usize> + 'a {
        self.set.matches(name).into_iter().map(move |i| self.owners[i])
    }
}

/// Builds a [`GlobOwners`], appending each pattern and its owner in lockstep.
struct GlobOwnersBuilder {
    builder: GlobSetBuilder,
    owners: Vec<usize>,
}

impl GlobOwnersBuilder {
    fn new() -> Self {
        GlobOwnersBuilder {
            builder: GlobSetBuilder::new(),
            owners: Vec::new(),
        }
    }

    /// Append `pattern` owned by `owner`. An unbuildable glob is skipped (matching
    /// the original lenient compile), so `owners` stays aligned with the set.
    fn add(&mut self, pattern: &str, owner: usize) {
        if let Ok(g) = GlobBuilder::new(pattern).literal_separator(true).build() {
            self.builder.add(g);
            self.owners.push(owner);
        }
    }

    fn build(self) -> GlobOwners {
        GlobOwners {
            set: self.builder.build().unwrap_or_else(|_| GlobSet::empty()),
            owners: self.owners,
        }
    }
}

/// Normalize a path segment for comparison. On case-insensitive filesystems
/// (Windows, macOS) names match regardless of case — mirroring the marker
/// `Path::join(..).exists()` check, which is already case-insensitive there; on
/// Linux matching stays case-sensitive.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) fn norm_seg(s: &str) -> std::borrow::Cow<'_, str> {
    std::borrow::Cow::Owned(s.to_lowercase())
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) fn norm_seg(s: &str) -> std::borrow::Cow<'_, str> {
    std::borrow::Cow::Borrowed(s)
}

/// Split a dir entry into path segments, each normalized via [`norm_seg`] so the
/// index keys and the walk's lookups agree on case per platform.
fn norm_segments(entry: &str) -> Vec<String> {
    entry
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .map(|s| norm_seg(s).into_owned())
        .collect()
}

impl Matcher {
    pub(crate) fn compile(rf: RuleFile) -> Matcher {
        let global_ignore: HashSet<String> = rf.defaults.global_ignore.into_iter().collect();

        let mut dir_index: HashMap<String, Vec<(usize, Vec<String>)>> = HashMap::new();
        let mut reclaim_rules = Vec::new();
        let mut glob_marker_exact: HashMap<String, Vec<usize>> = HashMap::new();
        let mut marker_globs = GlobOwnersBuilder::new();
        let mut rules = Vec::with_capacity(rf.rules.len());

        for (idx, r) in rf.rules.into_iter().enumerate() {
            let mut exact_markers = Vec::new();
            let mut glob_markers = Vec::new();
            for m in &r.markers {
                if is_glob(m) {
                    glob_markers.push(m.clone());
                } else {
                    exact_markers.push(m.clone());
                }
            }
            let mut exact_anti_markers = Vec::new();
            let mut glob_anti_markers = Vec::new();
            for m in &r.anti_markers {
                if is_glob(m) {
                    glob_anti_markers.push(m.clone());
                } else {
                    exact_anti_markers.push(m.clone());
                }
            }
            // dir entries are claimed name-first during the walk
            for d in &r.dirs {
                let segs = norm_segments(d);
                if let Some(last) = segs.last().cloned() {
                    dir_index.entry(last).or_default().push((idx, segs));
                }
            }
            if r.reclaim_root {
                reclaim_rules.push(idx);
            }
            // glob-bearing rules need their roots discovered from marker files
            if !r.globs.is_empty() {
                for m in &exact_markers {
                    glob_marker_exact.entry(m.clone()).or_default().push(idx);
                }
                for m in &glob_markers {
                    marker_globs.add(m, idx);
                }
            }
            rules.push(CompiledRule {
                ecosystem: r.ecosystem,
                exact_markers,
                marker_glob_set: build_globset(&glob_markers),
                exact_anti_markers,
                anti_marker_glob_set: build_globset(&glob_anti_markers),
                glob_set: build_globset(&r.globs),
                enabled: r.enabled,
            });
        }
        // stable precedence: lowest TOML index wins a contested path
        for v in dir_index.values_mut() {
            v.sort_by_key(|(i, _)| *i);
        }

        let mut junk_dir_index: HashMap<String, Vec<(usize, Vec<String>)>> = HashMap::new();
        let mut junk_globs = GlobOwnersBuilder::new();
        let mut junk = Vec::with_capacity(rf.junk.len());
        for (idx, j) in rf.junk.into_iter().enumerate() {
            for d in &j.dirs {
                let segs = norm_segments(d);
                if let Some(last) = segs.last().cloned() {
                    junk_dir_index.entry(last).or_default().push((idx, segs));
                }
            }
            for g in &j.globs {
                junk_globs.add(g, idx);
            }
            junk.push(CompiledJunk {
                ecosystem: j.ecosystem,
                enabled: j.enabled,
            });
        }
        for v in junk_dir_index.values_mut() {
            v.sort_by_key(|(i, _)| *i);
        }

        Matcher {
            global_ignore,
            dir_index,
            junk_dir_index,
            junk_globs: junk_globs.build(),
            reclaim_rules,
            glob_marker_exact,
            glob_markers: marker_globs.build(),
            rules,
            junk,
            global_caches: rf.global_cache,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::EMBEDDED;

    #[test]
    fn embedded_ruleset_parses_and_compiles() {
        let rf: RuleFile = toml::from_str(EMBEDDED).expect("embedded parses");
        assert!(rf.rules.len() >= 60, "expected the full rule set");
        assert_eq!(rf.junk.len(), 4);
        assert_eq!(rf.global_cache.len(), 21);
        let _ = Matcher::compile(rf); // must not panic
    }
}
