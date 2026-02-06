//! Lock snapshot helpers for deterministic planning and apply.
//!
//! The lock ties plan/apply to an exact set of inputs so status can detect
//! staleness without guessing.
use super::config::{resolve_inputs, validate_config};
use super::paths::rel_path;
use super::{DocPackPaths, EnrichConfig, EnrichLock, LockStatus, LOCK_SCHEMA_VERSION};
use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Load the lock snapshot from disk.
pub fn load_lock(doc_pack_root: &Path) -> Result<EnrichLock> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.lock_path();
    let bytes = fs::read(&path).with_context(|| format!("read lock {}", path.display()))?;
    let lock: EnrichLock = serde_json::from_slice(&bytes).context("parse enrich lock JSON")?;
    Ok(lock)
}

/// Persist the lock snapshot in a stable JSON format.
pub fn write_lock(doc_pack_root: &Path, lock: &EnrichLock) -> Result<()> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.lock_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create enrich dir")?;
    }
    let text = serde_json::to_string_pretty(lock).context("serialize enrich lock")?;
    fs::write(&path, text.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Build a lock snapshot from the current config and required inputs.
///
/// The input hash captures both scenario inputs and pack-derived artifacts
/// so later steps can detect drift.
pub fn build_lock(
    doc_pack_root: &Path,
    config: &EnrichConfig,
    binary_name: Option<&str>,
) -> Result<EnrichLock> {
    validate_config(config)?;
    let mut inputs = resolve_inputs(config, doc_pack_root)?;
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    inputs.push(paths.config_path());
    inputs.push(paths.semantics_path());
    inputs.push(paths.surface_overlays_path());
    inputs.push(paths.pack_manifest_path());
    if doc_pack_root.join("fixtures").is_dir() {
        inputs.push(doc_pack_root.join("fixtures"));
    }
    inputs.sort();
    inputs.dedup();
    let inputs_hash = hash_paths(doc_pack_root, &inputs)?;
    let inputs_rel = inputs
        .iter()
        .map(|path| rel_path(doc_pack_root, path))
        .collect::<Result<Vec<_>>>()?;
    Ok(EnrichLock {
        schema_version: LOCK_SCHEMA_VERSION,
        generated_at_epoch_ms: now_epoch_ms()?,
        binary_name: binary_name.map(|name| name.to_string()),
        config_path: rel_path(doc_pack_root, &paths.config_path())?,
        inputs: inputs_rel,
        inputs_hash,
    })
}

/// Compute lock presence and staleness against current inputs.
pub fn lock_status(doc_pack_root: &Path, lock: Option<&EnrichLock>) -> Result<LockStatus> {
    let Some(lock) = lock else {
        return Ok(LockStatus {
            present: false,
            stale: false,
            inputs_hash: None,
        });
    };
    let input_paths = lock
        .inputs
        .iter()
        .map(|rel| doc_pack_root.join(rel))
        .collect::<Vec<_>>();
    let current_hash = hash_paths(doc_pack_root, &input_paths)?;
    let stale = current_hash != lock.inputs_hash;
    Ok(LockStatus {
        present: true,
        stale,
        inputs_hash: Some(lock.inputs_hash.clone()),
    })
}

/// Hash a list of paths deterministically for staleness detection.
pub fn hash_paths(doc_pack_root: &Path, paths: &[PathBuf]) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut sorted = paths.to_vec();
    sorted.sort();
    for path in sorted {
        hash_path(&mut hasher, doc_pack_root, &path)?;
    }
    let digest = hasher.finalize();
    Ok(format!("{:x}", digest))
}

/// Current epoch time in milliseconds for artifact timestamps.
pub fn now_epoch_ms() -> Result<u128> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("compute timestamp")?
        .as_millis())
}

fn hash_path(hasher: &mut Sha256, root: &Path, path: &Path) -> Result<()> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    if !path.exists() {
        hasher.update(b"missing:");
        hasher.update(rel.to_string_lossy().as_bytes());
        return Ok(());
    }
    let meta = fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        hasher.update(b"symlink:");
        hasher.update(rel.to_string_lossy().as_bytes());
        let target = fs::read_link(path).with_context(|| format!("read {}", path.display()))?;
        hasher.update(target.to_string_lossy().as_bytes());
        return Ok(());
    }
    if file_type.is_dir() {
        hasher.update(b"dir:");
        hasher.update(rel.to_string_lossy().as_bytes());
        let mut entries: Vec<_> = fs::read_dir(path)
            .with_context(|| format!("read {}", path.display()))?
            .filter_map(|entry| entry.ok())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            hash_path(hasher, root, &entry.path())?;
        }
        return Ok(());
    }
    if file_type.is_file() {
        hasher.update(b"file:");
        hasher.update(rel.to_string_lossy().as_bytes());
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        if is_binary_lens_manifest_path(path) {
            if let Some(stable_bytes) = stable_binary_lens_manifest_bytes(&bytes) {
                hasher.update(b":stable_manifest:");
                hasher.update(&stable_bytes);
                return Ok(());
            }
        }
        hasher.update(&bytes);
        return Ok(());
    }
    Ok(())
}

fn is_binary_lens_manifest_path(path: &Path) -> bool {
    path.file_name() == Some(OsStr::new("manifest.json"))
        && path
            .parent()
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| name == OsStr::new("binary.lens"))
}

fn stable_binary_lens_manifest_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut manifest: Value = serde_json::from_slice(bytes).ok()?;
    if let Some(digest) = manifest
        .get("export_config_digest")
        .and_then(|v| v.as_str())
    {
        return serde_json::to_vec(&serde_json::json!({ "export_config_digest": digest })).ok();
    }

    if let Some(obj) = manifest.as_object_mut() {
        obj.remove("created_at");
        obj.remove("created_at_epoch_seconds");
        obj.remove("created_at_source");
        obj.remove("coverage_summary");
    }
    serde_json::to_vec(&manifest).ok()
}
