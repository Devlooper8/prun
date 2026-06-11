# Todo — Parallel scan + live progress

## Goal
Scanning felt frozen (silent until everything popped in at once) and ran
single-threaded. Make it **fast** (parallel) and **honest** (live progress bar +
current directory + streaming rows).

## Plan / progress
- [x] Add `rayon` dep; derive `Clone` on `Location`/`Category`; add `ScanEvent` enum (scanner.rs)
- [x] Rewrite `scanner::scan` as a parallel two-phase streaming walk
  - [x] Phase 1: `ignore::WalkParallel` discovery (standard_filters(false) + hidden(false) so git-ignored/dotdir artifacts are still found), `.git` skip, heartbeat events
  - [x] Phase 1.5: sequential size-independent filtering (prunignore / git-ignore / age)
  - [x] Phase 2: `rayon` parallel sizing, one `Located{done,total}` per dir
- [x] Wire `tauri::ipc::Channel<ScanEvent>` into the `scan` command (lib.rs)
- [x] Add `ScanEvent` TS union + `CATEGORY_LABELS` (types.ts); progress strip markup (index.html) + styles (styles.css)
- [x] Make `main.ts` event-driven: `runScan` over a Channel (+ browser `simulateScan`), live bar/path, streamed rows, rAF-coalesced render, re-entry guard
- [x] Verify: `cargo check` + `cargo build` (exit 0, no warnings), `tsc --noEmit` (exit 0), dev server boots + transforms cleanly

## Review
- **Approach:** Two phases. A multi-threaded `ignore::WalkParallel` discovery pass
  finds artifact dirs (emitting heartbeat "scanning N dirs · <path>" ticks), then
  a `rayon` pass sizes them in parallel, streaming one event per finished dir.
  Everything is delivered to the UI over a Tauri `Channel<ScanEvent>`.
- **UX:** Indeterminate sweep during discovery → determinate bar (`done/total`)
  during sizing; rows stream in (biggest first), category totals tick up live.
  Bar uses a running max so out-of-order parallel events never make it go backwards.
- **io_uring:** Evaluated and rejected (user-confirmed). Windows IoRing has no
  readdir/stat opcode; Linux io_uring lacks `getdents`. Thread-parallel (the
  approach ripgrep/fd/dua ship) is the right cross-platform choice; `ignore` was
  already a dependency.
- **Correctness safeguard:** `standard_filters(false)` is mandatory — the default
  walker would skip git-ignored `node_modules`/`target` and dotdir artifacts.
- **Files:** `src-tauri/{Cargo.toml, src/scanner.rs, src/lib.rs}`,
  `src/{types.ts, main.ts, styles.css}`, `index.html`.
- **Pre-existing build issue (fixed):** `npm run build` / `tauri build` were
  failing with "Cannot find package 'esbuild'" (rolldown-vite v8 needs esbuild
  separately). Fixed by `npm i -D esbuild`; `npm run build` now passes (exit 0).

---

# Todo — Group reclaimable locations by project

## Goal
The flat list showed each artifact's immediate parent as the "project", so
`prun/src-tauri/target` displayed as **src-tauri** — and `src-tauri` recurs across
every Tauri repo. Group locations under the real top-level project, shown as a
collapsible row: project name + total reclaimable size + arrow → child locations.

## Plan / progress
- [x] Grouping helpers + `state.expanded` (main.ts): key = **first path segment under the scan root** (`prun`, not `src-tauri`); `subPathOf` for child labels; `groupByProject`; `distinctCategories`; `esc()`
- [x] Render project groups (main.ts): header = arrow + tri-state checkbox + name + category dots + count + total size; indented child rows; in-place expand/collapse + selection sync (no full re-render → scroll preserved)
- [x] Styles (styles.css): `.group*`, `.loc--child` indent, `.loc__sub`/`.loc__leaf`, custom `:indeterminate` "mixed" checkbox dash
- [x] Verify: `tsc --noEmit` (0), `npm run build` (0), **8/8 pure-logic assertions** incl. the exact `D:\Projects\prun\src-tauri\target → "prun"` case

## Review
- **Grouping key** = first folder under the scan root. This is what makes the
  user's case work: marker-based grouping would have picked `src-tauri` (the dir
  holding Cargo.toml) — exactly the wrong answer. First-segment rolls every
  artifact anywhere beneath `…/prun/` up to **prun**.
- **Selection:** project checkbox = select/deselect all its locations, with a
  three-state (checked / mixed-dash / empty) box driven by child selection.
- **Caveat:** when the scan root *is* a single project, the top-level segments are
  that project's subfolders (e.g. `src-tauri`) — grouping is less meaningful then,
  which is expected. The feature targets scanning a directory of many projects.
- **Pure frontend change** — backend untouched; uses existing `path` + `root`.
- **Files:** `src/main.ts`, `src/styles.css`.

---

# Todo — Drive scanning from prun-rules.toml + classic blue progress bar

## Goal
The scanner ignored the hand-authored `prun-rules.toml` and used ~13 hardcoded
rules over a closed 5-variant `CategoryId` enum. Load the real ruleset (~65
rules, ~26 ecosystems, junk, global caches) and honour its *root-first* model.
Also: the progress bar streamed folder paths; show only the **scan root** with a
**classic solid-blue bar** under it.

