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
#[must_use]
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
#[must_use]
pub fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout().lock();
    run_args(&args, &mut out)
}

/// The testable core: dispatch `args` (subcommand first) and write to `out`.
pub(crate) fn run_args(args: &[String], out: &mut dyn Write) -> ExitCode {
    let (cmd, rest) = args
        .split_first()
        .map_or(("help", &[] as &[String]), |(c, r)| (c.as_str(), r));
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
        ScanEvent::Located { location, .. } => found
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(location),
        ScanEvent::Done {
            errors: e,
            error_samples,
            ..
        } => {
            errors.store(e, Ordering::Relaxed);
            *samples
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = error_samples;
        }
        _ => {}
    });
    Collected {
        locations: found
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        errors: errors.load(Ordering::Relaxed),
        error_samples: samples
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
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
        use std::fmt::Write as _;
        let _ = write!(summary, " ({errors} unreadable)");
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
    let dry_run = has_flag(args, "--dry-run");
    let scan_root = flag_value(args, "--scan");

    // Targets come either from a scan (`--scan ROOT` — the safe one-shot that
    // makes "clean what's untouched > N days" a single cron line) or as explicit
    // positional paths.
    let paths: Vec<String> = if let Some(root) = scan_root {
        let opts = ScanOptions {
            root: root.to_string(),
            min_age_days: flag_value(args, "--min-age").and_then(|v| v.parse::<u64>().ok()),
            skip_git_tracked: !has_flag(args, "--all"),
            respect_prunignore: true,
        };
        let collected = collect_scan(|emit| {
            let cancel = AtomicBool::new(false);
            scan(&opts, &cancel, emit)
        });
        if let Err(e) = collected.result {
            let _ = writeln!(out, "error: {e}");
            return ExitCode::FAILURE;
        }
        let mut locs = collected.locations;
        locs.sort_by(|a, b| b.size.cmp(&a.size)); // largest-first, like the GUI
        locs.into_iter().map(|l| l.path).collect()
    } else {
        positionals(args).iter().map(|s| (*s).to_string()).collect()
    };

    if paths.is_empty() {
        let _ = writeln!(
            out,
            "error: `clean` needs at least one path (or --scan PATH)"
        );
        return ExitCode::FAILURE;
    }

    // Deleting what a scan turned up needs an explicit --yes; without it we fall
    // back to a dry run (which deletes nothing). Explicit positional paths are
    // taken at face value — you named them, so no confirmation gate.
    let needs_confirm = scan_root.is_some() && !has_flag(args, "--yes");
    if dry_run || needs_confirm {
        for p in &paths {
            let _ = writeln!(out, "would remove  {p}");
        }
        let verb = if permanent { "delete" } else { "trash" };
        let n = paths.len();
        let mut line = format!(
            "dry run: would {verb} {n} path{}",
            if n == 1 { "" } else { "s" }
        );
        if needs_confirm && !dry_run {
            line.push_str(" — re-run with --yes to do it");
        }
        let _ = writeln!(out, "{line}");
        return ExitCode::SUCCESS;
    }

    // `clean` streams sequentially over `&mut dyn FnMut`, so the closure can bump
    // plain locals and write each line straight to `out` — its borrows release when
    // the call returns, freeing `out` and the tallies for the summary below.
    let mut removed = 0u64;
    let mut failed = 0u64;
    clean(&paths, !permanent, &mut |ev| match ev {
        CleanEvent::Removed { path, .. } => {
            removed += 1;
            let _ = writeln!(out, "removed  {path}");
        }
        CleanEvent::Failed { path, error, .. } => {
            failed += 1;
            let _ = writeln!(out, "failed   {path}: {error}");
        }
        _ => {}
    });
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
    if let Some(dir) = crate::diagnostics::log_dir() {
        let _ = writeln!(out, "{}", dir.display());
        ExitCode::SUCCESS
    } else {
        let _ = writeln!(out, "error: no data directory on this system");
        ExitCode::FAILURE
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
  prun clean [PATH...] move PATHs to the Trash (recoverable)
                       --scan ROOT  clean what a scan of ROOT finds (needs --yes)
                       --min-age N  with --scan: only dirs untouched >= N days
                       --all        with --scan: include git-tracked dirs too
                       --delete     remove permanently instead of trashing
                       --dry-run    print what would be removed, delete nothing
                       --yes        confirm a --scan clean (else it's a dry run)
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
        format!("{}.{} GB", bytes / GB, (bytes % GB) * 10 / GB)
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
        let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
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
    fn clean_dry_run_on_explicit_path_deletes_nothing() {
        let root = fresh_tmp("cli_clean_dry");
        let blob = root.join("node_modules");
        touch(&blob.join("x.js"));

        let p = blob.to_string_lossy().to_string();
        let (out, _) = run(&["clean", &p, "--dry-run"]);

        let still_there = blob.exists();
        let _ = std::fs::remove_dir_all(&root);
        assert!(out.contains("would remove"), "previews the path; got {out}");
        assert!(still_there, "--dry-run must delete nothing");
    }

    #[test]
    fn clean_scan_without_yes_is_a_dry_run() {
        let root = fresh_tmp("cli_clean_scan");
        let proj = root.join("rustproj");
        touch(&proj.join("Cargo.toml"));
        let target = proj.join("target");
        touch(&target.join("blob"));

        let path = root.to_string_lossy().to_string();
        let (out, code) = run(&["clean", "--scan", &path, "--all"]);

        let still_there = target.exists();
        let _ = std::fs::remove_dir_all(&root);
        assert!(out.contains("would remove"), "previews paths; got {out}");
        assert!(
            out.contains("--yes"),
            "hints how to do it for real; got {out}"
        );
        assert!(still_there, "a scan clean without --yes deletes nothing");
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    }

    #[test]
    fn clean_scan_with_yes_deletes_only_the_artifact() {
        let root = fresh_tmp("cli_clean_scan_yes");
        let proj = root.join("rustproj");
        touch(&proj.join("Cargo.toml"));
        let target = proj.join("target");
        touch(&target.join("blob"));

        let path = root.to_string_lossy().to_string();
        let (out, code) = run(&["clean", "--scan", &path, "--all", "--delete", "--yes"]);

        let target_gone = !target.exists();
        let marker_kept = proj.join("Cargo.toml").exists();
        let _ = std::fs::remove_dir_all(&root);
        assert!(out.contains("removed"), "reports the removal; got {out}");
        assert!(target_gone, "--yes deletes the scanned artifact");
        assert!(
            marker_kept,
            "only the artifact goes, not the project marker"
        );
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
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
