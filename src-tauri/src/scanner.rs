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
use std::time::{SystemTime, UNIX_EPOCH};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::{WalkBuilder, WalkState};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

/// The ruleset that ships with the binary. An optional user override at
/// `<config dir>/prun/rules.toml` (e.g. `%APPDATA%\prun\rules.toml`) wins when
/// present and parseable.
static EMBEDDED: &str = include_str!("../prun-rules.toml");

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
// TOML ruleset model
// ─────────────────────────────────────────────────────────────────────────────

fn d_true() -> bool {
    true
}
fn d_min_age() -> u64 {
    14
}
fn d_global_ignore() -> Vec<String> {
    [".git", ".hg", ".svn", ".jj"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn d_schema_version() -> u32 {
    3
}
fn is_false(b: &bool) -> bool {
    !*b
}

/// The whole ruleset. Doubles as the editor's wire DTO: it round-trips through
/// `toml::to_string_pretty` (root scalar `schema_version` is declared first so it
/// serializes before the `[defaults]` table and the arrays-of-tables).
#[derive(Deserialize, Serialize, Clone)]
pub struct RuleFile {
    #[serde(default = "d_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default, rename = "rule", skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub junk: Vec<Junk>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub global_cache: Vec<GlobalCache>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Defaults {
    #[serde(default = "d_min_age")]
    pub min_age_days: u64,
    #[serde(default = "d_true")]
    pub skip_git_tracked: bool,
    #[serde(default = "d_true")]
    pub respect_ignorefile: bool,
    #[serde(default = "d_true")]
    pub move_to_trash: bool,
    #[serde(default = "d_global_ignore")]
    pub global_ignore: Vec<String>,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            min_age_days: d_min_age(),
            skip_git_tracked: true,
            respect_ignorefile: true,
            move_to_trash: true,
            global_ignore: d_global_ignore(),
        }
    }
}

