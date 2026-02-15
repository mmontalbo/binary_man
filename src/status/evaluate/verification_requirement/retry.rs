//! Retry count tracking for behavior verification.
//!
//! Tracks how many times scenarios have been retried to detect stalled progress
//! and enforce retry caps.

use crate::enrich;
use crate::scenarios;
use crate::verification_progress::{
    outputs_equal_delta_signature, scenario_id_from_evidence_path, VerificationProgress,
};

use super::LedgerEntries;

/// Maximum retries before giving up on outputs_equal scenarios.
pub(super) const BEHAVIOR_RERUN_CAP: usize = 2;

/// Get the preferred behavior scenario ID from a verification entry.
pub(super) fn preferred_behavior_scenario_id(
    entry: &scenarios::VerificationEntry,
) -> Option<String> {
    entry
        .behavior_unverified_scenario_id
        .as_deref()
        .into_iter()
        .chain(entry.behavior_scenario_ids.iter().map(String::as_str))
        .map(str::trim)
        .find(|scenario_id| !scenario_id.is_empty())
        .map(str::to_string)
}

/// Extract scenario ID from an evidence file by reading its JSON.
fn scenario_id_from_evidence_file(paths: &enrich::DocPackPaths, rel_path: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct ScenarioEvidenceId {
        scenario_id: Option<String>,
    }

    let rel_path = rel_path.trim();
    if rel_path.is_empty() {
        return None;
    }
    let bytes = std::fs::read(paths.root().join(rel_path)).ok()?;
    serde_json::from_slice::<ScenarioEvidenceId>(&bytes)
        .ok()
        .and_then(|parsed| parsed.scenario_id)
        .map(|scenario_id| scenario_id.trim().to_string())
        .filter(|scenario_id| !scenario_id.is_empty())
}

/// Count historical retries for a verification entry by matching evidence paths.
fn historical_retry_count_for_entry(
    paths: &enrich::DocPackPaths,
    entry: &scenarios::VerificationEntry,
) -> Option<usize> {
    let scenario_id = preferred_behavior_scenario_id(entry)?;
    let mut evidence_paths: std::collections::BTreeSet<String> = entry
        .delta_evidence_paths
        .iter()
        .map(|path| path.trim())
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect();
    if evidence_paths.is_empty() {
        evidence_paths.extend(
            entry
                .behavior_scenario_paths
                .iter()
                .map(|path| path.trim())
                .filter(|path| !path.is_empty())
                .map(str::to_string),
        );
    }
    let matching_runs = evidence_paths
        .into_iter()
        .filter(|path| {
            scenario_id_from_evidence_path(path)
                .or_else(|| scenario_id_from_evidence_file(paths, path))
                .is_some_and(|path_scenario_id| path_scenario_id == scenario_id)
        })
        .count();
    (matching_runs > 0).then_some(matching_runs.saturating_sub(1))
}

/// Get outputs_equal retry count for a surface ID from progress tracking.
fn outputs_equal_retry_count_for_surface_id(
    progress: &VerificationProgress,
    surface_id: &str,
    delta_signature: &str,
) -> usize {
    progress
        .outputs_equal_retries_by_surface
        .get(surface_id)
        .filter(|entry| entry.delta_signature.as_deref().unwrap_or_default() == delta_signature)
        .map_or(0, |entry| entry.retry_count)
}

/// Load historical retry counts from evidence paths.
fn load_historical_behavior_retry_counts(
    paths: &enrich::DocPackPaths,
    ledger_entries: &LedgerEntries,
) -> std::collections::BTreeMap<String, usize> {
    let mut retry_counts = std::collections::BTreeMap::new();
    for (surface_id, entry) in ledger_entries {
        if let Some(retry_count) = historical_retry_count_for_entry(paths, entry) {
            retry_counts.insert(surface_id.clone(), retry_count);
        }
    }
    retry_counts
}

/// Load combined retry counts from both historical evidence and progress tracking.
pub(super) fn load_behavior_retry_counts(
    paths: &enrich::DocPackPaths,
    ledger_entries: &LedgerEntries,
    progress: &VerificationProgress,
    outputs_equal_surface_ids: &[String],
) -> std::collections::BTreeMap<String, usize> {
    let mut retry_counts = load_historical_behavior_retry_counts(paths, ledger_entries);
    for surface_id in super::normalize_target_ids(outputs_equal_surface_ids) {
        let delta_signature =
            outputs_equal_delta_signature(ledger_entries.get(surface_id.as_str()));
        let retry_count =
            outputs_equal_retry_count_for_surface_id(progress, &surface_id, &delta_signature);
        if retry_count == 0 {
            retry_counts.remove(&surface_id);
        } else {
            retry_counts.insert(surface_id, retry_count);
        }
    }
    retry_counts
}

/// Get the maximum retry count among target IDs.
pub(super) fn max_retry_count(
    target_ids: &[String],
    retry_counts: &std::collections::BTreeMap<String, usize>,
) -> Option<usize> {
    target_ids
        .iter()
        .filter_map(|surface_id| retry_counts.get(surface_id).copied())
        .max()
}

/// Partition surface IDs by whether they've hit the retry cap.
pub(super) fn partition_cap_hit(
    surface_ids: Vec<String>,
    retry_counts: &std::collections::BTreeMap<String, usize>,
) -> (Vec<String>, Vec<String>) {
    surface_ids.into_iter().partition(|surface_id| {
        retry_counts.get(surface_id).copied().unwrap_or(0) >= BEHAVIOR_RERUN_CAP
    })
}
