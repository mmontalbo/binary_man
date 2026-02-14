//! Prereq inference file types and helpers.
//!
//! The `enrich/prereqs.json` file stores LM-inferred prerequisite mappings
//! for surface items. This enables smarter auto-verification by skipping
//! interactive options and providing appropriate fixtures.

use crate::scenarios::{ScenarioSeedEntry, ScenarioSeedSpec, SeedEntryKind};
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

/// Flat seed format for simpler LM responses, converted to ScenarioSeedSpec.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct FlatSeed {
    /// Directory paths to create.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<String>,
    /// Files to create, mapping path to content.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
    /// Symlinks to create, mapping path to target.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub symlinks: BTreeMap<String, String>,
    /// Executables to create (mode 755), mapping path to content.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub executables: BTreeMap<String, String>,
}

impl FlatSeed {
    /// Convert flat seed format to canonical ScenarioSeedSpec.
    pub fn to_seed_spec(&self) -> ScenarioSeedSpec {
        let mut entries = Vec::new();

        // Add directories
        for path in &self.dirs {
            entries.push(ScenarioSeedEntry {
                path: path.clone(),
                kind: SeedEntryKind::Dir,
                contents: None,
                target: None,
                mode: None,
            });
        }

        // Add files
        for (path, content) in &self.files {
            entries.push(ScenarioSeedEntry {
                path: path.clone(),
                kind: SeedEntryKind::File,
                contents: Some(content.clone()),
                target: None,
                mode: None,
            });
        }

        // Add symlinks
        for (path, target) in &self.symlinks {
            entries.push(ScenarioSeedEntry {
                path: path.clone(),
                kind: SeedEntryKind::Symlink,
                contents: None,
                target: Some(target.clone()),
                mode: None,
            });
        }

        // Add executables
        for (path, content) in &self.executables {
            entries.push(ScenarioSeedEntry {
                path: path.clone(),
                kind: SeedEntryKind::File,
                contents: Some(content.clone()),
                target: None,
                mode: Some(0o755),
            });
        }

        ScenarioSeedSpec { entries }
    }
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

    /// Get surface IDs that don't have prereq mappings yet.
    pub fn unmapped_surface_ids<'a>(
        &self,
        surface_ids: impl Iterator<Item = &'a str>,
    ) -> Vec<String> {
        surface_ids
            .filter(|id| !self.surface_map.contains_key(*id))
            .map(|id| id.to_string())
            .collect()
    }

    /// Merge another prereqs file into this one.
    ///
    /// New definitions are added, existing ones are preserved.
    /// Surface mappings are merged, with new mappings taking precedence.
    pub fn merge(&mut self, other: &PrereqsFile) {
        // Add new definitions (don't overwrite existing)
        for (key, def) in &other.definitions {
            self.definitions
                .entry(key.clone())
                .or_insert_with(|| def.clone());
        }

        // Merge surface mappings (new mappings take precedence)
        for (surface_id, prereqs) in &other.surface_map {
            self.surface_map.insert(surface_id.clone(), prereqs.clone());
        }
    }

    /// Remove definitions that are not referenced by any surface mapping.
    pub fn gc_definitions(&mut self) {
        let referenced: std::collections::HashSet<_> = self
            .surface_map
            .values()
            .flat_map(|prereqs| prereqs.iter())
            .cloned()
            .collect();

        self.definitions.retain(|key, _| referenced.contains(key));
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

/// Merge seed entries, deduplicating by path.
fn merge_seed_entries(target: &mut Vec<ScenarioSeedEntry>, source: &[ScenarioSeedEntry]) {
    for entry in source {
        if !target.iter().any(|e| e.path == entry.path) {
            target.push(entry.clone());
        }
    }
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

/// Write the prereqs file to disk.
pub fn write_prereqs(prereqs_path: &Path, prereqs: &PrereqsFile) -> Result<()> {
    if let Some(parent) = prereqs_path.parent() {
        fs::create_dir_all(parent).context("create enrich dir")?;
    }
    let text = serde_json::to_string_pretty(prereqs).context("serialize prereqs")?;
    fs::write(prereqs_path, text.as_bytes())
        .with_context(|| format!("write {}", prereqs_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_seed_to_seed_spec() {
        let flat = FlatSeed {
            dirs: vec![".git".to_string()],
            files: BTreeMap::from([("config".to_string(), "value".to_string())]),
            symlinks: BTreeMap::new(),
            executables: BTreeMap::new(),
        };

        let spec = flat.to_seed_spec();
        assert_eq!(spec.entries.len(), 2);
        assert_eq!(spec.entries[0].path, ".git");
        assert!(matches!(spec.entries[0].kind, SeedEntryKind::Dir));
        assert_eq!(spec.entries[1].path, "config");
        assert!(matches!(spec.entries[1].kind, SeedEntryKind::File));
    }

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

    #[test]
    fn unmapped_surface_ids() {
        let mut prereqs = PrereqsFile::new();
        prereqs.surface_map.insert("--local".to_string(), vec![]);

        let unmapped =
            prereqs.unmapped_surface_ids(["--local", "--edit", "--global"].iter().copied());
        assert_eq!(unmapped, vec!["--edit", "--global"]);
    }

    #[test]
    fn gc_definitions() {
        let mut prereqs = PrereqsFile::new();
        prereqs.definitions.insert(
            "used".to_string(),
            PrereqInferenceDefinition {
                description: None,
                seed: None,
                exclude: false,
            },
        );
        prereqs.definitions.insert(
            "unused".to_string(),
            PrereqInferenceDefinition {
                description: None,
                seed: None,
                exclude: false,
            },
        );
        prereqs
            .surface_map
            .insert("--opt".to_string(), vec!["used".to_string()]);

        prereqs.gc_definitions();
        assert!(prereqs.definitions.contains_key("used"));
        assert!(!prereqs.definitions.contains_key("unused"));
    }
}