## Plan / progress
- [x] Copy the TOML to `src-tauri/prun-rules.toml` (for `include_str!`); add `globset`, `toml` deps
- [x] `scanner.rs` rewrite: serde model (RuleFile/Defaults/Rule/Junk/GlobalCache), compiled `Matcher` with precompiled indexes, `OnceLock` loader (override `%APPDATA%\prun\rules.toml` → embedded)
- [x] **Hybrid** two-phase scan: phase 1 dir/junk/reclaim **name-first + marker-validated** (claims/skips only validated artifacts — no over-pruning of coincidentally-named dirs); phase 2 rayon subtree walk resolves recursive `globs`, pruning claimed dirs + matched glob-dir contents; dedup by precedence + ancestor subsumption; existing prunignore/git/age gauntlet preserved
- [x] `String` ecosystem categories + `ecosystem_label`; dropped `current` from `Discovering`; new `scan_caches()` (platform-aware, `~` expansion)
- [x] `lib.rs`: registered `scan_caches`; fixed `clean` permanent-delete for files/dir-symlinks (`remove_dir_all` failed on plain files) + dangling-symlink existence check
- [x] Frontend: `categoryColor`/`categoryLabel` (curated + HSL-hash fallback), progress strip = root line + solid-blue bar (no paths/counts), **System caches** view (button, `mode` flag, never auto-selected), `index.html`/`styles.css` markup
- [x] Verify: `cargo test` **14/14** (ported + glob marker / nested dirs / reclaim_root / junk dir+file / recursive globs / no-descend-into-claimed / global_ignore / disabled / precedence / embedded parses / realistic multi-ecosystem tree), `cargo build` 0 warnings, `tsc --noEmit` + `npm run build` 0 errors, `tauri dev` compiles + boots clean (exit 0, no panic)

