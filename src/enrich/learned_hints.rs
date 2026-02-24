//! Persistence for learned hints from successful verifications.
//!
//! The LearnedHints struct stores working argv patterns and exclusion reasons
//! that can be loaded into future LM prompts to improve scenario generation.
//!
//! # Design
//!
//! Rather than building complex knowledge bases or fact stores, we persist only
//! concrete wins: argvs that produced `delta_seen` and exclusions with reasons.
//! These serve as examples for the LM, not rules.
//!
//! # Schema
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "working_argvs": {
//!     "--verbose": ["--verbose", "input.txt"]
//!   },
//!   "exclusions": {
//!     "--follow": "blocks_indefinitely"
//!   }
//! }
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Current schema version for learned_hints.json.
pub const LEARNED_HINTS_SCHEMA_VERSION: u32 = 1;

/// Learned hints from successful verifications.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LearnedHints {
    /// Schema version for forward compatibility.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// Working argv patterns indexed by surface_id.
    /// Each entry is the argv that produced `delta_seen` for that surface.
    #[serde(default)]
    pub working_argvs: BTreeMap<String, Vec<String>>,

    /// Exclusion reasons indexed by surface_id.
    /// Short description of why the surface was excluded.
    #[serde(default)]
    pub exclusions: BTreeMap<String, String>,
}

fn default_schema_version() -> u32 {
    LEARNED_HINTS_SCHEMA_VERSION
}

impl LearnedHints {
    /// Create an empty hints structure.
    pub fn new() -> Self {
        Self {
            schema_version: LEARNED_HINTS_SCHEMA_VERSION,
            working_argvs: BTreeMap::new(),
            exclusions: BTreeMap::new(),
        }
    }

    /// Record a working argv for a surface.
    pub fn record_working_argv(&mut self, surface_id: &str, argv: Vec<String>) {
        self.working_argvs.insert(surface_id.to_string(), argv);
    }

    /// Check if we have any hints.
    pub fn is_empty(&self) -> bool {
        self.working_argvs.is_empty() && self.exclusions.is_empty()
    }

    /// Get the count of working argvs.
    pub fn working_argv_count(&self) -> usize {
        self.working_argvs.len()
    }
}

/// Load learned hints from the doc-pack.
/// Returns empty hints if the file doesn't exist.
pub fn load_learned_hints(path: &Path) -> Result<LearnedHints> {
    if !path.exists() {
        return Ok(LearnedHints::new());
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("read learned hints: {}", path.display()))?;

    let hints: LearnedHints = serde_json::from_str(&content)
        .with_context(|| format!("parse learned hints: {}", path.display()))?;

    Ok(hints)
}

/// Write learned hints to the doc-pack.
pub fn write_learned_hints(path: &Path, hints: &LearnedHints) -> Result<()> {
    let content = serde_json::to_string_pretty(hints).context("serialize learned hints")?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir: {}", parent.display()))?;
    }

    fs::write(path, content).with_context(|| format!("write learned hints: {}", path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_record_and_persist() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("learned_hints.json");

        let mut hints = LearnedHints::new();
        hints.record_working_argv("--verbose", vec!["--verbose".into(), "input.txt".into()]);

        assert_eq!(hints.working_argv_count(), 1);
        assert!(!hints.is_empty());

        write_learned_hints(&path, &hints).unwrap();

        let loaded = load_learned_hints(&path).unwrap();
        assert_eq!(loaded.working_argvs.get("--verbose").unwrap().len(), 2);
    }

    #[test]
    fn test_load_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does_not_exist.json");

        let hints = load_learned_hints(&path).unwrap();
        assert!(hints.is_empty());
    }
}
