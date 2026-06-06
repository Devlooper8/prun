//! Core artifact scanner.
//!
//! Walks a project root, identifies reclaimable build artifacts using the rules
//! in `prun-rules.toml`, classifies them by ecosystem, and measures their
//! on-disk size. The ruleset is *root-first*: a directory is a "project root"
//! for a rule when one of that rule's markers sits directly inside it, and the
//! rule's `dirs`/`globs` under that root become reclaim candidates.
//!
//! The walk never descends *into* a matched artifact dir (no point sizing
//! node_modules file by file twice) and never enters a VCS metadata dir.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use ignore::{WalkBuilder, WalkState};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::fs_util::{
    dir_size, expand_root, is_git_ignored, leaf_artifact, load_prunignore, mtime_secs, now_secs,
    parent_name,
};
use crate::rules::{ecosystem_label, load_matcher, Matcher};

// ─────────────────────────────────────────────────────────────────────────────
// Wire types (serialized to the UI). `category` is the rule's ecosystem id.
// ─────────────────────────────────────────────────────────────────────────────

/// One reclaimable path (a directory or a single file).
#[derive(Clone, Serialize)]
pub struct Location {
    pub path: String,
    pub project: String,
    pub artifact: String,
    pub category: String,
    pub size: u64,
    pub age_secs: u64,
    pub git_ignored: bool,
}

