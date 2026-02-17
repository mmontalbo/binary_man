//! Progress tracking for the apply loop.
//!
//! Tracks LM failures, no-progress cycles, and auto-exclusion of stuck surfaces.

use crate::enrich;
use crate::scenarios;
use crate::surface;
use crate::surface::{build_exclusion_evidence, build_exclusion_note, derive_reason_code};
use crate::verification_progress::{
    load_verification_progress, outputs_equal_delta_signature, write_verification_progress,
};
use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// Result of checking progress between cycles
pub(super) enum CycleProgress {
    /// Made progress, reset counter
    Advanced,
    /// No progress, continue with updated count
    Stalled { count: usize },
    /// Hit max no-progress limit, should stop
    HitLimit { count: usize },
}

/// Check if we made progress compared to last cycle
pub(super) fn check_progress(
    current_unverified: usize,
    last_unverified: Option<usize>,
    no_progress_count: usize,
    max_no_progress: usize,
) -> CycleProgress {
    match last_unverified {
        Some(last) if current_unverified >= last => {
            let new_count = no_progress_count + 1;
            if new_count >= max_no_progress {
                CycleProgress::HitLimit { count: new_count }
            } else {
                CycleProgress::Stalled { count: new_count }
            }
        }
        _ => CycleProgress::Advanced,
    }
}

/// Result of processing LM invocation
pub(super) struct LmProcessingResult {
    /// Whether to increment no-progress counter
    pub increment_no_progress: bool,
    /// Scenario IDs that were updated and need rerun
    pub updated_scenario_ids: Vec<String>,
    /// Surfaces successfully processed by LM
    pub processed_surfaces: Vec<String>,
}

/// Process LM result and handle failures/successes
pub(super) fn process_lm_result(
    paths: &enrich::DocPackPaths,
    result: Result<(usize, Vec<String>)>,
    target_ids: &[String],
    current_targets: &BTreeSet<String>,
    max_lm_failures: usize,
    verbose: bool,
) -> LmProcessingResult {
    match result {
        Ok((applied_count, updated_scenario_ids)) => {
            if verbose {
                eprintln!("apply: LM applied {} responses", applied_count);
            }
            if applied_count == 0 {
                let auto_excluded =
                    handle_lm_failure_for_targets(paths, target_ids, max_lm_failures, verbose);
                if auto_excluded > 0 && verbose {
                    eprintln!("apply: auto-excluded {} stuck surface(s)", auto_excluded);
                }
                LmProcessingResult {
                    increment_no_progress: auto_excluded == 0,
                    updated_scenario_ids,
                    processed_surfaces: Vec::new(),
                }
            } else {
                clear_lm_failures_for_targets(paths, target_ids);
                LmProcessingResult {
                    increment_no_progress: false,
                    updated_scenario_ids,
                    processed_surfaces: current_targets.iter().cloned().collect(),
                }
            }
        }
        Err(err) => {
            eprintln!("apply: LM invocation failed: {}", err);
            LmProcessingResult {
                increment_no_progress: false,
                updated_scenario_ids: Vec::new(),
                processed_surfaces: Vec::new(),
            }
        }
    }
}

/// Handle LM failure for target surfaces.
/// Increments failure counts and auto-excludes surfaces that hit the cap.
/// Returns the number of surfaces that were auto-excluded.
pub(super) fn handle_lm_failure_for_targets(
    paths: &enrich::DocPackPaths,
    target_ids: &[String],
    max_failures: usize,
    verbose: bool,
) -> usize {
    let mut progress = load_verification_progress(paths);
    let mut to_auto_exclude = Vec::new();

    for surface_id in target_ids {
        let surface_id = surface_id.trim();
        if surface_id.is_empty() {
            continue;
        }
        let count = progress
            .lm_failures_by_surface
            .entry(surface_id.to_string())
            .or_insert(0);
        *count = count.saturating_add(1);
        if *count >= max_failures {
            to_auto_exclude.push(surface_id.to_string());
        }
    }

    // Persist the updated progress
    if let Err(err) = write_verification_progress(paths, &progress) {
        if verbose {
            eprintln!("warning: failed to persist LM failure counts: {}", err);
        }
    }

    // Auto-exclude surfaces that hit the cap
    let excluded_count = to_auto_exclude.len();
    if !to_auto_exclude.is_empty() {
        if let Err(err) = auto_exclude_stuck_surfaces(paths, &to_auto_exclude, verbose) {
            if verbose {
                eprintln!("warning: failed to auto-exclude stuck surfaces: {}", err);
            }
            return 0;
        }
        // Clear failure counts for excluded surfaces
        for surface_id in &to_auto_exclude {
            progress.lm_failures_by_surface.remove(surface_id);
        }
        let _ = write_verification_progress(paths, &progress);
    }

    excluded_count
}

