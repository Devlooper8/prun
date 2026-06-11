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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Held for the whole process so buffered log lines flush on exit.
    let _log_guard = diagnostics::init_logging();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "prun starting");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Auto-update support. Inert until an updater endpoint + signing pubkey are
        // configured (see RELEASING.md); wiring it now keeps that a config-only step.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Tracks what the last scan offered, so `clean` can only delete vetted paths.
        .manage(commands::Reclaimable::default())
        // Shared cancel flag so the UI can stop a long-running scan.
        .manage(commands::Cancel::default())
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
