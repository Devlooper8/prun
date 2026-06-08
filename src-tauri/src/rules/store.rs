//! Override-file persistence and the active-ruleset loader.
//!
//! The active ruleset is the user override at `<config dir>/prun/rules.toml` when
//! present and parseable, else the embedded default. This module owns reading,
//! validating, atomically writing, and resetting that override, plus building the
//! [`Matcher`] used by a scan and reporting status to the Settings panel.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use atomicwrites::{AllowOverwrite, AtomicFile};
use globset::GlobBuilder;
use serde::Serialize;

use super::matcher::Matcher;
use super::model::{Removed, RuleFile, EMBEDDED};

/// Path to the optional user override ruleset (`%APPDATA%\prun\rules.toml` on
/// Windows). `None` only if the OS exposes no config directory.
fn override_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("prun").join("rules.toml"))
}

/// Build the active matcher: the user override if present and parseable, else the
/// embedded default. Rebuilt per scan (parse + compile is well under a
/// millisecond) so edits to the override take effect on the next scan without
/// restarting the app.
pub(crate) fn load_matcher() -> Matcher {
    if let Some(path) = override_path() {
        if let Ok(text) = fs::read_to_string(&path) {
            match toml::from_str::<RuleFile>(&text) {
                Ok(rf) => return Matcher::compile(merge_over_embedded(rf)),
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
                merge_over_embedded(rf)
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
        cache_count: rf.global_cache.iter().filter(|c| c.enabled).count(),
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
            return merge_over_embedded(rf);
        }
    }
    toml::from_str(EMBEDDED).expect("embedded prun-rules.toml must parse")
}

fn load_rules_from(path: &Path) -> Result<RuleFile, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    toml::from_str(&text).map_err(|e| e.to_string())
}

// ── Override ⇄ embedded merge (the override LAYERS over the built-ins) ──────────
//
// The override is NOT a wholesale replacement: on load it is merged over the
// embedded base by `id`, so the user's edits/additions/removals are preserved while
// new or updated built-in rules still flow through. On save we store only the delta
// (plus a tombstone list of removed built-ins), so an unmodified built-in is never
// frozen into the override and keeps receiving future updates.

/// Layer an override over the embedded base. Per section, an override entry with a
/// matching `id` replaces the embedded one (kept in the embedded position so rule
/// precedence is stable), embedded ids the override doesn't mention come through
/// fresh, ids listed in `removed` are dropped, and override-only ids (the user's own
/// rules) are appended after the built-ins.
fn merge_over_embedded(ov: RuleFile) -> RuleFile {
    let base: RuleFile = toml::from_str(EMBEDDED).expect("embedded parses");
    RuleFile {
        schema_version: base.schema_version,
        defaults: ov.defaults,
        rules: merge_section(base.rules, ov.rules, &ov.removed.rules, |r| r.id.as_str()),
        junk: merge_section(base.junk, ov.junk, &ov.removed.junk, |j| j.id.as_str()),
        global_cache: merge_section(
            base.global_cache,
            ov.global_cache,
            &ov.removed.global_cache,
            |c| c.id.as_str(),
        ),
        removed: Removed::default(),
    }
}

fn merge_section<T: Clone>(
    base: Vec<T>,
    over: Vec<T>,
    removed: &[String],
    id_of: impl Fn(&T) -> &str + Copy,
) -> Vec<T> {
    let removed: HashSet<&str> = removed.iter().map(String::as_str).collect();
    let over_by_id: HashMap<&str, &T> = over.iter().map(|t| (id_of(t), t)).collect();
    let base_ids: HashSet<&str> = base.iter().map(id_of).collect();

    let mut out: Vec<T> = Vec::with_capacity(base.len() + over.len());
    for b in &base {
        let id = id_of(b);
        if removed.contains(id) {
            continue; // tombstoned built-in
        }
        out.push(match over_by_id.get(id) {
            Some(o) => (*o).clone(), // user's version wins, in the embedded position
            None => b.clone(),       // fresh embedded
        });
    }
    for o in &over {
        if !base_ids.contains(id_of(o)) {
            out.push(o.clone()); // user-added entry, appended after the built-ins
        }
    }
    out
}

