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

/// Initialize file-based logging in the OS data dir (e.g.
/// `%APPDATA%\prun\logs\prun.log` on Windows), daily-rotated and capped at a week
/// of files. Returns a guard that flushes the non-blocking writer when dropped —
/// the caller must hold it for the app's lifetime. Logging must never crash the
/// app, so any setup failure just disables the file log (returns `None`). Set the
/// `PRUN_LOG` env var (e.g. `PRUN_LOG=debug`) to raise verbosity; default is `info`.
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let dir = dirs::data_dir()?.join("prun").join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("prun")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_env("PRUN_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .init();
    Some(guard)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Held for the whole process so buffered log lines flush on exit.
    let _log_guard = init_logging();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "prun starting");

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
