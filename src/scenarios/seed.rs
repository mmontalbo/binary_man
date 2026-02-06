use crate::enrich;
use anyhow::{anyhow, Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::{
    ScenarioSeedEntry, ScenarioSeedSpec, SeedEntryKind, MAX_SEED_ENTRIES, MAX_SEED_TOTAL_BYTES,
};

pub(crate) const DEFAULT_BEHAVIOR_SEED_DIR: &str = "work";
const DEFAULT_BEHAVIOR_SEED_FILE1: &str = "work/file1.txt";
const DEFAULT_BEHAVIOR_SEED_FILE2: &str = "work/file2";
const DEFAULT_BEHAVIOR_SEED_SUBDIR: &str = "work/subdir";
const DEFAULT_BEHAVIOR_SEED_NESTED: &str = "work/subdir/nested.txt";
const DEFAULT_BEHAVIOR_SEED_LINK: &str = "work/link";

pub(crate) struct MaterializedSeed {
    pub(crate) rel_path: String,
    pub(crate) _abs_path: PathBuf,
}

pub(crate) fn materialize_inline_seed(
    staging_root: &Path,
    run_root: &Path,
    scenario_id: &str,
    seed: &ScenarioSeedSpec,
) -> Result<MaterializedSeed> {
    validate_seed_spec(seed).with_context(|| format!("validate seed for {scenario_id}"))?;
    let now = enrich::now_epoch_ms()?;
    let txn_root = staging_root
        .parent()
        .ok_or_else(|| anyhow!("staging root has no parent"))?;
    let seed_root = txn_root
        .join("scratch")
        .join("seeds")
        .join(format!("{scenario_id}-{now}"));
    fs::create_dir_all(&seed_root)
        .with_context(|| format!("create seed root {}", seed_root.display()))?;

    let mut seen = HashSet::new();
    let mut total_bytes = 0usize;

    for entry in &seed.entries {
        let rel_path = normalize_seed_path(&entry.path)
            .with_context(|| format!("seed entry path {:?}", entry.path))?;
        if !seen.insert(rel_path.clone()) {
            return Err(anyhow!("seed entry path {:?} is duplicated", rel_path));
        }
        let target_path = seed_root.join(&rel_path);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        match entry.kind {
            SeedEntryKind::Dir => {
                if entry.contents.is_some() {
                    return Err(anyhow!("seed dir {:?} must not include contents", rel_path));
                }
                if entry.target.is_some() {
                    return Err(anyhow!("seed dir {:?} must not include target", rel_path));
                }
                fs::create_dir_all(&target_path)
                    .with_context(|| format!("create dir {}", target_path.display()))?;
                apply_seed_mode(&target_path, entry.mode)?;
            }
            SeedEntryKind::File => {
                if entry.target.is_some() {
                    return Err(anyhow!("seed file {:?} must not include target", rel_path));
                }
                let contents = entry.contents.as_deref().unwrap_or("");
                total_bytes = total_bytes
                    .checked_add(contents.len())
                    .ok_or_else(|| anyhow!("seed size overflow"))?;
                if total_bytes > MAX_SEED_TOTAL_BYTES {
                    return Err(anyhow!(
                        "seed exceeds max total bytes ({MAX_SEED_TOTAL_BYTES})"
                    ));
                }
                fs::write(&target_path, contents.as_bytes())
                    .with_context(|| format!("write {}", target_path.display()))?;
                apply_seed_mode(&target_path, entry.mode)?;
            }
            SeedEntryKind::Symlink => {
                if entry.contents.is_some() {
                    return Err(anyhow!(
                        "seed symlink {:?} must not include contents",
                        rel_path
                    ));
                }
                let target = entry
                    .target
                    .as_ref()
                    .ok_or_else(|| anyhow!("seed symlink {:?} missing target", rel_path))?;
                let target_rel = normalize_seed_path(target)
                    .with_context(|| format!("symlink target {target:?}"))?;
                total_bytes = total_bytes
                    .checked_add(target_rel.len())
                    .ok_or_else(|| anyhow!("seed size overflow"))?;
                if total_bytes > MAX_SEED_TOTAL_BYTES {
                    return Err(anyhow!(
                        "seed exceeds max total bytes ({MAX_SEED_TOTAL_BYTES})"
                    ));
                }
                apply_seed_symlink(&target_rel, &target_path)?;
            }
        }
    }

    let rel_path = seed_root
        .strip_prefix(run_root)
        .with_context(|| format!("seed root {} outside run root", seed_root.display()))?
        .to_string_lossy()
        .to_string();

    Ok(MaterializedSeed {
        rel_path,
        _abs_path: seed_root,
    })
}

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

fn apply_seed_mode(path: &Path, mode: Option<u32>) -> Result<()> {
    let Some(mode) = mode else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .with_context(|| format!("inspect {}", path.display()))?
            .permissions();
        perms.set_mode(mode);
        fs::set_permissions(path, perms)
            .with_context(|| format!("set permissions on {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        return Err(anyhow!("seed mode is unsupported on this platform"));
    }
    Ok(())
}

fn apply_seed_symlink(target: &str, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, path)
            .with_context(|| format!("create symlink {}", path.display()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        let _ = path;
        Err(anyhow!("seed symlinks are unsupported on this platform"))
    }
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