## Review
- **Root-first re-architecture.** The TOML treats a dir as a project root when it
  holds a rule's marker; that rule's `dirs`/`globs` under it are candidates. Kept
  dir matching **name-first + marker-validated** (like the old code) so only real
  artifacts are skipped — the Plan agent's pure `prune_names` would have skipped a
  coincidentally-named `build/`/`out/` containing real sub-projects. Recursive
  globs get a dedicated phase-2 subtree walk that prunes claimed dirs and doesn't
  descend into matched glob dirs (so files inside `__pycache__` aren't re-listed).
- **One path = one `Location`.** No glob-grouping; bounded in practice because the
  walk skips *into* claimed dirs. `clean`/selection model untouched.
- **Ruleset sourcing.** Embedded via `include_str!` (ships working) with an
  optional `%APPDATA%\prun\rules.toml` override, parsed once in a `OnceLock`.
- **`clean` bug fixed (found by the Plan agent):** permanent delete used
  `remove_dir_all`, which fails on a plain file — fatal once globs match files.
- **Progress UX.** Root-only line + classic solid-blue bar (indeterminate marquee
  while discovering, determinate fill while sizing). No streamed paths or counts.
- **Files:** `src-tauri/{Cargo.toml, prun-rules.toml, src/scanner.rs, src/lib.rs}`,
  `src/{types.ts, main.ts, styles.css}`, `index.html`.
- **Known tradeoffs:** loose-file globs can yield many rows on pathological trees;
  `dirs=["build"]` also matches `Build/` on case-insensitive Windows (cosmetic);
  symlinks (`result`, `bazel-*`) size as ~0 B (we reclaim the link, never follow).

## Follow-up — Settings panel to expose the rules override
- [x] Backend: dropped the `OnceLock` — `load_matcher()` rebuilds per scan (parse+compile is sub-ms), so override edits apply on the **next scan** with no restart
- [x] `scanner.rs`: `rules_status()` + `RulesStatus` (override path, in-effect vs defaults vs parse-error, active counts) and `ensure_override_file()` (seeds the file with the full embedded ruleset, comments and all)
- [x] `lib.rs`: `rules_status` + `open_rules_file` commands (cfg-gated `os_open`: explorer/open/xdg-open), registered
- [x] Frontend: titlebar gear → Settings modal showing the override path, live status, and an "Open / create rules file" button; `types.ts` `RulesStatus`; Esc/backdrop close
- [x] Verify: `cargo build`/`test` 0 warnings, 14/14; `npm run build` 0 errors; `tauri dev` boots clean
- **Why:** the override file was invisible — users had no way to discover they
  could customize detection. The panel surfaces the path, current state, and a
  one-click open-or-create; per-scan reload makes edits actually take effect.

---

# Todo — In-app full Rules editor

## Goal
Let users edit the ruleset via a GUI instead of hand-editing 790 lines of TOML.
Full editor (every field of every rule / junk / system-cache + defaults,
add/delete), saving the complete ruleset to the override (full-copy model) with
Reset-to-defaults.

## Plan / progress
- [x] Backend: `RuleFile`/`Defaults`/`Rule`/`Junk`/`GlobalCache` made `Serialize`+`Clone`+`pub`, added `note`/`schema_version` (were dropped), skip-empty attrs; `load_rules`/`save_rules`(validate+atomic write)/`reset_rules` (+ `*_to`/`*_from` for hermetic tests); 6 new tests (round-trip, notes, save/load, validation, reset, matcher-unaffected)
- [x] lib.rs: registered `load_rules`/`save_rules`/`reset_rules`
- [x] Frontend: `types.ts` DTOs + `KNOWN_ECOSYSTEMS`; new `src/rules-editor.ts` (full-screen overlay, section tabs, editable model, chip string-lists, rule/junk/cache/defaults cards, Save/Reset/Cancel + dirty guard); `index.html` overlay + Settings "Edit rules in app" button; `styles.css` `.reditor*`
- [x] Verify: `cargo test` **20/20**, `cargo build` 0 warnings, `tsc`+`npm run build` 0 errors, `tauri dev` boots clean

## Review
- **TOML round-trip de-risked by reading the crate source** (Plan agent): `toml`
  0.8.2 serializes via `toml_edit`'s document tree — root scalars render before
  tables, so `to_string_pretty` round-trips with plain `#[derive(Serialize)]`; no
  new crates. `schema_version` declared first to stay ahead of `[defaults]`.
- **Two library-assumption traps caught:** `skip_serializing_if` also hides fields
  from the UI JSON → a `normalize()` pass restores them on load; `Matcher::compile`
  *silently drops* bad globs (not a validator) → `validate_rules` checks
  `markers`/`globs` with `GlobBuilder::build()` explicitly.
- **Full-copy override:** the editor loads the active set, edits, Save writes the
  whole thing (atomic temp+rename); per-scan reload applies it next scan; Reset
  deletes the override.
- **No dead controls:** cache cards omit the `enabled` toggle (`scan_caches`
  ignores it); value still round-trips.
- **Vanilla-TS focus rule:** text/boolean edits mutate the model and never
  re-render; only structural changes (add/delete entry/chip) touch the DOM.
- **Files:** `src-tauri/src/{scanner.rs, lib.rs}`, `src/{types.ts, rules-editor.ts,
  main.ts, styles.css}`, `index.html`.
- **Not headless-verifiable:** the interactive editor (rendering, chips, save flow)
  needs a real click-through — builds + type-checks + 20 backend tests + clean boot
  cover everything else.

---

# Todo — Rules editor redesign (in-app screen, sidebar nav, master–detail)

## Goal
User feedback on the v1 editor: it was a dialog (wanted an in-app screen), the
titlebar gear was disliked, and ~65 expanded cards in one scroll were unreadable.

## Plan / progress
- [x] `index.html`: `.app` → titlebar + `.shell` (`.nav` rail + `.main`); scan chrome moved into `#view-clean`; new `#view-rules` (header + status + open-file + Reset/Save + sub-tabs + `.reditor__split` = `#re-list` / `#re-detail`); removed the gear, `#settings` modal, old `#rules-editor` overlay
- [x] `rules-editor.ts`: rewritten from overlay to embedded **master–detail** — searchable, collapsible ecosystem-grouped list with quick on/off toggles; selected entry's form on the right; status line + external-open; Save/Reset; focus-preserving in-place row updates
- [x] `main.ts`: `state.view` + `setView` + left-rail nav; removed all settings code; scoped the filter-pill selector to `.filters .pill` (the editor tabs are `.pill` too)
- [x] `styles.css`: `.shell`/`.nav`/`.main`/`.view`; embedded `.reditor__*` + master-detail list/detail; removed dead `.modal`/`.setting`/overlay rules
- [x] Verify: `tsc` + `npm run build` 0 errors; backend unchanged (20/20 still); `tauri dev` boots clean

## Review
- **Nav:** left rail (Clean / Rules), no titlebar gear. `setView` toggles the two
  `#view-*` containers and calls `enterRulesView()` on entry.
- **Master–detail:** chosen over the flat card wall. List = compact rows grouped by
  ecosystem (collapsible, searchable, quick toggle); detail = one form at a time.
  Caches rows omit the toggle (`scan_caches` ignores `enabled`).
- **Edits survive view switches:** `enterRulesView` reloads from disk only when not
  dirty; the in-memory model persists across Clean↔Rules.
- **Backend untouched** — pure frontend restructure over the existing
  load/save/reset/status commands.
- **Files:** `index.html`, `src/{rules-editor.ts, main.ts, styles.css}`.
- **Needs click-through:** rail nav, list/search/groups/toggles, select/edit/add/
  delete, Save→rescan, Reset — headless covers build + boot only.

---

# Todo — Streaming "Clean selected" (live progress + list updates)

## Goal
Clicking **Clean selected** gave no feedback and left the list stale: deleted
folders stayed listed at their **original** size. Root cause — `clean` was a
single fire-and-forget call that returned **all-or-nothing**: one locked file
(routine on Windows: an IDE / rust-analyzer / AV holding a handle in
`target`/`node_modules`) made it return `Err`, so the frontend `catch` removed
**nothing** — even the dirs it *had* deleted — and a half-emptied folder kept its
full size. Fix: stream `clean` per-path (the pattern `scan` already uses), drive
an in-app progress bar, drop each row the moment its deletion confirms, and leave
only genuinely-stuck (in-use) folders behind, marked.

## Plan / progress
- [x] `scanner.rs`: `CleanEvent` enum (`removing`/`removed`/`failed`/`done`, serde-tagged camelCase, mirrors `ScanEvent`); `pub fn clean(paths, to_trash, emit)` — sequential, largest-first (caller-ordered), already-absent path counts as removed, per-path failures reported and never abort the batch; **moved** `remove_path` here from lib.rs
- [x] `lib.rs`: `clean` command → thin `spawn_blocking` + `Channel<CleanEvent>` wrapper (near-exact copy of `scan`); dropped the old all-or-nothing loop, local `remove_path`, and unused `use std::path::Path`
- [x] `types.ts`: `CleanEvent` union mirroring the Rust enum
- [x] `main.ts`: `CleanHandlers`/`dispatchClean`/`runClean` (+ browser `simulateClean` that fails the last path); `showCleanbar`/`cleanProgress` reuse the scan strip as a determinate "Cleaning… <path>" bar; `state.cleaning` guard + `state.failed` (path→error); rewritten `doClean` (largest-first, live `removeLocation` per `removed`, failed rows kept+selected for retry, rolled-up categories + toast summary); `cleaning` guard added to `doScan`/`doScanCaches`; `updateFooter` keeps Clean disabled mid-clean; `loc--failed` marker + tooltip in `renderChild`
- [x] `styles.css`: `.loc--failed` warm-amber accent (warning, not delete-red)
- [x] New test `clean_streams_and_removes` (two real dirs + one already-absent → 3×`Removed`, `Done{removed:3,failed:0}`, dirs gone on disk)
- [x] Verify: `cargo test` **21/21**, `cargo build --lib` exit 0 / 0 warnings, `tsc --noEmit` + `npm run build` exit 0

## Review
- **One change fixes both bugs.** Streaming per-path results means successes are
  removed live and failures stay listed — the all-or-nothing `Err` that discarded
  every UI update is gone. Deletion semantics are **unchanged** (`trash::delete` /
  `remove_dir_all`); only feedback + UI reconciliation changed.
- **UX (confirmed with user):** the app's own progress bar, not a native OS dialog
  — cross-platform, matches the scan strip, and the native `IFileOperation` route
  was Windows-only + still wouldn't reconcile the in-app list.
- **Partial failure is now first-class:** stuck rows keep a warm `loc--failed`
  accent + tooltip (the OS error) and **stay selected**, so Clean re-enables for an
  immediate retry; a successful retry clears the stale failure (`removeLocation`).
- **Largest-first ordering** (frontend-sorted) frees the biggest space first →
  strong perceived progress; sequential deletion keeps the single-path label honest.
- **Summary tally** comes from `state.failed.size` (what's actually marked), which
  agrees with the backend `Done` counts.
- **Files:** `src-tauri/src/{scanner.rs, lib.rs}`, `src/{types.ts, main.ts, styles.css}`.
- **Needs click-through:** real progress bar + live row removal during a multi-GB
  delete, and the in-use partial-failure path (hold a file open, Clean → that row
  stays marked, others vanish, toast "N deleted · 1 couldn't be removed") — headless
  covers tests + build + the browser `simulateClean` preview only.

---

# Todo — Refactor Rust backend into modular, SOLID-aligned structure

## Goal
Backend lived in 3 files (`main.rs`, `lib.rs`, `scanner.rs` @ 1821 lines mixing
10+ concerns). Split into focused modules with one reason to change each.
**Pure refactor — no behavior change.** Public `#[command]` surface frozen.

Baseline before touching: 21 lib tests pass, clippy `-D warnings` clean.

## Target module tree
```
src-tauri/src/
  main.rs        (unchanged)
  lib.rs         module wiring + run() only
  commands.rs    #[command] handlers (thin) + os_open
  fs_util.rs     shared FS helpers (sizes, mtimes, names, expand_root, ignore/git queries)
  clean.rs       CleanEvent + clean + remove_path
  rules/{mod,model,matcher,store,labels}.rs
  scan/{mod,project,caches}.rs
```

## Commits (each: cargo build + cargo test + clippy -D warnings green)
- [x] 1. Extract `fs_util.rs` (leaf FS helpers) — `e6c5871`
- [x] 2. Extract `clean.rs` + `testsupport` — `0d9d3b1`
- [x] 3. Extract `rules/` (model -> matcher -> store -> labels) + move rules tests — `8cdb932`
- [x] 4. Extract `scan/` (types+rollup -> project -> caches); delete `scanner.rs` — `4140aa0`
- [x] 5. Extract `commands.rs`; slim `lib.rs` — `b73d80b`
- [x] 6. Readability pass: `//!` docs (added inline per commit), final clippy/test

## Constraints
- Identical command names/signatures/return types; no new deps; same unsafe/panic/error semantics.
- Tests co-located with their code; shared temp-dir helpers in `#[cfg(test)] testsupport`.
- Cross-module internals widened to `pub(crate)` only (no public API change).
- Kept existing param-injection seams (`*_with`, `*_to/_from`, `emit` closure);
  deliberately NOT adding single-impl `Scanner`/`RuleSource` traits.

## Latent issues — found during the refactor, then FIXED (follow-up, user-requested)
1. [x] Cache `enabled` ignored: `scan_caches` scanned every cache and `rules_status`
   counted ALL caches, while rules/junk honor `enabled`. Now both honor it. — `43311dd`
2. [x] `match_dir_entry`/dir-index were case-sensitive while marker `exists()` is
   case-insensitive on Windows. Added `norm_seg` (lowercase on Win/macOS, identity
   on Linux); keys + lookups + segment compare all go through it. — `ff891d8`
3. [x] GlobSet<->owner parallel arrays (fragile index coupling) encapsulated in a
   `GlobOwners` type built via `add(pattern, owner)`, exposing `matches()`. — `5976ede`

These three are BEHAVIOR changes (fixes 1-2) / robustness (fix 3), made after the
pure-refactor commits at the user's request. 23 tests (was 21; +2 for fixes 1-2),
full build/clippy clean.

## Review
- **Result:** 1821-line `scanner.rs` + a fat `lib.rs` became 11 focused files.
  Backend now: `main.rs` (entry) · `lib.rs` (wiring + `run()` only) ·
  `commands.rs` (8 thin `#[command]`s + `os_open`) · `fs_util.rs` ·
  `clean.rs` · `rules/{mod,model,matcher,store,labels}.rs` ·
  `scan/{mod,project,caches}.rs` · `testsupport.rs` (cfg(test)).
- **Behavior frozen:** command names/signatures/return types unchanged →
  frontend untouched (zero TS edits). 21 tests green at every commit; full
  `cargo build`, `cargo test`, `clippy --all-targets -D warnings` clean. No deps added.
- **Dependency direction (acyclic):** commands → {scan, clean, rules};
  scan → {rules, fs_util}; rules/clean/fs_util are leaves.
- **SOLID, pragmatic:** SRP via the split. Kept the existing param-injection
  seams (`scan_with`/`scan_caches_with`, `save_rules_to`/`load_rules_from`/
  `reset_rules_to`, the `emit` closure) and **deliberately did not add**
  single-impl `Scanner`/`RuleSource` traits — they'd buy nothing the seams
  don't already give (tests prove it).
- **Tradeoff:** `Matcher` fields are `pub(crate)` because the scan walk reads
  the compiled indexes directly. Encapsulating them behind methods would mean
  re-homing `visit`/`glob_walk` into the matcher — too invasive for a
  behavior-preserving refactor. Documented at the struct.
- **Tests co-located** with the code they exercise; shared temp-dir helpers in
  `#[cfg(test)] testsupport`. `EMBEDDED` re-export is `#[cfg(test)]`-gated
  (only cross-module tests use it; production code reaches it within `rules`).
- **`include_str!`** path moved to `../../prun-rules.toml` (now in `rules/model.rs`).
- **Latent issues:** the 3 found were then FIXED in follow-up commits at the
  user's request (see the section above) — `43311dd`, `ff891d8`, `5976ede`.
- **On a branch:** `refactor/backend-modules` (6 refactor + 3 fix commits),
  left for review/merge.

---

# Todo — CMake build trees: reclaim the whole dir by marker (anti-marker guard)

## Problem
"Largest reclaimable locations" fragmented CMake build dirs. `cmake-build-debug` (an
exact `dirs` name) listed as one clean entry, but arbitrarily-named build dirs
(`cmake-build-debug-visual-studio`, `cmake-build-minsizerel-system`,
`cmake-build-docker-build-arm`, …) matched NO exact name, so the `cmake` rule's
`globs` (CMakeFiles, CMakeCache.txt, build.ninja, .ninja_deps) caught their
*contents* individually → many rows for one disposable dir. Extending the `dirs`
list / globbing `cmake-build-*` is a losing game (build dirs can be named anything).

## Fix (user-approved: anti-marker guard)
Identify a build tree by what's INSIDE it, not its name — exactly the `python-venv`
pattern. New `reclaim_root` rule keyed on `CMakeCache.txt` (CMake writes it at every
build-tree root, every generator, never committed) → reclaims the whole dir whatever
its name. Guarded by a new `anti_markers` field: suppress reclaim when the source
`CMakeLists.txt` is also present (in-source build → leave the source tree alone; its
loose artifacts stay handled by the existing globs, exactly as before).

## Changes
- [x] `rules/model.rs`: `Rule.anti_markers: Vec<String>` (serde default + skip-empty)
- [x] `rules/matcher.rs`: compile anti-markers (exact/glob split); `anti_marker_in()`;
  refactored a shared `dir_has_any()` behind `marker_in`/`anti_marker_in`
- [x] `scan/project.rs`: reclaim step gated `marker_in(p) && !anti_marker_in(p)`; +2 tests
- [x] `rules/store.rs`: `validate_rules` also glob-checks `anti_markers`
- [x] `prun-rules.toml`: new `cmake-build` rule (`markers=["CMakeCache.txt"]`,
  `anti_markers=["CMakeLists.txt"]`, `reclaim_root=true`)
- [x] `types.ts` + `rules-editor.ts`: `anti_markers` made first-class in the editor
  (interface, normalize, blankRule, preview sample, anti-markers chip-list field)
- [x] Verify: `cargo test` **25/25** (+2), `clippy -D warnings` clean, `tsc --noEmit` 0,
  `npm run build` 0

## Review
- **Why marker-based, not name-based:** the screenshot's fragmented dirs all share
  one trait — a `CMakeCache.txt` at their root. Keying on it collapses every one to a
  single entry regardless of name; the existing ancestor-subsumption (`has_ancestor_in`)
  + glob_walk pruning drop everything beneath the now-claimed build dir for free.
- **Safety:** `anti_markers` makes the in-source case behave EXACTLY as before (globs
  catch loose files; the source root is never reclaimed). No `schema_version` bump —
  the field is additive/optional, old override files parse unchanged.
- **Precedence unchanged:** for `build/` / `cmake-build-debug` (already in `dirs`), the
  name rule (visit step 1) still fires before reclaim (step 4), so those cases stay
  byte-identical; only previously-unmatched build dirs change. (Verified: the existing
  `scans_a_realistic_projects_tree` test, which has an out-of-source `build/`, still passes.)
- **Bonus:** an orphaned build dir (source deleted, parent has no CMakeLists.txt) is
  now caught too — the old `dirs` rule needed the parent marker to fire.
- **Files:** `src-tauri/src/rules/{model,matcher,store}.rs`,
  `src-tauri/src/scan/project.rs`, `src-tauri/prun-rules.toml`,
  `src/{types.ts, rules-editor.ts}`.
- **Not headless-verifiable:** the editor's new anti-markers chip-list needs a
  click-through; tests + build cover the detection logic + round-trip.

## Follow-up — the embedded fix didn't reach the user (override shadowing)
- After the above, the user re-scanned and the fragmentation persisted. Root cause:
  a **user override at `%APPDATA%\prun\rules.toml`** (saved 06-04 via the in-app
  editor) which `load_matcher()` prefers *wholesale* over the embedded ruleset — so
  the embedded `cmake-build` rule was never loaded. The override also carried a real
  customization (`make-objects` enabled), so "Reset to defaults" wasn't acceptable.
- [x] Patched the override in place: inserted the `cmake-build` rule after `cmake`
  (additive; `make-objects` and all else preserved). Validated it parses (66 rules,
  cmake-build present, make-objects still enabled).
- [x] **Proved end-to-end** with a temporary `#[ignore]`d test running the real
  `scan_with(&load_matcher(), …)` against the user's actual tree
  `D:\Kingston\pracovna plocha\projects\CLionProjects\untitled1`: output went from
  many fragments to exactly **3 whole build dirs** (`cmake-build-debug`,
  `cmake-build-debug-system`, `cmake-build-minsizerel-system`), zero
  CMakeFiles/CMakeCache.txt/build.ninja/.ninja_deps. Temp test then removed; suite
  back to **25/25**.
- **Open design issue (root cause):** the full-copy override model means anyone who
  has saved an override silently never receives built-in rule updates. Worth fixing
  properly — e.g. layer the override over the embedded base by id (override/disable
  wins per-id, new built-ins appear automatically), or store only a delta. Flagged
  to the user; not yet implemented. See lessons.md "Tests pass ≠ fixed".

---

# Todo — Enterprise-grade hardening (Tier 0 → Tier 3)

## Goal
Take Prun from "well-architected small app" to enterprise grade: close the one
real security hole, build the missing engineering scaffolding (CI, logging,
tests, licensing), fix the correctness limitations, and lay distribution
groundwork — all WITHOUT a framework rewrite or trait ceremony that would hurt
the junior-readable simplicity. Branch: `feat/enterprise-grade-tier0-3`.

## Tier 0 — Security & legal (close the critical hole) ✅
- [x] `esc()` the category label in `main.ts` (XSS sink at the sidebar)
- [x] Real Content-Security-Policy in `tauri.conf.json` (was `csp: null`)
- [x] Validate ecosystem ids `[a-z0-9_-]+` (and rule/junk/cache id charset) in `validate_rules`
- [x] Confine `clean` to paths a scan actually emitted (`Reclaimable` managed state; refuses unoffered paths)
- [x] LICENSE (MIT) + `license` field in Cargo.toml / package.json
- Verify: cargo test **31/31**, clippy -D warnings clean, tsc + vite build clean

## Tier 1 — Engineering infrastructure ✅
- [x] GitHub Actions CI: fmt/clippy/test (ubuntu + windows) + tsc + vite build + vitest, on push/PR
- [x] `cargo audit` + `npm audit` advisory job in CI; `.github/dependabot.yml` (cargo/npm/actions)
- [x] `tracing` + daily-rotated file log (7-file cap) in OS data dir; replaced the lone `eprintln!`
- [x] Vitest: 15 tests over the extracted pure logic (format/grouping/rollup/filter)
- Verify: cargo test **31/31**, clippy clean, fmt --check clean, vitest 15/15, tsc + vite build clean; CI/dependabot YAML parse-checked

## Tier 2 — Correctness & robustness ✅
- [x] Scan cancellation: `Cancel` (Arc<AtomicBool>) managed state + `cancel_scan` command; walk Quits, sizing short-circuits; cancel button in the progress strip
- [x] Surface read errors: `measure_tree` counts unreadable entries; summed into `ScanEvent::Done.errors`; toast shows "· N items unreadable"
- [x] Honest age: deep newest-mtime folded into the one sizing walk; age gate moved post-size (project + caches)
- [x] Size semantics: hard-link dedup on Unix (dev+ino); apparent-vs-on-disk documented on `Measured`
- [x] Split `main.ts` pure logic into `format.ts` / `grouping.ts` (done in Tier 1)
- Verify: cargo test **34/34** (+3), clippy clean, fmt --check clean, vitest 15/15, tsc + vite build clean

## Tier 3 — Distribution groundwork ✅
- [x] Headless `prun` CLI over the pure core (scan/caches/rules/clean, text + --json); 5 tests; verified live (version/rules/help/scan)
- [x] `tauri-plugin-updater` wired (dep + plugin init, compile-verified); endpoint/keypair documented in RELEASING.md
- [x] Release workflow (`release.yml`, tauri-action, tag-triggered, draft release) + RELEASING.md (Win Authenticode, macOS Developer ID + notarization, updater keypair, secret table)
- [x] CHANGELOG.md (Keep a Changelog) + bump to **0.2.0** (Cargo.toml / package.json / tauri.conf.json); README refreshed (stale scanner.rs ref, CLI + Development sections)
- Verify: cargo test **39/39**, clippy + fmt clean, vitest 15/15, tsc + vite build clean; updater plugin compiles; CI/release YAML parse-checked

## Constraints (keep the simplicity)
- No frontend framework; no single-impl `Scanner`/`RuleSource` traits (seams already test fine)
- No async runtime for the CPU/IO-bound walk; keep `spawn_blocking`
- Public `#[command]` surface stays compatible; behavior changes only where listed
- Each tier: `cargo test` + `clippy -D warnings` + `tsc` + `vite build` green before commit

## Review

All four tiers landed on `feat/enterprise-grade-tier0-3` as 7 commits, each
verified green before the next. Final state: **cargo test 39/39** (was 30),
**clippy -D warnings clean**, **cargo fmt --check clean**, **vitest 15/15** (new),
**tsc + vite build clean**. CLI verified live (`version`/`rules`/`help`/`scan`).

**Commits**
- `9a78879` Tier 0 — security & legal
- `d6a6325` Tier 1 — frontend split + Vitest + tracing
- `537353e` rustfmt the tree (so CI can gate `fmt --check`)
- `d66447f` Tier 1 — CI + Dependabot
- `1c44bee` Tier 2 — honest sizing, read-error reporting, cancellation
- `0b96124` Tier 3 — headless CLI
- `fcde152` Tier 3 — updater wiring, release pipeline, CHANGELOG + 0.2.0

**Key decisions**
- **`clean` confinement** lives at the IPC edge (`Reclaimable` managed state), not
  in the pure `clean` fn — keeps the core reusable (the CLI reuses it) and unit-
  testable while still neutralizing the XSS→delete escalation.
- **Age moved post-sizing.** Honest "newest mtime" needs the same walk as sizing,
  so the age gate runs after `measure_tree`. Too-fresh dirs are still walked (the
  progress bar completes) but not offered. Documented perf trade-off.
- **rustfmt the tree** (one standalone commit) rather than leave CI fmt advisory —
  enforced formatting is the enterprise norm; the author's compact enum style was
  expanded but stays readable.
- **Updater is wired but inert.** The plugin compiles and is initialized; the
  endpoint + signing key are config/secrets (RELEASING.md) so we didn't commit a
  throwaway key or risk an unverifiable bundle config.
- **Honored the simplicity constraints**: no framework, no single-impl
  `Scanner`/`RuleSource` traits, no async runtime for the blocking walk; public
  `#[command]` surface stayed compatible (frontend untouched except additive
  cancel + error wiring).

**Couldn't fully verify here** (need a real environment / secrets):
- Full `tauri build` bundle (huge + network); the updater config block is
  therefore documented, not committed.
- Code signing / notarization (requires the user's certs) — scaffolded in CI + docs.
- The GUI interactions (cancel button click, CSP at runtime, unreadable-count
  toast) — covered by build + unit tests + the browser preview, but a click-through
  on the desktop app is the honest final check.

**Follow-ups not in scope** (flagged for later): `Mutex<Vec>` discovery hot-path
contention (§3.6 of the analysis); CLI `clean` is explicit-paths-only by design;
README "your-org" placeholders in CHANGELOG compare links.

---

# Todo — Override: merge built-ins by id (stop shadowing built-in updates)

## Goal
The override is a full copy that REPLACES the built-ins, so saving one freezes the
user out of all future built-in rule updates (the cmake-build bug). Make the override
a LAYER over the embedded base instead, so new/updated built-ins flow through while
user edits/additions/removals are preserved. User chose this (vs a "sync" button).

## Design (backend-only; frontend untouched)
- **Load** (`load_matcher` for scans, `load_rules` for editor): embedded is the base;
  per section (rule/junk/global_cache) an override entry with a matching `id` replaces
  the embedded one (at the embedded position, preserving precedence); embedded ids the
  override doesn't mention come through fresh; new built-ins appear automatically;
  override-only ids (user rules) are appended; tombstoned ids are dropped.
- **Save** (`save_rules`): validate the submitted full model, then write only the
  DELTA — entries that are new or differ from the embedded entry of the same id —
  plus a per-section `removed` tombstone list = embedded ids absent from the
  submission (i.e. the user deleted them). Unmodified built-ins are omitted so their
  future fixes flow.
- **Deletion truthful, no frontend change:** the editor keeps loading the full merged
  set and saving the whole thing; the backend derives the tombstones by diffing the
  submission against embedded. Removing a built-in → tombstone (stays gone, re-appears
  only if you clear it by re-adding); removing a custom rule → just dropped.
- **`removed` tombstone** lives on `RuleFile` but `skip_serializing_if = empty`, so the
  editor's JSON never carries it (always empty on the wire) — only the on-disk override
  holds it when non-empty.
- **Self-healing:** the user's current full-copy override keeps working via merge; the
  first editor save compacts it to a tiny delta (just `make-objects=true`).

## Plan / progress
- [x] `rules/model.rs`: derive `PartialEq` on Rule/Junk/GlobalCache; add
  `Removed { rules, junk, global_cache: Vec<String> }` + `RuleFile.removed`
  (skip-empty, so omitted from the editor's wire JSON). No `schema_version` bump.
- [x] `rules/store.rs`: `merge_over_embedded()` + `merge_section()` (load);
  `delta_against_embedded()` + `delta_section()` (save); wired into
  `load_matcher`/`load_rules`/`save_rules_to`; `rules_status` now counts the merged set
- [x] Tests (+5): empty-delta on default save + merge-back; new built-in surfaces;
  per-id edit wins; tombstone suppresses; delta keeps only changes + tombstones removed;
  save→merge-load preserves a customization
- [x] Verify: cargo test **30/30**, clippy -D warnings clean, tsc + vite build clean;
  temp `#[ignore]`d real-data test confirmed the merge surfaces cmake-build for the
  user's actual override (stripped to simulate the original) while keeping make-objects

## Review
- **Override is now a layer, not a replacement.** Load merges the override over the
  embedded base by id (per section); save stores only the delta + a `[removed]`
  tombstone list. New/updated built-ins flow through; user edits/additions/removals
  persist. Frontend untouched (the editor still loads/edits/saves the full set; the
  backend derives delta+tombstones by diffing against embedded).
- **Deletion stays honest without a frontend change:** `save` derives tombstones as
  the embedded ids absent from the submitted set; `load` drops them. A removed custom
  rule (id not in embedded) simply isn't re-added.
- **Self-healing:** an existing full-copy override keeps working via merge; the first
  editor save compacts it to a tiny delta (proven: saving the unmodified default set
  stores an empty delta that merges back to the full 66 rules).
- **`PartialEq` powers the delta** (an entry is "unmodified" iff structurally equal to
  its embedded twin). Order differences in markers/dirs/globs count as modified — a
  harmless false-positive (keeps a rule that won't auto-update), never data loss.
- **Files:** `src-tauri/src/rules/{model.rs, store.rs}`. No frontend, no schema bump.
- **The user's immediate file** was already patched with cmake-build; the merge makes
  that redundant going forward and will be compacted on their next editor save.

---

# Todo — Enterprise-grade Tier 4 → Tier 6

## Goal
Close the trust / diagnostics / robustness gaps from the architecture review.
Everything machine-actionable is implemented; items that physically need the
owner's accounts or money (certs, Apple ID, winget submission, updater keypair)
are scaffolded to "add-secret-and-go" and listed under Deferred.
Branch: `feat/enterprise-grade-tier4-6` (stacked on tier0-3).

## Tier 4 — Trust & distribution (no-secrets items)
- [ ] CI: macOS job (clippy + test); clippy added to the Windows job
- [ ] CI + release: pin all third-party actions to commit SHAs (dtolnay pinned ⇒ explicit `toolchain: stable` input)
- [ ] release.yml: CycloneDX SBOM (anchore/sbom-action, ubuntu leg) as workflow artifact
- [ ] release.yml: build-provenance attestations for every bundle (actions/attest-build-provenance)
- [ ] RELEASING.md: SBOM/attestation section + Azure Trusted Signing pointer
- [ ] DEFERRED (owner-gated): Authenticode cert, Apple notarization, updater keypair, winget manifest

## Tier 5 — Diagnostics & governance
- [ ] Panic hook → crash file (message + backtrace) in the log dir; works under `panic = "abort"`; GUI + CLI
- [ ] "Open logs" affordance: `open_logs_dir` command + nav-rail button; CLI `prun logs`
- [ ] Per-path scan-error samples: `Measured` collects capped samples → `ScanEvent::Done.error_samples` → UI toast detail + CLI `--json`
- [ ] Governance docs: SECURITY.md, CONTRIBUTING.md, .github/CODEOWNERS, issue + PR templates
- [ ] ARCHITECTURE.md: module map, data flow, the load-bearing invariants

## Tier 6 — Robustness & scale
- [ ] Backend scan-in-flight guard (managed flag; concurrent scan rejected loudly; reset on all exits) + test
- [ ] Junction safety (Windows): test that a junction inside the tree never offers/sizes its target; clean removes the link only; fix if the test exposes an escape
- [ ] Frontend: extract `backend.ts` (every invoke/Channel + browser simulators); main.ts / rules-editor.ts consume it
- [ ] DTO contract fixtures: committed JSON checked by a Rust serde test AND a Vitest type/shape test
- [ ] Debounce the age input
- [ ] eslint (typescript-eslint) + prettier (isolated format commit) + CI lint step
- [ ] Vitest coverage (informational) in CI
- [ ] E2E smoke: evaluate tauri-driver feasibility; implement if verifiable here, else scaffold + document honestly

## Constraints (unchanged from Tier 0-3)
- No framework, no single-impl trait ceremony, no async-runtime swap
- Public `#[command]` surface stays compatible (additive only)
- Each commit verified: cargo test + clippy -D warnings + fmt --check + tsc + vite build + vitest green

