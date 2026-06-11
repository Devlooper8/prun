# Prun architecture

One page: how the pieces fit, where the boundaries are, and the invariants the
code depends on but can't fully express. Read this before changing the scanner
or the IPC layer.

## Shape

```
                    ┌───────────────────────────────────┐
   GUI (webview)    │ commands.rs — thin IPC adapters   │    CLI (headless)
   src/main.ts ────▶│  + Reclaimable confinement        │◀── cli.rs
   src/rules-editor │  + Cancel flag, spawn_blocking    │    (text / --json)
                    └─────────────────┬─────────────────┘
                                      ▼
                    ┌───────────────────────────────────┐
                    │ Pure core — no Tauri imports      │
                    │  scan/{project,caches}  clean.rs  │
                    │  rules/{model,matcher,store,labels}│
                    │  fs_util.rs   diagnostics.rs      │
                    └───────────────────────────────────┘
```

Dependency direction is acyclic: `commands`/`cli` → `scan`/`clean`/`rules` →
`fs_util`. The core never knows which front-end is driving it; both reach it
through plain functions and an `emit: &(dyn Fn(Event) + Sync)` closure. That
closure **is** our dependency inversion — we deliberately use no
single-implementation traits (see CONTRIBUTING.md).

Detection is data, not code: `prun-rules.toml` (embedded at compile time)
defines rules; a user override at `%APPDATA%\prun\rules.toml` /
`~/.config/prun/rules.toml` is **layered over the embedded base by id** —
override entries win per-id, new built-ins flow through, deletions are
tombstoned in `[removed]`. The override stores only the delta.

## A project scan, in four phases (`scan/project.rs`)

1. **Parallel discovery** — `ignore::WalkBuilder` (capped threads) classifies
   every directory against the compiled `Matcher`: exact dir names need a
   sibling marker file (`target` + `Cargo.toml`), `reclaim_root` rules claim a
   dir by a marker *inside* it (vetoed by `anti_markers`), glob-bearing rules
   record project roots for phase 2.
2. **Glob walk** — per discovered root, a rayon subtree walk resolves the
   recursive globs, pruning already-claimed dirs.
3. **Sequential filtering** — dedup by precedence, subsume nested candidates,
   then `.prunignore` / git-ignore / skip-git-tracked checks.
4. **Parallel sizing** — one `measure_tree` walk per candidate yields size,
   newest mtime, and read errors; each kept candidate streams out as
   `ScanEvent::Located`; the run ends with `Done { categories, errors,
   error_samples }`.

`clean(paths, to_trash, emit)` is sequential and per-path: trash (default) or
permanent delete, each success/failure streamed so the UI drops rows live and
keeps failed ones marked for retry.

## Security model

The webview is untrusted (it renders strings that came from disk). Two fences:

- **CSP** in `tauri.conf.json` plus `esc()` for every interpolation into
  `innerHTML`.
- **`Reclaimable`** (`commands.rs`): `clean` refuses any path the current
  scan did not offer. The check lives at the IPC edge — never inside the pure
  `clean()` — so the CLI and tests reuse the core without inheriting the
  webview's threat model. Keep it that way.

## Load-bearing invariants

Things the compiler can't enforce; each has an inline comment at its site, but
they're easy to break from a distance:

1. **Discovery `Sink` mutexes are push-only during the walk** and drained once
   after `run()` returns. Holding a lock across walk callbacks would serialize
   (or deadlock) the parallel walk.
2. **Phase 3 (git checks) must stay single-threaded** — `git2::Repository` is
   `!Send`; the repo cache lives on one thread by design.
3. **The min-age gate runs after sizing, not before** — honest "untouched for
   N days" needs the *newest file* mtime, which only the sizing walk knows.
   Moving it earlier "as an optimization" silently breaks age semantics.
4. **Rule precedence = TOML order** — a contested path goes to the lowest rule
   index (junk rules rank after all `[[rule]]` entries). Reordering the TOML
   is a behavior change.
5. **Path-segment matching is case-normalized per platform** (`norm_seg`:
   lowercase on Windows/macOS, identity on Linux). Hardcoding `.to_lowercase()`
   anywhere else breaks Linux; comparing raw breaks Windows.
6. **The discovery walker keeps `standard_filters(false)` + `hidden(false)`** —
   the artifacts we hunt are usually git-ignored dotdirs; default filters would
   hide exactly them.
7. **Frontend: behavior binds to data-attributes, not style classes** —
   handlers target `.wbtn[data-win]`, `.nav__item[data-view]`,
   `.filters .pill`. Reusing a styled class on a new element must not acquire
   its behavior (this bug shipped once; see `tasks/lessons.md`).
8. **Frontend state quirks**: an empty `state.catsOn` set means "all categories
   on" (the first manual toggle materializes it); `state.expanded` is keyed by
   project *name*. `reconcileSelection()` must run after anything that changes
   which rows are visible.
9. **Crash reporting relies on synchronous writes** (`diagnostics.rs`) —
   release builds `panic = "abort"`, so the async tracing buffer is lost on a
   crash; only the panic hook's direct `std::fs` write survives. Don't route
   the crash report through `tracing`.

## Testing philosophy

Tests run against the **real filesystem** (temp dirs via `testsupport`), not
mocks — locked files, junctions, and case-insensitivity are this app's actual
risk surface, and a mock can't reproduce them. Keep new tests in the same
style: build a small real tree, run the real function, assert on the streamed
events. The CLI doubles as a GUI-free integration surface.

Wire DTOs are mirrored by hand in `src/types.ts`; serde shape tests (e.g.
`done_event_wire_shape_is_stable`) pin the field names so a Rust rename fails
a test instead of silently `undefined`-ing a TS field.
