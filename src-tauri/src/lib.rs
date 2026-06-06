mod clean;
mod fs_util;
mod rules;
mod scan;

#[cfg(test)]
mod testsupport;

use clean::{clean as run_clean, CleanEvent};
use rules::{
    ensure_override_file, load_rules as get_rules, reset_rules as do_reset_rules,
    rules_status as get_rules_status, save_rules as do_save_rules, RuleFile, RulesStatus,
};
use scan::{scan as run_scan, scan_caches as run_scan_caches, ScanEvent, ScanOptions};
use tauri::ipc::Channel;

/// Walk the root and stream progress + each reclaimable dir to the UI as it is
/// discovered and sized. Results arrive over `on_event` rather than as a single
/// return value, so the window stays responsive on huge trees.
#[tauri::command]
async fn scan(opts: ScanOptions, on_event: Channel<ScanEvent>) -> Result<(), String> {
    // Filesystem walking is blocking; keep the UI thread free.
    tauri::async_runtime::spawn_blocking(move || {
        run_scan(&opts, &move |event| {
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
async fn scan_caches(on_event: Channel<ScanEvent>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        run_scan_caches(&move |event| {
            let _ = on_event.send(event);
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Remove the selected paths, streaming per-path progress to the UI over
/// `on_event` (like `scan`) so it can show a progress bar and drop each row as
/// its deletion confirms. When `to_trash` is true paths go to the system Trash
/// (recoverable); otherwise they are permanently deleted. Each path may be a
/// directory, a file, or a symlink. Per-path failures are reported in the stream
/// and never abort the rest of the batch.
#[tauri::command]
async fn clean(
    paths: Vec<String>,
    to_trash: bool,
    on_event: Channel<CleanEvent>,
) -> Result<(), String> {
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
fn rules_status() -> RulesStatus {
    get_rules_status()
}

/// Load the active ruleset (override if present and valid, else built-in
/// defaults) as structured data for the in-app editor.
#[tauri::command]
fn load_rules() -> RuleFile {
    get_rules()
}

/// Validate and save the full ruleset to the override file. The editor's next
/// scan picks it up (the matcher reloads per scan).
#[tauri::command]
fn save_rules(rules: RuleFile) -> Result<(), String> {
    do_save_rules(rules)
}

/// Delete the override so the built-in defaults take over again.
#[tauri::command]
fn reset_rules() -> Result<(), String> {
    do_reset_rules()
}

/// Create the override rules file from the built-in defaults if it doesn't yet
/// exist, then open it in the user's default editor. Returns the path.
#[tauri::command]
fn open_rules_file() -> Result<String, String> {
    let path = ensure_override_file()?;
    os_open(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Open a path with the OS default handler (detached; we don't wait on it).
#[cfg(target_os = "windows")]
fn os_open(path: &str) -> std::io::Result<()> {
    std::process::Command::new("explorer").arg(path).spawn().map(|_| ())
}
#[cfg(target_os = "macos")]
fn os_open(path: &str) -> std::io::Result<()> {
    std::process::Command::new("open").arg(path).spawn().map(|_| ())
}
#[cfg(all(unix, not(target_os = "macos")))]
fn os_open(path: &str) -> std::io::Result<()> {
    std::process::Command::new("xdg-open").arg(path).spawn().map(|_| ())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            scan,
            scan_caches,
            clean,
            rules_status,
            open_rules_file,
            load_rules,
            save_rules,
            reset_rules
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