/// Reduce a full ruleset to the minimal override: entries that are new or differ
/// from the embedded entry of the same `id`, plus a per-section list of embedded ids
/// absent from the submission (built-ins the user removed). Unmodified built-ins are
/// dropped so their future updates keep flowing.
fn delta_against_embedded(full: &RuleFile) -> RuleFile {
    let base: RuleFile = toml::from_str(EMBEDDED).expect("embedded parses");
    let (rules, rm_rules) = delta_section(&base.rules, &full.rules, |r| r.id.as_str());
    let (junk, rm_junk) = delta_section(&base.junk, &full.junk, |j| j.id.as_str());
    let (global_cache, rm_cache) =
        delta_section(&base.global_cache, &full.global_cache, |c| c.id.as_str());
    RuleFile {
        schema_version: full.schema_version,
        defaults: full.defaults.clone(),
        rules,
        junk,
        global_cache,
        removed: Removed {
            rules: rm_rules,
            junk: rm_junk,
            global_cache: rm_cache,
        },
    }
}

fn delta_section<T: Clone + PartialEq>(
    base: &[T],
    full: &[T],
    id_of: impl Fn(&T) -> &str + Copy,
) -> (Vec<T>, Vec<String>) {
    let base_by_id: HashMap<&str, &T> = base.iter().map(|t| (id_of(t), t)).collect();
    let full_ids: HashSet<&str> = full.iter().map(id_of).collect();

    let mut kept: Vec<T> = Vec::new();
    for t in full {
        match base_by_id.get(id_of(t)) {
            Some(b) if *b == t => {}       // unmodified built-in → not stored
            _ => kept.push(t.clone()),     // new or modified → stored
        }
    }
    let removed: Vec<String> = base
        .iter()
        .map(id_of)
        .filter(|id| !full_ids.contains(id))
        .map(|s| s.to_string())
        .collect();
    (kept, removed)
}

/// Validate, serialize, and atomically write the full ruleset to the override.
pub fn save_rules(rules: RuleFile) -> Result<(), String> {
    let path = override_path().ok_or("no OS config directory available")?;
    save_rules_to(&path, &rules)
}

