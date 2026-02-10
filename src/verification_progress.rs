use crate::enrich;
use crate::scenarios;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) const VERIFICATION_PROGRESS_SCHEMA_VERSION: u32 = 1;

/// Signature for detecting no-op edit loops. Captures the essential identity
/// of a next-action to detect repeated identical edits with no evidence change.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct ActionSignature {
    /// The reason code (e.g., "assertion_failed", "outputs_equal")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason_code: Option<String>,
    /// The target surface ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) target_id: Option<String>,
    /// Hash of the edit content (deterministic)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) content_hash: Option<String>,
    /// Fingerprint of current evidence state
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence_fingerprint: Option<String>,
}

impl ActionSignature {
    pub(crate) fn is_empty(&self) -> bool {
        self.reason_code.is_none()
            && self.target_id.is_none()
            && self.content_hash.is_none()
            && self.evidence_fingerprint.is_none()
    }

    /// Returns true if this signature matches another (same content, same evidence).
    pub(crate) fn matches(&self, other: &ActionSignature) -> bool {
        self.reason_code == other.reason_code
            && self.target_id == other.target_id
            && self.content_hash == other.content_hash
            && self.evidence_fingerprint == other.evidence_fingerprint
    }
}

/// Progress entry for assertion_failed loop tracking.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub(crate) struct AssertionFailedProgressEntry {
    /// Number of no-progress edit attempts
    #[serde(default)]
    pub(crate) no_progress_count: usize,
    /// Last emitted action signature
    #[serde(default, skip_serializing_if = "ActionSignature::is_empty")]
    pub(crate) last_signature: ActionSignature,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct VerificationProgress {
    pub(crate) schema_version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) outputs_equal_retries_by_surface: BTreeMap<String, OutputsEqualRetryProgressEntry>,
    /// Loop tracking for assertion_failed reason code
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) assertion_failed_by_surface: BTreeMap<String, AssertionFailedProgressEntry>,
}

