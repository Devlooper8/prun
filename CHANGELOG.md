# Changelog

All notable changes to Prun are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Crash reports** — a panic hook writes the panic message, location, and
  backtrace to `crash-<time>.txt` in the log dir (release builds abort with
  stripped symbols, so this file is the one diagnostic that survives). A
  **Logs** button on the nav rail opens the folder; `prun logs` prints it.
- **Concrete read-error examples** — "N items unreadable" now carries up to 5
  "path: reason" samples: appended to the scan toast, listed by the CLI, in
  `--json` as `error_samples`, and warn-logged for correlation.
- **Backend scan serialization** — a second scan invoked while one runs is
  refused at the backend (the per-window UI guard wasn't enough for the
  app-global `Reclaimable`/`Cancel` state).
- **Junction-safety tests** (Windows): the discovery walker, the sizing walk,
  and `clean` are pinned to treat NTFS junctions as non-traversable links —
  nothing behind a junction is offered, sized, or deleted.
- **Wire-contract fixtures** — shared JSON fixtures are pinned by a Rust serde
  test and a typed Vitest test, so a DTO rename breaks a test instead of
  silently `undefined`-ing a frontend field.
- **Supply-chain scaffolding** — release builds attach build-provenance
  attestations (`gh attestation verify`) and a CycloneDX SBOM artifact; all
  GitHub Actions are pinned to commit SHAs; macOS joined CI (clippy + tests)
  and the Windows job now runs clippy.
- **Governance docs** — SECURITY.md (private disclosure, deletion-bug scope),
  CONTRIBUTING.md, CODEOWNERS, issue forms, a PR template, and an
  ARCHITECTURE.md recording the module shape and load-bearing invariants.
- **Frontend lint/format** — eslint (typescript-eslint) + prettier, gated in
  CI alongside a vitest coverage report.

### Changed
- **`backend.ts` is now the only frontend file that talks to Tauri** — every
  invoke/Channel sits behind a typed function; sample preview data moved to
  `sample-data.ts`; the age filter re-render is debounced while typing.
- The rules editor's status line reports a status-read failure instead of
  going silently blank.

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
