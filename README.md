# Prun — project artifact cleaner

A small Tauri v2 desktop app that scans a projects root, finds build-artifact
directories (`target`, `node_modules`, `.venv`, `.gradle`, `vendor`, …),
groups them by ecosystem, and lets you reclaim the space.

The UI is a faithful rebuild of the reference mock: custom titlebar, path bar
with rescan, three filter pills, a category sidebar, a sorted list of the
largest reclaimable locations, and a footer with the running total.

## Stack

- **Frontend** — Vanilla TypeScript + Vite. No framework; `src/main.ts` for the
  UI, with pure logic split into `src/format.ts` / `src/grouping.ts` (unit-tested
  with Vitest) plus `src/rules-editor.ts` and `src/styles.css`.
- **Backend** — Rust (`src-tauri/`). Real disk scanner, not a mock, split into
  focused modules (`scan/`, `clean`, `rules/`, `fs_util`, `commands`, `cli`).

## Run

Prereqs: Node 18+, the Rust toolchain, and the
[Tauri system dependencies](https://tauri.app/start/prerequisites/) for your
platform (on your Gentoo box: `webkit2gtk-4.1`, `libsoup-3`, plus the usual
`base-devel`).

```bash
npm install
npm run tauri dev      # hot-reloading dev build
npm run tauri build    # bundled release binary + installer
```

Generate icons once before `build`:

```bash
npm run tauri icon path/to/1024.png
```

You can also preview the UI in a plain browser — `npm run dev` then open the
Vite URL. Without the Tauri shell it falls back to sample data (the same set
shown in the mock) so layout and interactions stay explorable.

## Command-line use

The same scan/clean/rules core is exposed as a headless CLI (handy for scripting
or CI cache cleanup). Any subcommand runs the CLI; with no arguments the desktop
app launches instead.

```bash
prun scan [PATH] [--all] [--min-age DAYS] [--json]   # list reclaimable artifacts
prun caches [--json]                                 # per-user system caches
prun rules [--json]                                  # active ruleset status
prun clean PATH... [--delete]                        # Trash (or permanently delete)
prun logs                                            # print the log/crash-report dir
```

(During development: `cargo run --manifest-path src-tauri/Cargo.toml -- scan ~/Projects`.
On Windows *release* builds the binary is GUI-subsystem, so CLI output won't
attach to a console there — use a dev build for piped output.)

## How the scanner works (`src-tauri/src/scan/`)

1. Walks the root with `walkdir`, skipping `.git` and never descending into a
   matched artifact dir.
2. Classifies a directory by name against a rule table. Ambiguous names
   (`target`, `build`, `vendor`) require a sibling marker file
   (`Cargo.toml`, `build.gradle*`, `composer.json`) so it won't nuke an
   unrelated `build/` or `vendor/`.
3. Sizes each match, records its mtime as `age_secs`, and asks libgit2
   whether the path is git-ignored.
4. Rolls everything up into category totals, sorted largest-first.

### Filter semantics

- **Untouched > N days** — drops dirs whose mtime is newer than the cutoff.
  Applied live in the UI (no rescan needed); also enforced backend-side when
  `minAgeDays` is set.
- **Skip git-tracked** — keeps only directories git *ignores*. If a build dir
  isn't ignored it may be tracked, so it's left alone. This is the safe
  reading of the toggle.
- **Respect .prunignore** — if a `.prunignore` (gitignore syntax) exists at
  the root, matching paths are excluded from results.

### Cleaning

`clean(paths, to_trash)` either moves each directory to the system Trash
(`trash` crate, recoverable — the default) or `remove_dir_all`s it permanently
when the footer checkbox is unticked.

## Notes / tradeoffs

- `age_secs` uses the newest mtime found while sizing the tree (one walk), so
  "untouched for N days" reflects the most recent file change rather than the top
  directory's often-stale mtime.
- Sizing runs in parallel across locations (`rayon`); each tree is measured in a
  single walk that also yields the newest mtime and a count of unreadable entries.
  Reported sizes are *apparent* (sum of file lengths, hard links counted once on
  Unix), not on-disk allocation — close enough for "how much will I get back".
- `git2` is pulled with `default-features = false`; it'll vendor libgit2. If
  you'd rather link the system libgit2, enable the `vendored-libgit2` feature
  accordingly.

## Development

```bash
cargo test --manifest-path src-tauri/Cargo.toml   # backend unit tests
npm test                                          # frontend (Vitest)
npm run build                                     # tsc --noEmit + vite build
npm run lint && npm run format:check              # eslint + prettier
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

CI (`.github/workflows/ci.yml`) runs all of the above on Linux, Windows, and
macOS, plus `cargo fmt --check`, a vitest coverage report, and an advisory
`npm`/`cargo audit`. See `ARCHITECTURE.md` for the module map and invariants,
`CONTRIBUTING.md` for the PR bar, `CHANGELOG.md` for release history, and
`RELEASING.md` for the signing/auto-update release process.
