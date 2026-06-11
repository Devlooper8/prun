## What & why

<!-- A sentence or two. Link the issue if one exists. -->

## Verification

All four green locally (CI runs the same):

- [ ] `cargo test --manifest-path src-tauri/Cargo.toml`
- [ ] `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings`
- [ ] `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check`
- [ ] `npm run build && npm test`

## Click-through

- [ ] This PR changes interactive UI, so it **needs a click-through** —
      headless verification can't cover real clicks (see `tasks/lessons.md`).
      I've noted above what to click.