// `enabled` is always emitted (a disabled entry must round-trip visibly); empty
// arrays / `None` notes / a false `reclaim_root` are skipped to keep the file clean.
// NOTE: skipped fields are also absent from the JSON sent to the UI, so the
// frontend normalizes them back (markers ?? [], note ?? null, …).
#[derive(Deserialize, Serialize, Clone)]
pub struct Rule {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub ecosystem: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub reclaim_root: bool,
    #[serde(default = "d_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Junk {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub ecosystem: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,
    #[serde(default = "d_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct GlobalCache {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub ecosystem: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default = "d_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Compiled matcher
// ─────────────────────────────────────────────────────────────────────────────

/// A rule prepared for fast matching during the walk.
struct CompiledRule {
    ecosystem: String,
    /// Exact-name markers (e.g. `Cargo.toml`), checked with `join(..).exists()`.
    exact_markers: Vec<String>,
    /// Glob markers (e.g. `*.csproj`), checked against a dir's children.
    marker_glob_set: Option<GlobSet>,
    /// The rule's candidate globs (matched recursively under the project root).
    glob_set: Option<GlobSet>,
    enabled: bool,
}

impl CompiledRule {
    /// Is one of this rule's markers a direct child of `dir`?
    fn marker_in(&self, dir: &Path) -> bool {
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

struct CompiledJunk {
    ecosystem: String,
    enabled: bool,
}

pub struct Matcher {
    global_ignore: HashSet<String>,
    /// last path segment -> (rule_idx, full segments) for every rule `dirs` entry.
    dir_index: HashMap<String, Vec<(usize, Vec<String>)>>,
    /// last path segment -> (junk_idx, full segments) for every junk `dirs` entry.
    junk_dir_index: HashMap<String, Vec<(usize, Vec<String>)>>,
    /// Combined junk file/dir glob patterns (matched against a base name).
    junk_glob_set: GlobSet,
    junk_glob_owner: Vec<usize>,
    /// Rules whose marker presence makes the *containing* dir the artifact.
    reclaim_rules: Vec<usize>,
    /// For glob-bearing rules: marker name -> rule indices (root detection).
    glob_marker_exact: HashMap<String, Vec<usize>>,
    glob_marker_set: GlobSet,
    glob_marker_owner: Vec<usize>,
    rules: Vec<CompiledRule>,
    junk: Vec<CompiledJunk>,
    global_caches: Vec<GlobalCache>,
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
    fn compile(rf: RuleFile) -> Matcher {
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

/// Path to the optional user override ruleset (`%APPDATA%\prun\rules.toml` on
/// Windows). `None` only if the OS exposes no config directory.
fn override_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("prun").join("rules.toml"))
}

/// Build the active matcher: the user override if present and parseable, else the
/// embedded default. Rebuilt per scan (parse + compile is well under a
/// millisecond) so edits to the override take effect on the next scan without
/// restarting the app.
fn load_matcher() -> Matcher {
    if let Some(path) = override_path() {
        if let Ok(text) = fs::read_to_string(&path) {
            match toml::from_str::<RuleFile>(&text) {
                Ok(rf) => return Matcher::compile(rf),
                Err(e) => eprintln!(
                    "prun: override rules.toml failed to parse ({e}); using embedded ruleset"
                ),
            }
        }
    }
    Matcher::compile(toml::from_str(EMBEDDED).expect("embedded prun-rules.toml must parse"))
}

/// Where the override lives and whether it is currently in effect — surfaced in
/// the Settings panel so users discover the file and can edit it.
#[derive(Serialize)]
pub struct RulesStatus {
    pub override_path: String,
    pub override_exists: bool,
    pub using_override: bool,
    pub error: Option<String>,
    pub rule_count: usize,
    pub junk_count: usize,
    pub cache_count: usize,
}

pub fn rules_status() -> RulesStatus {
    let path = override_path();
    let path_str = path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let exists = path.as_ref().map(|p| p.exists()).unwrap_or(false);

    let mut using_override = false;
    let mut error = None;
    let rf: RuleFile = match (exists, path.as_ref()) {
        (true, Some(p)) => match fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|t| {
            toml::from_str::<RuleFile>(&t).map_err(|e| e.to_string())
        }) {
            Ok(rf) => {
                using_override = true;
                rf
            }
            Err(e) => {
                error = Some(e);
                toml::from_str(EMBEDDED).expect("embedded parses")
            }
        },
        _ => toml::from_str(EMBEDDED).expect("embedded parses"),
    };

    RulesStatus {
        override_path: path_str,
        override_exists: exists,
        using_override,
        error,
        rule_count: rf.rules.iter().filter(|r| r.enabled).count(),
        junk_count: rf.junk.iter().filter(|j| j.enabled).count(),
        cache_count: rf.global_cache.len(),
    }
}

/// Ensure the override file exists, seeding it with the full embedded ruleset
/// (comments and all) so there is a complete, working template to edit. Returns
/// the absolute path.
pub fn ensure_override_file() -> Result<String, String> {
    let path = override_path().ok_or("no OS config directory available")?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&path, EMBEDDED).map_err(|e| e.to_string())?;
    }
    Ok(path.to_string_lossy().into_owned())
}

// ── In-app rules editor: load / save / reset the override ──────────────────────

/// The active ruleset as structured data for the editor: the override if present
/// and parseable, else the embedded default. Never fails — a broken override
/// falls back to embedded so the editor always opens with valid data.
pub fn load_rules() -> RuleFile {
    if let Some(path) = override_path() {
        if let Ok(rf) = load_rules_from(&path) {
            return rf;
        }
    }
    toml::from_str(EMBEDDED).expect("embedded prun-rules.toml must parse")
}

fn load_rules_from(path: &Path) -> Result<RuleFile, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    toml::from_str(&text).map_err(|e| e.to_string())
}

/// Validate, serialize, and atomically write the full ruleset to the override.
pub fn save_rules(rules: RuleFile) -> Result<(), String> {
    let path = override_path().ok_or("no OS config directory available")?;
    save_rules_to(&path, &rules)
}

