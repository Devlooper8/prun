//! The scanning domain: finding reclaimable artifacts and streaming them to the UI.
//!
//! Two entry points share the wire DTOs and the category roll-up defined here:
//! - [`project`] — the *root-first* scan of a project tree (the main view).
//! - [`caches`] — the per-user "System caches" view.
//!
//! "Root-first" means a directory is a project root for a rule when one of that
//! rule's markers sits directly inside it; that rule's `dirs`/`globs` under the
//! root become reclaim candidates.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::rules::ecosystem_label;

mod caches;
mod project;

pub(crate) use caches::scan_caches;
pub(crate) use project::scan;

// ── Wire types (serialized to the UI). `category` is the rule's ecosystem id. ──

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

/// Sum location sizes per ecosystem into the category roll-up shown beside the
/// list, largest first. Shared by both scan entry points.
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
