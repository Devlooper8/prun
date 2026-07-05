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
use std::sync::Mutex;

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
    /// Scan complete, with the final category roll-up. `errors` is how many entries
    /// couldn't be read while sizing (permissions, races) — surfaced so a wrong total
    /// doesn't masquerade as authoritative. `error_samples` carries up to
    /// [`ERROR_SAMPLE_CAP`] concrete "path: reason" examples so the user can tell
    /// *what* was skipped, not just how much.
    Done {
        root: String,
        categories: Vec<Category>,
        errors: u64,
        error_samples: Vec<String>,
    },
}

/// How many concrete read-error examples a scan reports (the count in
/// `Done.errors` still reflects everything).
pub(crate) const ERROR_SAMPLE_CAP: usize = 5;

/// Record one read-error example into the capped list — the first
/// [`ERROR_SAMPLE_CAP`] win. Shared by the parallel sizing loops, hence the Mutex.
pub(crate) fn push_error_sample(samples: &Mutex<Vec<String>>, sample: &str) {
    let mut s = samples
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if s.len() < ERROR_SAMPLE_CAP {
        s.push(sample.to_string());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The frontend's `types.ts` mirrors this wire shape by hand — pin the field
    /// names the UI relies on so a rename here fails a test instead of silently
    /// `undefined`-ing a TS field.
    #[test]
    fn done_event_wire_shape_is_stable() {
        let ev = ScanEvent::Done {
            root: "/projects".into(),
            categories: vec![],
            errors: 2,
            error_samples: vec!["/projects/x: denied".into()],
        };
        let json = serde_json::to_string(&ev).unwrap();
        for key in [
            "\"kind\":\"done\"",
            "\"root\"",
            "\"categories\"",
            "\"errors\"",
            "\"error_samples\"",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
    }

    #[test]
    fn error_samples_are_capped() {
        let samples = Mutex::new(Vec::new());
        for i in 0..(ERROR_SAMPLE_CAP + 3) {
            push_error_sample(&samples, &format!("path-{i}: denied"));
        }
        assert_eq!(samples.into_inner().unwrap().len(), ERROR_SAMPLE_CAP);
    }

    /// One side of the wire-contract triangle (see src/wire-contract.test.ts):
    /// this pins `fixture == serde's actual serialization`; the Vitest side
    /// pins `fixture == types.ts`. Change the wire format → update the fixture
    /// and both tests together.
    #[test]
    fn scan_event_fixture_matches_serialization() {
        let events = vec![
            ScanEvent::Discovering { scanned: 480 },
            ScanEvent::Discovered { total: 2 },
            ScanEvent::Located {
                location: Location {
                    path: "/projects/app/target".into(),
                    project: "app".into(),
                    artifact: "/target".into(),
                    category: "rust".into(),
                    size: 6_600_000_000,
                    age_secs: 1_728_000,
                    git_ignored: true,
                },
                done: 1,
                total: 2,
            },
            ScanEvent::Done {
                root: "/projects".into(),
                categories: vec![Category {
                    id: "rust".into(),
                    label: "Rust".into(),
                    size: 6_600_000_000,
                }],
                errors: 1,
                error_samples: vec!["/projects/app/target/locked.bin: access denied".into()],
            },
        ];
        let ours = serde_json::to_value(&events).unwrap();
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/scan-events.json")).unwrap();
        assert_eq!(
            ours, fixture,
            "fixtures/scan-events.json must match the Rust wire format"
        );
    }
}