fn save_rules_to(path: &Path, rules: &RuleFile) -> Result<(), String> {
    validate_rules(rules)?;
    let body = toml::to_string_pretty(rules).map_err(|e| format!("serialize: {e}"))?;
    let text = format!(
        "# Prun rules — managed by the in-app editor. This override replaces the\n\
         # built-in defaults wholesale; use \"Reset to defaults\" in the app to revert.\n\n{body}"
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    write_atomic(path, &text)
}

/// Delete the override so the embedded defaults take over again. Idempotent.
pub fn reset_rules() -> Result<(), String> {
    match override_path() {
        Some(path) => reset_rules_to(&path),
        None => Ok(()),
    }
}

fn reset_rules_to(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Reject anything that would make a confusing or non-round-trippable ruleset:
/// empty / duplicate ids, empty ecosystems, and unbuildable glob patterns (which
/// `Matcher::compile` would otherwise silently drop).
fn validate_rules(rf: &RuleFile) -> Result<(), String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut check_id = |kind: &str, id: &str| -> Result<(), String> {
        if id.trim().is_empty() {
            return Err(format!("a {kind} entry has an empty id"));
        }
        if !seen.insert(format!("{kind}:{id}")) {
            return Err(format!("duplicate {kind} id: {id}"));
        }
        Ok(())
    };
    for r in &rf.rules {
        check_id("rule", &r.id)?;
        if r.ecosystem.trim().is_empty() {
            return Err(format!("rule \"{}\" has an empty ecosystem", r.id));
        }
        check_globs(&r.id, &r.markers)?;
        check_globs(&r.id, &r.globs)?;
    }
    for j in &rf.junk {
        check_id("junk", &j.id)?;
        if j.ecosystem.trim().is_empty() {
            return Err(format!("junk \"{}\" has an empty ecosystem", j.id));
        }
        check_globs(&j.id, &j.globs)?;
    }
    for c in &rf.global_cache {
        check_id("cache", &c.id)?;
    }
    Ok(())
}

/// Glob fields only (`markers`, `globs`) — not `dirs` (path segments) or cache
/// `paths` (may contain `~`), which aren't glob-matched.
fn check_globs(id: &str, patterns: &[String]) -> Result<(), String> {
    for p in patterns {
        if GlobBuilder::new(p).build().is_err() {
            return Err(format!("rule \"{id}\": invalid glob pattern \"{p}\""));
        }
    }
    Ok(())
}

/// Write via a temp sibling + rename so a crash mid-write can't leave a
/// half-written rules file behind.
fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, contents).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Human-readable label for an ecosystem id (curated, with a title-case fallback).
pub fn ecosystem_label(id: &str) -> String {
    let s = match id {
        "rust" => "Rust",
        "go" => "Go",
        "cpp" => "C/C++",
        "bazel" => "Bazel",
        "zig" => "Zig",
        "nim" => "Nim",
        "swift" => "Swift",
        "dotnet" => ".NET",
        "jvm" => "JVM",
        "node" => "Node.js",
        "python" => "Python",
        "php" => "PHP",
        "ruby" => "Ruby",
        "dart" => "Dart / Flutter",
        "beam" => "Erlang / Elixir",
        "haskell" => "Haskell",
        "crystal" => "Crystal",
        "gamedev" => "Game engines",
        "infra" => "Infra / IaC",
        "latex" => "LaTeX",
        "nix" => "Nix",
        "data" => "Data",
        "docs" => "Docs / SSG",
        "testing" => "Testing / E2E",
        "junk" => "OS / junk",
        "editor" => "Editor caches",
        other => return title_case(other),
    };
    s.to_string()
}