impl Default for VerificationProgress {
    fn default() -> Self {
        Self {
            schema_version: VERIFICATION_PROGRESS_SCHEMA_VERSION,
            outputs_equal_retries_by_surface: BTreeMap::new(),
            assertion_failed_by_surface: BTreeMap::new(),
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

/// Compute a deterministic content hash for edit content.
/// Uses SHA-256 truncated to 16 hex chars for compactness.
pub(crate) fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let result = hasher.finalize();
    // Truncate to first 8 bytes (16 hex chars) for compactness
    format!("{:x}", result)[..16].to_string()
}

/// Build a deterministic evidence fingerprint for a target/scenario.
/// Normalizes to scenario IDs to avoid timestamp/path churn sensitivity.
pub(crate) fn evidence_fingerprint(entry: Option<&scenarios::VerificationEntry>) -> String {
    let entry = match entry {
        Some(e) => e,
        None => return String::new(),
    };

    let mut tokens = Vec::new();

    // Include behavior reason code
    if let Some(reason) = entry.behavior_unverified_reason_code.as_deref() {
        let reason = reason.trim();
        if !reason.is_empty() {
            tokens.push(format!("reason:{reason}"));
        }
    }

    // Include scenario ID
    if let Some(scenario_id) = entry.behavior_unverified_scenario_id.as_deref() {
        let scenario_id = scenario_id.trim();
        if !scenario_id.is_empty() {
            tokens.push(format!("scenario:{scenario_id}"));
        }
    }

    // Include assertion kind if present
    if let Some(kind) = entry.behavior_unverified_assertion_kind.as_deref() {
        let kind = kind.trim();
        if !kind.is_empty() {
            tokens.push(format!("assertion_kind:{kind}"));
        }
    }

    // Include normalized evidence paths (scenario IDs extracted)
    for path in &entry.delta_evidence_paths {
        if let Some(token) = outputs_equal_signature_token(path) {
            tokens.push(token);
        }
    }
    for path in &entry.behavior_scenario_paths {
        if let Some(token) = outputs_equal_signature_token(path) {
            tokens.push(token);
        }
    }

    tokens.sort();
    tokens.dedup();
    tokens.join("|")
}

/// Build an action signature for a candidate next action.
pub(crate) fn build_action_signature(
    reason_code: Option<&str>,
    target_id: &str,
    content: &str,
    entry: Option<&scenarios::VerificationEntry>,
) -> ActionSignature {
    ActionSignature {
        reason_code: reason_code.map(str::to_string),
        target_id: Some(target_id.to_string()),
        content_hash: Some(content_hash(content)),
        evidence_fingerprint: Some(evidence_fingerprint(entry)),
    }
}

/// Check if a candidate action is a no-op (identical to persisted signature).
pub(crate) fn is_noop_action(
    progress: &VerificationProgress,
    surface_id: &str,
    candidate: &ActionSignature,
) -> bool {
    if let Some(entry) = progress.assertion_failed_by_surface.get(surface_id) {
        if !entry.last_signature.is_empty() && entry.last_signature.matches(candidate) {
            return true;
        }
    }
    false
}

/// Get the no-progress count for a surface with assertion_failed.
pub(crate) fn get_assertion_failed_no_progress_count(
    progress: &VerificationProgress,
    surface_id: &str,
) -> usize {
    progress
        .assertion_failed_by_surface
        .get(surface_id)
        .map(|e| e.no_progress_count)
        .unwrap_or(0)
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

    fn assertion_failed_entry(scenario_id: &str) -> scenarios::VerificationEntry {
        scenarios::VerificationEntry {
            surface_id: "--color".to_string(),
            status: "verified".to_string(),
            behavior_status: "unverified".to_string(),
            behavior_exclusion_reason_code: None,
            behavior_unverified_reason_code: Some("assertion_failed".to_string()),
            behavior_unverified_scenario_id: Some(scenario_id.to_string()),
            behavior_unverified_assertion_kind: Some(
                "variant_stdout_contains_seed_path".to_string(),
            ),
            behavior_unverified_assertion_seed_path: Some("work/item.txt".to_string()),
            behavior_unverified_assertion_token: Some("item.txt".to_string()),
            scenario_ids: Vec::new(),
            scenario_paths: Vec::new(),
            behavior_scenario_ids: vec![scenario_id.to_string()],
            behavior_assertion_scenario_ids: Vec::new(),
            behavior_scenario_paths: vec![format!("inventory/scenarios/{scenario_id}-100.json")],
            delta_outcome: Some("outputs_differ".to_string()),
            delta_evidence_paths: vec![format!("inventory/scenarios/{scenario_id}-100.json")],
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
        assert!(missing.assertion_failed_by_surface.is_empty());

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

    #[test]
    fn content_hash_is_deterministic() {
        let content = r#"{"upsert_scenarios":[{"id":"verify_color"}]}"#;
        let hash1 = content_hash(content);
        let hash2 = content_hash(content);
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 16); // 8 bytes = 16 hex chars

        let different_content = r#"{"upsert_scenarios":[{"id":"verify_other"}]}"#;
        let hash3 = content_hash(different_content);
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn evidence_fingerprint_is_stable_across_timestamp_churn() {
        let entry_100 = assertion_failed_entry("verify_color");
        let mut entry_200 = assertion_failed_entry("verify_color");
        entry_200.behavior_scenario_paths =
            vec!["inventory/scenarios/verify_color-200.json".to_string()];
        entry_200.delta_evidence_paths =
            vec!["inventory/scenarios/verify_color-200.json".to_string()];

        let fp1 = evidence_fingerprint(Some(&entry_100));
        let fp2 = evidence_fingerprint(Some(&entry_200));
        assert_eq!(fp1, fp2);
        assert!(fp1.contains("reason:assertion_failed"));
        assert!(fp1.contains("scenario:verify_color"));
    }

    #[test]
    fn action_signature_matches_detects_identical_signatures() {
        let entry = assertion_failed_entry("verify_color");
        let content = r#"{"upsert_scenarios":[{"id":"verify_color"}]}"#;

        let sig1 =
            build_action_signature(Some("assertion_failed"), "--color", content, Some(&entry));
        let sig2 =
            build_action_signature(Some("assertion_failed"), "--color", content, Some(&entry));
        assert!(sig1.matches(&sig2));

        let different_content = r#"{"upsert_scenarios":[{"id":"verify_color","assertions":[]}]}"#;
        let sig3 = build_action_signature(
            Some("assertion_failed"),
            "--color",
            different_content,
            Some(&entry),
        );
        assert!(!sig1.matches(&sig3));
    }

    #[test]
    fn is_noop_action_detects_repeated_identical_edits() {
        let entry = assertion_failed_entry("verify_color");
        let content = r#"{"upsert_scenarios":[{"id":"verify_color"}]}"#;
        let candidate =
            build_action_signature(Some("assertion_failed"), "--color", content, Some(&entry));

        let mut progress = VerificationProgress::default();
        assert!(!is_noop_action(&progress, "--color", &candidate));

        progress.assertion_failed_by_surface.insert(
            "--color".to_string(),
            AssertionFailedProgressEntry {
                no_progress_count: 1,
                last_signature: candidate.clone(),
            },
        );
        assert!(is_noop_action(&progress, "--color", &candidate));

        let different_content = r#"{"upsert_scenarios":[{"id":"verify_color","assertions":[]}]}"#;
        let different_candidate = build_action_signature(
            Some("assertion_failed"),
            "--color",
            different_content,
            Some(&entry),
        );
        assert!(!is_noop_action(&progress, "--color", &different_candidate));
    }

    #[test]
    fn no_progress_count_tracks_retry_attempts() {
        let mut progress = VerificationProgress::default();
        assert_eq!(
            get_assertion_failed_no_progress_count(&progress, "--color"),
            0
        );

        progress.assertion_failed_by_surface.insert(
            "--color".to_string(),
            AssertionFailedProgressEntry {
                no_progress_count: 2,
                last_signature: ActionSignature::default(),
            },
        );
        assert_eq!(
            get_assertion_failed_no_progress_count(&progress, "--color"),
            2
        );
    }

    #[test]
    fn assertion_failed_progress_serializes_with_tolerant_reads() {
        let root = temp_doc_pack_root("bman-progress-assertion-failed");
        let paths = enrich::DocPackPaths::new(root.clone());

        let mut progress = VerificationProgress::default();
        progress.assertion_failed_by_surface.insert(
            "--color".to_string(),
            AssertionFailedProgressEntry {
                no_progress_count: 2,
                last_signature: ActionSignature {
                    reason_code: Some("assertion_failed".to_string()),
                    target_id: Some("--color".to_string()),
                    content_hash: Some("abc123".to_string()),
                    evidence_fingerprint: Some(
                        "reason:assertion_failed|scenario:verify_color".to_string(),
                    ),
                },
            },
        );
        write_verification_progress(&paths, &progress).expect("write progress");

        let loaded = load_verification_progress(&paths);
        assert_eq!(loaded.assertion_failed_by_surface.len(), 1);
        let entry = loaded.assertion_failed_by_surface.get("--color").unwrap();
        assert_eq!(entry.no_progress_count, 2);
        assert_eq!(entry.last_signature.content_hash.as_deref(), Some("abc123"));

        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
