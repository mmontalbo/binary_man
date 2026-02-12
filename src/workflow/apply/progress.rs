//! Progress tracking for the apply loop.
//!
//! Tracks LM failures, no-progress cycles, and auto-exclusion of stuck surfaces.

use crate::enrich;
use crate::scenarios;
use crate::verification_progress::{load_verification_progress, write_verification_progress};
use anyhow::{anyhow, Result};
use std::collections::BTreeSet;
use std::fs;

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

        overlays_array[idx]["behavior_exclusion"] = serde_json::json!({
            "reason_code": "assertion_gap",
            "note": "Auto-excluded after repeated LM failures",
            "evidence": {
                "delta_variant_path": delta_path
            }
        });

        if verbose {
            eprintln!("  auto-excluded {} after repeated LM failures", surface_id);
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
