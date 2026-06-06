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
