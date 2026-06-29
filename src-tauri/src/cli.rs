//! A small headless CLI over the Tauri-free core (scan / caches / rules / clean).
//!
//! It exists for two reasons: scripting (pipe `prun scan --json` into other tools,
//! run it in CI to reclaim cache space) and as a fast, GUI-free test surface over
//! the same `scan`/`clean`/`rules` code the app uses. No external arg-parser — the
//! handful of flags are matched by hand to keep the dependency list and the code
//! approachable.
//!
//! NOTE: on Windows *release* builds the binary is in the GUI subsystem, so output
//! won't attach to an existing console. Use a dev build (or a console-subsystem
//! build) when you need piped CLI output on Windows; it works as-is on Linux/macOS.

use std::io::Write;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use crate::clean::{clean, CleanEvent};
use crate::rules::rules_status;
use crate::scan::{scan, scan_caches, Location, ScanEvent, ScanOptions};

/// Is `arg` one of our subcommands? `main` uses this so a stray OS-passed argument
/// doesn't divert a normal GUI launch into CLI mode.
pub fn is_subcommand(arg: &str) -> bool {
    matches!(
        arg,
        "scan"
            | "caches"
            | "rules"
            | "clean"
            | "logs"
            | "help"
            | "--help"
            | "-h"
            | "version"
            | "--version"
            | "-V"
    )
}

/// Entry point: parse `std::env::args` and run, writing to real stdout.
pub fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout().lock();
    run_args(&args, &mut out)
}