#[derive(Clone, Serialize)]
pub struct Category {
    pub id: String,
    pub label: String,
    pub size: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanOptions {
    pub root: String,
    pub min_age_days: Option<u64>,
    pub skip_git_tracked: bool,
    pub respect_prunignore: bool,
}

/// Streamed progress from a running scan, delivered to the UI over a Channel.
/// The `kind` tag plus camelCase variants line up with the TS `ScanEvent` union.
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ScanEvent {
    /// Periodic heartbeat while walking the tree, before any size is known.
    Discovering { scanned: u64 },
    /// Discovery finished; `total` artifacts are about to be sized.
    Discovered { total: usize },
    /// One artifact has been sized. `done`/`total` drive the progress bar.
    Located {
        location: Location,
        done: usize,
        total: usize,
    },
    /// Scan complete, with the final category roll-up.
    Done {
        root: String,
        categories: Vec<Category>,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Scanning
// ─────────────────────────────────────────────────────────────────────────────

/// A reclaim candidate discovered before the size-independent filters run.
#[derive(Clone)]
struct Candidate {
    path: PathBuf,
    ecosystem: String,
    /// Precedence key: lower wins a contested path (rule index; junk offset by rules.len()).
    rank: usize,
    is_dir: bool,
}

/// Thread-shared discovery output.
#[derive(Default)]
struct Sink {
    candidates: Mutex<Vec<Candidate>>,
    roots: Mutex<Vec<(PathBuf, usize)>>,
    scanned: AtomicU64,
}

pub fn scan(opts: &ScanOptions, emit: &(dyn Fn(ScanEvent) + Sync)) -> Result<(), String> {
    scan_with(&load_matcher(), opts, emit)
}

fn scan_with(
    matcher: &Matcher,
    opts: &ScanOptions,
    emit: &(dyn Fn(ScanEvent) + Sync),
) -> Result<(), String> {
    let root = expand_root(&opts.root);
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }

    let prunignore = if opts.respect_prunignore {
        load_prunignore(&root)
    } else {
        None
    };
    let now = now_secs();

    // ── Phase 1: parallel discovery ──────────────────────────────────
    // standard_filters(false)/hidden(false) are essential: the defaults honour
    // .gitignore and skip dotdirs, hiding the very artifacts we hunt
    // (node_modules/target are usually git-ignored; .venv/.gradle/.next are dotdirs).
    let sink = Sink::default();
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(4);
    {
        let sink = &sink;
        WalkBuilder::new(&root)
            .standard_filters(false)
            .hidden(false)
            .follow_links(false)
            .threads(threads)
            .build_parallel()
            .run(move || Box::new(move |result| visit(result, matcher, sink, emit)));
    }

    let phase1 = std::mem::take(&mut *sink.candidates.lock().unwrap());
    let claimed_dirs: HashSet<PathBuf> = phase1
        .iter()
        .filter(|c| c.is_dir)
        .map(|c| c.path.clone())
        .collect();

    // ── Phase 2: resolve recursive globs under each discovered project root ──
    let mut roots = std::mem::take(&mut *sink.roots.lock().unwrap());
    roots.sort();
    roots.dedup();
    let phase2: Vec<Candidate> = roots
        .par_iter()
        .flat_map_iter(|(r, ri)| glob_walk(r, *ri, matcher, &claimed_dirs))
        .collect();

    // ── Combine, dedup by path (precedence), subsume nested candidates ──
    let mut by_path: HashMap<PathBuf, Candidate> = HashMap::new();
    for c in phase1.into_iter().chain(phase2.into_iter()) {
        match by_path.get(&c.path) {
            Some(e) if e.rank <= c.rank => {}
            _ => {
                by_path.insert(c.path.clone(), c);
            }
        }
    }
    let cand_dirs: HashSet<PathBuf> = by_path
        .values()
        .filter(|c| c.is_dir)
        .map(|c| c.path.clone())
        .collect();

    // ── Phase 2.5: sequential size-independent filtering ──────────────
    // git2's repository cache is !Send/!Sync, so keep it on one thread. The
    // candidate set is small (tens to low hundreds), so this is cheap.
    let mut git_cache: HashMap<PathBuf, Option<git2::Repository>> = HashMap::new();
    let mut pending: Vec<PendingLocation> = Vec::new();
    for c in by_path.into_values() {
        if has_ancestor_in(&c.path, &cand_dirs) {
            continue; // subsumed by a larger reclaim above it
        }
        if let Some(gi) = &prunignore {
            let rel = c.path.strip_prefix(&root).unwrap_or(&c.path);
            if gi.matched_path_or_any_parents(rel, c.is_dir).is_ignore() {
                continue;
            }
        }
        let git_ignored = is_git_ignored(&c.path, &mut git_cache);
        if opts.skip_git_tracked && !git_ignored {
            continue; // not ignored by git => possibly tracked => leave alone
        }
        let age_secs = now.saturating_sub(mtime_secs(&c.path));
        if let Some(min_days) = opts.min_age_days {
            if age_secs < min_days * 86_400 {
                continue;
            }
        }
        pending.push(PendingLocation {
            project: parent_name(&c.path),
            artifact: leaf_artifact(&c.path),
            path: c.path,
            category: c.ecosystem,
            age_secs,
            git_ignored,
        });
    }

    let total = pending.len();
    emit(ScanEvent::Discovered { total });

    // ── Phase 3: parallel sizing ─────────────────────────────────────
    let done = AtomicUsize::new(0);
    let mut locations: Vec<Location> = pending
        .par_iter()
        .map(|p| {
            let size = dir_size(&p.path);
            let location = Location {
                path: p.path.to_string_lossy().into_owned(),
                project: p.project.clone(),
                artifact: p.artifact.clone(),
                category: p.category.clone(),
                size,
                age_secs: p.age_secs,
                git_ignored: p.git_ignored,
            };
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            emit(ScanEvent::Located {
                location: location.clone(),
                done: n,
                total,
            });
            location
        })
        .collect();

    locations.sort_by(|a, b| b.size.cmp(&a.size));
    emit(ScanEvent::Done {
        root: root.to_string_lossy().into_owned(),
        categories: rollup(&locations),
    });
    Ok(())
}

/// Per-entry classification during the parallel discovery walk.
fn visit(
    result: Result<ignore::DirEntry, ignore::Error>,
    m: &Matcher,
    sink: &Sink,
    emit: &(dyn Fn(ScanEvent) + Sync),
) -> WalkState {
    let entry = match result {
        Ok(e) => e,
        Err(_) => return WalkState::Continue,
    };
    let path = entry.path();
    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
    let name = entry.file_name().to_string_lossy();

    // Heartbeat: prove liveness long before any size is known.
    let n = sink.scanned.fetch_add(1, Ordering::Relaxed);
    if n % 300 == 0 {
        emit(ScanEvent::Discovering { scanned: n });
    }

    if is_dir {
        if m.global_ignore.contains(name.as_ref()) {
            return WalkState::Skip;
        }
        // 1. dir rule, name-first + marker-validated (claims only real artifacts)
        if let Some(entries) = m.dir_index.get(name.as_ref()) {
            for (ri, segs) in entries {
                let rule = &m.rules[*ri];
                if !rule.enabled {
                    continue;
                }
                if let Some(root) = match_dir_entry(path, segs) {
                    if rule.marker_in(&root) {
                        push_cand(sink, path, &rule.ecosystem, *ri, true);
                        return WalkState::Skip;
                    }
                }
            }
        }
        // 2. junk dir (marker-less)
        if let Some(entries) = m.junk_dir_index.get(name.as_ref()) {
            for (ji, segs) in entries {
                if !m.junk[*ji].enabled {
                    continue;
                }
                if match_dir_entry(path, segs).is_some() {
                    push_cand(sink, path, &m.junk[*ji].ecosystem, m.rules.len() + *ji, true);
                    return WalkState::Skip;
                }
            }
        }
        // 3. junk glob matching a directory name
        if let Some(ji) = junk_glob_owner(m, name.as_ref()) {
            push_cand(sink, path, &m.junk[ji].ecosystem, m.rules.len() + ji, true);
            return WalkState::Skip;
        }
        // 4. reclaim_root: the dir holding the marker IS the artifact
        for ri in &m.reclaim_rules {
            let rule = &m.rules[*ri];
            if rule.enabled && rule.marker_in(path) {
                push_cand(sink, path, &rule.ecosystem, *ri, true);
                return WalkState::Skip;
            }
        }
        WalkState::Continue
    } else {
        // 5. junk glob matching a file name
        if let Some(ji) = junk_glob_owner(m, name.as_ref()) {
            push_cand(sink, path, &m.junk[ji].ecosystem, m.rules.len() + ji, false);
        }
        // 6. glob-rule root detection from marker files
        if let Some(parent) = path.parent() {
            if let Some(idxs) = m.glob_marker_exact.get(name.as_ref()) {
                let mut roots = sink.roots.lock().unwrap();
                for ri in idxs {
                    roots.push((parent.to_path_buf(), *ri));
                }
            }
            let hits = m.glob_marker_set.matches(Path::new(name.as_ref()));
            if !hits.is_empty() {
                let mut roots = sink.roots.lock().unwrap();
                for gi in hits {
                    roots.push((parent.to_path_buf(), m.glob_marker_owner[gi]));
                }
            }
        }
        WalkState::Continue
    }
}

/// Lowest-index junk rule whose glob matches `name`, if any.
fn junk_glob_owner(m: &Matcher, name: &str) -> Option<usize> {
    let hits = m.junk_glob_set.matches(Path::new(name));
    hits.into_iter()
        .map(|gi| m.junk_glob_owner[gi])
        .filter(|&ji| m.junk[ji].enabled)
        .min()
}

fn push_cand(sink: &Sink, path: &Path, ecosystem: &str, rank: usize, is_dir: bool) {
    sink.candidates.lock().unwrap().push(Candidate {
        path: path.to_path_buf(),
        ecosystem: ecosystem.to_string(),
        rank,
        is_dir,
    });
}

/// Walk one project root's subtree, collecting recursive-glob candidates while
/// pruning already-claimed dirs, VCS metadata, and the contents of matched
/// glob directories (so files inside `__pycache__` aren't claimed separately).
fn glob_walk(root: &Path, rule_idx: usize, m: &Matcher, claimed: &HashSet<PathBuf>) -> Vec<Candidate> {
    let rule = &m.rules[rule_idx];
    if !rule.enabled {
        return Vec::new();
    }
    let set = match &rule.glob_set {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut found: Vec<Candidate> = Vec::new();
    {
        let pred = |e: &walkdir::DirEntry| -> bool {
            if e.depth() == 0 {
                return true; // always enter the root itself
            }
            let is_dir = e.file_type().is_dir();
            let nm = e.file_name().to_string_lossy();
            if is_dir && (claimed.contains(e.path()) || m.global_ignore.contains(nm.as_ref())) {
                return false; // prune this subtree
            }
            if set.is_match(Path::new(nm.as_ref())) {
                found.push(Candidate {
                    path: e.path().to_path_buf(),
                    ecosystem: rule.ecosystem.clone(),
                    rank: rule_idx,
                    is_dir,
                });
                if is_dir {
                    return false; // record the dir but don't descend into it
                }
            }
            true
        };
        let walker = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(pred);
        for _ in walker {}
    }
    found
}

/// If `p`'s last `segs.len()` components equal `segs`, return the project root
/// (`p` with those components stripped). Comparison is case-sensitive, matching
/// the directory name as it appears on disk.
fn match_dir_entry(p: &Path, segs: &[String]) -> Option<PathBuf> {
    let comps: Vec<&std::ffi::OsStr> = p
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    if comps.len() < segs.len() {
        return None;
    }
    let start = comps.len() - segs.len();
    for (i, seg) in segs.iter().enumerate() {
        if comps[start + i] != std::ffi::OsStr::new(seg) {
            return None;
        }
    }
    p.ancestors().nth(segs.len()).map(|r| r.to_path_buf())
}

fn has_ancestor_in(p: &Path, set: &HashSet<PathBuf>) -> bool {
    let mut cur = p.parent();
    while let Some(a) = cur {
        if set.contains(a) {
            return true;
        }
        cur = a.parent();
    }
    false
}

/// A candidate that has passed every size-independent filter, awaiting sizing.
struct PendingLocation {
    path: PathBuf,
    project: String,
    artifact: String,
    category: String,
    age_secs: u64,
    git_ignored: bool,
}

fn rollup(locations: &[Location]) -> Vec<Category> {
    let mut totals: HashMap<String, u64> = HashMap::new();
    for loc in locations {
        *totals.entry(loc.category.clone()).or_default() += loc.size;
    }
    let mut categories: Vec<Category> = totals
        .into_iter()
        .map(|(id, size)| Category {
            label: ecosystem_label(&id),
            id,
            size,
        })
        .collect();
    categories.sort_by(|a, b| b.size.cmp(&a.size));
    categories
}

// ─────────────────────────────────────────────────────────────────────────────
// System caches (per-user shared caches; a separate view, never auto-selected)
// ─────────────────────────────────────────────────────────────────────────────

pub fn scan_caches(emit: &(dyn Fn(ScanEvent) + Sync)) -> Result<(), String> {
    scan_caches_with(&load_matcher(), emit)
}

fn scan_caches_with(
    matcher: &Matcher,
    emit: &(dyn Fn(ScanEvent) + Sync),
) -> Result<(), String> {
    let now = now_secs();
    let mut pending: Vec<(PathBuf, String, String)> = Vec::new(); // (path, ecosystem, cache name)
    for gc in &matcher.global_caches {
        if !cache_applies(&gc.platform) {
            continue;
        }
        for raw in &gc.paths {
            let p = expand_root(raw);
            if fs::symlink_metadata(&p).is_ok() {
                pending.push((p, gc.ecosystem.clone(), gc.name.clone()));
            }
        }
    }

    let total = pending.len();
    emit(ScanEvent::Discovered { total });

    let done = AtomicUsize::new(0);
    let mut locations: Vec<Location> = pending
        .par_iter()
        .map(|(p, eco, name)| {
            let location = Location {
                path: p.to_string_lossy().into_owned(),
                project: name.clone(),
                artifact: leaf_artifact(p),
                category: eco.clone(),
                size: dir_size(p),
                age_secs: now.saturating_sub(mtime_secs(p)),
                git_ignored: true,
            };
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            emit(ScanEvent::Located {
                location: location.clone(),
                done: n,
                total,
            });
            location
        })
        .collect();

    locations.sort_by(|a, b| b.size.cmp(&a.size));
    emit(ScanEvent::Done {
        root: "System caches".to_string(),
        categories: rollup(&locations),
    });
    Ok(())
}

/// A `platform = "macos"` cache is only relevant on macOS.
fn cache_applies(platform: &Option<String>) -> bool {
    match platform.as_deref() {
        Some("macos") => cfg!(target_os = "macos"),
        Some("windows") => cfg!(target_os = "windows"),
        Some("linux") => cfg!(target_os = "linux"),
        _ => true,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{RuleFile, EMBEDDED};
    use crate::testsupport::{fresh_tmp, mkdir, touch};

    fn embedded() -> Matcher {
        Matcher::compile(toml::from_str(EMBEDDED).expect("embedded parses"))
    }

    fn compile_str(s: &str) -> Matcher {
        Matcher::compile(toml::from_str(s).expect("test toml parses"))
    }

    fn opts(root: &Path) -> ScanOptions {
        ScanOptions {
            root: root.to_string_lossy().into_owned(),
            min_age_days: None,
            skip_git_tracked: false,
            respect_prunignore: false,
        }
    }

    /// Run a scan and return (artifact, category) pairs.
    fn run(m: &Matcher, root: &Path) -> Vec<(String, String)> {
        let out: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
        scan_with(m, &opts(root), &|ev| {
            if let ScanEvent::Located { location, .. } = ev {
                out.lock().unwrap().push((location.artifact, location.category));
            }
        })
        .unwrap();
        out.into_inner().unwrap()
    }

    // ── ported originals ─────────────────────────────────────────────

    #[test]
    fn finds_gitignored_and_dotdir_artifacts() {
        let root = fresh_tmp("walk");
        let proj = root.join("proj");
        mkdir(&proj);
        fs::write(proj.join(".gitignore"), "node_modules/\n.venv/\n").unwrap();
        touch(&proj.join("package.json")); // marker for node_modules
        touch(&proj.join("node_modules").join("a.js"));
        touch(&proj.join(".venv").join("pyvenv.cfg")); // reclaim_root marker

        let arts = run(&embedded(), &root);
        let _ = fs::remove_dir_all(&root);
        assert!(
            arts.iter().any(|(a, c)| a == "/node_modules" && c == "node"),
            "git-ignored node_modules must be found as node; got {arts:?}"
        );
        assert!(
            arts.iter().any(|(a, c)| a == "/.venv" && c == "python"),
            "dotdir .venv must be found as python; got {arts:?}"
        );
    }

    #[test]
    fn emits_discovered_then_done() {
        let root = fresh_tmp("events");
        let proj = root.join("rustproj");
        mkdir(&proj);
        touch(&proj.join("Cargo.toml"));
        touch(&proj.join("target").join("blob"));

        let kinds: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
        let total: Mutex<Option<usize>> = Mutex::new(None);
        let cat: Mutex<Option<String>> = Mutex::new(None);
        scan_with(&embedded(), &opts(&root), &|ev| {
            let tag = match &ev {
                ScanEvent::Discovering { .. } => "discovering",
                ScanEvent::Discovered { total: t } => {
                    *total.lock().unwrap() = Some(*t);
                    "discovered"
                }
                ScanEvent::Located { location, .. } => {
                    *cat.lock().unwrap() = Some(location.category.clone());
                    "located"
                }
                ScanEvent::Done { .. } => "done",
            };
            kinds.lock().unwrap().push(tag);
        })
        .unwrap();
        let _ = fs::remove_dir_all(&root);

        let kinds = kinds.into_inner().unwrap();
        assert_eq!(*total.lock().unwrap(), Some(1), "one target dir expected");
        assert_eq!(*cat.lock().unwrap(), Some("rust".to_string()));
        assert_eq!(kinds.last(), Some(&"done"), "Done must be the final event");
        assert_eq!(kinds.iter().filter(|k| **k == "located").count(), 1);
    }

    // ── new model coverage ───────────────────────────────────────────

    #[test]
    fn glob_marker_csproj() {
        let m = compile_str(
            r#"
            [[rule]]
            id="dotnet"
            ecosystem="dotnet"
            markers=["*.csproj"]
            dirs=["bin","obj"]
            "#,
        );
        let root = fresh_tmp("csproj");
        let proj = root.join("App");
        mkdir(&proj);
        touch(&proj.join("App.csproj"));
        mkdir(&proj.join("bin"));
        mkdir(&proj.join("obj"));

        let arts = run(&m, &root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(arts.len(), 2, "bin + obj; got {arts:?}");
        assert!(arts.iter().all(|(_, c)| c == "dotnet"));
    }

    #[test]
    fn nested_dir_sbt() {
        let m = compile_str(
            r#"
            [[rule]]
            id="sbt"
            ecosystem="jvm"
            markers=["build.sbt"]
            dirs=["target","project/target"]
            "#,
        );
        let root = fresh_tmp("sbt");
        let proj = root.join("scala");
        mkdir(&proj);
        touch(&proj.join("build.sbt"));
        mkdir(&proj.join("target"));
        mkdir(&proj.join("project").join("target"));

        let arts = run(&m, &root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(arts.len(), 2, "target + project/target; got {arts:?}");
    }

    #[test]
    fn reclaim_root_pyvenv() {
        let m = compile_str(
            r#"
            [[rule]]
            id="venv"
            ecosystem="python"
            markers=["pyvenv.cfg"]
            reclaim_root=true
            "#,
        );
        let root = fresh_tmp("venv");
        let odd = root.join("totally-not-named-venv");
        mkdir(&odd);
        touch(&odd.join("pyvenv.cfg"));

        let arts = run(&m, &root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(arts.len(), 1);
        assert_eq!(arts[0].1, "python");
        assert!(arts[0].0.ends_with("not-named-venv"));
    }

    #[test]
    fn junk_dir_and_file() {
        let m = compile_str(
            r#"
            [[junk]]
            id="ccls"
            ecosystem="editor"
            dirs=[".ccls-cache"]
            [[junk]]
            id="os"
            ecosystem="junk"
            globs=[".DS_Store"]
            "#,
        );
        let root = fresh_tmp("junk");
        mkdir(&root.join("src").join(".ccls-cache")); // marker-less dir
        touch(&root.join("docs").join(".DS_Store")); // marker-less file

        let arts = run(&m, &root);
        let _ = fs::remove_dir_all(&root);
        assert!(arts.iter().any(|(a, c)| a == "/.ccls-cache" && c == "editor"));
        assert!(arts.iter().any(|(a, c)| a == "/.DS_Store" && c == "junk"));
    }

    #[test]
    fn recursive_glob_under_root() {
        let m = compile_str(
            r#"
            [[rule]]
            id="latex"
            ecosystem="latex"
            markers=["*.tex"]
            globs=["*.aux"]
            "#,
        );
        let root = fresh_tmp("latex");
        let proj = root.join("paper");
        mkdir(&proj);
        touch(&proj.join("main.tex"));
        touch(&proj.join("a").join("b").join("out.aux")); // deep

        let arts = run(&m, &root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(arts.len(), 1, "got {arts:?}");
        assert!(arts[0].0.ends_with("out.aux") && arts[0].1 == "latex");
    }

    #[test]
    fn glob_does_not_descend_into_claimed_dir() {
        let m = compile_str(
            r#"
            [[rule]]
            id="cmake"
            ecosystem="cpp"
            markers=["CMakeLists.txt"]
            dirs=["build"]
            globs=["*.o"]
            "#,
        );
        let root = fresh_tmp("cmake");
        let proj = root.join("native");
        mkdir(&proj);
        touch(&proj.join("CMakeLists.txt"));
        touch(&proj.join("build").join("foo.o")); // inside the claimed build dir

        let arts = run(&m, &root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(arts.len(), 1, "only build/, not foo.o; got {arts:?}");
        assert_eq!(arts[0].0, "/build");
    }

    #[test]
    fn global_ignore_skips_git() {
        let m = compile_str(
            r#"
            [[rule]]
            id="rust"
            ecosystem="rust"
            markers=["Cargo.toml"]
            dirs=["target"]
            "#,
        );
        let root = fresh_tmp("gitignore");
        let proj = root.join("proj").join(".git");
        mkdir(&proj);
        touch(&proj.join("Cargo.toml"));
        mkdir(&proj.join("target"));

        let arts = run(&m, &root);
        let _ = fs::remove_dir_all(&root);
        assert!(arts.is_empty(), "nothing inside .git; got {arts:?}");
    }

    #[test]
    fn disabled_rule_is_off() {
        let m = compile_str(
            r#"
            [[rule]]
            id="go"
            ecosystem="go"
            markers=["go.mod"]
            dirs=["vendor"]
            enabled=false
            "#,
        );
        let root = fresh_tmp("disabled");
        let proj = root.join("svc");
        mkdir(&proj);
        touch(&proj.join("go.mod"));
        mkdir(&proj.join("vendor"));

        let arts = run(&m, &root);
        let _ = fs::remove_dir_all(&root);
        assert!(arts.is_empty(), "disabled go rule must not fire; got {arts:?}");
    }

    #[test]
    fn precedence_lowest_index_wins() {
        let m = compile_str(
            r#"
            [[rule]]
            id="rust"
            ecosystem="rust"
            markers=["Cargo.toml"]
            dirs=["target"]
            [[rule]]
            id="maven"
            ecosystem="jvm"
            markers=["pom.xml"]
            dirs=["target"]
            "#,
        );
        let root = fresh_tmp("prec");
        let proj = root.join("poly");
        mkdir(&proj);
        touch(&proj.join("Cargo.toml"));
        touch(&proj.join("pom.xml"));
        mkdir(&proj.join("target"));

        let arts = run(&m, &root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(arts.len(), 1);
        assert_eq!(arts[0].1, "rust", "rust precedes maven in TOML order");
    }

    /// End-to-end against the *real* embedded ruleset: a directory of mixed
    /// projects plus loose junk. Proves the shipped rules detect many ecosystems
    /// at once and that files inside a claimed dir aren't listed separately.
    #[test]
    fn scans_a_realistic_projects_tree() {
        let root = fresh_tmp("realistic");
        let p = |rel: &str| root.join(rel);

        touch(&p("projects/rustapp/Cargo.toml"));
        touch(&p("projects/rustapp/target/blob"));

        touch(&p("projects/webapp/package.json"));
        touch(&p("projects/webapp/vite.config.ts"));
        touch(&p("projects/webapp/node_modules/x.js"));
        touch(&p("projects/webapp/dist/bundle.js"));

        touch(&p("projects/pyapp/pyproject.toml"));
        touch(&p("projects/pyapp/.venv/pyvenv.cfg")); // reclaim_root
        touch(&p("projects/pyapp/__pycache__/m.cpython-311.pyc")); // glob dir
        touch(&p("projects/pyapp/build/lib/out.txt")); // dir rule

        touch(&p("projects/native/CMakeLists.txt"));
        touch(&p("projects/native/build/CMakeCache.txt"));

        touch(&p("projects/csapp/App.csproj")); // glob marker
        touch(&p("projects/csapp/bin/app.dll"));
        touch(&p("projects/csapp/obj/app.o"));

        touch(&p("projects/webapp/.DS_Store")); // loose junk file
        touch(&p("projects/Thumbs.db"));

        let arts = run(&embedded(), &root);
        let _ = fs::remove_dir_all(&root);

        let ecos: HashSet<&str> = arts.iter().map(|(_, c)| c.as_str()).collect();
        for want in ["rust", "node", "python", "cpp", "dotnet", "junk"] {
            assert!(ecos.contains(want), "expected ecosystem {want}; got {arts:?}");
        }
        assert!(arts.iter().any(|(a, _)| a == "/node_modules"));
        assert!(arts.iter().any(|(a, _)| a == "/target"));
        assert!(arts.iter().any(|(a, _)| a == "/.venv"));
        // files inside a claimed dir must not be listed on their own
        assert!(
            !arts.iter().any(|(a, _)| a.ends_with(".pyc")),
            "contents of a claimed __pycache__ leaked: {arts:?}"
        );
        assert!(
            !arts.iter().any(|(a, _)| a == "/CMakeCache.txt"),
            "contents of a claimed build/ leaked: {arts:?}"
        );
    }

    #[test]
    fn macos_only_cache_excluded_off_mac() {
        // The macOS-only cache (Xcode DerivedData) only applies on macOS.
        assert_eq!(
            cache_applies(&Some("macos".to_string())),
            cfg!(target_os = "macos")
        );
        assert!(cache_applies(&None));
    }

    fn round_trip(rf: &RuleFile) -> RuleFile {
        let s = toml::to_string_pretty(rf).expect("serialize");
        toml::from_str(&s).expect("reparse")
    }

    #[test]
    fn round_tripped_ruleset_still_detects() {
        let rf: RuleFile = toml::from_str(EMBEDDED).unwrap();
        let m = Matcher::compile(round_trip(&rf));
        let root = fresh_tmp("rtdetect");
        touch(&root.join("proj/Cargo.toml"));
        touch(&root.join("proj/target/blob"));
        touch(&root.join("web/package.json"));
        touch(&root.join("web/node_modules/x.js"));
        let arts = run(&m, &root);
        let _ = fs::remove_dir_all(&root);
        assert!(arts.iter().any(|(a, c)| a == "/target" && c == "rust"));
        assert!(arts.iter().any(|(a, c)| a == "/node_modules" && c == "node"));
    }
}
