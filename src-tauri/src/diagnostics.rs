//! Crash + log diagnostics: where logs live, file-logging setup, and a panic
//! hook that leaves a crash report behind.
//!
//! Release builds abort on panic (`panic = "abort"` in Cargo.toml) and strip
//! symbols, so a crash would otherwise vanish without a trace — stderr is
//! invisible for a GUI-subsystem app and the async tracing writer's buffer is
//! lost on abort. The hook writes a plain file with synchronous `std::fs`
//! before the abort, which is the one channel that reliably survives.

use std::fmt::Display;
use std::path::{Path, PathBuf};

/// Where logs and crash reports live (e.g. `%APPDATA%\prun\logs` on Windows,
/// `~/.local/share/prun/logs` on Linux). `None` if the OS reports no data dir.
pub(crate) fn log_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("prun").join("logs"))
}

/// Initialize file-based logging in [`log_dir`], daily-rotated and capped at a
/// week of files. Returns a guard that flushes the non-blocking writer when
/// dropped — the caller must hold it for the app's lifetime. Logging must never
/// crash the app, so any setup failure just disables the file log (returns
/// `None`). Set the `PRUN_LOG` env var (e.g. `PRUN_LOG=debug`) to raise
/// verbosity; default is `info`.
pub(crate) fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let dir = log_dir()?;
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

/// How many crash reports to keep; older ones are pruned before a new write.
const KEEP_CRASH_REPORTS: usize = 5;

/// Install a process-wide panic hook that writes a crash report into
/// [`log_dir`], then falls through to the previous hook (so dev builds keep the
/// stderr message). `main` installs this before anything else runs; both the
/// GUI and the CLI paths are covered.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = log_dir() {
            write_crash_report(&dir, info);
        }
        previous(info);
    }));
}

/// Write one crash report (panic message + location, thread, app version,
/// backtrace) into `dir`, pruning old reports so at most
/// [`KEEP_CRASH_REPORTS`] remain. Runs while the process is already dying, so
/// it must never panic itself — every error is deliberately swallowed.
fn write_crash_report(dir: &Path, info: &dyn Display) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    prune_crash_reports(dir, KEEP_CRASH_REPORTS - 1);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let report = format!(
        "prun {} crash report (unix time {now})\nthread: {}\n\n{info}\n\nbacktrace:\n{}\n",
        env!("CARGO_PKG_VERSION"),
        std::thread::current().name().unwrap_or("<unnamed>"),
        // Mostly raw addresses in release builds (symbols are stripped); the
        // panic message + location above carry the real signal.
        std::backtrace::Backtrace::force_capture(),
    );
    let _ = std::fs::write(dir.join(format!("crash-{now}.txt")), report);
}

/// Delete the oldest `crash-*.txt` files so at most `keep` remain. The unix
/// timestamp in the name keeps lexical order == age order (digit count is
/// stable until the year 2286).
fn prune_crash_reports(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut crashes: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            let starts_with_crash = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("crash-"));
            let has_txt_ext = p
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"));
            starts_with_crash && has_txt_ext
        })
        .collect();
    crashes.sort();
    while crashes.len() > keep {
        let _ = std::fs::remove_file(crashes.remove(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsupport::fresh_tmp;

    #[test]
    fn crash_report_is_written_with_message_and_version() {
        let dir = fresh_tmp("diag_report");
        write_crash_report(&dir, &"panicked at src/lib.rs:1:1:\nboom");

        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1, "exactly one report written");
        let body = std::fs::read_to_string(&files[0]).unwrap();
        assert!(body.contains("boom"), "carries the panic message: {body}");
        assert!(
            body.contains(env!("CARGO_PKG_VERSION")),
            "carries the version: {body}"
        );
        assert!(body.contains("backtrace:"), "carries a backtrace: {body}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_crash_reports_are_pruned_other_files_untouched() {
        let dir = fresh_tmp("diag_prune");
        std::fs::create_dir_all(&dir).unwrap();
        for t in 1..=3 {
            std::fs::write(dir.join(format!("crash-{t}.txt")), "old").unwrap();
        }
        std::fs::write(dir.join("prun.2026-06-11.log"), "a log").unwrap();

        prune_crash_reports(&dir, 2);

        assert!(!dir.join("crash-1.txt").exists(), "oldest pruned");
        assert!(dir.join("crash-2.txt").exists(), "newer kept");
        assert!(dir.join("crash-3.txt").exists(), "newest kept");
        assert!(dir.join("prun.2026-06-11.log").exists(), "logs untouched");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
