use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::summary::Summary;

fn baselines_dir() -> PathBuf {
    PathBuf::from("tools/baselines")
}

fn eval_data_dir() -> PathBuf {
    PathBuf::from("tools/eval_data")
}

/// Tag the most recent eval results as a named baseline.
pub fn tag(pack_name: &str, name: &str) -> Result<()> {
    let pack_dir = eval_data_dir().join(pack_name);
    if !pack_dir.exists() {
        anyhow::bail!("no eval data found for pack '{}'", pack_name);
    }

    // Find the most recent commit directory by modification time
    let mut commit_dirs: Vec<_> = std::fs::read_dir(&pack_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .collect();

    if commit_dirs.is_empty() {
        anyhow::bail!("no eval data directories in {}", pack_dir.display());
    }

    commit_dirs.sort_by(|a, b| {
        let a_time = a
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let b_time = b
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        b_time.cmp(&a_time)
    });

    let commit_dir = commit_dirs[0].path();
    let commit_name = commit_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let summary_path = commit_dir.join("summary.json");
    if !summary_path.exists() {
        anyhow::bail!("no summary.json in {}", commit_dir.display());
    }

    let summary_raw = std::fs::read_to_string(&summary_path)
        .with_context(|| format!("read {}", summary_path.display()))?;
    let summary: Summary = serde_json::from_str(&summary_raw).context("parse summary.json")?;

    // Load or create baselines file
    let baselines_path = baselines_dir();
    std::fs::create_dir_all(&baselines_path)?;
    let file_path = baselines_path.join(format!("{}.json", pack_name));

    let mut data: serde_json::Value = if file_path.exists() {
        let raw = std::fs::read_to_string(&file_path)?;
        serde_json::from_str(&raw)?
    } else {
        serde_json::json!({"baselines": {}})
    };

    data["baselines"][name] = serde_json::json!({
        "name": name,
        "commit": commit_name,
        "summary": summary,
    });

    let json = serde_json::to_string_pretty(&data)?;
    std::fs::write(&file_path, format!("{json}\n"))?;

    eprintln!(
        "Tagged baseline '{}' for {} @ {}",
        name, pack_name, commit_name
    );
    Ok(())
}

/// Load a baseline or prior eval for comparison.
///
/// Tries: baseline name, then exact commit hash, then commit prefix match.
pub fn load(pack_name: &str, reference: &str) -> Result<Summary> {
    // Try as baseline name
    let baseline_path = baselines_dir().join(format!("{}.json", pack_name));
    if baseline_path.exists() {
        let raw = std::fs::read_to_string(&baseline_path)?;
        let data: serde_json::Value = serde_json::from_str(&raw)?;
        if let Some(entry) = data["baselines"].get(reference) {
            let summary: Summary = serde_json::from_value(entry["summary"].clone())
                .context("parse baseline summary")?;
            return Ok(summary);
        }
    }

    // Try as commit hash (exact or prefix)
    let pack_dir = eval_data_dir().join(pack_name);
    if pack_dir.exists() {
        // Exact match
        let commit_dir = pack_dir.join(reference);
        if commit_dir.exists() {
            return load_summary_from_dir(&commit_dir);
        }

        // Prefix match
        let matches: Vec<_> = std::fs::read_dir(&pack_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_ok_and(|t| t.is_dir())
                    && e.file_name().to_string_lossy().starts_with(reference)
            })
            .collect();

        if matches.len() == 1 {
            return load_summary_from_dir(&matches[0].path());
        }
        if matches.len() > 1 {
            let names: Vec<_> = matches
                .iter()
                .map(|m| m.file_name().to_string_lossy().to_string())
                .collect();
            anyhow::bail!(
                "ambiguous commit prefix '{}': {:?}",
                reference,
                names
            );
        }
    }

    anyhow::bail!(
        "comparison ref '{}' not found (tried baseline name and commit hash)",
        reference
    )
}

fn load_summary_from_dir(dir: &std::path::Path) -> Result<Summary> {
    let summary_path = dir.join("summary.json");
    if !summary_path.exists() {
        anyhow::bail!("no summary.json in {}", dir.display());
    }
    let raw = std::fs::read_to_string(&summary_path)
        .with_context(|| format!("read {}", summary_path.display()))?;
    serde_json::from_str(&raw).context("parse summary.json")
}