/// Clear LM failure counts for successfully processed surfaces.
/// Note: This only clears lm_failures (when LM returns 0 applied).
/// The lm_no_progress counter is NOT cleared here because even if LM applies changes,
/// the surface might still be unverified. The no-progress counter is only cleared
/// when the surface is verified (stops appearing in target_ids).
pub(super) fn clear_lm_failures_for_targets(paths: &enrich::DocPackPaths, target_ids: &[String]) {
    let mut progress = load_verification_progress(paths);
    let mut changed = false;
    for surface_id in target_ids {
        let surface_id = surface_id.trim();
        if progress.lm_failures_by_surface.remove(surface_id).is_some() {
            changed = true;
        }
    }
    if changed {
        let _ = write_verification_progress(paths, &progress);
    }
}

/// Handle surfaces that were targeted by LM but are still unverified.
/// This catches the case where LM applies changes but they don't lead to verification.
/// Returns the number of surfaces that were auto-excluded.
pub(super) fn handle_lm_no_progress_for_targets(
    paths: &enrich::DocPackPaths,
    still_unverified: &[String],
    max_no_progress: usize,
    verbose: bool,
) -> usize {
    let mut progress = load_verification_progress(paths);
    let mut to_auto_exclude = Vec::new();

    for surface_id in still_unverified {
        let surface_id = surface_id.trim();
        if surface_id.is_empty() {
            continue;
        }
        let count = progress
            .lm_no_progress_by_surface
            .entry(surface_id.to_string())
            .or_insert(0);
        *count = count.saturating_add(1);
        if verbose {
            eprintln!(
                "apply: {} targeted {} time(s) without verification progress",
                surface_id, count
            );
        }
        if *count >= max_no_progress {
            to_auto_exclude.push(surface_id.to_string());
        }
    }

    // Persist the updated progress
    if let Err(err) = write_verification_progress(paths, &progress) {
        if verbose {
            eprintln!("warning: failed to persist LM no-progress counts: {}", err);
        }
    }

    // Auto-exclude surfaces that hit the cap
    let excluded_count = to_auto_exclude.len();
    if !to_auto_exclude.is_empty() {
        if let Err(err) = auto_exclude_stuck_surfaces(paths, &to_auto_exclude, verbose) {
            if verbose {
                eprintln!("warning: failed to auto-exclude stuck surfaces: {}", err);
            }
            return 0;
        }
        // Clear no-progress counts for excluded surfaces
        for surface_id in &to_auto_exclude {
            progress.lm_no_progress_by_surface.remove(surface_id);
        }
        let _ = write_verification_progress(paths, &progress);
    }

    excluded_count
}

/// Load cached verification ledger entries (best-effort).
/// Returns an empty map if the cache is unavailable or invalid.
fn load_cached_ledger_entries(
    paths: &enrich::DocPackPaths,
) -> BTreeMap<String, scenarios::VerificationEntry> {
    let cache_path = paths.root().join("inventory/verification_cache.json");
    let Ok(content) = fs::read_to_string(&cache_path) else {
        return BTreeMap::new();
    };

    #[derive(serde::Deserialize)]
    struct Cache {
        ledger: scenarios::VerificationLedger,
    }

    let Ok(cache) = serde_json::from_str::<Cache>(&content) else {
        return BTreeMap::new();
    };

    cache
        .ledger
        .entries
        .into_iter()
        .map(|e| (e.surface_id.clone(), e))
        .collect()
}