/// The testable core: dispatch `args` (subcommand first) and write to `out`.
pub(crate) fn run_args(args: &[String], out: &mut dyn Write) -> ExitCode {
    let (cmd, rest) = args
        .split_first()
        .map(|(c, r)| (c.as_str(), r))
        .unwrap_or(("help", &[]));
    match cmd {
        "scan" => cmd_scan(rest, out),
        "caches" => cmd_caches(rest, out),
        "rules" => cmd_rules(rest, out),
        "clean" => cmd_clean(rest, out),
        "logs" => cmd_logs(out),
        "version" | "--version" | "-V" => {
            let _ = writeln!(out, "prun {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "help" | "--help" | "-h" => {
            print_help(out);
            ExitCode::SUCCESS
        }
        other => {
            let _ = writeln!(out, "unknown command: {other}\n");
            print_help(out);
            ExitCode::FAILURE
        }
    }
}

fn cmd_scan(args: &[String], out: &mut dyn Write) -> ExitCode {
    let json = has_flag(args, "--json");
    let all = has_flag(args, "--all"); // include git-tracked dirs too
    let min_age = flag_value(args, "--min-age").and_then(|v| v.parse::<u64>().ok());
    let root = positionals(args)
        .first()
        .copied()
        .unwrap_or(".")
        .to_string();

    let opts = ScanOptions {
        root,
        min_age_days: min_age,
        skip_git_tracked: !all,
        respect_prunignore: true,
    };
    let collected = collect_scan(|emit| {
        let cancel = AtomicBool::new(false);
        scan(&opts, &cancel, emit)
    });
    finish_scan(out, "scan", collected, json)
}

fn cmd_caches(args: &[String], out: &mut dyn Write) -> ExitCode {
    let json = has_flag(args, "--json");
    let collected = collect_scan(|emit| {
        let cancel = AtomicBool::new(false);
        scan_caches(&cancel, emit)
    });
    finish_scan(out, "caches", collected, json)
}

/// Everything a buffered (non-streaming) CLI scan needs from the event stream.
struct Collected {
    locations: Vec<Location>,
    errors: u64,
    error_samples: Vec<String>,
    result: Result<(), String>,
}

/// Run a streaming scan, collecting its located paths + error reporting.
fn collect_scan(run: impl FnOnce(&(dyn Fn(ScanEvent) + Sync)) -> Result<(), String>) -> Collected {
    let found: Mutex<Vec<Location>> = Mutex::new(Vec::new());
    let errors = AtomicU64::new(0);
    let samples: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let result = run(&|ev| match ev {
        ScanEvent::Located { location, .. } => found.lock().unwrap().push(location),
        ScanEvent::Done {
            errors: e,
            error_samples,
            ..
        } => {
            errors.store(e, Ordering::Relaxed);
            *samples.lock().unwrap() = error_samples;
        }
        _ => {}
    });
    Collected {
        locations: found.into_inner().unwrap(),
        errors: errors.load(Ordering::Relaxed),
        error_samples: samples.into_inner().unwrap(),
        result,
    }
}

fn finish_scan(out: &mut dyn Write, what: &str, collected: Collected, json: bool) -> ExitCode {
    let Collected {
        mut locations,
        errors,
        error_samples,
        result,
    } = collected;
    if let Err(e) = result {
        let _ = writeln!(out, "error: {e}");
        return ExitCode::FAILURE;
    }
    locations.sort_by(|a, b| b.size.cmp(&a.size));

    if json {
        let payload = serde_json::json!({
            "kind": what,
            "errors": errors,
            "error_samples": error_samples,
            "locations": locations,
        });
        let _ = writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }

    let total: u64 = locations.iter().map(|l| l.size).sum();
    for l in &locations {
        let _ = writeln!(
            out,
            "  {:>9}  {:<8}  {}",
            human_size(l.size),
            l.category,
            l.path
        );
    }
    let mut summary = format!(
        "total: {} across {} location{}",
        human_size(total),
        locations.len(),
        if locations.len() == 1 { "" } else { "s" }
    );
    if errors > 0 {
        summary.push_str(&format!(" ({errors} unreadable)"));
    }
    let _ = writeln!(out, "{summary}");
    for sample in &error_samples {
        let _ = writeln!(out, "  unreadable: {sample}");
    }
    ExitCode::SUCCESS
}

fn cmd_rules(args: &[String], out: &mut dyn Write) -> ExitCode {
    let status = rules_status();
    if has_flag(args, "--json") {
        let _ = writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&status).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    let source = if status.error.is_some() {
        "override has an error — using built-in defaults"
    } else if status.using_override {
        "using your override"
    } else {
        "using built-in defaults"
    };
    let _ = writeln!(out, "rules file: {}", status.override_path);
    let _ = writeln!(out, "status:     {source}");
    let _ = writeln!(
        out,
        "active:     {} rules, {} junk, {} caches",
        status.rule_count, status.junk_count, status.cache_count
    );
    ExitCode::SUCCESS
}

fn cmd_clean(args: &[String], out: &mut dyn Write) -> ExitCode {
    let permanent = has_flag(args, "--delete"); // default is recoverable Trash
    let paths: Vec<String> = positionals(args).iter().map(|s| s.to_string()).collect();
    if paths.is_empty() {
        let _ = writeln!(out, "error: `clean` needs at least one path");
        return ExitCode::FAILURE;
    }
    // `clean` streams sequentially over `&mut dyn FnMut`, so the closure can bump
    // plain locals and buffer lines directly; flush them after.
    let mut lines: Vec<String> = Vec::new();
    let mut removed = 0u64;
    let mut failed = 0u64;
    clean(&paths, !permanent, &mut |ev| match ev {
        CleanEvent::Removed { path, .. } => {
            removed += 1;
            lines.push(format!("removed  {path}"));
        }
        CleanEvent::Failed { path, error, .. } => {
            failed += 1;
            lines.push(format!("failed   {path}: {error}"));
        }
        _ => {}
    });
    for line in lines {
        let _ = writeln!(out, "{line}");
    }
    let verb = if permanent { "deleted" } else { "trashed" };
    let _ = writeln!(out, "{verb} {removed}, {failed} failed");
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Print where the log files and crash reports live, so a bug report can say
/// "run `prun logs` and attach what's there".
fn cmd_logs(out: &mut dyn Write) -> ExitCode {
    match crate::diagnostics::log_dir() {
        Some(dir) => {
            let _ = writeln!(out, "{}", dir.display());
            ExitCode::SUCCESS
        }
        None => {
            let _ = writeln!(out, "error: no data directory on this system");
            ExitCode::FAILURE
        }
    }
}

fn print_help(out: &mut dyn Write) {
    let _ = write!(
        out,
        "prun {ver} — project artifact cleaner (headless CLI)

USAGE:
  prun scan [PATH] [--all] [--min-age DAYS] [--json]
                       list reclaimable build artifacts under PATH (default: .)
                       --all      include git-tracked dirs (default: ignored only)
                       --min-age  only dirs untouched for >= DAYS
  prun caches [--json] list per-user system caches (Cargo, npm, Gradle, …)
  prun rules  [--json] show the active ruleset status
  prun clean PATH...   move PATHs to the Trash (recoverable)
                       --delete   remove permanently instead
  prun logs            print the log / crash-report directory
  prun version
  prun help

With no subcommand the desktop app launches instead.
",
        ver = env!("CARGO_PKG_VERSION")
    );
}

// ── tiny arg helpers (no external parser) ──────────────────────────────────────

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn positionals(args: &[String]) -> Vec<&str> {
    args.iter()
        .filter(|a| !a.starts_with('-'))
        .map(String::as_str)
        .collect()
}

/// Human-readable size in SI units, matching the GUI's `fmtSize`.
fn human_size(bytes: u64) -> String {
    const KB: u64 = 1_000;
    const MB: u64 = 1_000_000;
    const GB: u64 = 1_000_000_000;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsupport::{fresh_tmp, touch};

    fn run(args: &[&str]) -> (String, ExitCode) {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mut buf: Vec<u8> = Vec::new();
        let code = run_args(&owned, &mut buf);
        (String::from_utf8(buf).unwrap(), code)
    }

    #[test]
    fn help_and_version() {
        let (help, _) = run(&["help"]);
        assert!(help.contains("USAGE"), "help lists usage; got {help}");
        let (ver, _) = run(&["version"]);
        assert!(ver.starts_with("prun "), "version line; got {ver}");
    }

    #[test]
    fn unknown_command_fails() {
        let (out, code) = run(&["frobnicate"]);
        assert!(out.contains("unknown command"));
        // ExitCode has no Eq; format it to compare.
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    #[test]
    fn scan_lists_artifacts_as_text_and_json() {
        let root = fresh_tmp("cli_scan");
        let proj = root.join("rustproj");
        touch(&proj.join("Cargo.toml"));
        touch(&proj.join("target").join("blob"));

        let path = root.to_string_lossy().to_string();
        let (text, _) = run(&["scan", &path, "--all"]);
        // separator-agnostic: the printed path uses the OS separator
        assert!(
            text.contains("target"),
            "text scan lists target; got {text}"
        );
        assert!(text.contains("rust"), "with its ecosystem; got {text}");
        assert!(text.contains("total:"), "and a total line; got {text}");

        let (json, _) = run(&["scan", &path, "--all", "--json"]);
        assert!(
            json.contains("\"locations\""),
            "json has locations; got {json}"
        );
        assert!(
            json.contains("\"category\": \"rust\""),
            "json carries category; got {json}"
        );
        assert!(
            json.contains("\"error_samples\""),
            "json carries error samples; got {json}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_requires_paths() {
        let (out, code) = run(&["clean"]);
        assert!(out.contains("needs at least one path"));
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    #[test]
    fn rules_reports_status() {
        let (out, _) = run(&["rules"]);
        assert!(out.contains("rules file:"), "got {out}");
        assert!(out.contains("active:"), "got {out}");
    }

    #[test]
    fn logs_prints_the_log_dir() {
        let (out, code) = run(&["logs"]);
        // Every CI/dev platform we run on has a data dir, so expect the path.
        assert!(out.trim().ends_with("logs"), "prints the dir; got {out}");
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    }
}