fn save_rules_to(path: &Path, rules: &RuleFile) -> Result<(), String> {
    validate_rules(rules)?;
    let delta = delta_against_embedded(rules);
    let body = toml::to_string_pretty(&delta).map_err(|e| format!("serialize: {e}"))?;
    let text = format!(
        "# Prun rules — managed by the in-app editor. Stored as a DELTA over the\n\
         # built-in defaults: only your added/changed entries (plus a [removed] list of\n\
         # built-ins you turned off) are kept here, so future built-in rule updates keep\n\
         # reaching you. Use \"Reset to defaults\" in the app to delete this file.\n\n{body}"
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
        check_globs(&r.id, &r.anti_markers)?;
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

/// Write atomically: a temp file in the same dir is written, fsynced, then
/// atomically renamed over `path` (with the parent dir fsynced on Unix), so a
/// crash or power loss mid-write can't leave a half-written or unsynced rules
/// file behind. Handled by the `atomicwrites` crate.
fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    AtomicFile::new(path, AllowOverwrite)
        .write(|f| f.write_all(contents.as_bytes()))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsupport::fresh_tmp;

    #[test]
    fn save_default_set_stores_empty_delta_and_merges_back() {
        let root = fresh_tmp("save");
        let path = root.join("rules.toml");
        let rf: RuleFile = toml::from_str(EMBEDDED).unwrap();
        save_rules_to(&path, &rf).expect("save");
        let raw = load_rules_from(&path).expect("load"); // the stored delta
        assert!(
            raw.rules.is_empty() && raw.junk.is_empty() && raw.global_cache.is_empty(),
            "saving the unmodified default set must store an empty delta; got {} rules",
            raw.rules.len()
        );
        let merged = merge_over_embedded(raw); // re-expands to the full set
        let _ = fs::remove_dir_all(&root);
        assert_eq!(rf.rules.len(), merged.rules.len());
        assert_eq!(rf.global_cache.len(), merged.global_cache.len());
    }

    /// The core of the override-shadowing fix: a built-in rule the override predates
    /// (so the override doesn't mention it) still surfaces in the merged set.
    #[test]
    fn merge_surfaces_new_builtin() {
        let mut ov: RuleFile = toml::from_str(EMBEDDED).unwrap();
        ov.rules.retain(|r| r.id != "cmake-build"); // an override saved before this rule shipped
        let merged = merge_over_embedded(ov);
        assert!(
            merged.rules.iter().any(|r| r.id == "cmake-build"),
            "a new built-in must surface for an override that predates it"
        );
    }

    /// A per-id edit in the override wins over the embedded version.
    #[test]
    fn merge_override_edit_wins() {
        let mut ov: RuleFile = toml::from_str(EMBEDDED).unwrap();
        ov.rules.iter_mut().find(|r| r.id == "rust-cargo").unwrap().enabled = false;
        let merged = merge_over_embedded(ov);
        let rust = merged.rules.iter().find(|r| r.id == "rust-cargo").unwrap();
        assert!(!rust.enabled, "the override's disabled state must win over embedded");
    }

    /// A tombstone in a (delta) override suppresses the matching built-in, while
    /// untouched built-ins still come through.
    #[test]
    fn merge_tombstone_suppresses_builtin() {
        let ov: RuleFile = toml::from_str("schema_version = 3\n[removed]\nrules = [\"go\"]\n").unwrap();
        let merged = merge_over_embedded(ov);
        assert!(!merged.rules.iter().any(|r| r.id == "go"), "tombstoned built-in must be dropped");
        assert!(merged.rules.iter().any(|r| r.id == "rust-cargo"), "other built-ins still present");
    }

    /// Save keeps only modified + user-added entries and tombstones removed built-ins;
    /// unmodified built-ins are dropped so their future updates keep flowing.
    #[test]
    fn delta_captures_only_changes() {
        let base: RuleFile = toml::from_str(EMBEDDED).unwrap();
        let mut full = base.clone();
        full.rules.iter_mut().find(|r| r.id == "rust-cargo").unwrap().enabled = false; // modify
        let mut mine = base.rules[0].clone();
        mine.id = "my-custom".to_string();
        full.rules.push(mine); // new user rule
        full.rules.retain(|r| r.id != "go"); // remove a built-in

        let d = delta_against_embedded(&full);
        let ids: Vec<&str> = d.rules.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"rust-cargo"), "modified built-in kept; got {ids:?}");
        assert!(ids.contains(&"my-custom"), "user rule kept; got {ids:?}");
        assert!(!ids.contains(&"maven"), "unmodified built-in must be stripped; got {ids:?}");
        assert_eq!(d.removed.rules, vec!["go".to_string()], "removed built-in tombstoned");
    }

    /// Full cycle: save a customization, reload via merge, customization survives and
    /// no built-ins are lost.
    #[test]
    fn save_then_merge_load_preserves_customization() {
        let root = fresh_tmp("rt");
        let path = root.join("rules.toml");
        let mut full: RuleFile = toml::from_str(EMBEDDED).unwrap();
        full.rules.iter_mut().find(|r| r.id == "make-objects").unwrap().enabled = true;
        save_rules_to(&path, &full).expect("save");
        let merged = merge_over_embedded(load_rules_from(&path).expect("load"));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(full.rules.len(), merged.rules.len(), "no rules lost across the cycle");
        assert!(
            merged.rules.iter().find(|r| r.id == "make-objects").unwrap().enabled,
            "the user's customization must survive save→load"
        );
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
}