/// Auto-exclude surfaces that are stuck after repeated LM failures.
pub(super) fn auto_exclude_stuck_surfaces(
    paths: &enrich::DocPackPaths,
    surface_ids: &[String],
    verbose: bool,
) -> Result<()> {
    let overlays_path = paths.surface_overlays_path();

    // Load existing overlays or create new structure
    let mut overlays: serde_json::Value = if overlays_path.is_file() {
        serde_json::from_str(&fs::read_to_string(&overlays_path)?)?
    } else {
        serde_json::json!({
            "schema_version": 3,
            "items": [],
            "overlays": []
        })
    };

    let overlays_array = overlays["overlays"]
        .as_array_mut()
        .ok_or_else(|| anyhow!("overlays must be an array"))?;

    // Load scenarios to get delta evidence paths
    let scenarios_path = paths.scenarios_plan_path();
    let scenario_evidence: std::collections::BTreeMap<String, String> = if scenarios_path.is_file()
    {
        let plan = scenarios::load_plan(&scenarios_path, paths.root())?;
        let evidence_dir = paths.root().join("inventory").join("scenarios");
        plan.scenarios
            .iter()
            .filter(|s| !s.covers.is_empty())
            .filter_map(|s| {
                let surface_id = s.covers.first()?;
                let sanitized_id = s.id.replace([' ', '/'], "_");
                let evidence_rel = format!("inventory/scenarios/{sanitized_id}.json");
                evidence_dir
                    .join(format!("{sanitized_id}.json"))
                    .exists()
                    .then(|| (surface_id.clone(), evidence_rel))
            })
            .collect()
    } else {
        std::collections::BTreeMap::new()
    };

    // Load ledger entries for contextual notes
    let ledger_entries = load_cached_ledger_entries(paths);

    for surface_id in surface_ids {
        let surface_id = surface_id.trim();
        if surface_id.is_empty() {
            continue;
        }

        // Find or create overlay
        let idx = if let Some(idx) = overlays_array
            .iter()
            .position(|o| o["id"].as_str() == Some(surface_id))
        {
            idx
        } else {
            overlays_array.push(serde_json::json!({
                "id": surface_id,
                "kind": "option",
                "invocation": {}
            }));
            overlays_array.len() - 1
        };

        // Skip if already excluded
        if overlays_array[idx]["behavior_exclusion"].is_object() {
            continue;
        }

        // Get delta evidence path if available
        let delta_path = scenario_evidence
            .get(surface_id)
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "inventory/scenarios/verify_{}.json",
                    surface_id.replace('-', "_")
                )
            });

        // Use shared builders for reason_code, note, and evidence
        let entry = ledger_entries.get(surface_id);
        let reason_code = derive_reason_code(entry);
        let note = build_exclusion_note(entry);
        let evidence = build_exclusion_evidence(entry, &delta_path);

        overlays_array[idx]["behavior_exclusion"] = serde_json::json!({
            "reason_code": reason_code,
            "note": note,
            "evidence": evidence
        });

        if verbose {
            eprintln!("  auto-excluded {}: {}", surface_id, note);
        }
    }

    // Write updated overlays
    let overlays_json = serde_json::to_string_pretty(&overlays)?;
    fs::write(&overlays_path, overlays_json.as_bytes())?;

    Ok(())
}

/// Extract unverified count from status summary
pub(super) fn get_unverified_count(summary: &enrich::StatusSummary) -> usize {
    summary
        .requirements
        .iter()
        .find(|r| r.id == enrich::RequirementId::Verification)
        .and_then(|r| r.behavior_unverified_count)
        .unwrap_or(0)
}

