// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    // A recognized subcommand (scan/caches/rules/clean/help/version) runs the
    // headless CLI; anything else launches the desktop app. Checking against the
    // known set means a stray OS-passed argument can't divert a normal GUI launch.
    if let Some(first) = std::env::args().nth(1) {
        if prun_lib::cli::is_subcommand(&first) {
            return prun_lib::cli::run();
        }
    }
    prun_lib::run();
    ExitCode::SUCCESS
}
