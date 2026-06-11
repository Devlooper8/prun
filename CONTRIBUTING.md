# Contributing to Prun

Small project, small rules. Issues and PRs welcome.

## Dev setup

- Node 18+
- Rust stable (via rustup)
- The [Tauri system dependencies](https://tauri.app/start/prerequisites/) for
  your platform (webkit2gtk-4.1, libsoup-3, … on Linux; just the usual build
  tools on Windows/macOS)

```bash
npm install
npm run tauri dev    # desktop app, hot reload
npm run dev          # UI only in a plain browser, with sample data
```

## Before you open a PR

All four must be green locally — CI runs exactly these:

```bash
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
npm run build && npm test
```

If your change touches interactive UI, say so in the PR: tests and builds
can't click buttons, so flag what needs a manual click-through
(see `tasks/lessons.md`).

## Commits

Conventional Commits, matching the existing log:

```
feat(scan): honest sizing, read-error reporting, and scan cancellation
fix(clean): refuse paths a scan never surfaced
ci: add GitHub Actions pipeline + Dependabot
```

Types in use: `feat`, `fix`, `ci`, `docs`, `refactor`, `test`. Add a scope
(`scan`, `clean`, `rules`, `cli`, `dist`, …) when it helps.

## Pull requests

- Small, focused diffs — one concern per PR.
- Behavior changes come with tests (Rust unit tests or Vitest).
- No drive-by reformatting; `cargo fmt` and the existing style settle it.

## Code philosophy

Simplicity first. No frontend framework, no traits with a single
implementation, no abstraction "for later". The core is pure functions over
data with thin adapters on top — the Tauri commands and the CLI both wrap the
same core. Junior-readable code is a hard requirement: if it needs a diagram
to follow, simplify it.

Detection rules are **data, not code**: new artifact directories, marker
files, anti-markers, and categories belong in `src-tauri/prun-rules.toml`, not
in the matcher. If your feature can be a rules entry, make it one.
