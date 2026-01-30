//! Reporting and history persistence for enrich runs.
//!
//! These artifacts are append-only or snapshot-based to keep workflow history
//! auditable without mutating the underlying inputs.
use super::{DocPackPaths, EnrichHistoryEntry, EnrichReport};
use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::Path;

/// Write the latest enrich report snapshot.
pub fn write_report(doc_pack_root: &Path, report: &EnrichReport) -> Result<()> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.report_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create enrich dir")?;
    }
    let text = serde_json::to_string_pretty(report).context("serialize enrich report")?;
    fs::write(&path, text.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Append a history entry as JSONL.
pub fn append_history(doc_pack_root: &Path, entry: &EnrichHistoryEntry) -> Result<()> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.history_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create enrich dir")?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let line = serde_json::to_string(entry).context("serialize enrich history entry")?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}
