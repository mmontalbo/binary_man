//! Verification requirement evaluation for behavior testing.
//!
//! This module determines whether CLI surface items (options, subcommands) are
//! adequately tested by behavior scenarios. It is the most complex requirement
//! evaluator because behavior verification involves multiple fallback strategies
//! and progress tracking to avoid infinite loops.
//!
//! # Why This Exists
//!
//! Binary documentation requires proof that each CLI option actually does what
//! the man page claims. This module evaluates scenario execution results against
//! surface items to determine:
//! 1. Which items are verified (have passing behavior scenarios)
//! 2. Which items need scenarios (and what kind)
//! 3. What the next action should be to make progress
//!
//! # Evaluation Flow
//!
//! ```text
//! Surface Items (--verbose, --help, etc.)
//!         │
//!         ▼
//! ┌───────────────────┐
//! │ Auto-Verification │ ← Simple flags tested automatically
//! └─────────┬─────────┘
//!           │ remaining
//!           ▼
//! ┌───────────────────┐
//! │ Behavior Scenarios│ ← LM-generated test scenarios
//! └─────────┬─────────┘
//!           │ unverified
//!           ▼
//! ┌───────────────────┐
//! │ Scaffold/Exclusion│ ← Generate scaffolds or mark untestable
//! └───────────────────┘
//! ```
//!
//! # Submodules
//!
//! - [`auto`]: Handles flag-only options verified by exit code alone
//! - [`inputs`]: Loads verification inputs (ledger, progress, policy)
//! - [`ledger`]: Builds verification ledger entries from SQL query results
//! - [`next_action`]: Determines what action to take for unverified items
//! - [`overlays`]: Generates surface overlay stubs (value_examples, requires_argv)
//! - [`reasoning`]: Determines why items are unverified and builds diagnostics
//! - [`retry`]: Tracks retry counts for behavior verification
//! - [`scaffold`]: Generates scenario scaffolds for the LM
//! - [`selectors`]: Filters and prioritizes surface IDs for next action
//!
//! # Key Concepts
//!
//! - **Verification Ledger**: SQL-computed mapping of surface_id → verification status
//! - **Delta Comparison**: Comparing baseline vs variant scenario outputs
//! - **Outputs Equal**: When delta shows no difference, needs retry with different seed
//! - **Exclusions**: Items marked untestable (fixture_gap, requires_tty, etc.)
//!
//! # Progress Tracking
//!
//! The module uses [`VerificationProgress`] to track retry attempts and detect
//! no-op loops. Without this, the LM could generate the same failing scenario
//! repeatedly. Key mechanisms:
//!
//! - `outputs_equal_retries`: Caps retries when baseline == variant output
//! - `assertion_failed_by_surface`: Detects repeated identical failures
//! - `delta_signature`: Fingerprints evidence to detect stale retries

mod auto;
mod inputs;
mod ledger;
mod next_action;
mod overlays;
mod reasoning;
mod retry;
mod scaffold;
mod selectors;

use super::{format_preview, preview_ids, EvalState};
use crate::status::verification_policy::{DeltaOutcomeKind, VerificationStatus, VerificationTier};
use anyhow::Result;
use auto::{eval_auto_verification, AutoVerificationContext};
use inputs::{base_evidence, ensure_verification_policy, load_verification_inputs};
use ledger::{build_verification_ledger_entries, LedgerBuildInputs};
use next_action::{maybe_set_behavior_next_action, BehaviorEvalState, BEHAVIOR_BATCH_LIMIT};
use reasoning::{
    build_behavior_reason_summary, build_behavior_unverified_preview, build_behavior_warnings,
    load_behavior_exclusion_state,
};
use retry::load_behavior_retry_counts;
use selectors::{
    behavior_counts_for_ids, behavior_scenario_surface_ids, collect_missing_value_examples,
    needs_apply_ids, select_delta_outcome_ids_for_remaining, surface_has_requires_argv_hint,
    BehaviorLookupContext,
};

use crate::enrich;
use crate::scenarios;
use crate::verification_progress::load_verification_progress;

type LedgerEntries = std::collections::BTreeMap<String, scenarios::VerificationEntry>;

