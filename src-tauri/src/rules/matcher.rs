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
    /// The rule's candidate globs (matched recursively under the project root).
    pub(crate) glob_set: Option<GlobSet>,
    pub(crate) enabled: bool,
}

impl CompiledRule {
    /// Is one of this rule's markers a direct child of `dir`?
    pub(crate) fn marker_in(&self, dir: &Path) -> bool {
        for m in &self.exact_markers {
            if dir.join(m).exists() {
                return true;
            }
        }
        if let Some(set) = &self.marker_glob_set {
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
    /// Combined junk file/dir glob patterns (matched against a base name).
    pub(crate) junk_glob_set: GlobSet,
    pub(crate) junk_glob_owner: Vec<usize>,
    /// Rules whose marker presence makes the *containing* dir the artifact.
    pub(crate) reclaim_rules: Vec<usize>,
    /// For glob-bearing rules: marker name -> rule indices (root detection).
    pub(crate) glob_marker_exact: HashMap<String, Vec<usize>>,
    pub(crate) glob_marker_set: GlobSet,
    pub(crate) glob_marker_owner: Vec<usize>,
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

fn split_segments(entry: &str) -> Vec<String> {
    entry
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

impl Matcher {
    pub(crate) fn compile(rf: RuleFile) -> Matcher {
        let global_ignore: HashSet<String> = rf.defaults.global_ignore.into_iter().collect();

        let mut dir_index: HashMap<String, Vec<(usize, Vec<String>)>> = HashMap::new();
        let mut reclaim_rules = Vec::new();
        let mut glob_marker_exact: HashMap<String, Vec<usize>> = HashMap::new();
        let mut glob_marker_builder = GlobSetBuilder::new();
        let mut glob_marker_owner: Vec<usize> = Vec::new();
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
            // dir entries are claimed name-first during the walk
            for d in &r.dirs {
                let segs = split_segments(d);
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
                    if let Ok(g) = GlobBuilder::new(m).literal_separator(true).build() {
                        glob_marker_builder.add(g);
                        glob_marker_owner.push(idx);
                    }
                }
            }
            rules.push(CompiledRule {
                ecosystem: r.ecosystem,
                exact_markers,
                marker_glob_set: build_globset(&glob_markers),
                glob_set: build_globset(&r.globs),
                enabled: r.enabled,
            });
        }
        // stable precedence: lowest TOML index wins a contested path
        for v in dir_index.values_mut() {
            v.sort_by_key(|(i, _)| *i);
        }

        let mut junk_dir_index: HashMap<String, Vec<(usize, Vec<String>)>> = HashMap::new();
        let mut junk_glob_builder = GlobSetBuilder::new();
        let mut junk_glob_owner: Vec<usize> = Vec::new();
        let mut junk = Vec::with_capacity(rf.junk.len());
        for (idx, j) in rf.junk.into_iter().enumerate() {
            for d in &j.dirs {
                let segs = split_segments(d);
                if let Some(last) = segs.last().cloned() {
                    junk_dir_index.entry(last).or_default().push((idx, segs));
                }
            }
            for g in &j.globs {
                if let Ok(glob) = GlobBuilder::new(g).literal_separator(true).build() {
                    junk_glob_builder.add(glob);
                    junk_glob_owner.push(idx);
                }
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
            junk_glob_set: junk_glob_builder.build().unwrap_or_else(|_| GlobSet::empty()),
            junk_glob_owner,
            reclaim_rules,
            glob_marker_exact,
            glob_marker_set: glob_marker_builder.build().unwrap_or_else(|_| GlobSet::empty()),
            glob_marker_owner,
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