fn title_case(id: &str) -> String {
    if id.is_empty() {
        return "Other".to_string();
    }
    id.split(['-', '_'])
        .filter(|s| !s.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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

/// Expand a leading `~` to the user's home directory.
pub fn expand_root(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            let rest = rest.trim_start_matches(['/', '\\']);
            return if rest.is_empty() { home } else { home.join(rest) };
        }
    }
    PathBuf::from(input)
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
// Filesystem helpers
// ─────────────────────────────────────────────────────────────────────────────

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn leaf_artifact(p: &Path) -> String {
    format!(
        "/{}",
        p.file_name().map(|s| s.to_string_lossy()).unwrap_or_default()
    )
}

fn parent_name(p: &Path) -> String {
    p.parent()
        .and_then(|d| d.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn dir_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

fn mtime_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_prunignore(root: &Path) -> Option<Gitignore> {
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
fn is_git_ignored(path: &Path, cache: &mut HashMap<PathBuf, Option<git2::Repository>>) -> bool {
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

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_tmp(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("prun_test_{}_{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn touch(p: &Path) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, b"x").unwrap();
    }

    fn mkdir(p: &Path) {
        fs::create_dir_all(p).unwrap();
    }

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

    #[test]
    fn embedded_ruleset_parses_and_compiles() {
        let rf: RuleFile = toml::from_str(EMBEDDED).expect("embedded parses");
        assert!(rf.rules.len() >= 60, "expected the full rule set");
        assert_eq!(rf.junk.len(), 4);
        assert_eq!(rf.global_cache.len(), 21);
        let _ = Matcher::compile(rf); // must not panic
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

    // ── rules editor: serialize round-trip + save/load/validate/reset ─

    fn round_trip(rf: &RuleFile) -> RuleFile {
        let s = toml::to_string_pretty(rf).expect("serialize");
        toml::from_str(&s).expect("reparse")
    }

    #[test]
    fn embedded_round_trips_through_toml() {
        let rf: RuleFile = toml::from_str(EMBEDDED).unwrap();
        let back = round_trip(&rf);
        assert_eq!(rf.rules.len(), back.rules.len());
        assert_eq!(rf.junk.len(), back.junk.len());
        assert_eq!(rf.global_cache.len(), back.global_cache.len());
        assert_eq!(rf.schema_version, back.schema_version);
        assert_eq!(rf.defaults.global_ignore, back.defaults.global_ignore);
    }

    #[test]
    fn round_trip_preserves_notes() {
        let rf: RuleFile = toml::from_str(EMBEDDED).unwrap();
        let back = round_trip(&rf);
        let go = back.rules.iter().find(|r| r.id == "go").expect("go rule present");
        assert!(
            go.note.as_deref().unwrap_or("").contains("vendor"),
            "go note must survive the round-trip; got {:?}",
            go.note
        );
    }

    #[test]
    fn save_then_load_on_disk() {
        let root = fresh_tmp("save");
        let path = root.join("rules.toml");
        let rf: RuleFile = toml::from_str(EMBEDDED).unwrap();
        save_rules_to(&path, &rf).expect("save");
        let loaded = load_rules_from(&path).expect("load");
        let _ = fs::remove_dir_all(&root);
        assert_eq!(rf.rules.len(), loaded.rules.len());
        assert_eq!(rf.global_cache.len(), loaded.global_cache.len());
    }

    #[test]
    fn validation_rejects_bad_input() {
        let base: RuleFile = toml::from_str(EMBEDDED).unwrap();
        assert!(validate_rules(&base).is_ok(), "embedded set must be valid");

        let mut dup = base.clone();
        dup.rules.push(dup.rules[0].clone());
        assert!(validate_rules(&dup).is_err(), "duplicate id");

        let mut empty_id = base.clone();
        empty_id.rules[0].id = String::new();
        assert!(validate_rules(&empty_id).is_err(), "empty id");

        let mut empty_eco = base.clone();
        empty_eco.rules[0].ecosystem = String::new();
        assert!(validate_rules(&empty_eco).is_err(), "empty ecosystem");

        let mut bad_glob = base.clone();
        bad_glob.rules[0].globs.push("[".to_string());
        assert!(validate_rules(&bad_glob).is_err(), "unbuildable glob");
    }

    #[test]
    fn reset_is_idempotent() {
        let root = fresh_tmp("reset");
        let path = root.join("rules.toml");
        fs::write(&path, "schema_version = 3\n").unwrap();
        assert!(reset_rules_to(&path).is_ok());
        assert!(!path.exists());
        assert!(reset_rules_to(&path).is_ok(), "missing file → still Ok");
        let _ = fs::remove_dir_all(&root);
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
