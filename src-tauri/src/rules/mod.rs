//! The rule domain.
//!
//! - [`model`] — the TOML ruleset schema (also the editor's wire DTO).
//! - [`matcher`] — a [`Matcher`] compiled from a ruleset, consumed by a scan.
//! - [`store`] — override-file persistence and the active-matcher loader.
//! - [`labels`] — ecosystem id -> human label.

mod labels;
mod matcher;
mod model;
mod store;

pub(crate) use labels::ecosystem_label;
pub(crate) use matcher::{norm_seg, Matcher};
pub(crate) use model::RuleFile;
pub(crate) use store::{
    ensure_override_file, load_matcher, load_rules, reset_rules, rules_status, save_rules,
    RulesStatus,
};

/// Re-exported for cross-module tests only (the scan engine's tests build a
/// matcher from the embedded ruleset); production code reaches it within `rules`.
#[cfg(test)]
pub(crate) use model::EMBEDDED;
