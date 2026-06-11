//! The root-first project scan, in phases:
//!
//! 1. Parallel discovery ([`ignore`]'s parallel walker) classifies every entry,
//!    claiming artifact dirs and recording glob-rule project roots.
//! 2. A parallel subtree walk resolves each rule's recursive `globs` under those
//!    roots, pruning already-claimed dirs.
//! 3. Sequential size-independent filtering (subsumption, prunignore, git-ignore,
//!    age) — git2's repo cache is !Send, so this stays single-threaded.
//! 4. Parallel sizing, streaming one `Located` event per artifact.
//!
//! The walk never descends *into* a matched artifact dir, and never enters VCS
//! metadata (via the ruleset's `global_ignore`).

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use ignore::{WalkBuilder, WalkState};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::fs_util::{
    expand_root, is_git_ignored, leaf_artifact, load_prunignore, measure_tree, now_secs,
    parent_name,
};
use crate::rules::{load_matcher, norm_seg, Matcher};

use super::{rollup, Location, ScanEvent, ScanOptions};

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

pub fn scan(
    opts: &ScanOptions,
    cancel: &AtomicBool,
    emit: &(dyn Fn(ScanEvent) + Sync),
) -> Result<(), String> {
    scan_with(&load_matcher(), opts, cancel, emit)
}

fn scan_with(
    matcher: &Matcher,
    opts: &ScanOptions,
    cancel: &AtomicBool,
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
            .run(move || Box::new(move |result| visit(result, matcher, sink, cancel, emit)));
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
        // Age is no longer decided here: a deep "newest file" mtime needs the same
        // walk as sizing, so the age gate moves into phase 3 (see below).
        pending.push(PendingLocation {
            project: parent_name(&c.path),
            artifact: leaf_artifact(&c.path),
            path: c.path,
            category: c.ecosystem,
            git_ignored,
        });
    }

    let total = pending.len();
    emit(ScanEvent::Discovered { total });

    // ── Phase 3: parallel sizing (also resolves deep age + read errors) ──
    // One walk per candidate yields size, newest mtime, and an error count. The
    // min-age gate runs here, post-walk, because honest "untouched for N days" needs
    // the newest file's mtime — not the top dir's, which goes stale during rebuilds.
    // Too-fresh candidates are still walked (so the progress bar completes) but not
    // offered. `cancel` lets an in-flight scan stop promptly when the user asks.
    let min_age_secs = opts.min_age_days.map(|d| d * 86_400);
    let done = AtomicUsize::new(0);
    let errors = AtomicU64::new(0);
    let error_samples = Mutex::new(Vec::new());
    let mut locations: Vec<Location> = pending
        .par_iter()
        .filter_map(|p| {
            if cancel.load(Ordering::Relaxed) {
                return None; // user cancelled — stop sizing further candidates
            }
            let measured = measure_tree(&p.path);
            errors.fetch_add(measured.errors, Ordering::Relaxed);
            if let Some(sample) = &measured.first_error {
                super::push_error_sample(&error_samples, sample);
            }
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            let age_secs = now.saturating_sub(measured.newest_mtime);
            if min_age_secs.is_some_and(|min| age_secs < min) {
                return None; // walked for the progress bar, but too fresh to offer
            }
            let location = Location {
                path: p.path.to_string_lossy().into_owned(),
                project: p.project.clone(),
                artifact: p.artifact.clone(),
                category: p.category.clone(),
                size: measured.size,
                age_secs,
                git_ignored: p.git_ignored,
            };
            emit(ScanEvent::Located {
                location: location.clone(),
                done: n,
                total,
            });
            Some(location)
        })
        .collect();

    locations.sort_by(|a, b| b.size.cmp(&a.size));
    let error_samples = error_samples.into_inner().unwrap();
    for sample in &error_samples {
        tracing::warn!("unreadable during scan: {sample}");
    }
    emit(ScanEvent::Done {
        root: root.to_string_lossy().into_owned(),
        categories: rollup(&locations),
        errors: errors.load(Ordering::Relaxed),
        error_samples,
    });
    Ok(())
}

