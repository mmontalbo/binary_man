use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub fn ensure_doc_pack_root(path: &Path, create: bool) -> Result<PathBuf> {
    if create {
        fs::create_dir_all(path).context("create doc pack root")?;
    }
    path.canonicalize()
        .with_context(|| format!("resolve doc pack root {}", path.display()))
}

pub fn doc_pack_root_for_status(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        path.canonicalize()
            .with_context(|| format!("resolve doc pack root {}", path.display()))
    } else {
        Ok(path.to_path_buf())
    }
}
