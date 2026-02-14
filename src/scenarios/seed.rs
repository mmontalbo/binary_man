use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::{
    ScenarioSeedEntry, ScenarioSeedSpec, SeedEntryKind, MAX_SEED_ENTRIES, MAX_SEED_TOTAL_BYTES,
};

/// Seed spec format expected by binary_lens (created inside sandbox).
#[derive(Debug, Serialize)]
pub(crate) struct BinaryLensSeedSpec {
    pub entries: Vec<BinaryLensSeedEntry>,
}

/// Single seed entry in binary_lens format.
#[derive(Debug, Serialize)]
pub(crate) struct BinaryLensSeedEntry {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

/// Convert bman's ScenarioSeedSpec to binary_lens format.
pub(crate) fn to_binary_lens_seed_spec(seed: &ScenarioSeedSpec) -> Result<BinaryLensSeedSpec> {
    validate_seed_spec(seed).context("validate seed spec")?;

    let entries = seed
        .entries
        .iter()
        .map(|entry| {
            let path = normalize_seed_path(&entry.path)?;
            Ok(match entry.kind {
                SeedEntryKind::File => BinaryLensSeedEntry {
                    path,
                    kind: None, // default when content present
                    content: Some(entry.contents.clone().unwrap_or_default()),
                    encoding: None,
                    target: None,
                    mode: entry.mode,
                },
                SeedEntryKind::Dir => BinaryLensSeedEntry {
                    path,
                    kind: Some("dir".to_string()),
                    content: None,
                    encoding: None,
                    target: None,
                    mode: entry.mode,
                },
                SeedEntryKind::Symlink => {
                    let target = entry
                        .target
                        .as_ref()
                        .ok_or_else(|| anyhow!("symlink missing target"))?;
                    let target_normalized = normalize_seed_path(target)?;
                    BinaryLensSeedEntry {
                        path,
                        kind: Some("symlink".to_string()),
                        content: None,
                        encoding: None,
                        target: Some(target_normalized),
                        mode: None,
                    }
                }
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(BinaryLensSeedSpec { entries })
}

pub(crate) const DEFAULT_BEHAVIOR_SEED_DIR: &str = "work";
const DEFAULT_BEHAVIOR_SEED_FILE1: &str = "work/file1.txt";
const DEFAULT_BEHAVIOR_SEED_FILE2: &str = "work/file2";
const DEFAULT_BEHAVIOR_SEED_SUBDIR: &str = "work/subdir";
const DEFAULT_BEHAVIOR_SEED_NESTED: &str = "work/subdir/nested.txt";
const DEFAULT_BEHAVIOR_SEED_LINK: &str = "work/link";

pub(crate) fn validate_seed_spec(seed: &ScenarioSeedSpec) -> Result<()> {
    if seed.entries.len() > MAX_SEED_ENTRIES {
        return Err(anyhow!("seed exceeds max entries ({MAX_SEED_ENTRIES})"));
    }
    let mut seen = HashSet::new();
    let mut total_bytes = 0usize;
    for entry in &seed.entries {
        let rel_path = normalize_seed_path(&entry.path)
            .with_context(|| format!("seed entry path {:?}", entry.path))?;
        if !seen.insert(rel_path) {
            return Err(anyhow!("seed entry paths must be unique"));
        }
        match entry.kind {
            SeedEntryKind::Dir => {
                if entry.contents.is_some() {
                    return Err(anyhow!("seed dir must not include contents"));
                }
                if entry.target.is_some() {
                    return Err(anyhow!("seed dir must not include target"));
                }
            }
            SeedEntryKind::File => {
                if entry.target.is_some() {
                    return Err(anyhow!("seed file must not include target"));
                }
                let contents_len = entry.contents.as_deref().unwrap_or("").len();
                total_bytes = total_bytes
                    .checked_add(contents_len)
                    .ok_or_else(|| anyhow!("seed size overflow"))?;
            }
            SeedEntryKind::Symlink => {
                #[cfg(not(unix))]
                {
                    return Err(anyhow!("seed symlinks are unsupported on this platform"));
                }
                if entry.contents.is_some() {
                    return Err(anyhow!("seed symlink must not include contents"));
                }
                let target = entry
                    .target
                    .as_ref()
                    .ok_or_else(|| anyhow!("seed symlink missing target"))?;
                normalize_seed_path(target)
                    .with_context(|| format!("symlink target {target:?}"))?;
                total_bytes = total_bytes
                    .checked_add(target.len())
                    .ok_or_else(|| anyhow!("seed size overflow"))?;
            }
        }
        validate_seed_mode(entry.mode)?;
        if total_bytes > MAX_SEED_TOTAL_BYTES {
            return Err(anyhow!(
                "seed exceeds max total bytes ({MAX_SEED_TOTAL_BYTES})"
            ));
        }
    }
    Ok(())
}

pub(crate) fn normalize_seed_path(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("seed paths must not be empty"));
    }
    let normalized = trimmed.replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute() {
        return Err(anyhow!("seed paths must be relative"));
    }
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => cleaned.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(anyhow!("seed paths must not contain '..'"));
            }
            _ => return Err(anyhow!("seed paths must be relative")),
        }
    }
    let cleaned = cleaned.to_string_lossy().to_string();
    if cleaned.is_empty() {
        return Err(anyhow!("seed paths must not be empty"));
    }
    Ok(cleaned)
}

fn validate_seed_mode(mode: Option<u32>) -> Result<()> {
    if let Some(mode) = mode {
        #[cfg(not(unix))]
        {
            return Err(anyhow!("seed mode is unsupported on this platform"));
        }
        if mode > 0o777 {
            return Err(anyhow!("seed mode must be <= 0777"));
        }
    }
    Ok(())
}

/// Default inline seed skeleton for behavior scenarios and delta checks.
pub(crate) fn default_behavior_seed() -> ScenarioSeedSpec {
    let mut entries = vec![
        ScenarioSeedEntry {
            path: DEFAULT_BEHAVIOR_SEED_DIR.to_string(),
            kind: SeedEntryKind::Dir,
            contents: None,
            target: None,
            mode: None,
        },
        ScenarioSeedEntry {
            path: DEFAULT_BEHAVIOR_SEED_FILE1.to_string(),
            kind: SeedEntryKind::File,
            contents: Some("a\n".to_string()),
            target: None,
            mode: None,
        },
        ScenarioSeedEntry {
            path: DEFAULT_BEHAVIOR_SEED_FILE2.to_string(),
            kind: SeedEntryKind::File,
            contents: Some("b\n".to_string()),
            target: None,
            mode: None,
        },
        ScenarioSeedEntry {
            path: DEFAULT_BEHAVIOR_SEED_SUBDIR.to_string(),
            kind: SeedEntryKind::Dir,
            contents: None,
            target: None,
            mode: None,
        },
        ScenarioSeedEntry {
            path: DEFAULT_BEHAVIOR_SEED_NESTED.to_string(),
            kind: SeedEntryKind::File,
            contents: Some("c\n".to_string()),
            target: None,
            mode: None,
        },
    ];
    #[cfg(unix)]
    {
        entries.push(ScenarioSeedEntry {
            path: DEFAULT_BEHAVIOR_SEED_LINK.to_string(),
            kind: SeedEntryKind::Symlink,
            contents: None,
            target: Some("file1.txt".to_string()),
            mode: None,
        });
    }
    ScenarioSeedSpec { entries }
}
