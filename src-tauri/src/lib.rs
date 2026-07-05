//! Prun library crate: wires the backend modules together and boots the Tauri
//! app. The work lives in focused modules:
//!
//! - [`scan`] — find reclaimable artifacts (project walk + system caches).
//! - [`clean`] — delete selected paths, streamed.
//! - [`rules`] — the ruleset model, matcher, override persistence, and labels.
//! - [`fs_util`] — shared filesystem helpers.
//! - [`diagnostics`] — log dir, file logging, and the crash-report panic hook.
//! - [`commands`] — the thin Tauri command handlers exposed to the frontend.
//! - [`cli`] — a headless CLI over the same core, for scripting and GUI-free tests.

mod clean;
pub mod cli;
mod commands;
mod diagnostics;
mod fs_util;
mod rules;
mod scan;

#[cfg(test)]
mod testsupport;

// `main` installs the hook before doing anything else (GUI and CLI alike).
pub use diagnostics::install_panic_hook;

/// Boots the Tauri app.
///
/// # Errors
///
/// Returns an error if the Tauri runtime fails to start.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> Result<(), String> {
    // Held for the whole process so buffered log lines flush on exit.
    let _log_guard = diagnostics::init_logging();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "prun starting");

    let context = tauri::generate_context!();
    // Auto-update support is opt-in BY CONFIG: tauri-plugin-updater requires a
    // `plugins.updater` block (pubkey + endpoints, see RELEASING.md) and aborts
    // the whole app at boot when the block is absent. Register the plugin only
    // once that config exists — activation stays a config-only step, and an
    // unconfigured build keeps booting.
    let updater_configured = context.config().plugins.0.contains_key("updater");

    let mut builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    if updater_configured {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }
    builder
        // Tracks what the last scan offered, so `clean` can only delete vetted paths.
        .manage(commands::Reclaimable::default())
        // Shared cancel flag so the UI can stop a long-running scan.
        .manage(commands::Cancel::default())
        // One scan at a time: Reclaimable/Cancel are app-global, so overlapping
        // scans (e.g. a second window) would corrupt each other's state.
        .manage(commands::ScanLock::default())
        .invoke_handler(tauri::generate_handler![
            commands::scan,
            commands::scan_caches,
            commands::cancel_scan,
            commands::clean,
            commands::rules_status,
            commands::open_rules_file,
            commands::open_logs_dir,
            commands::load_rules,
            commands::save_rules,
            commands::reset_rules
        ])
        .run(context)
        .map_err(|e| e.to_string())
}