#[derive(Default)]
struct VerificationEvalOutput {
    triage_summary: Option<enrich::VerificationTriageSummary>,
    unverified_ids: Vec<String>,
    behavior_verified_count: Option<usize>,
    behavior_unverified_count: Option<usize>,
}

struct QueueVerificationContext<'a> {
    plan: &'a scenarios::ScenarioPlan,
    surface: &'a crate::surface::SurfaceInventory,
    semantics: Option<&'a crate::semantics::Semantics>,
    include_full: bool,
    ledger_entries: Option<&'a LedgerEntries>,
    evidence: &'a mut Vec<enrich::EvidenceRef>,
    local_blockers: &'a mut Vec<enrich::Blocker>,
    verification_next_action: &'a mut Option<enrich::NextAction>,
    missing: &'a [String],
    paths: &'a enrich::DocPackPaths,
    surface_evidence: &'a enrich::EvidenceRef,
    scenarios_evidence: &'a enrich::EvidenceRef,
}

#[derive(Default)]
struct BehaviorExclusionState {
    excluded_by_id: std::collections::BTreeMap<String, crate::surface::SurfaceBehaviorExclusion>,
    excluded_ids: Vec<String>,
    excluded_preview: Vec<String>,
    excluded: Vec<enrich::VerificationExclusion>,
    excluded_reason_summary: Vec<enrich::VerificationReasonSummary>,
}

/// Result of partitioning required IDs into verified/unverified.
struct RemainingIdsResult {
    remaining_ids: Vec<String>,
    verified_count: usize,
    collected_evidence: Vec<enrich::EvidenceRef>,
}

/// Partitioned outputs_equal buckets for behavior verification.
struct OutputsEqualPartitions {
    without_workaround: Vec<String>,
    with_workaround_needs_rerun: Vec<String>,
    with_workaround_ready_for_exclusion: Vec<String>,
}

fn normalize_target_ids(target_ids: &[String]) -> Vec<String> {
    let mut ids = target_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

/// Partition required IDs into verified and remaining (unverified) sets.
fn partition_remaining_ids(
    required_ids: &[String],
    excluded_set: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
) -> RemainingIdsResult {
    let mut remaining_ids = Vec::new();
    let mut verified_count = 0;
    let mut collected_evidence = Vec::new();

    for surface_id in required_ids {
        if excluded_set.contains(surface_id) {
            continue;
        }
        let status = VerificationStatus::from_entry(
            ledger_entries.get(surface_id),
            VerificationTier::Behavior,
        );
        if status == VerificationStatus::Verified {
            verified_count += 1;
        } else {
            remaining_ids.push(surface_id.clone());
            if let Some(entry) = ledger_entries.get(surface_id) {
                collected_evidence.extend(entry.evidence.iter().cloned());
            }
        }
    }
    remaining_ids.sort();
    remaining_ids.dedup();

    RemainingIdsResult {
        remaining_ids,
        verified_count,
        collected_evidence,
    }
}

/// Partition outputs_equal IDs into workaround buckets.
fn partition_outputs_equal(
    outputs_equal_ids: Vec<String>,
    surface: &crate::surface::SurfaceInventory,
    ledger_entries: &LedgerEntries,
    paths: &enrich::DocPackPaths,
) -> OutputsEqualPartitions {
    let (with_workaround, without_workaround): (Vec<_>, Vec<_>) = outputs_equal_ids
        .into_iter()
        .partition(|surface_id| surface_has_requires_argv_hint(surface, surface_id));

    let (needs_rerun, ready_for_exclusion): (Vec<_>, Vec<_>) =
        with_workaround.into_iter().partition(|surface_id| {
            ledger_entries
                .get(surface_id.as_str())
                .is_some_and(|entry| outputs_equal_workaround_needs_delta_rerun(paths, entry))
        });

    OutputsEqualPartitions {
        without_workaround,
        with_workaround_needs_rerun: needs_rerun,
        with_workaround_ready_for_exclusion: ready_for_exclusion,
    }
}

/// Check if outputs_equal workaround needs a delta rerun.
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

fn modified_epoch_ms(path: &std::path::Path) -> Option<u128> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let duration = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(duration.as_millis())
}

