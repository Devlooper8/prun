//! Prun library crate: wires the backend modules together and boots the Tauri
//! app. The work lives in focused modules:
//!
//! - [`scan`] — find reclaimable artifacts (project walk + system caches).
//! - [`clean`] — delete selected paths, streamed.
//! - [`rules`] — the ruleset model, matcher, override persistence, and labels.
//! - [`fs_util`] — shared filesystem helpers.
//! - [`commands`] — the thin Tauri command handlers exposed to the frontend.

mod clean;
mod commands;
mod fs_util;
mod rules;
mod scan;

#[cfg(test)]
mod testsupport;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Tracks what the last scan offered, so `clean` can only delete vetted paths.
        .manage(commands::Reclaimable::default())
        .invoke_handler(tauri::generate_handler![
            commands::scan,
            commands::scan_caches,
            commands::clean,
            commands::rules_status,
            commands::open_rules_file,
            commands::load_rules,
            commands::save_rules,
            commands::reset_rules
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
