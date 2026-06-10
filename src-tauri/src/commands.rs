//! The Tauri command layer.
//!
//! Thin handlers: they adapt IPC input, call into the scan / clean / rules
//! modules, and map the result back. The streaming commands run their blocking
//! work off the UI thread and forward each event over a `Channel`. A dropped
//! receiver (the window closed mid-operation) is ignored rather than aborting.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::ipc::Channel;
use tauri::State;

use crate::clean::{clean as run_clean, CleanEvent};
use crate::rules::{
    ensure_override_file, load_rules as get_rules, reset_rules as do_reset_rules,
    rules_status as get_rules_status, save_rules as do_save_rules, RuleFile, RulesStatus,
};
use crate::scan::{scan as run_scan, scan_caches as run_scan_caches, ScanEvent, ScanOptions};

/// The set of paths the most recent scan offered as reclaimable. `clean` refuses any
/// path not in here, so a compromised webview (e.g. via an XSS payload) can only ask
/// to delete what a real scan already surfaced to the user — never an arbitrary path
/// on disk. This is the security boundary at the IPC edge, kept out of the pure
/// `scan`/`clean` functions so those stay reusable and unit-testable. The set is
/// reset at the start of every scan and filled as each location streams out, so it
/// always mirrors exactly what the UI is currently showing.
#[derive(Default, Clone)]
pub struct Reclaimable(Arc<Mutex<HashSet<PathBuf>>>);

impl Reclaimable {
    fn reset(&self) {
        self.0.lock().unwrap().clear();
    }
    fn offer(&self, path: &str) {
        self.0.lock().unwrap().insert(PathBuf::from(path));
    }
    /// The first path that no scan has offered, if any — that clean must be refused.
    fn first_unoffered<'a>(&self, paths: &'a [String]) -> Option<&'a str> {
        let set = self.0.lock().unwrap();
        paths
            .iter()
            .map(String::as_str)
            .find(|p| !set.contains(Path::new(p)))
    }
}