/// Check if auto verification is stuck: all remaining IDs have auto_verify scenarios
/// that ran but are still unverified. This happens for binaries like `grep` that require
/// positional arguments - auto_verify runs `grep -a` which fails with usage errors.
fn auto_verification_is_stuck(remaining_ids: &[String], paths: &enrich::DocPackPaths) -> bool {
    if remaining_ids.is_empty() {
        return false;
    }
    // Load the scenario index to check if auto_verify scenarios exist for remaining IDs
    let index_path = paths.root().join("inventory/scenarios/index.json");
    let Ok(index_bytes) = std::fs::read(&index_path) else {
        return false;
    };
    let Ok(index) = serde_json::from_slice::<scenarios::ScenarioIndex>(&index_bytes) else {
        return false;
    };
    // Build a set of surface IDs that have auto_verify scenarios in the index
    let auto_verify_surface_ids: std::collections::BTreeSet<String> = index
        .scenarios
        .iter()
        .filter_map(|entry| {
            // auto_verify scenario IDs have format: auto_verify::--flag
            entry
                .scenario_id
                .strip_prefix("auto_verify::")
                .map(str::to_string)
        })
        .collect();

    // Load prereqs to find items excluded from auto-verify (e.g., interactive)
    let prereq_excluded_ids: std::collections::BTreeSet<String> =
        if let Ok(Some(prereqs)) = enrich::load_prereqs(&paths.prereqs_path()) {
            prereqs
                .surface_map
                .iter()
                .filter(|(_, keys)| {
                    keys.iter()
                        .any(|key| prereqs.definitions.get(key).is_some_and(|def| def.exclude))
                })
                .map(|(id, _)| id.clone())
                .collect()
        } else {
            std::collections::BTreeSet::new()
        };

    // Check if ALL remaining IDs (excluding prereq-excluded) have auto_verify scenarios
    remaining_ids.iter().all(|surface_id| {
        prereq_excluded_ids.contains(surface_id) || auto_verify_surface_ids.contains(surface_id)
    })
}

