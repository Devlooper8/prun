//! Shared test helpers: throwaway temp dirs and quick file/dir creation.
//! Compiled only under `cfg(test)`.

use std::fs;
use std::path::{Path, PathBuf};

/// A fresh, empty temp dir unique to this `tag` + process id.
pub(crate) fn fresh_tmp(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("prun_test_{}_{}", tag, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

/// Create `p` (and any missing parents) as a 1-byte file.
pub(crate) fn touch(p: &Path) {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, b"x").unwrap();
}

/// Create `p` (and any missing parents) as a directory.
pub(crate) fn mkdir(p: &Path) {
    fs::create_dir_all(p).unwrap();
}

/// Windows-only: create an NTFS junction at `link` pointing at `target`.
/// Junctions, unlike symlinks, need no special privileges to create.
#[cfg(windows)]
pub(crate) fn junction(link: &Path, target: &Path) {
    use std::os::windows::process::CommandExt;
    let status = std::process::Command::new("cmd")
        // raw_arg: cmd.exe re-parses its command line, so hand it one
        // pre-quoted string instead of letting std re-quote each arg.
        .raw_arg(format!(
            r#"/C mklink /J "{}" "{}""#,
            link.display(),
            target.display()
        ))
        .stdout(std::process::Stdio::null())
        .status()
        .expect("cmd mklink runs");
    assert!(status.success(), "junction creation failed");
}