/// A shared "stop the current scan" flag. The UI's cancel button flips it via the
/// `cancel_scan` command; the running walk/sizing checks it and bails promptly.
/// Each new scan clears it, so a stale cancel never aborts the next run.
#[derive(Default, Clone)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// Clear the flag for a fresh scan and hand back the shared flag to check.
    fn begin(&self) -> Arc<AtomicBool> {
        self.0.store(false, Ordering::Relaxed);
        self.0.clone()
    }
    fn signal(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Walk the root and stream progress + each reclaimable dir to the UI as it is
/// discovered and sized. Results arrive over `on_event` rather than as a single
/// return value, so the window stays responsive on huge trees.
#[tauri::command]
pub async fn scan(
    opts: ScanOptions,
    on_event: Channel<ScanEvent>,
    reclaimable: State<'_, Reclaimable>,
    cancel: State<'_, Cancel>,
) -> Result<(), String> {
    let offered = reclaimable.inner().clone();
    // A fresh scan replaces what the previous one offered.
    offered.reset();
    // Clear any stale cancel and share the flag with the walk.
    let cancel = cancel.begin();
    // Filesystem walking is blocking; keep the UI thread free.
    tauri::async_runtime::spawn_blocking(move || {
        run_scan(&opts, &cancel, &move |event| {
            // Record each discovered path so a later `clean` can be authorized.
            if let ScanEvent::Located { location, .. } = &event {
                offered.offer(&location.path);
            }
            // A dropped receiver (window closed mid-scan) is not an error worth
            // aborting the walk over — just stop trying to deliver.
            let _ = on_event.send(event);
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Scan per-user shared caches (the "System caches" view). These are surfaced
/// separately from a project scan and are never auto-selected in the UI.
#[tauri::command]
pub async fn scan_caches(
    on_event: Channel<ScanEvent>,
    reclaimable: State<'_, Reclaimable>,
    cancel: State<'_, Cancel>,
) -> Result<(), String> {
    let offered = reclaimable.inner().clone();
    offered.reset(); // the caches view replaces what a project scan offered
    let cancel = cancel.begin();
    tauri::async_runtime::spawn_blocking(move || {
        run_scan_caches(&cancel, &move |event| {
            if let ScanEvent::Located { location, .. } = &event {
                offered.offer(&location.path);
            }
            let _ = on_event.send(event);
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Ask the running scan (project or caches) to stop. Idempotent and safe to call
/// when nothing is scanning — the next scan clears the flag before starting.
#[tauri::command]
pub fn cancel_scan(cancel: State<'_, Cancel>) {
    cancel.signal();
}

/// Remove the selected paths, streaming per-path progress to the UI over
/// `on_event` (like `scan`) so it can show a progress bar and drop each row as
/// its deletion confirms. When `to_trash` is true paths go to the system Trash
/// (recoverable); otherwise they are permanently deleted. Each path may be a
/// directory, a file, or a symlink. Per-path failures are reported in the stream
/// and never abort the rest of the batch.
#[tauri::command]
pub async fn clean(
    paths: Vec<String>,
    to_trash: bool,
    on_event: Channel<CleanEvent>,
    reclaimable: State<'_, Reclaimable>,
) -> Result<(), String> {
    // Authorization: only delete what a scan actually surfaced. A well-behaved UI
    // never sends anything else; a path outside the set means a bug or an attack,
    // so refuse the whole batch loudly rather than touch an unvetted path.
    if let Some(bad) = reclaimable.first_unoffered(&paths) {
        return Err(format!(
            "refused: \"{bad}\" was not offered by a scan (clean only removes paths a scan surfaced)"
        ));
    }
    tauri::async_runtime::spawn_blocking(move || {
        run_clean(&paths, to_trash, &move |event| {
            // A dropped receiver (window closed mid-clean) is not worth aborting
            // the deletions over — just stop trying to deliver.
            let _ = on_event.send(event);
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Report where the rules override file lives and whether it is in effect, so
/// the Settings panel can tell the user how to customize detection.
#[tauri::command]
pub fn rules_status() -> RulesStatus {
    get_rules_status()
}

/// Load the active ruleset (override if present and valid, else built-in
/// defaults) as structured data for the in-app editor.
#[tauri::command]
pub fn load_rules() -> RuleFile {
    get_rules()
}

/// Validate and save the full ruleset to the override file. The editor's next
/// scan picks it up (the matcher reloads per scan).
#[tauri::command]
pub fn save_rules(rules: RuleFile) -> Result<(), String> {
    do_save_rules(rules)
}

/// Delete the override so the built-in defaults take over again.
#[tauri::command]
pub fn reset_rules() -> Result<(), String> {
    do_reset_rules()
}

/// Create the override rules file from the built-in defaults if it doesn't yet
/// exist, then open it with the OS default handler (detached; we don't wait on
/// it). Returns the path.
#[tauri::command]
pub fn open_rules_file() -> Result<String, String> {
    let path = ensure_override_file()?;
    open::that_detached(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_authorization_refuses_unoffered_paths() {
        let r = Reclaimable::default();
        r.offer("/projects/app/target");

        // An offered path is authorized.
        assert!(r
            .first_unoffered(&["/projects/app/target".to_string()])
            .is_none());

        // An arbitrary path the scan never surfaced is caught.
        assert_eq!(
            r.first_unoffered(&["/etc/passwd".to_string()]),
            Some("/etc/passwd")
        );

        // A mix is rejected on the first unoffered entry.
        assert!(r
            .first_unoffered(&[
                "/projects/app/target".to_string(),
                "/somewhere/else".to_string(),
            ])
            .is_some());

        // A fresh scan resets the grant.
        r.reset();
        assert_eq!(
            r.first_unoffered(&["/projects/app/target".to_string()]),
            Some("/projects/app/target")
        );
    }
}
