# Prun — project artifact cleaner

A small Tauri v2 desktop app that scans a projects root, finds build-artifact
directories (`target`, `node_modules`, `.venv`, `.gradle`, `vendor`, …),
groups them by ecosystem, and lets you reclaim the space.

The UI is a faithful rebuild of the reference mock: custom titlebar, path bar
with rescan, three filter pills, a category sidebar, a sorted list of the
largest reclaimable locations, and a footer with the running total.

## Stack

- **Frontend** — Vanilla TypeScript + Vite. No framework; the whole UI is
  ~250 lines in `src/main.ts` plus `src/styles.css`.
- **Backend** — Rust (`src-tauri/`). Real disk scanner, not a mock.

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

## How the scanner works (`src-tauri/src/scanner.rs`)

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

- `age_secs` uses the directory's own mtime, not a deep max over its contents —
  cheap and good enough for "stale build dir" detection. Swap to a recursive
  max if you want stricter "untouched" semantics.
- Size walking is sequential. For very large trees, parallelising `dir_size`
  with `rayon` or switching to `jwalk` is the obvious win.
- `git2` is pulled with `default-features = false`; it'll vendor libgit2. If
  you'd rather link the system libgit2, enable the `vendored-libgit2` feature
  accordingly.

The frontend was typechecked and built clean in CI; the Rust crate compiles
with the standard Tauri toolchain (it wasn't linked in the authoring sandbox,
which lacked the webkit system libs).
