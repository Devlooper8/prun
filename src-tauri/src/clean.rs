//! Deletion of selected paths, streamed per-path to the UI.
//!
//! Stays out of the scan/rules domains: given a list of paths it removes each
//! one (to Trash or permanently) and reports progress over an `emit` closure,
//! exactly mirroring how the scan engine streams its own events.

use std::fs;
use std::path::Path;

use serde::Serialize;

/// Streamed progress from a running clean, delivered to the UI over a Channel.
/// `done`/`total` drive the progress bar; the UI drops each `Removed` path from
/// the list live and leaves `Failed` ones behind (marked). Mirrors `CleanEvent`
/// in TS. `done` counts paths *finished* — excluding the current one on
/// `Removing`, including it on `Removed`/`Failed`.
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CleanEvent {
    /// About to delete `path` (drives the "Cleaning… <path>" label).
    Removing {
        path: String,
        done: usize,
        total: usize,
    },
    /// `path` is gone (deleted now, or already absent).
    Removed {
        path: String,
        done: usize,
        total: usize,
    },
    /// `path` could not be removed (e.g. a file inside it is locked/in use).
    Failed {
        path: String,
        error: String,
        done: usize,
        total: usize,
    },
    /// All paths processed, with the final tally.
    Done { removed: usize, failed: usize },
}

/// Delete each path in turn, streaming progress so the UI can show a determinate
/// bar and drop rows the moment each deletion confirms. Paths are processed
/// sequentially (clear single-path progress, and parallel deletes on one disk
/// rarely help); the caller orders them largest-first. When `to_trash` is true a
/// path goes to the system Trash (recoverable), otherwise it is permanently
/// removed. A path already absent counts as removed (its row should clear too).
/// Per-path failures are reported and never abort the rest.
pub fn clean(paths: &[String], to_trash: bool, emit: &mut dyn FnMut(CleanEvent)) {
    let total = paths.len();
    let mut removed = 0usize;
    let mut failed = 0usize;
    for p in paths {
        emit(CleanEvent::Removing {
            path: p.clone(),
            done: removed + failed, // paths finished before this one
            total,
        });
        let path = Path::new(p);
        // symlink_metadata (not exists): an already-absent path is "gone", so
        // treat it as removed; a dangling symlink is still removable.
        let outcome = if fs::symlink_metadata(path).is_err() {
            Ok(())
        } else if to_trash {
            trash::delete(path).map_err(|e| e.to_string())
        } else {
            remove_path(path).map_err(|e| e.to_string())
        };
        match outcome {
            Ok(()) => {
                removed += 1;
                emit(CleanEvent::Removed {
                    path: p.clone(),
                    done: removed + failed,
                    total,
                });
            }
            Err(error) => {
                failed += 1;
                emit(CleanEvent::Failed {
                    path: p.clone(),
                    error,
                    done: removed + failed,
                    total,
                });
            }
        }
    }
    emit(CleanEvent::Done { removed, failed });
}

/// Permanently remove a file, directory, or symlink. A directory's contents are
/// removed recursively; a symlink is removed without following it (on Windows a
/// directory symlink needs `remove_dir`, so fall back to it).
fn remove_path(path: &Path) -> std::io::Result<()> {
    let ft = fs::symlink_metadata(path)?.file_type();
    if ft.is_symlink() {
        fs::remove_file(path).or_else(|_| fs::remove_dir(path))
    } else if ft.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsupport::{fresh_tmp, touch};

    #[test]
    fn clean_streams_and_removes() {
        let root = fresh_tmp("clean");
        let a = root.join("a").join("target");
        let b = root.join("b").join("node_modules");
        touch(&a.join("blob"));
        touch(&b.join("x.js"));
        let missing = root.join("already-gone"); // never created
        let paths = vec![
            a.to_string_lossy().into_owned(),
            b.to_string_lossy().into_owned(),
            missing.to_string_lossy().into_owned(),
        ];

        let mut events: Vec<CleanEvent> = Vec::new();
        clean(&paths, false, &mut |ev| events.push(ev));

        let a_gone = !a.exists();
        let b_gone = !b.exists();
        let _ = fs::remove_dir_all(&root);
        assert!(a_gone && b_gone, "both artifact dirs must be deleted");

        let removed = events
            .iter()
            .filter(|e| matches!(e, CleanEvent::Removed { .. }))
            .count();
        let failed = events
            .iter()
            .filter(|e| matches!(e, CleanEvent::Failed { .. }))
            .count();
        // two real dirs + one already-absent path all report Removed
        assert_eq!(removed, 3, "every path should resolve to Removed");
        assert_eq!(failed, 0, "nothing should fail in the happy path");
        match events.last() {
            Some(CleanEvent::Done { removed, failed }) => {
                assert_eq!((*removed, *failed), (3, 0), "final tally");
            }
            _ => panic!("Done must be the final event"),
        }
    }

    /// One side of the wire-contract triangle (see src/wire-contract.test.ts):
    /// pins `fixture == serde's actual serialization` for `CleanEvent`.
    #[test]
    fn clean_event_fixture_matches_serialization() {
        let events = vec![
            CleanEvent::Removing {
                path: "/projects/app/target".into(),
                done: 0,
                total: 2,
            },
            CleanEvent::Removed {
                path: "/projects/app/target".into(),
                done: 1,
                total: 2,
            },
            CleanEvent::Failed {
                path: "/projects/web/node_modules".into(),
                error: "in use".into(),
                done: 2,
                total: 2,
            },
            CleanEvent::Done {
                removed: 1,
                failed: 1,
            },
        ];
        let ours = serde_json::to_value(&events).unwrap();
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../fixtures/clean-events.json")).unwrap();
        assert_eq!(
            ours, fixture,
            "fixtures/clean-events.json must match the Rust wire format"
        );
    }

    /// Windows: cleaning a junction removes the LINK itself, never the target's
    /// contents — a junction inside an artifact tree must not nuke what it
    /// points at (see SECURITY.md scope).
    #[cfg(windows)]
    #[test]
    fn cleaning_a_junction_removes_link_not_target() {
        let root = fresh_tmp("clean_junction");
        let target = root.join("real");
        touch(&target.join("keep.txt"));
        let link = root.join("link");
        crate::testsupport::junction(&link, &target);

        clean(&[link.to_string_lossy().into_owned()], false, &mut |_| {});

        let link_gone = fs::symlink_metadata(&link).is_err();
        let kept = target.join("keep.txt").exists();
        let _ = fs::remove_dir_all(&root);
        assert!(link_gone, "the junction itself must be removed");
        assert!(kept, "the junction's target contents must survive");
    }
}
