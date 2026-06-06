//! The "System caches" view: per-user shared caches listed from the ruleset's
//! `global_cache` entries. A separate scan from a project walk, and never
//! auto-selected in the UI (these are shared by all projects).

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;

use crate::fs_util::{dir_size, expand_root, leaf_artifact, mtime_secs, now_secs};
use crate::rules::{load_matcher, Matcher};

use super::{rollup, Location, ScanEvent};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_only_cache_excluded_off_mac() {
        // The macOS-only cache (Xcode DerivedData) only applies on macOS.
        assert_eq!(
            cache_applies(&Some("macos".to_string())),
            cfg!(target_os = "macos")
        );
        assert!(cache_applies(&None));
    }
}
