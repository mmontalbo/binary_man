use crate::enrich;
use crate::scenarios;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) const VERIFICATION_PROGRESS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct VerificationProgress {
    pub(crate) schema_version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) outputs_equal_retries_by_surface: BTreeMap<String, OutputsEqualRetryProgressEntry>,
}

impl Default for VerificationProgress {
    fn default() -> Self {
        Self {
            schema_version: VERIFICATION_PROGRESS_SCHEMA_VERSION,
            outputs_equal_retries_by_surface: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub(crate) struct OutputsEqualRetryProgressEntry {
    #[serde(default)]
    pub(crate) retry_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delta_signature: Option<String>,
}

pub(crate) fn load_verification_progress(paths: &enrich::DocPackPaths) -> VerificationProgress {
    let path = paths.verification_progress_path();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return VerificationProgress::default(),
    };
    let parsed: VerificationProgress = match serde_json::from_slice(&bytes) {
        Ok(progress) => progress,
        Err(_) => return VerificationProgress::default(),
    };
    if parsed.schema_version != VERIFICATION_PROGRESS_SCHEMA_VERSION {
        return VerificationProgress::default();
    }
    parsed
}

pub(crate) fn write_verification_progress(
    paths: &enrich::DocPackPaths,
    progress: &VerificationProgress,
) -> Result<()> {
    let mut serializable = progress.clone();
    serializable.schema_version = VERIFICATION_PROGRESS_SCHEMA_VERSION;
    let text =
        serde_json::to_string_pretty(&serializable).context("serialize verification progress")?;
    let path = paths.verification_progress_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir {}", parent.display()))?;
    }
    std::fs::write(&path, text.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub(crate) fn outputs_equal_delta_signature(
    entry: Option<&scenarios::VerificationEntry>,
) -> String {
    let mut evidence_paths = entry
        .into_iter()
        .flat_map(|entry| {
            entry
                .delta_evidence_paths
                .iter()
                .map(String::as_str)
                .chain(entry.behavior_scenario_paths.iter().map(String::as_str))
        })
        .filter_map(outputs_equal_signature_token)
        .collect::<Vec<_>>();
    evidence_paths.sort();
    evidence_paths.dedup();
    evidence_paths.join("|")
}

pub(crate) fn outputs_equal_signature_token(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(scenario_id) = scenario_id_from_evidence_path(trimmed) {
        return Some(format!("scenario:{scenario_id}"));
    }
    Some(trimmed.to_string())
}

pub(crate) fn scenario_id_from_evidence_path(path: &str) -> Option<String> {
    let filename = Path::new(path.trim()).file_name()?.to_str()?;
    let stem = filename.strip_suffix(".json")?;
    let (scenario_id, epoch_suffix) = stem.rsplit_once('-')?;
    epoch_suffix
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| scenario_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_doc_pack_root(name: &str) -> std::path::PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{name}-{}-{now}", std::process::id()));
        std::fs::create_dir_all(root.join("inventory")).expect("create inventory");
        root
    }

    fn verification_entry(delta_path: &str) -> scenarios::VerificationEntry {
        scenarios::VerificationEntry {
            surface_id: "--color".to_string(),
            status: "verified".to_string(),
            behavior_status: "unverified".to_string(),
            behavior_exclusion_reason_code: None,
            behavior_unverified_reason_code: Some("outputs_equal".to_string()),
            behavior_unverified_scenario_id: Some("verify_color".to_string()),
            behavior_unverified_assertion_kind: None,
            behavior_unverified_assertion_seed_path: None,
            behavior_unverified_assertion_token: None,
            scenario_ids: Vec::new(),
            scenario_paths: Vec::new(),
            behavior_scenario_ids: vec!["verify_color".to_string()],
            behavior_assertion_scenario_ids: Vec::new(),
            behavior_scenario_paths: vec![delta_path.to_string()],
            delta_outcome: Some("outputs_equal".to_string()),
            delta_evidence_paths: vec![delta_path.to_string()],
            behavior_confounded_scenario_ids: Vec::new(),
            behavior_confounded_extra_surface_ids: Vec::new(),
            evidence: Vec::new(),
        }
    }

    #[test]
    fn load_verification_progress_defaults_on_missing_invalid_or_schema_mismatch() {
        let root = temp_doc_pack_root("bman-progress-load-defaults");
        let paths = enrich::DocPackPaths::new(root.clone());
        let progress_path = paths.verification_progress_path();

        let missing = load_verification_progress(&paths);
        assert_eq!(missing.schema_version, VERIFICATION_PROGRESS_SCHEMA_VERSION);
        assert!(missing.outputs_equal_retries_by_surface.is_empty());

        std::fs::write(&progress_path, b"{not-json").expect("write invalid");
        let invalid = load_verification_progress(&paths);
        assert_eq!(invalid.schema_version, VERIFICATION_PROGRESS_SCHEMA_VERSION);
        assert!(invalid.outputs_equal_retries_by_surface.is_empty());

        std::fs::write(
            &progress_path,
            br#"{"schema_version":99,"outputs_equal_retries_by_surface":{"--color":{"retry_count":5}}}"#,
        )
        .expect("write schema mismatch");
        let mismatch = load_verification_progress(&paths);
        assert_eq!(
            mismatch.schema_version,
            VERIFICATION_PROGRESS_SCHEMA_VERSION
        );
        assert!(mismatch.outputs_equal_retries_by_surface.is_empty());

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn outputs_equal_signature_normalization_is_stable_across_timestamped_evidence_paths() {
        let entry_100 = verification_entry("inventory/scenarios/verify_color-100.json");
        let entry_300 = verification_entry("inventory/scenarios/verify_color-300.json");
        assert_eq!(
            outputs_equal_delta_signature(Some(&entry_100)),
            outputs_equal_delta_signature(Some(&entry_300))
        );
        assert_eq!(
            outputs_equal_delta_signature(Some(&entry_100)),
            "scenario:verify_color"
        );
    }

    #[test]
    fn write_verification_progress_serializes_deterministically() {
        let root = temp_doc_pack_root("bman-progress-deterministic-write");
        let paths = enrich::DocPackPaths::new(root.clone());
        let mut progress = VerificationProgress::default();
        progress.outputs_equal_retries_by_surface.insert(
            "--zeta".to_string(),
            OutputsEqualRetryProgressEntry {
                retry_count: 2,
                delta_signature: Some("scenario:verify_zeta".to_string()),
            },
        );
        progress.outputs_equal_retries_by_surface.insert(
            "--alpha".to_string(),
            OutputsEqualRetryProgressEntry {
                retry_count: 1,
                delta_signature: Some("scenario:verify_alpha".to_string()),
            },
        );
        write_verification_progress(&paths, &progress).expect("write progress");
        let serialized = std::fs::read_to_string(paths.verification_progress_path())
            .expect("read serialized progress");
        let expected = r#"{
  "schema_version": 1,
  "outputs_equal_retries_by_surface": {
    "--alpha": {
      "retry_count": 1,
      "delta_signature": "scenario:verify_alpha"
    },
    "--zeta": {
      "retry_count": 2,
      "delta_signature": "scenario:verify_zeta"
    }
  }
}"#;
        assert_eq!(serialized, expected);

        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