/// Per-entry classification during the parallel discovery walk.
fn visit(
    result: Result<ignore::DirEntry, ignore::Error>,
    m: &Matcher,
    sink: &Sink,
    cancel: &AtomicBool,
    emit: &(dyn Fn(ScanEvent) + Sync),
) -> WalkState {
    if cancel.load(Ordering::Relaxed) {
        return WalkState::Quit; // user cancelled mid-discovery
    }
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
        if let Some(entries) = m.dir_index.get(norm_seg(name.as_ref()).as_ref()) {
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
        if let Some(entries) = m.junk_dir_index.get(norm_seg(name.as_ref()).as_ref()) {
            for (ji, segs) in entries {
                if !m.junk[*ji].enabled {
                    continue;
                }
                if match_dir_entry(path, segs).is_some() {
                    push_cand(
                        sink,
                        path,
                        &m.junk[*ji].ecosystem,
                        m.rules.len() + *ji,
                        true,
                    );
                    return WalkState::Skip;
                }
            }
        }
        // 3. junk glob matching a directory name
        if let Some(ji) = junk_glob_owner(m, name.as_ref()) {
            push_cand(sink, path, &m.junk[ji].ecosystem, m.rules.len() + ji, true);
            return WalkState::Skip;
        }
        // 4. reclaim_root: the dir holding the marker IS the artifact — unless an
        //    anti-marker says it's really a source root (an in-source build, where
        //    CMakeCache.txt sits next to the source CMakeLists.txt).
        for ri in &m.reclaim_rules {
            let rule = &m.rules[*ri];
            if rule.enabled && rule.marker_in(path) && !rule.anti_marker_in(path) {
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
            let mut owners = m.glob_markers.matches(Path::new(name.as_ref())).peekable();
            if owners.peek().is_some() {
                let mut roots = sink.roots.lock().unwrap();
                for ri in owners {
                    roots.push((parent.to_path_buf(), ri));
                }
            }
        }
        WalkState::Continue
    }
}

/// Lowest-index junk rule whose glob matches `name`, if any.
fn junk_glob_owner(m: &Matcher, name: &str) -> Option<usize> {
    m.junk_globs
        .matches(Path::new(name))
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
fn glob_walk(
    root: &Path,
    rule_idx: usize,
    m: &Matcher,
    claimed: &HashSet<PathBuf>,
) -> Vec<Candidate> {
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
/// (`p` with those components stripped). `segs` are already case-normalized by the
/// matcher, so each disk component is normalized the same way before comparison —
/// case-insensitive on Windows/macOS, case-sensitive on Linux (see `norm_seg`).
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
        if norm_seg(&comps[start + i].to_string_lossy()).as_ref() != seg.as_str() {
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
/// Age is resolved during sizing (phase 3), so it isn't carried here.
struct PendingLocation {
    path: PathBuf,
    project: String,
    artifact: String,
    category: String,
    git_ignored: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{RuleFile, EMBEDDED};
    use crate::testsupport::{fresh_tmp, mkdir, touch};
    use std::fs;

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
        let cancel = AtomicBool::new(false);
        scan_with(m, &opts(root), &cancel, &|ev| {
            if let ScanEvent::Located { location, .. } = ev {
                out.lock()
                    .unwrap()
                    .push((location.artifact, location.category));
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
            arts.iter()
                .any(|(a, c)| a == "/node_modules" && c == "node"),
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
        let cancel = AtomicBool::new(false);
        scan_with(&embedded(), &opts(&root), &cancel, &|ev| {
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

    #[test]
    fn cancelled_scan_offers_nothing() {
        let root = fresh_tmp("cancel");
        let proj = root.join("rustproj");
        mkdir(&proj);
        touch(&proj.join("Cargo.toml"));
        touch(&proj.join("target").join("blob"));

        // Pre-cancelled: the discovery walk Quits before classifying anything, so no
        // candidate is offered. Pins the cancellation wiring end-to-end (a live
        // mid-scan cancel is best-effort, but the flag is honored).
        let cancel = AtomicBool::new(true);
        let located: Mutex<usize> = Mutex::new(0);
        scan_with(&embedded(), &opts(&root), &cancel, &|ev| {
            if let ScanEvent::Located { .. } = ev {
                *located.lock().unwrap() += 1;
            }
        })
        .unwrap();
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            located.into_inner().unwrap(),
            0,
            "a cancelled scan must not offer any location"
        );
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

    /// A CMake build tree is identified by the `CMakeCache.txt` it holds, so an
    /// out-of-source build dir is reclaimed WHOLE regardless of its name — instead
    /// of fragmenting into CMakeFiles/, CMakeCache.txt and build.ninja listed
    /// separately (the bug the cmake-build reclaim rule fixes).
    #[test]
    fn cmake_build_tree_reclaimed_whole_by_marker() {
        let root = fresh_tmp("cmakebuild");
        let proj = root.join("proj");
        mkdir(&proj);
        touch(&proj.join("CMakeLists.txt")); // source root (out-of-source layout)
                                             // an arbitrarily-named build dir, NOT in the cmake `dirs` list
        let bd = proj.join("cmake-build-minsizerel-system");
        touch(&bd.join("CMakeCache.txt"));
        touch(&bd.join("build.ninja"));
        touch(&bd.join("CMakeFiles").join("x.stamp"));

        let arts = run(&embedded(), &root);
        let _ = fs::remove_dir_all(&root);

        assert!(
            arts.iter()
                .any(|(a, c)| a == "/cmake-build-minsizerel-system" && c == "cpp"),
            "the whole build tree should be one cpp entry; got {arts:?}"
        );
        assert!(
            !arts.iter().any(|(a, _)| a.contains("CMakeCache")
                || a.contains("CMakeFiles")
                || a.contains("ninja")),
            "build-tree contents must not be listed separately; got {arts:?}"
        );
    }

    /// In-source build: `CMakeCache.txt` sits next to the source `CMakeLists.txt`.
    /// The anti-marker must stop `reclaim_root` from grabbing the whole source
    /// tree; the loose generated files are still caught by the cmake globs.
    #[test]
    fn cmake_in_source_build_not_reclaimed_wholesale() {
        let root = fresh_tmp("cmakeinsrc");
        let proj = root.join("proj");
        mkdir(&proj);
        touch(&proj.join("CMakeLists.txt")); // source
        touch(&proj.join("main.cpp")); // source
        touch(&proj.join("CMakeCache.txt")); // generated, in-source
        touch(&proj.join("CMakeFiles").join("x.stamp"));

        let arts = run(&embedded(), &root);
        let _ = fs::remove_dir_all(&root);

        assert!(
            !arts.iter().any(|(a, _)| a == "/proj"),
            "the in-source source root must NOT be reclaimed wholesale; got {arts:?}"
        );
        assert!(
            arts.iter()
                .any(|(a, c)| a == "/CMakeCache.txt" && c == "cpp"),
            "loose in-source CMakeCache.txt should still be caught by globs; got {arts:?}"
        );
        assert!(
            arts.iter().any(|(a, c)| a == "/CMakeFiles" && c == "cpp"),
            "loose in-source CMakeFiles should still be caught by globs; got {arts:?}"
        );
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
        assert!(arts
            .iter()
            .any(|(a, c)| a == "/.ccls-cache" && c == "editor"));
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
        assert!(
            arts.is_empty(),
            "disabled go rule must not fire; got {arts:?}"
        );
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
            assert!(
                ecos.contains(want),
                "expected ecosystem {want}; got {arts:?}"
            );
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

    /// On case-insensitive filesystems a `Target/` dir on disk must match a
    /// `dirs=["target"]` rule, mirroring the case-insensitive marker `exists()`
    /// check. Gated to those platforms (on Linux the FS — and matching — is
    /// case-sensitive, and the dir couldn't share a name anyway).
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn dir_match_is_case_insensitive_on_case_insensitive_fs() {
        let m = compile_str(
            r#"
            [[rule]]
            id="rust"
            ecosystem="rust"
            markers=["Cargo.toml"]
            dirs=["target"]
            "#,
        );
        let root = fresh_tmp("caseins");
        let proj = root.join("proj");
        mkdir(&proj);
        touch(&proj.join("Cargo.toml"));
        mkdir(&proj.join("Target")); // capital T — the same dir on a case-insensitive FS

        let arts = run(&m, &root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            arts.len(),
            1,
            "Target/ should match dirs=[\"target\"]; got {arts:?}"
        );
        assert!(arts[0].0.eq_ignore_ascii_case("/target"), "got {arts:?}");
        assert_eq!(arts[0].1, "rust");
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
        assert!(arts
            .iter()
            .any(|(a, c)| a == "/node_modules" && c == "node"));
    }
}
