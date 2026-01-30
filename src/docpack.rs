//! Doc-pack root helpers.
//!
//! We centralize root resolution here so call sites can stay focused on
//! workflow decisions rather than filesystem edge cases.
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Resolve a doc-pack root, optionally creating it.
///
/// This keeps init deterministic by ensuring the root exists (when requested)
/// and is normalized before any other work begins.
pub fn ensure_doc_pack_root(path: &Path, create: bool) -> Result<PathBuf> {
    if create {
        fs::create_dir_all(path).context("create doc pack root")?;
    }
    path.canonicalize()
        .with_context(|| format!("resolve doc pack root {}", path.display()))
}

/// Resolve a doc-pack root for status without requiring it to exist.
///
/// Status is allowed to point at a missing pack so we can surface a precise
/// next action instead of failing early.
pub fn doc_pack_root_for_status(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        path.canonicalize()
            .with_context(|| format!("resolve doc pack root {}", path.display()))
    } else {
        Ok(path.to_path_buf())
    }
}
