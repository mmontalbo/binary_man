use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub fn write_staged_bytes(staging_root: &Path, rel_path: &str, bytes: &[u8]) -> Result<()> {
    let staging_path = staging_root.join(rel_path);
    if let Some(parent) = staging_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&staging_path, bytes).with_context(|| format!("write {}", staging_path.display()))?;
    Ok(())
}

pub fn write_staged_text(staging_root: &Path, rel_path: &str, text: &str) -> Result<()> {
    write_staged_bytes(staging_root, rel_path, text.as_bytes())?;
    Ok(())
}

pub fn write_staged_json<T: serde::Serialize>(
    staging_root: &Path,
    rel_path: &str,
    value: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).context("serialize staged JSON")?;
    write_staged_bytes(staging_root, rel_path, &bytes)?;
    Ok(())
}

pub fn publish_staging(staging_root: &Path, doc_pack_root: &Path) -> Result<Vec<PathBuf>> {
    if !staging_root.exists() {
        return Ok(Vec::new());
    }
    let files = collect_files_recursive(staging_root)?;
    let txn_root = staging_root
        .parent()
        .ok_or_else(|| anyhow!("staging root has no parent"))?;
    let backup_root = txn_root.join("backup");
    fs::create_dir_all(&backup_root)
        .with_context(|| format!("create {}", backup_root.display()))?;
    let mut published = Vec::new();
    let mut backups: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut created: Vec<PathBuf> = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(staging_root)
            .context("strip staging prefix")?;
        let dest = doc_pack_root.join(rel);
        if dest.exists() {
            let backup = backup_root.join(rel);
            if let Some(parent) = backup.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            fs::rename(&dest, &backup)
                .or_else(|_| fs::copy(&dest, &backup).map(|_| ()))
                .with_context(|| format!("backup {}", dest.display()))?;
            backups.push((dest.clone(), backup));
        } else {
            created.push(dest.clone());
        }

        if let Err(err) = publish_file(&file, &dest) {
            rollback_publish(&published, &backups, &created)?;
            return Err(err);
        }
        published.push(dest);
    }
    Ok(published)
}

pub fn collect_files_recursive(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.exists() {
        return Ok(files);
    }
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            files.extend(collect_files_recursive(&path)?);
        } else if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn publish_file(source: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let file_name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("staged");
    let tmp_path = dest
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{file_name}.tmp"));
    fs::copy(source, &tmp_path).with_context(|| format!("publish {}", dest.display()))?;
    fs::rename(&tmp_path, dest).with_context(|| format!("publish {}", dest.display()))?;
    Ok(())
}

fn rollback_publish(
    published: &[PathBuf],
    backups: &[(PathBuf, PathBuf)],
    created: &[PathBuf],
) -> Result<()> {
    for path in published {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
    for path in created {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
    for (dest, backup) in backups {
        if let Some(parent) = dest.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::rename(backup, dest).or_else(|_| fs::copy(backup, dest).map(|_| ()));
    }
    Ok(())
}
