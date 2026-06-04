# Lessons

## Perceived speed vs actual speed
When a user reports something "feels frozen / looks like it does nothing,"
address **both** axes, not just the obvious one:
- *Perceived responsiveness* — progress feedback (bar, current item, streamed results).
- *Actual throughput* — is the work itself slow? (here: single-threaded → parallel).
The first plan only added a progress bar; the user then asked for parallel
scanning. Lead with "make it feel alive AND make it faster" for slow-operation UX.

## Verify a user's specific tech request before implementing it
The user asked for io_uring (incl. "Windows's io_uring"). Rather than blindly
implementing or silently dropping it, I researched and found Windows IoRing has
**no directory-enumeration/stat opcode** and Linux io_uring lacks `getdents` — so
it can't drive a tree-walk + file-sizing workload on a Windows-primary app.
Surfaced the finding, recommended thread-parallel (`ignore::WalkParallel` + rayon,
what ripgrep/fd/dua use), and confirmed via AskUserQuestion before coding.
**Pattern:** when a requested technology conflicts with the platform/workload,
verify feasibility, present the trade-off, and get sign-off — don't assume.

## A user-provided spec artifact is the source of truth, not a hint
The user authored a rich `prun-rules.toml` but the code shipped ~13 hardcoded
rules — they had to point out "you forgot to use this file." When a user hands
over a config/ruleset/schema, **wire it in as the source of truth**, don't
reimplement a subset inline. Re-read its own header comments for the intended
semantics (here: *root-first* — a dir is a project root if it holds a marker;
that rule's `dirs`/`globs` under it are candidates) and implement THAT model.

## Don't let a subagent's optimization regress existing correctness
The Plan agent proposed `prune_names` (skip any dir literally named
`build`/`out`/`vendor`…) for speed. That would silently skip a coincidentally
named non-artifact dir containing real sub-projects — a regression vs. the
current marker-validated skip. Kept dir matching **name-first + marker-validated**
(skip only validated artifacts) and used a phase-2 subtree walk only for the
genuinely-recursive globs. **Pattern:** weigh a subagent's design against the
behaviour you already have; adopt the parts that don't trade correctness for speed.

## Match the requested UX presentation exactly
"Show only the project root + a classic blue/green bar" meant: drop the streamed
per-folder paths and the "Sizing N/total" counts entirely — not just tweak them.
When a user describes a specific presentation, implement that presentation, and
confirm the ambiguous bit (here: solid blue vs blue→green) with one quick question.

## Reusing a styled class can inherit behavior bound by a broad selector
Added a settings gear + modal-close button with `class="wbtn"` to reuse the
titlebar-button style. But `wire()` bound a window-control handler to **every**
`.wbtn` (`document.querySelectorAll(".wbtn")`), and `windowAction` ended in a bare
`else await w.close()` — so the gear (no `data-win`) called `windowAction(undefined)`
→ the `else` → **closed the window**. The modal flashed then the app exited.
**Fixes:** scope the behavior selector to what actually has the behavior
(`.wbtn[data-win]`), and never let a dispatcher's catch-all `else` do something
destructive — branch explicitly (`else if (action === "close")`).
**Pattern:** a class is for *styling*; bind *behavior* to a role/attribute the
element actually declares, not to the shared style class.

## Headless verification can't catch click-driven behavior
`cargo test` + `tsc`/`vite build` + a boot check all passed, yet clicking the gear
closed the app — the bug only fired on a real click, which I couldn't perform in a
headless session. **Pattern:** when a change adds interactive UI, explicitly flag
that the click/keyboard paths need manual testing (or a webdriver), and don't
imply "verified" covers behavior that only a real interaction exercises.
