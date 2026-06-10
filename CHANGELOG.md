# Changelog

All notable changes to Prun are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-06-11

### Security
- **Fixed an XSS-to-delete hole.** The category label in the sidebar was
  interpolated into `innerHTML` unescaped; a ruleset-controlled ecosystem string
  could run script in the webview and, through the IPC bridge, invoke `clean` on
  arbitrary paths. The label is now escaped, ids/ecosystems are charset-validated
  (`[a-z0-9_-]`), and a real Content-Security-Policy replaces `csp: null`.
- **`clean` is now confined to scanned paths.** A new `Reclaimable` guard records
  what each scan offered; `clean` refuses any path a scan never surfaced, so even
  a compromised webview can't ask to delete something off-list.

### Added
- **Headless CLI** (`prun scan|caches|rules|clean`, text or `--json`) over the
  same core the app uses — for scripting and as a GUI-free test surface.
- **Scan cancellation** — a Cancel button in the progress strip stops an
  in-flight scan promptly.
- **Read-error reporting** — sizes that hit unreadable entries surface a count
  ("N items unreadable") instead of silently under-reporting.
- **CI** (GitHub Actions: fmt, clippy, tests on Linux + Windows, frontend build +
  Vitest, advisory npm/cargo audit) and Dependabot.
- **Structured logging** via `tracing` to a daily-rotated file in the OS data dir
  (`PRUN_LOG` controls verbosity).
- **Vitest** unit tests for the frontend's pure logic.

### Changed
- **Honest "untouched" age** — the age gate now uses the newest mtime found while
  sizing the tree, not the top directory's often-stale mtime.
- **Hard-link-aware sizes** on Unix (a file reached by several links is counted
  once); apparent-vs-on-disk semantics documented.
- **Frontend split** — pure formatting/grouping logic extracted from `main.ts`
  into `format.ts` / `grouping.ts`.
- Added an MIT `LICENSE` and license metadata.

## [0.1.0]

Initial Tauri desktop app: parallel streaming scanner driven by an embedded,
user-overridable `prun-rules.toml`; project-grouped reclaimable list; streaming
trash/delete; system-caches view; in-app rules editor.

[Unreleased]: https://github.com/your-org/prun/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/your-org/prun/releases/tag/v0.2.0
[0.1.0]: https://github.com/your-org/prun/releases/tag/v0.1.0
