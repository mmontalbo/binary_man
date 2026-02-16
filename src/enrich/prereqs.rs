//! Prereq inference file types and helpers.
//!
//! The `enrich/prereqs.json` file stores LM-inferred prerequisite mappings
//! for surface items. This enables smarter auto-verification by skipping
//! interactive options and providing appropriate fixtures.

use crate::scenarios::{ScenarioSeedEntry, ScenarioSeedSpec};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Current schema version for `enrich/prereqs.json`.
pub const PREREQS_SCHEMA_VERSION: u32 = 1;

/// LM-inferred prerequisite mappings for surface items.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct PrereqsFile {
    pub schema_version: u32,
    /// Reusable prereq definitions, keyed by prereq name.
    #[serde(default)]
    pub definitions: BTreeMap<String, PrereqInferenceDefinition>,
    /// Mapping from surface_id to prereq keys.
    #[serde(default)]
    pub surface_map: BTreeMap<String, Vec<String>>,
}

/// A single prereq definition inferred by LM.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct PrereqInferenceDefinition {
    /// Human-readable description of what this prereq provides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Seed template to use when this prereq is needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<ScenarioSeedSpec>,
    /// If true, options with this prereq should be excluded from auto-verify.
    #[serde(default)]
    pub exclude: bool,
}

/// Resolved prereq information for a single surface item.
#[derive(Debug, Clone, Default)]
pub struct ResolvedPrereq {
    /// Whether this item should be excluded from auto-verify.
    pub exclude: bool,
    /// Merged seed from all referenced prereqs.
    pub seed: Option<ScenarioSeedSpec>,
}

impl PrereqsFile {
    /// Create a new empty prereqs file.
    pub fn new() -> Self {
        Self {
            schema_version: PREREQS_SCHEMA_VERSION,
            definitions: BTreeMap::new(),
            surface_map: BTreeMap::new(),
        }
    }

    /// Resolve prereqs for a surface item.
    pub fn resolve(&self, surface_id: &str) -> ResolvedPrereq {
        // Get prereq keys from surface_map
        let prereq_keys = self
            .surface_map
            .get(surface_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        // Merge all referenced definitions
        let mut exclude = false;
        let mut seed_entries = Vec::new();

        for key in prereq_keys {
            if let Some(def) = self.definitions.get(key) {
                if def.exclude {
                    exclude = true;
                }
                if let Some(seed) = &def.seed {
                    merge_seed_entries(&mut seed_entries, &seed.entries);
                }
            }
        }

        ResolvedPrereq {
            exclude,
            seed: if seed_entries.is_empty() {
                None
            } else {
                Some(ScenarioSeedSpec {
                    entries: seed_entries,
                })
            },
        }
    }
}

/// Merge seed entries, deduplicating by path and filtering invalid paths.
fn merge_seed_entries(target: &mut Vec<ScenarioSeedEntry>, source: &[ScenarioSeedEntry]) {
    for entry in source {
        // Skip entries with invalid paths (e.g., ".", "..", absolute paths)
        if !is_valid_seed_path(&entry.path) {
            continue;
        }
        if !target.iter().any(|e| e.path == entry.path) {
            target.push(entry.clone());
        }
    }
}

/// Check if a seed path is valid (relative, non-empty after normalization, no parent refs).
fn is_valid_seed_path(path: &str) -> bool {
    use std::path::Path;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return false;
    }
    let normalized = trimmed.replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute() {
        return false;
    }
    let mut has_normal_component = false;
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) => has_normal_component = true,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => return false,
            _ => return false,
        }
    }
    has_normal_component
}

/// Load the prereqs file from disk, returning None if it doesn't exist.
pub fn load_prereqs(prereqs_path: &Path) -> Result<Option<PrereqsFile>> {
    if !prereqs_path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(prereqs_path)
        .with_context(|| format!("read prereqs {}", prereqs_path.display()))?;
    let prereqs: PrereqsFile = serde_json::from_slice(&bytes).context("parse prereqs JSON")?;
    Ok(Some(prereqs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::SeedEntryKind;

    #[test]
    fn resolve_with_definitions() {
        let mut prereqs = PrereqsFile::new();
        prereqs.definitions.insert(
            "git_repo".to_string(),
            PrereqInferenceDefinition {
                description: Some("git repository".to_string()),
                seed: Some(ScenarioSeedSpec {
                    entries: vec![ScenarioSeedEntry {
                        path: ".git".to_string(),
                        kind: SeedEntryKind::Dir,
                        contents: None,
                        target: None,
                        mode: None,
                    }],
                }),
                exclude: false,
            },
        );
        prereqs.definitions.insert(
            "interactive".to_string(),
            PrereqInferenceDefinition {
                description: Some("requires TTY".to_string()),
                seed: None,
                exclude: true,
            },
        );
        prereqs
            .surface_map
            .insert("--local".to_string(), vec!["git_repo".to_string()]);
        prereqs
            .surface_map
            .insert("--edit".to_string(), vec!["interactive".to_string()]);
        prereqs.surface_map.insert("--global".to_string(), vec![]);

        // --local should get git_repo seed, not excluded
        let resolved = prereqs.resolve("--local");
        assert!(!resolved.exclude);
        assert!(resolved.seed.is_some());
        assert_eq!(resolved.seed.unwrap().entries[0].path, ".git");

        // --edit should be excluded
        let resolved = prereqs.resolve("--edit");
        assert!(resolved.exclude);
        assert!(resolved.seed.is_none());

        // --global should have no prereqs
        let resolved = prereqs.resolve("--global");
        assert!(!resolved.exclude);
        assert!(resolved.seed.is_none());

        // Unknown should have no prereqs
        let resolved = prereqs.resolve("--unknown");
        assert!(!resolved.exclude);
        assert!(resolved.seed.is_none());
    }
}
