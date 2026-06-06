//! Override-file persistence and the active-ruleset loader.
//!
//! The active ruleset is the user override at `<config dir>/prun/rules.toml` when
//! present and parseable, else the embedded default. This module owns reading,
//! validating, atomically writing, and resetting that override, plus building the
//! [`Matcher`] used by a scan and reporting status to the Settings panel.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use serde::Serialize;

use super::matcher::Matcher;
use super::model::{RuleFile, EMBEDDED};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsupport::fresh_tmp;

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
}
