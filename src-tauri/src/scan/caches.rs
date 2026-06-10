//! The "System caches" view: per-user shared caches listed from the ruleset's
//! `global_cache` entries. A separate scan from a project walk, and never
//! auto-selected in the UI (these are shared by all projects).

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use rayon::prelude::*;

use crate::fs_util::{expand_root, leaf_artifact, measure_tree, now_secs};
use crate::rules::{load_matcher, Matcher};

use super::{rollup, Location, ScanEvent};

pub fn scan_caches(cancel: &AtomicBool, emit: &(dyn Fn(ScanEvent) + Sync)) -> Result<(), String> {
    scan_caches_with(&load_matcher(), cancel, emit)
}

fn scan_caches_with(
    matcher: &Matcher,
    cancel: &AtomicBool,
    emit: &(dyn Fn(ScanEvent) + Sync),
) -> Result<(), String> {
    let now = now_secs();
    let mut pending: Vec<(PathBuf, String, String)> = Vec::new(); // (path, ecosystem, cache name)
    for gc in &matcher.global_caches {
        if !gc.enabled || !cache_applies(&gc.platform) {
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
    let errors = AtomicU64::new(0);
    let mut locations: Vec<Location> = pending
        .par_iter()
        .filter_map(|(p, eco, name)| {
            if cancel.load(Ordering::Relaxed) {
                return None; // user cancelled — stop sizing further caches
            }
            let measured = measure_tree(p);
            errors.fetch_add(measured.errors, Ordering::Relaxed);
            let location = Location {
                path: p.to_string_lossy().into_owned(),
                project: name.clone(),
                artifact: leaf_artifact(p),
                category: eco.clone(),
                size: measured.size,
                age_secs: now.saturating_sub(measured.newest_mtime),
                git_ignored: true,
            };
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            emit(ScanEvent::Located {
                location: location.clone(),
                done: n,
                total,
            });
            Some(location)
        })
        .collect();

    locations.sort_by(|a, b| b.size.cmp(&a.size));
    emit(ScanEvent::Done {
        root: "System caches".to_string(),
        categories: rollup(&locations),
        errors: errors.load(Ordering::Relaxed),
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

    #[test]
    fn disabled_cache_is_skipped() {
        use crate::testsupport::{fresh_tmp, mkdir};
        use std::sync::Mutex;

        let root = fresh_tmp("syscaches");
        let on = root.join("cache_on");
        let off = root.join("cache_off");
        mkdir(&on);
        mkdir(&off);
        // Single-quoted TOML literal strings so Windows backslashes survive.
        let toml = format!(
            r#"[[global_cache]]
id = "on"
ecosystem = "rust"
paths = ['{on}']

[[global_cache]]
id = "off"
ecosystem = "rust"
enabled = false
paths = ['{off}']
"#,
            on = on.display(),
            off = off.display()
        );
        let m = Matcher::compile(toml::from_str(&toml).expect("test toml parses"));

        let paths: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let cancel = AtomicBool::new(false);
        scan_caches_with(&m, &cancel, &|ev| {
            if let ScanEvent::Located { location, .. } = ev {
                paths.lock().unwrap().push(location.path);
            }
        })
        .unwrap();
        let paths = paths.into_inner().unwrap();
        let _ = fs::remove_dir_all(&root);

        assert!(
            paths.iter().any(|p| p.ends_with("cache_on")),
            "enabled cache must be scanned; got {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.ends_with("cache_off")),
            "disabled cache must be skipped; got {paths:?}"
        );
    }
}