/// Evaluate behavior verification status and compute next action.
fn eval_behavior_verification(ctx: &mut QueueVerificationContext<'_>) -> VerificationEvalOutput {
    let Some(semantics) = ctx.semantics else {
        return VerificationEvalOutput::default();
    };
    let Some(targets) =
        scenarios::auto_verification_targets_for_behavior(ctx.plan, ctx.surface, semantics)
    else {
        return VerificationEvalOutput::default();
    };
    let required_ids = &targets.target_ids;

    let Some(ledger_entries) = ctx.ledger_entries else {
        return VerificationEvalOutput::default();
    };
    let plan_behavior_ids = behavior_scenario_surface_ids(ctx.plan);
    let mut behavior_exclusions = match load_behavior_exclusion_state(
        ctx.paths,
        required_ids,
        ledger_entries,
        ctx.include_full,
    ) {
        Ok(state) => state,
        Err(err) => {
            let mut blocker_evidence =
                vec![ctx.surface_evidence.clone(), ctx.scenarios_evidence.clone()];
            if let Ok(evidence) = ctx
                .paths
                .evidence_from_path(&ctx.paths.surface_overlays_path())
            {
                blocker_evidence.push(evidence);
            }
            ctx.local_blockers.push(enrich::Blocker {
                code: "behavior_exclusion_invalid".to_string(),
                message: err.to_string(),
                evidence: blocker_evidence,
                next_action: Some("fix inventory/surface.overlays.json".to_string()),
            });
            BehaviorExclusionState::default()
        }
    };
    let excluded_set: std::collections::BTreeSet<String> =
        behavior_exclusions.excluded_by_id.keys().cloned().collect();

    let partition_result = partition_remaining_ids(required_ids, &excluded_set, ledger_entries);
    let remaining_ids = partition_result.remaining_ids;
    let behavior_verified_count = partition_result.verified_count;
    ctx.evidence.extend(partition_result.collected_evidence);

    let remaining_set: std::collections::BTreeSet<String> = remaining_ids.iter().cloned().collect();
    let remaining_preview = preview_ids(&remaining_ids);
    let missing_value_examples =
        collect_missing_value_examples(ctx.surface, &remaining_ids, ledger_entries);
    let needs_apply_ids = needs_apply_ids(&plan_behavior_ids, &remaining_set, ledger_entries);
    let lookup_ctx = BehaviorLookupContext {
        remaining_ids: &remaining_set,
        missing_value_examples: &missing_value_examples,
        needs_apply_ids: &needs_apply_ids,
        ledger_entries,
    };
    let outputs_equal_ids = select_delta_outcome_ids_for_remaining(
        required_ids,
        &lookup_ctx,
        DeltaOutcomeKind::OutputsEqual,
        BEHAVIOR_BATCH_LIMIT,
    );
    let outputs_equal_partitions =
        partition_outputs_equal(outputs_equal_ids, ctx.surface, ledger_entries, ctx.paths);
    let outputs_equal_without_workaround = outputs_equal_partitions.without_workaround;
    let outputs_equal_with_workaround_needs_rerun =
        outputs_equal_partitions.with_workaround_needs_rerun;
    let outputs_equal_with_workaround_ready_for_exclusion =
        outputs_equal_partitions.with_workaround_ready_for_exclusion;
    let verification_progress = load_verification_progress(ctx.paths);
    let outputs_equal_retry_ids = outputs_equal_without_workaround
        .iter()
        .chain(outputs_equal_with_workaround_needs_rerun.iter())
        .chain(outputs_equal_with_workaround_ready_for_exclusion.iter())
        .cloned()
        .collect::<Vec<_>>();
    let retry_counts = load_behavior_retry_counts(
        ctx.paths,
        ledger_entries,
        &verification_progress,
        &outputs_equal_retry_ids,
    );
    let behavior_unverified_reasons =
        build_behavior_reason_summary(&remaining_ids, &missing_value_examples, ledger_entries);
    let behavior_unverified_preview =
        build_behavior_unverified_preview(&remaining_ids, &missing_value_examples, ledger_entries);
    let behavior_unverified_diagnostics = reasoning::build_behavior_unverified_diagnostics(
        &remaining_ids,
        &missing_value_examples,
        ledger_entries,
        ctx.include_full,
    );
    let behavior_warnings = build_behavior_warnings(required_ids, ledger_entries, ctx.include_full);
    let remaining_by_kind_summary = vec![enrich::VerificationKindSummary {
        kind: "option".to_string(),
        target_count: required_ids.len(),
        remaining_count: remaining_ids.len(),
        remaining_preview: preview_ids(&remaining_ids),
    }];
    let excluded_count = (!behavior_exclusions.excluded_ids.is_empty())
        .then_some(behavior_exclusions.excluded_ids.len());

    let mut summary = enrich::VerificationTriageSummary {
        triaged_unverified_count: remaining_ids.len(),
        triaged_unverified_preview: remaining_preview,
        remaining_by_kind: remaining_by_kind_summary,
        excluded_count,
        behavior_excluded_count: behavior_exclusions.excluded_ids.len(),
        behavior_excluded_preview: behavior_exclusions.excluded_preview.clone(),
        behavior_excluded_reasons: behavior_exclusions.excluded_reason_summary.clone(),
        excluded: std::mem::take(&mut behavior_exclusions.excluded),
        behavior_unverified_reasons,
        behavior_unverified_preview,
        behavior_unverified_diagnostics,
        behavior_warnings,
        stub_blockers_preview: Vec::new(),
    };

    let eval_state = BehaviorEvalState {
        required_ids,
        lookup_ctx: &lookup_ctx,
        outputs_equal_without_workaround: &outputs_equal_without_workaround,
        outputs_equal_with_workaround_needs_rerun: &outputs_equal_with_workaround_needs_rerun,
        outputs_equal_with_workaround_ready_for_exclusion:
            &outputs_equal_with_workaround_ready_for_exclusion,
        retry_counts: &retry_counts,
    };
    maybe_set_behavior_next_action(ctx, &mut summary, &eval_state);

    let summary_preview = format!(
        "behavior verification: {} remaining ({})",
        summary.triaged_unverified_count,
        format_preview(
            summary.triaged_unverified_count,
            &summary.triaged_unverified_preview
        )
    );
    let has_unverified = summary.triaged_unverified_count > 0;

    let mut output = VerificationEvalOutput {
        triage_summary: Some(summary),
        unverified_ids: Vec::new(),
        behavior_verified_count: Some(behavior_verified_count),
        behavior_unverified_count: Some(remaining_ids.len()),
    };
    if has_unverified {
        output.unverified_ids.push(summary_preview);
    }

    output
}

