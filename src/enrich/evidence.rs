//! Evidence reference helpers.
//!
//! Evidence is referenced by path + hash so downstream summaries can remain
//! lightweight while still traceable.
use super::paths::rel_path;
use super::EvidenceRef;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// De-duplicate evidence refs by path while preserving order.
pub fn dedupe_evidence_refs(entries: &mut Vec<EvidenceRef>) {
    let mut seen = BTreeSet::new();
    entries.retain(|entry| seen.insert(entry.path.clone()));
}

/// Build an evidence reference from an absolute path.
pub fn evidence_from_path(doc_pack_root: &Path, path: &Path) -> Result<EvidenceRef> {
    let rel = rel_path(doc_pack_root, path)?;
    let sha256 = if path.exists() {
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        Some(format!("{:x}", hasher.finalize()))
    } else {
        None
    };
    Ok(EvidenceRef { path: rel, sha256 })
}

/// Build an evidence reference from a doc-pack relative path.
pub fn evidence_from_rel(doc_pack_root: &Path, rel: &str) -> Result<EvidenceRef> {
    let path = doc_pack_root.join(rel);
    evidence_from_path(doc_pack_root, &path)
}