/// Extract excluded count from status summary
pub(super) fn get_excluded_count(summary: &enrich::StatusSummary) -> usize {
    summary
        .requirements
        .iter()
        .find(|r| r.id == enrich::RequirementId::Verification)
        .and_then(|r| r.verification.as_ref())
        .map(|v| v.behavior_excluded_count)
        .unwrap_or(0)
}

// --- Verification retry progress tracking ---

fn modified_epoch_ms(path: &Path) -> Option<u128> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let duration = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(duration.as_millis())
}

fn outputs_equal_workaround_needs_delta_rerun(
    paths: &enrich::DocPackPaths,
    entry: &scenarios::VerificationEntry,
) -> bool {
    let overlays_path = paths.surface_overlays_path();
    let Some(overlays_modified_ms) = modified_epoch_ms(&overlays_path) else {
        return false;
    };
    let latest_delta_modified_ms = entry
        .delta_evidence_paths
        .iter()
        .filter_map(|rel| {
            let rel = rel.trim();
            if rel.is_empty() {
                return None;
            }
            let abs = paths.root().join(rel);
            modified_epoch_ms(&abs)
        })
        .max();
    match latest_delta_modified_ms {
        Some(delta_modified_ms) => delta_modified_ms <= overlays_modified_ms,
        None => true,
    }
}

fn surface_has_requires_argv_hint(surface: &surface::SurfaceInventory, surface_id: &str) -> bool {
    surface::primary_surface_item_by_id(surface, surface_id)
        .is_some_and(|item| !item.invocation.requires_argv.is_empty())
}

fn fallback_behavior_scenario_id_for_surface_id(surface_id: &str) -> String {
    format!(
        "verify_{}",
        surface_id.trim_start_matches('-').trim().replace('-', "_")
    )
}

fn behavior_scenario_ids_for_entry(
    surface_id: &str,
    entry: &scenarios::VerificationEntry,
) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if let Some(scenario_id) = entry.behavior_unverified_scenario_id.as_deref() {
        let scenario_id = scenario_id.trim();
        if !scenario_id.is_empty() {
            ids.insert(scenario_id.to_string());
        }
    }
    for scenario_id in &entry.behavior_scenario_ids {
        let scenario_id = scenario_id.trim();
        if scenario_id.is_empty() {
            continue;
        }
        ids.insert(scenario_id.to_string());
    }
    if ids.is_empty() {
        ids.insert(fallback_behavior_scenario_id_for_surface_id(surface_id));
    }
    ids
}