pub(super) fn eval_verification_requirement(
    state: &mut EvalState,
    req: enrich::RequirementId,
) -> Result<enrich::RequirementStatus> {
    let paths = state.paths;
    let binary_name = state.binary_name;
    let config = state.config;
    let include_full = state.include_full;
    let missing_artifacts = &mut *state.missing_artifacts;
    let blockers = &mut *state.blockers;
    let verification_next_action = &mut *state.verification_next_action;

    let mut local_blockers = Vec::new();
    let mut missing = Vec::new();
    let mut unverified_ids = Vec::new();
    let mut triage_summary: Option<enrich::VerificationTriageSummary> = None;
    let mut behavior_verified_override: Option<usize> = None;
    let mut behavior_unverified_override: Option<usize> = None;
    let mut ledger_snapshot = None;
    let verification_tier = VerificationTier::from_config(config.verification_tier.as_deref());
    let tier_label = verification_tier.label();
    let tier_tag = format!("tier={}", verification_tier.as_str());

    let inputs =
        load_verification_inputs(paths, missing_artifacts, &mut missing, &mut local_blockers)?;
    let mut evidence = base_evidence(&inputs);

    // Load semantics for tier targeting (optional - fallback to legacy behavior if unavailable)
    let semantics = crate::semantics::load_semantics(paths.root()).ok();

    if let (Some(surface), Some(plan)) = (inputs.surface.as_ref(), inputs.plan.as_ref()) {
        if inputs.template_path.is_file() && inputs.semantics_path.is_file() {
            let ledger_inputs = LedgerBuildInputs {
                binary_name,
                surface,
                plan,
                paths,
                template_path: &inputs.template_path,
                template_evidence: &inputs.template_evidence,
            };
            ledger_snapshot =
                build_verification_ledger_entries(&ledger_inputs, &mut local_blockers);
        }
        let ledger_entries = ledger_snapshot.as_ref().map(|snapshot| &snapshot.entries);

        ensure_verification_policy(plan, &mut missing, verification_next_action, binary_name);

        let behavior_targets = if verification_tier.is_behavior() {
            semantics.as_ref().and_then(|sem| {
                scenarios::auto_verification_targets_for_behavior(plan, surface, sem)
            })
        } else {
            None
        };
        if let (Some(targets), Some(entries)) = (behavior_targets.as_ref(), ledger_entries) {
            let (verified, unverified) = behavior_counts_for_ids(&targets.target_ids, entries);
            behavior_verified_override = Some(verified);
            behavior_unverified_override = Some(unverified);
        }

        let output = if !verification_tier.is_behavior() {
            if let Some(auto_state) = crate::status::verification::auto_verification_state(
                plan,
                surface,
                ledger_entries,
                verification_tier.as_str(),
            ) {
                let mut ctx = AutoVerificationContext {
                    ledger_entries,
                    evidence: &mut evidence,
                    verification_next_action,
                    local_blockers: &local_blockers,
                    missing: &missing,
                    paths,
                };
                eval_auto_verification(auto_state, &mut ctx)
            } else {
                VerificationEvalOutput::default()
            }
        } else {
            let mut existence_output = None;
            if let Some(targets) = behavior_targets {
                let auto_state = crate::status::verification::auto_verification_state_for_targets(
                    targets,
                    ledger_entries,
                    VerificationTier::Accepted,
                );
                // Save remaining_ids before auto_state is consumed
                let auto_remaining_ids = auto_state.remaining_ids.clone();
                let mut ctx = AutoVerificationContext {
                    ledger_entries,
                    evidence: &mut evidence,
                    verification_next_action,
                    local_blockers: &local_blockers,
                    missing: &missing,
                    paths,
                };
                let output = eval_auto_verification(auto_state, &mut ctx);
                if output
                    .triage_summary
                    .as_ref()
                    .is_some_and(|summary| summary.triaged_unverified_count > 0)
                {
                    // Skip auto verification if stuck: all remaining items have scenarios
                    // that ran but are still unverified (e.g., binaries like grep where
                    // auto_verify scenarios fail with usage errors due to missing args)
                    let stuck = auto_verification_is_stuck(&auto_remaining_ids, paths);
                    if stuck {
                        // Clear the auto verification next_action so behavior verification
                        // can set its own next_action
                        *verification_next_action = None;
                    } else {
                        existence_output = Some(output);
                    }
                }
            }

            if let Some(output) = existence_output {
                output
            } else {
                let mut ctx = QueueVerificationContext {
                    plan,
                    surface,
                    semantics: semantics.as_ref(),
                    ledger_entries,
                    evidence: &mut evidence,
                    local_blockers: &mut local_blockers,
                    verification_next_action,
                    missing: &missing,
                    paths,
                    include_full,
                    surface_evidence: &inputs.surface_evidence,
                    scenarios_evidence: &inputs.scenarios_evidence,
                };
                eval_behavior_verification(&mut ctx)
            }
        };
        unverified_ids = output.unverified_ids;
        triage_summary = output.triage_summary;
        if output.behavior_verified_count.is_some() {
            behavior_verified_override = output.behavior_verified_count;
        }
        if output.behavior_unverified_count.is_some() {
            behavior_unverified_override = output.behavior_unverified_count;
        }
    }

    enrich::dedupe_evidence_refs(&mut evidence);
    let (
        accepted_verified_count,
        accepted_unverified_count,
        mut behavior_verified_count,
        mut behavior_unverified_count,
    ) = if let Some(snapshot) = ledger_snapshot.as_ref() {
        (
            Some(snapshot.verified_count),
            Some(snapshot.unverified_count),
            Some(snapshot.behavior_verified_count),
            Some(snapshot.behavior_unverified_count),
        )
    } else {
        (
            None,
            triage_summary
                .as_ref()
                .map(|summary| summary.triaged_unverified_count),
            None,
            None,
        )
    };
    if behavior_verified_override.is_some() {
        behavior_verified_count = behavior_verified_override;
    }
    if behavior_unverified_override.is_some() {
        behavior_unverified_count = behavior_unverified_override;
    }
    let behavior_remaining = if !verification_tier.is_behavior() {
        behavior_unverified_count.filter(|count| *count > 0)
    } else {
        None
    };

    let (status, reason) = if !local_blockers.is_empty() {
        (
            enrich::RequirementState::Blocked,
            format!("verification inputs blocked ({tier_tag})"),
        )
    } else if !missing.is_empty() {
        (
            enrich::RequirementState::Unmet,
            format!(
                "verification inputs missing: {} ({tier_tag})",
                missing.join("; ")
            ),
        )
    } else if !unverified_ids.is_empty() {
        (
            enrich::RequirementState::Unmet,
            format!("verification {tier_label} incomplete ({tier_tag})"),
        )
    } else {
        let mut tag = tier_tag;
        if let Some(remaining) = behavior_remaining {
            tag.push_str(&format!("; behavior_remaining={remaining} not required"));
        }
        (
            enrich::RequirementState::Met,
            format!("verification {tier_label} complete ({tag})"),
        )
    };

    blockers.extend(local_blockers.clone());
    Ok(enrich::RequirementStatus {
        id: req,
        status,
        reason,
        verification_tier: Some(verification_tier.as_str().to_string()),
        accepted_verified_count,
        unverified_ids,
        accepted_unverified_count,
        behavior_verified_count,
        behavior_unverified_count,
        verification: triage_summary,
        evidence,
        blockers: local_blockers,
    })
}

#[cfg(test)]
mod tests;
