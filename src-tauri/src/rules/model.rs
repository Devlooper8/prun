//! The TOML ruleset data model.
//!
//! These serde structs are both the on-disk `prun-rules.toml` schema and the
//! wire DTO for the in-app rules editor. `RuleFile` round-trips through
//! `toml::to_string_pretty`: the root scalar `schema_version` is declared first
//! so it serializes ahead of the `[defaults]` table and the arrays-of-tables.

use serde::{Deserialize, Serialize};

/// The ruleset that ships with the binary. An optional user override at
/// `<config dir>/prun/rules.toml` (e.g. `%APPDATA%\prun\rules.toml`) wins when
/// present and parseable.
pub(crate) static EMBEDDED: &str = include_str!("../../prun-rules.toml");

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
    /// Tombstones: built-in ids the user removed. Only meaningful in an on-disk
    /// override, where it suppresses the matching embedded entry on merge. The
    /// editor's wire JSON omits it (always empty post-merge); `save_rules` derives
    /// it by diffing the submitted set against the embedded base. See
    /// `store::merge_over_embedded` / `store::delta_against_embedded`.
    #[serde(default, skip_serializing_if = "Removed::is_empty")]
    pub removed: Removed,
}

/// Built-in ids the user has removed, per section — see [`RuleFile::removed`].
#[derive(Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct Removed {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub junk: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub global_cache: Vec<String>,
}

impl Removed {
    fn is_empty(&self) -> bool {
        self.rules.is_empty() && self.junk.is_empty() && self.global_cache.is_empty()
    }
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
#[derive(Deserialize, Serialize, Clone, PartialEq)]
pub struct Rule {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub ecosystem: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<String>,
    /// Negative markers: if any is a direct child of a candidate dir, the rule is
    /// suppressed there. Lets `reclaim_root` skip an in-source build — a dir that
    /// holds both the build marker and the source one (e.g. CMakeCache.txt next to
    /// CMakeLists.txt) — instead of reclaiming the whole source tree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anti_markers: Vec<String>,
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

#[derive(Deserialize, Serialize, Clone, PartialEq)]
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

#[derive(Deserialize, Serialize, Clone, PartialEq)]
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