fn normalize_rerun_ids(ids: &[String]) -> BTreeSet<String> {
    ids.iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

/// Update outputs_equal retry progress after scenario executions.
/// Tracks retry counts for surfaces stuck in outputs_equal state.
pub fn update_outputs_equal_retry_progress_after_apply(
    paths: &enrich::DocPackPaths,
    executed_forced_rerun_scenario_ids: &[String],
    ledger_entries: &BTreeMap<String, scenarios::VerificationEntry>,
) -> Result<()> {
    if !paths.surface_path().is_file() {
        return Ok(());
    }

    let surface = surface::load_surface_inventory(&paths.surface_path())
        .with_context(|| format!("load {}", paths.surface_path().display()))?;
    let executed_forced_rerun_ids = normalize_rerun_ids(executed_forced_rerun_scenario_ids);
    let mut progress = load_verification_progress(paths);

    let active_outputs_equal_surface_ids: BTreeSet<String> = ledger_entries
        .iter()
        .filter(|(surface_id, entry)| {
            entry.delta_outcome.as_deref() == Some("outputs_equal")
                && surface_has_requires_argv_hint(&surface, surface_id)
                && outputs_equal_workaround_needs_delta_rerun(paths, entry)
        })
        .map(|(surface_id, _)| surface_id.clone())
        .collect();

    let before_len = progress.outputs_equal_retries_by_surface.len();
    progress
        .outputs_equal_retries_by_surface
        .retain(|surface_id, _| active_outputs_equal_surface_ids.contains(surface_id));
    let mut changed = progress.outputs_equal_retries_by_surface.len() != before_len;

    for surface_id in &active_outputs_equal_surface_ids {
        let Some(entry) = ledger_entries.get(surface_id.as_str()) else {
            continue;
        };
        let scenario_ids = behavior_scenario_ids_for_entry(surface_id, entry);
        let was_forced_rerun_executed = scenario_ids
            .iter()
            .any(|scenario_id| executed_forced_rerun_ids.contains(scenario_id));
        let delta_signature = outputs_equal_delta_signature(Some(entry));

        if !was_forced_rerun_executed {
            if let Some(progress_entry) = progress
                .outputs_equal_retries_by_surface
                .get_mut(surface_id)
            {
                if progress_entry.delta_signature.as_deref() != Some(delta_signature.as_str()) {
                    progress_entry.retry_count = 0;
                    progress_entry.delta_signature = Some(delta_signature);
                    changed = true;
                }
            }
            continue;
        }

        let progress_entry = progress
            .outputs_equal_retries_by_surface
            .entry(surface_id.clone())
            .or_default();
        if progress_entry.delta_signature.as_deref() != Some(delta_signature.as_str()) {
            progress_entry.retry_count = 0;
        }
        progress_entry.retry_count = progress_entry.retry_count.saturating_add(1);
        progress_entry.delta_signature = Some(delta_signature);
        changed = true;
    }

    if changed {
        write_verification_progress(paths, &progress)?;
    }

    Ok(())
}

/// Update assertion_failed loop progress after scenario executions.
/// Advances loop state and no_progress_count when targeted reruns are executed.
pub fn update_assertion_failed_progress_after_apply(
    paths: &enrich::DocPackPaths,
    executed_forced_rerun_scenario_ids: &[String],
    ledger_entries: &BTreeMap<String, scenarios::VerificationEntry>,
) -> Result<()> {
    let executed_forced_rerun_ids = normalize_rerun_ids(executed_forced_rerun_scenario_ids);
    let mut progress = load_verification_progress(paths);

    // Find surfaces with assertion_failed that had forced reruns executed
    let assertion_failed_surface_ids: BTreeSet<String> = ledger_entries
        .iter()
        .filter(|(_, entry)| {
            entry.behavior_unverified_reason_code.as_deref() == Some("assertion_failed")
        })
        .map(|(surface_id, _)| surface_id.clone())
        .collect();

    let before_len = progress.assertion_failed_by_surface.len();
    // Remove entries for surfaces no longer in assertion_failed state
    progress
        .assertion_failed_by_surface
        .retain(|surface_id, _| assertion_failed_surface_ids.contains(surface_id));
    let mut changed = progress.assertion_failed_by_surface.len() != before_len;

    for surface_id in &assertion_failed_surface_ids {
        let Some(entry) = ledger_entries.get(surface_id.as_str()) else {
            continue;
        };
        let scenario_ids = behavior_scenario_ids_for_entry(surface_id, entry);
        let was_forced_rerun_executed = scenario_ids
            .iter()
            .any(|scenario_id| executed_forced_rerun_ids.contains(scenario_id));

        if !was_forced_rerun_executed {
            continue;
        }

        // Compute current evidence fingerprint
        let current_fingerprint = crate::verification_progress::evidence_fingerprint(Some(entry));

        let progress_entry = progress
            .assertion_failed_by_surface
            .entry(surface_id.clone())
            .or_default();

        // Check if evidence has changed
        let evidence_changed = progress_entry
            .last_signature
            .evidence_fingerprint
            .as_deref()
            != Some(current_fingerprint.as_str());

        if evidence_changed {
            // Evidence changed - this is progress, reset counter
            progress_entry.no_progress_count = 0;
            progress_entry.last_signature.evidence_fingerprint = Some(current_fingerprint);
            changed = true;
        } else {
            // Evidence unchanged after rerun - no progress made
            progress_entry.no_progress_count = progress_entry.no_progress_count.saturating_add(1);
            changed = true;
        }
    }

    if changed {
        write_verification_progress(paths, &progress)?;
    }

    Ok(())
}
