//! Next action determination for behavior verification.
//!
//! Determines the next step to take for unverified surface items, including
//! scaffold generation, scenario reruns, and exclusion suggestions.

use crate::enrich;
use crate::scenarios;
use crate::status::verification_policy::{BehaviorAction, BehaviorReasonKind};
use crate::verification_progress::{
    build_action_signature, get_assertion_failed_no_progress_count, is_noop_action,
    load_verification_progress,
};

use super::overlays::{
    build_stub_blockers_preview, surface_overlays_behavior_exclusion_stub_batch,
    surface_overlays_requires_argv_stub_batch, STUB_REASON_OUTPUTS_EQUAL_AFTER_WORKAROUND,
    STUB_REASON_OUTPUTS_EQUAL_NEEDS_WORKAROUND,
};
use super::reasoning::{behavior_reason_code_for_id, behavior_unverified_reason};
use super::retry::{
    max_retry_count, partition_cap_hit, preferred_behavior_scenario_id, BEHAVIOR_RERUN_CAP,
};
use super::scaffold::{
    assertion_starters_for_missing_assertions, build_existing_behavior_scenarios_scaffold,
    build_missing_assertions_scaffold_content, build_scaffold_context,
    first_valid_scaffold_content, required_value_argv_rewrite_hint,
};
use super::selectors::{
    first_reason_id, first_reason_id_by_priority, surface_kind_for_id, BehaviorLookupContext,
};
use super::{normalize_target_ids, LedgerEntries, QueueVerificationContext};

/// Maximum targets to batch in a single next action.
pub(super) const BEHAVIOR_BATCH_LIMIT: usize = 15;

/// Maximum no-progress retries for assertion_failed before pivoting to exclusion.
pub const ASSERTION_FAILED_NOOP_CAP: usize = 2;

/// Fallback delta path when no evidence exists.
const DELTA_PATH_FALLBACK: &str = "inventory/scenarios/<delta_variant>.json";

/// Arguments for building a behavior next-action payload.
pub(super) struct BehaviorPayloadArgs<'a> {
    pub surface: Option<&'a crate::surface::SurfaceInventory>,
    pub target_ids: &'a [String],
    pub reason_code: Option<&'a str>,
    /// Semantic action type being taken.
    pub action_kind: Option<BehaviorAction>,
    pub retry_counts: &'a std::collections::BTreeMap<String, usize>,
    pub ledger_entries: &'a LedgerEntries,
    pub suggested_overlay_keys: &'a [&'a str],
    pub assertion_starters: Vec<enrich::BehaviorAssertionStarter>,
    pub suggested_exclusion_payload: Option<enrich::SuggestedBehaviorExclusionPayload>,
    pub paths: &'a enrich::DocPackPaths,
}

/// Computed state from behavior evaluation used for setting next actions.
pub(super) struct BehaviorEvalState<'a> {
    pub required_ids: &'a [String],
    pub lookup_ctx: &'a BehaviorLookupContext<'a>,
    pub outputs_equal_without_workaround: &'a [String],
    pub outputs_equal_with_workaround_needs_rerun: &'a [String],
    pub outputs_equal_with_workaround_ready_for_exclusion: &'a [String],
    pub retry_counts: &'a std::collections::BTreeMap<String, usize>,
    /// Surfaces that failed post-execution judgment and need retry with feedback.
    pub judgment_retry_ids: &'a std::collections::BTreeSet<String>,
}

/// Result of determining the next behavior action.
///
/// This encapsulates the action type, target IDs, and reason kind for dispatch.
pub(super) struct DeterminedAction {
    pub action: BehaviorAction,
    pub target_ids: Vec<String>,
    pub reason_kind: BehaviorReasonKind,
}

/// Determine the next behavior action based on evaluation state.
///
/// This is the core state machine that decides what action to take next.
/// Returns `None` if no action is needed.
pub(super) fn determine_behavior_action(
    ctx: &QueueVerificationContext<'_>,
    state: &BehaviorEvalState<'_>,
) -> Option<DeterminedAction> {
    // Priority 1: Initial scenarios (surface exists but no behavior scenarios yet)
    if is_initial_behavior_cycle(
        ctx.plan,
        state.required_ids,
        state.lookup_ctx.ledger_entries,
    ) {
        let target_ids: Vec<String> = state
            .required_ids
            .iter()
            .take(BEHAVIOR_BATCH_LIMIT)
            .cloned()
            .collect();
        return Some(DeterminedAction {
            action: BehaviorAction::GenerateScenarios,
            target_ids,
            reason_kind: BehaviorReasonKind::InitialScenarios,
        });
    }

    // Priority 2: Outputs equal without workaround → add workaround
    if !state.outputs_equal_without_workaround.is_empty() {
        return Some(DeterminedAction {
            action: BehaviorAction::AddWorkaround,
            target_ids: state.outputs_equal_without_workaround.to_vec(),
            reason_kind: BehaviorReasonKind::OutputsEqual,
        });
    }

    // Priority 3: Outputs equal with workaround needs rerun
    if !state.outputs_equal_with_workaround_needs_rerun.is_empty() {
        let (cap_hit, needs_rerun) = partition_cap_hit(
            state.outputs_equal_with_workaround_needs_rerun.to_vec(),
            state.retry_counts,
        );
        // If cap hit, exclude; otherwise rerun
        if !cap_hit.is_empty() {
            return Some(DeterminedAction {
                action: BehaviorAction::Exclude,
                target_ids: cap_hit,
                reason_kind: BehaviorReasonKind::OutputsEqual,
            });
        }
        if !needs_rerun.is_empty() {
            return Some(DeterminedAction {
                action: BehaviorAction::Rerun,
                target_ids: needs_rerun,
                reason_kind: BehaviorReasonKind::OutputsEqual,
            });
        }
    }

    // Priority 4: Outputs equal ready for exclusion
    if !state
        .outputs_equal_with_workaround_ready_for_exclusion
        .is_empty()
    {
        let (cap_hit, ready_for_exclusion) = partition_cap_hit(
            state
                .outputs_equal_with_workaround_ready_for_exclusion
                .to_vec(),
            state.retry_counts,
        );
        if !cap_hit.is_empty() {
            return Some(DeterminedAction {
                action: BehaviorAction::Exclude,
                target_ids: cap_hit,
                reason_kind: BehaviorReasonKind::OutputsEqual,
            });
        }
        if !ready_for_exclusion.is_empty() {
            return Some(DeterminedAction {
                action: BehaviorAction::Exclude,
                target_ids: ready_for_exclusion,
                reason_kind: BehaviorReasonKind::OutputsEqual,
            });
        }
    }

    // Priority 5: Judgment retry targets
    let judgment_retry_targets: Vec<String> = state
        .required_ids
        .iter()
        .filter(|id| {
            state.judgment_retry_ids.contains(*id)
                && state.lookup_ctx.remaining_ids.contains(*id)
        })
        .take(BEHAVIOR_BATCH_LIMIT)
        .cloned()
        .collect();
    if !judgment_retry_targets.is_empty() {
        return Some(DeterminedAction {
            action: BehaviorAction::Apply,
            target_ids: judgment_retry_targets,
            reason_kind: BehaviorReasonKind::JudgmentRetry,
        });
    }

    // Priority 6: Reason-based targets (scenario_error, assertion_failed, no_scenario, outputs_equal)
    if let Some(next_id) = first_behavior_reason_target(
        state.required_ids,
        state.lookup_ctx.remaining_ids,
        state.lookup_ctx.needs_apply_ids,
        state.lookup_ctx.ledger_entries,
    ) {
        let action_reason_kind = action_reason_kind_for_surface_id(
            &next_id,
            state.lookup_ctx.missing_value_examples,
            state.lookup_ctx.ledger_entries,
        );
        // Batch targets for reason kinds that benefit from batching
        let target_ids = if matches!(
            action_reason_kind,
            BehaviorReasonKind::NoScenario | BehaviorReasonKind::OutputsEqual
        ) {
            let batched = batched_target_ids_for_reason(
                state.required_ids,
                state.lookup_ctx.remaining_ids,
                state.lookup_ctx.missing_value_examples,
                state.lookup_ctx.needs_apply_ids,
                state.lookup_ctx.ledger_entries,
                action_reason_kind,
                BEHAVIOR_BATCH_LIMIT,
            );
            if batched.is_empty() {
                vec![next_id]
            } else {
                batched
            }
        } else {
            vec![next_id]
        };
        // Map reason kind to action
        let action = action_reason_kind.suggested_action();
        return Some(DeterminedAction {
            action,
            target_ids,
            reason_kind: action_reason_kind,
        });
    }

    // Priority 7: Needs apply targets
    let batched_needs_apply: Vec<String> = state
        .required_ids
        .iter()
        .filter(|id| state.lookup_ctx.needs_apply_ids.contains(*id))
        .take(BEHAVIOR_BATCH_LIMIT)
        .cloned()
        .collect();
    if !batched_needs_apply.is_empty() {
        return Some(DeterminedAction {
            action: BehaviorAction::Apply,
            target_ids: batched_needs_apply,
            // Use NoScenario as a generic "needs work" reason
            reason_kind: BehaviorReasonKind::NoScenario,
        });
    }

    None
}

/// Get the latest delta path from a verification entry.
fn latest_delta_path_for_entry(entry: Option<&scenarios::VerificationEntry>) -> Option<String> {
    let entry = entry?;
    entry
        .delta_evidence_paths
        .iter()
        .map(|path| path.trim())
        .find(|path| !path.is_empty() && path.contains("variant"))
        .or_else(|| {
            entry
                .delta_evidence_paths
                .iter()
                .map(|path| path.trim())
                .find(|path| !path.is_empty())
        })
        .map(str::to_string)
}

/// Get the latest delta path for any of the target IDs.
fn latest_delta_path_for_ids(
    target_ids: &[String],
    ledger_entries: &LedgerEntries,
) -> Option<String> {
    target_ids
        .iter()
        .find_map(|surface_id| latest_delta_path_for_entry(ledger_entries.get(surface_id)))
}

/// Build a behavior next-action payload.
pub(super) fn behavior_payload(
    args: BehaviorPayloadArgs<'_>,
) -> Option<enrich::BehaviorNextActionPayload> {
    let target_ids = normalize_target_ids(args.target_ids);
    let reason_code_str = args
        .reason_code
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .map(str::to_string);
    let retry_count = max_retry_count(&target_ids, args.retry_counts);
    let mut latest_delta_path = latest_delta_path_for_ids(&target_ids, args.ledger_entries);
    if latest_delta_path.is_none()
        && reason_code_str
            .as_deref()
            .is_some_and(|code| matches!(code, "outputs_equal" | "missing_delta_assertion"))
    {
        latest_delta_path = Some(DELTA_PATH_FALLBACK.to_string());
    }
    let suggested_overlay_keys = args
        .suggested_overlay_keys
        .iter()
        .map(|key| key.to_string())
        .collect();
    let scaffold_context = build_scaffold_context(args.surface, &target_ids, args.reason_code);

    // Build scenario output for targets (for LM error feedback)
    let target_scenario_output = build_target_scenario_output(&target_ids, args.ledger_entries);

    // Build judgment feedback for targets that failed judgment
    let target_judgment_feedback = build_target_judgment_feedback(&target_ids, args.paths);

    let action_kind = args.action_kind.map(|a| a.as_code().to_string());

    let payload = enrich::BehaviorNextActionPayload {
        target_ids,
        reason_code: reason_code_str,
        action_kind,
        retry_count,
        latest_delta_path,
        suggested_overlay_keys,
        assertion_starters: args.assertion_starters,
        suggested_exclusion_payload: args.suggested_exclusion_payload,
        scaffold_context,
        target_scenario_output,
        target_judgment_feedback,
    };
    (!payload.is_empty()).then_some(payload)
}

/// Build scenario output context for target IDs from ledger entries.
fn build_target_scenario_output(
    target_ids: &[String],
    ledger_entries: &LedgerEntries,
) -> Vec<enrich::TargetScenarioOutput> {
    target_ids
        .iter()
        .filter_map(|surface_id| {
            let entry = ledger_entries.get(surface_id)?;
            // Prefer behavior scenario output (from actual verification scenarios)
            // over auto_verify output (from initial auto-generated scenarios).
            // Behavior stderr has more useful error messages like "Two strings must be given".
            let (exit_code, stderr_preview) =
                if entry.behavior_exit_code.is_some() || entry.behavior_stderr.is_some() {
                    (entry.behavior_exit_code, entry.behavior_stderr.clone())
                } else {
                    (
                        entry.auto_verify_exit_code,
                        entry.auto_verify_stderr.clone(),
                    )
                };
            // Only include if we have useful output data
            if exit_code.is_none() && stderr_preview.is_none() {
                return None;
            }
            Some(enrich::TargetScenarioOutput {
                surface_id: surface_id.clone(),
                exit_code,
                stderr_preview,
            })
        })
        .collect()
}

/// Build judgment feedback for targets that failed behavior judgment.
///
/// Loads progress from consolidated `verification_progress.json` and extracts
/// feedback for surfaces in `judgment_pending_retry` state.
fn build_target_judgment_feedback(
    target_ids: &[String],
    paths: &enrich::DocPackPaths,
) -> Vec<enrich::TargetJudgmentFeedback> {
    let progress = crate::verification_progress::load_verification_progress(paths);

    target_ids
        .iter()
        .filter_map(|surface_id| {
            let feedback = progress.judgment_pending_retry.get(surface_id)?;
            Some(enrich::TargetJudgmentFeedback {
                surface_id: surface_id.clone(),
                reason: feedback.reason.clone(),
                suggested_setup: feedback.suggested_setup.clone(),
                failure_count: feedback.failure_count,
            })
        })
        .collect()
}

/// Get the action reason kind for a surface ID.
///
/// Returns the `BehaviorReasonKind` that determines what action should be taken.
/// Normalizes `MissingValueExamples` to `NoScenario` when no scenario exists.
fn action_reason_kind_for_surface_id(
    surface_id: &str,
    missing_value_examples: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
) -> BehaviorReasonKind {
    let reason_code =
        behavior_reason_code_for_id(surface_id, missing_value_examples, ledger_entries);
    let reason_kind = BehaviorReasonKind::from_code(Some(&reason_code));
    let entry = ledger_entries.get(surface_id);
    let scenario_missing = entry.is_some_and(|entry| entry.behavior_scenario_ids.is_empty());
    // Normalize missing_value_examples to no_scenario when scenario is absent
    if scenario_missing && reason_kind == BehaviorReasonKind::MissingValueExamples {
        BehaviorReasonKind::NoScenario
    } else {
        reason_kind
    }
}

/// Select target IDs that match a specific reason kind.
pub(super) fn batched_target_ids_for_reason(
    required_ids: &[String],
    remaining_set: &std::collections::BTreeSet<String>,
    missing_value_examples: &std::collections::BTreeSet<String>,
    needs_apply_ids: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
    reason_kind: BehaviorReasonKind,
    limit: usize,
) -> Vec<String> {
    let mut selected = Vec::new();
    for surface_id in required_ids {
        if selected.len() >= limit {
            break;
        }
        if !remaining_set.contains(surface_id) {
            continue;
        }
        if needs_apply_ids.contains(surface_id) {
            continue;
        }
        // Only skip missing_value_examples when not batching for no_scenario
        // (missing_value_examples normalizes to no_scenario when scenario is absent)
        if missing_value_examples.contains(surface_id)
            && reason_kind != BehaviorReasonKind::NoScenario
        {
            continue;
        }
        if action_reason_kind_for_surface_id(surface_id, missing_value_examples, ledger_entries)
            != reason_kind
        {
            continue;
        }
        selected.push(surface_id.clone());
    }
    selected
}

/// Build a suggested exclusion payload.
fn suggested_exclusion_payload(
    surface_kind: &str,
    surface_id: &str,
    reason_code: &str,
    retry_count: usize,
    delta_variant_path: Option<&str>,
) -> enrich::SuggestedBehaviorExclusionPayload {
    let exclusion_reason_code = match reason_code {
        "missing_delta_assertion" => "assertion_gap",
        _ => "fixture_gap",
    };
    let note = format!(
        "reason_code={reason_code}; rerun cap reached after {retry_count} retries; exclude only if behavior remains unverifiable"
    );
    enrich::SuggestedBehaviorExclusionPayload {
        kind: surface_kind.to_string(),
        id: surface_id.to_string(),
        behavior_exclusion: enrich::SuggestedBehaviorExclusion {
            reason_code: exclusion_reason_code.to_string(),
            note: Some(note),
            evidence: enrich::SuggestedBehaviorExclusionEvidence {
                delta_variant_path: Some(
                    delta_variant_path
                        .unwrap_or(DELTA_PATH_FALLBACK)
                        .to_string(),
                ),
            },
        },
    }
}

/// Build a next action that only suggests exclusion (no scaffold).
pub(super) fn suggested_exclusion_only_next_action(
    ctx: &QueueVerificationContext<'_>,
    target_ids: &[String],
    reason_code: &str,
    retry_counts: &std::collections::BTreeMap<String, usize>,
    ledger_entries: &LedgerEntries,
) -> enrich::NextAction {
    let next_id = target_ids.first().cloned().unwrap_or_default();
    let retry_count = retry_counts.get(&next_id).copied().unwrap_or(0);
    let suggested = suggested_exclusion_payload(
        &surface_kind_for_id(ctx.surface, &next_id, "option"),
        &next_id,
        reason_code,
        retry_count,
        latest_delta_path_for_entry(ledger_entries.get(&next_id)).as_deref(),
    );
    let payload = behavior_payload(BehaviorPayloadArgs {
        surface: Some(ctx.surface),
        target_ids,
        reason_code: Some(reason_code),
        action_kind: Some(BehaviorAction::Exclude),
        retry_counts,
        ledger_entries,
        suggested_overlay_keys: &["overlays[].behavior_exclusion"],
        assertion_starters: Vec::new(),
        suggested_exclusion_payload: Some(suggested),
        paths: ctx.paths,
    });
    let root = ctx.paths.root().display();
    enrich::NextAction::Command {
        command: format!("bman status --doc-pack {root}"),
        reason: format!(
            "rerun cap reached for {reason_code}; review next_action.payload.suggested_exclusion_payload and apply exclusion manually if justified"
        ),
        hint: Some("Review suggested exclusion and apply if justified".to_string()),
        payload,
    }
}

/// Set next action for outputs_equal plateau (max retries reached).
/// Emits AutoExclude action to automatically write exclusion overlays.
fn set_outputs_equal_plateau_next_action(
    ctx: &mut QueueVerificationContext<'_>,
    summary: &mut enrich::VerificationTriageSummary,
    cap_hit: &[String],
    retry_counts: &std::collections::BTreeMap<String, usize>,
    ledger_entries: &LedgerEntries,
) -> bool {
    if cap_hit.is_empty() {
        return false;
    }
    summary.stub_blockers_preview = build_stub_blockers_preview(
        ctx,
        cap_hit,
        ledger_entries,
        STUB_REASON_OUTPUTS_EQUAL_AFTER_WORKAROUND,
        true,
    );
    let content = surface_overlays_behavior_exclusion_stub_batch(
        ctx.paths,
        ctx.surface,
        cap_hit,
        ledger_entries,
    );

    // Collect delta variant paths from ledger for evidence
    let delta_variant_paths: Vec<String> = cap_hit
        .iter()
        .filter_map(|id| ledger_entries.get(id.as_str()))
        .flat_map(|entry| entry.delta_evidence_paths.iter().cloned())
        .collect();

    // Get max retry count for evidence
    let max_retry = cap_hit
        .iter()
        .filter_map(|id| retry_counts.get(id).copied())
        .max()
        .unwrap_or(BEHAVIOR_RERUN_CAP);

    *ctx.verification_next_action = Some(enrich::NextAction::AutoExclude {
        path: "inventory/surface.overlays.json".to_string(),
        content,
        reason: format!(
            "auto-excluding {} surface(s) after {} outputs_equal retries with no progress",
            cap_hit.len(),
            max_retry
        ),
        target_ids: cap_hit.to_vec(),
        evidence: enrich::AutoExcludeEvidence {
            reason_code: "outputs_equal_exhausted".to_string(),
            retry_count: max_retry,
            delta_variant_paths,
        },
    });
    true
}

/// Get scenario IDs to rerun for surface IDs.
fn rerun_scenario_ids_for_surface_ids(
    surface_ids: &[String],
    ledger_entries: &LedgerEntries,
) -> Vec<String> {
    let mut scenario_ids = std::collections::BTreeSet::new();
    for surface_id in normalize_target_ids(surface_ids) {
        let Some(entry) = ledger_entries.get(surface_id.as_str()) else {
            continue;
        };
        if let Some(scenario_id) = preferred_behavior_scenario_id(entry) {
            scenario_ids.insert(scenario_id);
        }
        for scenario_id in &entry.behavior_scenario_ids {
            let scenario_id = scenario_id.trim();
            if scenario_id.is_empty() {
                continue;
            }
            scenario_ids.insert(scenario_id.to_string());
        }
    }
    scenario_ids.into_iter().collect()
}

/// Build a targeted rerun command for outputs_equal scenarios.
fn targeted_outputs_equal_rerun_command(
    doc_pack_root: &std::path::Path,
    scenario_ids: &[String],
) -> String {
    let mut command = format!("bman apply --doc-pack {}", doc_pack_root.display());
    for scenario_id in scenario_ids {
        command.push_str(" --rerun-scenario-id ");
        command.push_str(scenario_id);
    }
    command
}

/// Find the first behavior reason target with priority ordering.
fn first_behavior_reason_target(
    required_ids: &[String],
    remaining_set: &std::collections::BTreeSet<String>,
    needs_apply_ids: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
) -> Option<String> {
    let empty = std::collections::BTreeSet::new();
    let lookup_ctx = BehaviorLookupContext {
        remaining_ids: remaining_set,
        missing_value_examples: &empty,
        needs_apply_ids,
        ledger_entries,
    };
    // Priority: scenario_error > assertion_failed > no_scenario > outputs_equal
    // NoScenario before OutputsEqual so we scaffold new scenarios first,
    // then deal with outputs_equal (which often just need exclusion)
    first_reason_id_by_priority(
        required_ids,
        &lookup_ctx,
        &[
            BehaviorReasonKind::ScenarioError,
            BehaviorReasonKind::AssertionFailed,
            BehaviorReasonKind::NoScenario,
            BehaviorReasonKind::OutputsEqual,
        ],
    )
    .or_else(|| first_reason_id(required_ids, &lookup_ctx))
}

/// Build a reason-based behavior next action.
fn reason_based_behavior_next_action(
    ctx: &mut QueueVerificationContext<'_>,
    summary: &mut enrich::VerificationTriageSummary,
    target_ids: &[String],
    missing_value_examples: &std::collections::BTreeSet<String>,
    retry_counts: &std::collections::BTreeMap<String, usize>,
    ledger_entries: &LedgerEntries,
) -> Option<enrich::NextAction> {
    let mut ordered_target_ids = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for target_id in target_ids {
        let target_id = target_id.trim();
        if target_id.is_empty() || !seen.insert(target_id.to_string()) {
            continue;
        }
        ordered_target_ids.push(target_id.to_string());
    }
    let next_id = ordered_target_ids.first()?.clone();
    let reason_kind =
        action_reason_kind_for_surface_id(&next_id, missing_value_examples, ledger_entries);
    let reason_code = reason_kind.as_code();
    let entry = ledger_entries.get(&next_id);
    let scenario_missing = entry.is_some_and(|entry| entry.behavior_scenario_ids.is_empty());
    let scenario_id = entry
        .and_then(|entry| {
            entry
                .behavior_unverified_scenario_id
                .as_deref()
                .or_else(|| entry.behavior_scenario_ids.first().map(String::as_str))
        })
        .map(str::to_string)
        .unwrap_or_else(|| next_id.to_string());
    let assertion_kind =
        entry.and_then(|entry| entry.behavior_unverified_assertion_kind.as_deref());
    let assertion_seed_path =
        entry.and_then(|entry| entry.behavior_unverified_assertion_seed_path.as_deref());
    let retry_count = retry_counts.get(&next_id).copied().unwrap_or(0);

    // Early exit: MissingDeltaAssertion at cap goes to exclusion
    if reason_kind == BehaviorReasonKind::MissingDeltaAssertion
        && retry_count >= BEHAVIOR_RERUN_CAP
    {
        return Some(suggested_exclusion_only_next_action(
            ctx,
            &[next_id],
            reason_code,
            retry_counts,
            ledger_entries,
        ));
    }

    let scaffold_candidates = if scenario_missing {
        summary.stub_blockers_preview = build_stub_blockers_preview(
            ctx,
            &ordered_target_ids,
            ledger_entries,
            reason_code,
            false,
        );
        vec![
            crate::status::verification::behavior_scenarios_batch_stub(
                ctx.plan,
                ctx.surface,
                &ordered_target_ids,
            ),
            crate::status::verification::behavior_scenarios_batch_stub(
                ctx.plan,
                ctx.surface,
                std::slice::from_ref(&next_id),
            ),
            crate::status::verification::behavior_baseline_stub(ctx.plan, ctx.surface),
        ]
    } else {
        // Use enum method to check if this needs assertion repair
        let needs_assertion_repair = reason_kind.needs_scenario_fix();
        // For assertion repair, prioritize scaffold that adds assertions
        let mut candidates = Vec::new();
        if needs_assertion_repair {
            candidates.push(build_missing_assertions_scaffold_content(
                ctx.plan,
                ledger_entries,
                std::slice::from_ref(&next_id),
            ));
        }
        candidates.push(build_existing_behavior_scenarios_scaffold(
            ctx.plan,
            ledger_entries,
            std::slice::from_ref(&next_id),
        ));
        candidates.push(crate::status::verification::behavior_scenario_stub(
            ctx.plan,
            &scenario_id,
        ));
        candidates.push(crate::status::verification::behavior_scenarios_batch_stub(
            ctx.plan,
            ctx.surface,
            std::slice::from_ref(&next_id),
        ));
        candidates
    };
    let content = first_valid_scaffold_content(ctx.plan, ctx.paths.root(), scaffold_candidates)?;

    // No-op guard for assertion_failed: detect repeated identical edits with no evidence change
    if reason_kind == BehaviorReasonKind::AssertionFailed {
        let verification_progress = load_verification_progress(ctx.paths);
        let candidate_signature =
            build_action_signature(Some("assertion_failed"), &next_id, &content, entry);

        if is_noop_action(&verification_progress, &next_id, &candidate_signature) {
            let no_progress_count =
                get_assertion_failed_no_progress_count(&verification_progress, &next_id);

            // If at/over cap, pivot to exclusion
            if no_progress_count >= ASSERTION_FAILED_NOOP_CAP {
                return Some(suggested_exclusion_only_next_action(
                    ctx,
                    &[next_id],
                    "assertion_failed",
                    retry_counts,
                    ledger_entries,
                ));
            }

            // Otherwise, pivot to targeted rerun command
            let scenario_ids =
                rerun_scenario_ids_for_surface_ids(std::slice::from_ref(&next_id), ledger_entries);
            let command = targeted_outputs_equal_rerun_command(ctx.paths.root(), &scenario_ids);
            let payload = behavior_payload(BehaviorPayloadArgs {
                surface: Some(ctx.surface),
                target_ids: std::slice::from_ref(&next_id),
                reason_code: Some("assertion_failed"),
                action_kind: Some(BehaviorAction::Rerun),
                retry_counts,
                ledger_entries,
                suggested_overlay_keys: &[],
                assertion_starters: Vec::new(),
                suggested_exclusion_payload: None,
                paths: ctx.paths,
            });
            return Some(enrich::NextAction::Command {
                command,
                reason: format!(
                    "assertion_failed edit would be identical to previous with no evidence change; pivot to targeted rerun for {} scenario ids (no-progress attempt {}/{})",
                    scenario_ids.len(),
                    no_progress_count.saturating_add(1),
                    ASSERTION_FAILED_NOOP_CAP
                ),
                hint: Some("Rerun scenario to detect evidence changes".to_string()),
                payload,
            });
        }
    }

    let mut reason = behavior_unverified_reason(
        Some(reason_code),
        &scenario_id,
        &next_id,
        assertion_kind,
        assertion_seed_path,
    );
    // Note: "required_value_missing" is a legacy code, keep string check for compatibility
    if reason_code == "required_value_missing" {
        reason.push_str("; ");
        reason.push_str(&required_value_argv_rewrite_hint(ctx.surface, &next_id));
    }
    if scenario_missing && reason_kind == BehaviorReasonKind::MissingValueExamples {
        reason.push_str(
            "; scaffold argv uses a placeholder value token (optional: add value_examples overlay later)",
        );
    }
    if ordered_target_ids.len() > 1 {
        reason.push_str(&format!(
            "; batched deterministic scaffold for {} targets",
            ordered_target_ids.len()
        ));
    }
    reason.push_str("; apply patch as merge/upsert by scenario.id");
    let assertion_starters = if reason_kind == BehaviorReasonKind::NoScenario {
        assertion_starters_for_missing_assertions(entry, ctx.include_full)
    } else {
        Vec::new()
    };
    let payload = behavior_payload(BehaviorPayloadArgs {
        surface: Some(ctx.surface),
        target_ids: &ordered_target_ids,
        reason_code: Some(reason_code),
        action_kind: Some(reason_kind.suggested_action()),
        retry_counts,
        ledger_entries,
        suggested_overlay_keys: &[],
        assertion_starters,
        suggested_exclusion_payload: None,
        paths: ctx.paths,
    });
    Some(enrich::NextAction::Edit {
        path: "scenarios/plan.json".to_string(),
        content,
        reason,
        hint: Some("Add or fix behavior scenario assertions".to_string()),
        edit_strategy: crate::status::verification::BEHAVIOR_SCENARIO_EDIT_STRATEGY.to_string(),
        payload,
    })
}

/// Check if this is the initial behavior cycle (surface exists but no behavior scenarios).
///
/// Returns false if:
/// - required_ids is empty
/// - behavior scenarios exist in the plan
/// - ledger entries show failure reasons that imply scenarios were run
fn is_initial_behavior_cycle(
    plan: &scenarios::ScenarioPlan,
    required_ids: &[String],
    ledger_entries: &LedgerEntries,
) -> bool {
    // Must have some required surface items
    if required_ids.is_empty() {
        return false;
    }
    // Check if any behavior scenarios exist in the plan
    let has_behavior_scenarios = plan.scenarios.iter().any(|scenario| {
        scenario.coverage_tier.as_deref() == Some("behavior") && !scenario.covers.is_empty()
    });
    if has_behavior_scenarios {
        return false;
    }
    // Check if ledger entries show failure reasons that imply scenarios were run.
    // Reasons like scenario_error, outputs_equal, assertion_failed mean scenarios ran.
    let has_scenario_run_evidence = ledger_entries.values().any(|entry| {
        entry
            .behavior_unverified_reason_code
            .as_deref()
            .is_some_and(|code| {
                matches!(
                    code,
                    "scenario_error" | "outputs_equal" | "assertion_failed" | "auto_verify_timeout"
                )
            })
    });
    !has_scenario_run_evidence
}

/// Maybe set behavior next action based on evaluation state.
///
/// Uses a state machine to determine the action, then dispatches to action-specific handlers.
pub(super) fn maybe_set_behavior_next_action(
    ctx: &mut QueueVerificationContext<'_>,
    summary: &mut enrich::VerificationTriageSummary,
    state: &BehaviorEvalState<'_>,
) {
    let can_set_next_action = ctx.verification_next_action.is_none()
        && ctx.missing.is_empty()
        && ctx.local_blockers.is_empty();
    if !can_set_next_action {
        return;
    }

    // Determine the action using the state machine
    let Some(determined) = determine_behavior_action(ctx, state) else {
        return;
    };

    // Dispatch based on action type
    match determined.action {
        BehaviorAction::GenerateScenarios => {
            handle_generate_scenarios(ctx, summary, state, &determined);
        }
        BehaviorAction::AddWorkaround => {
            handle_add_workaround(ctx, summary, state, &determined);
        }
        BehaviorAction::Rerun => {
            handle_rerun(ctx, summary, state, &determined);
        }
        BehaviorAction::Exclude => {
            handle_exclude(ctx, summary, state, &determined);
        }
        BehaviorAction::FixScenario => {
            handle_fix_scenario(ctx, summary, state, &determined);
        }
        BehaviorAction::Apply => {
            handle_apply(ctx, state, &determined);
        }
        BehaviorAction::Defer => {
            // Deferred items are skipped - no action needed
        }
    }
}

/// Handle GenerateScenarios action.
fn handle_generate_scenarios(
    ctx: &mut QueueVerificationContext<'_>,
    summary: &mut enrich::VerificationTriageSummary,
    state: &BehaviorEvalState<'_>,
    determined: &DeterminedAction,
) {
    let target_ids = &determined.target_ids;
    let reason_code = determined.reason_kind.as_code();

    // For initial scenarios, build scaffold
    if determined.reason_kind == BehaviorReasonKind::InitialScenarios {
        let scaffold_context =
            build_scaffold_context(Some(ctx.surface), target_ids, Some(reason_code));
        let payload = behavior_payload(BehaviorPayloadArgs {
            surface: Some(ctx.surface),
            target_ids,
            reason_code: Some(reason_code),
            action_kind: Some(determined.action),
            retry_counts: state.retry_counts,
            ledger_entries: state.lookup_ctx.ledger_entries,
            suggested_overlay_keys: &[],
            assertion_starters: Vec::new(),
            suggested_exclusion_payload: None,
            paths: ctx.paths,
        });
        let content = crate::status::verification::behavior_scenarios_batch_stub(
            ctx.plan,
            ctx.surface,
            target_ids,
        )
        .unwrap_or_default();
        *ctx.verification_next_action = Some(enrich::NextAction::Edit {
            path: "scenarios/plan.json".to_string(),
            content,
            reason: format!(
                "generate initial behavior scenarios for {} surface items",
                target_ids.len()
            ),
            hint: Some("Generate initial behavior scenarios".to_string()),
            edit_strategy: crate::status::verification::BEHAVIOR_SCENARIO_EDIT_STRATEGY.to_string(),
            payload: payload.map(|mut p| {
                p.scaffold_context = scaffold_context;
                p
            }),
        });
    } else {
        // Other scenario generation (e.g., no_scenario) - use reason_based handler
        if let Some(action) = reason_based_behavior_next_action(
            ctx,
            summary,
            target_ids,
            state.lookup_ctx.missing_value_examples,
            state.retry_counts,
            state.lookup_ctx.ledger_entries,
        ) {
            *ctx.verification_next_action = Some(action);
        }
    }
}

/// Handle AddWorkaround action.
fn handle_add_workaround(
    ctx: &mut QueueVerificationContext<'_>,
    summary: &mut enrich::VerificationTriageSummary,
    state: &BehaviorEvalState<'_>,
    determined: &DeterminedAction,
) {
    let target_ids = &determined.target_ids;
    let content = surface_overlays_requires_argv_stub_batch(ctx.paths, ctx.surface, target_ids);
    summary.stub_blockers_preview = build_stub_blockers_preview(
        ctx,
        target_ids,
        state.lookup_ctx.ledger_entries,
        STUB_REASON_OUTPUTS_EQUAL_NEEDS_WORKAROUND,
        true,
    );
    let payload = behavior_payload(BehaviorPayloadArgs {
        surface: Some(ctx.surface),
        target_ids,
        reason_code: Some("outputs_equal"),
        action_kind: Some(determined.action),
        retry_counts: state.retry_counts,
        ledger_entries: state.lookup_ctx.ledger_entries,
        suggested_overlay_keys: &["overlays[].invocation.requires_argv"],
        assertion_starters: Vec::new(),
        suggested_exclusion_payload: None,
        paths: ctx.paths,
    });
    *ctx.verification_next_action = Some(enrich::NextAction::Edit {
        path: "inventory/surface.overlays.json".to_string(),
        content,
        reason: "add requires_argv workaround overlays in inventory/surface.overlays.json; see verification.stub_blockers_preview".to_string(),
        hint: Some("Add requires_argv workaround overlays".to_string()),
        edit_strategy: enrich::default_edit_strategy(),
        payload,
    });
}

/// Handle Rerun action.
fn handle_rerun(
    ctx: &mut QueueVerificationContext<'_>,
    summary: &mut enrich::VerificationTriageSummary,
    state: &BehaviorEvalState<'_>,
    determined: &DeterminedAction,
) {
    let target_ids = &determined.target_ids;
    summary.stub_blockers_preview = build_stub_blockers_preview(
        ctx,
        target_ids,
        state.lookup_ctx.ledger_entries,
        STUB_REASON_OUTPUTS_EQUAL_AFTER_WORKAROUND,
        true,
    );
    let scenario_ids = {
        let ids = rerun_scenario_ids_for_surface_ids(target_ids, state.lookup_ctx.ledger_entries);
        if ids.is_empty() {
            normalize_target_ids(target_ids)
                .into_iter()
                .map(|surface_id| {
                    format!(
                        "verify_{}",
                        surface_id.trim_start_matches('-').trim().replace('-', "_")
                    )
                })
                .collect::<Vec<_>>()
        } else {
            ids
        }
    };
    let command = targeted_outputs_equal_rerun_command(ctx.paths.root(), &scenario_ids);
    let payload = behavior_payload(BehaviorPayloadArgs {
        surface: Some(ctx.surface),
        target_ids,
        reason_code: Some("outputs_equal"),
        action_kind: Some(determined.action),
        retry_counts: state.retry_counts,
        ledger_entries: state.lookup_ctx.ledger_entries,
        suggested_overlay_keys: &["overlays[].behavior_exclusion"],
        assertion_starters: Vec::new(),
        suggested_exclusion_payload: None,
        paths: ctx.paths,
    });
    let retry = max_retry_count(target_ids, state.retry_counts).unwrap_or(0);
    *ctx.verification_next_action = Some(enrich::NextAction::Command {
        command,
        reason: format!(
            "requires_argv workaround is present but outputs_equal evidence has not progressed; rerun targeted behavior delta checks for {} scenario ids ({} targets, no-progress retry {}/{})",
            scenario_ids.len(),
            target_ids.len(),
            retry.saturating_add(1),
            BEHAVIOR_RERUN_CAP
        ),
        hint: Some("Rerun to verify workaround effect".to_string()),
        payload,
    });
}

/// Handle Exclude action (auto-exclude after cap hit).
fn handle_exclude(
    ctx: &mut QueueVerificationContext<'_>,
    summary: &mut enrich::VerificationTriageSummary,
    state: &BehaviorEvalState<'_>,
    determined: &DeterminedAction,
) {
    // Use the existing plateau handler for exclusions
    set_outputs_equal_plateau_next_action(
        ctx,
        summary,
        &determined.target_ids,
        state.retry_counts,
        state.lookup_ctx.ledger_entries,
    );
}

/// Handle FixScenario action.
fn handle_fix_scenario(
    ctx: &mut QueueVerificationContext<'_>,
    summary: &mut enrich::VerificationTriageSummary,
    state: &BehaviorEvalState<'_>,
    determined: &DeterminedAction,
) {
    // Delegate to the reason-based handler which handles assertion repair
    if let Some(action) = reason_based_behavior_next_action(
        ctx,
        summary,
        &determined.target_ids,
        state.lookup_ctx.missing_value_examples,
        state.retry_counts,
        state.lookup_ctx.ledger_entries,
    ) {
        *ctx.verification_next_action = Some(action);
    }
}

/// Handle Apply action (run apply command).
fn handle_apply(
    ctx: &mut QueueVerificationContext<'_>,
    state: &BehaviorEvalState<'_>,
    determined: &DeterminedAction,
) {
    let target_ids = &determined.target_ids;
    let reason_code = determined.reason_kind.as_code();
    let root = ctx.paths.root().display();

    let payload = behavior_payload(BehaviorPayloadArgs {
        surface: Some(ctx.surface),
        target_ids,
        reason_code: Some(reason_code),
        action_kind: Some(determined.action),
        retry_counts: state.retry_counts,
        ledger_entries: state.lookup_ctx.ledger_entries,
        suggested_overlay_keys: &[],
        assertion_starters: Vec::new(),
        suggested_exclusion_payload: None,
        paths: ctx.paths,
    });

    let (reason, hint) = match determined.reason_kind {
        BehaviorReasonKind::JudgmentRetry => {
            let reason_preview = if target_ids.len() == 1 {
                target_ids[0].clone()
            } else {
                format!("{} targets", target_ids.len())
            };
            (
                format!(
                    "retry behavior verification for {} (failed post-execution judgment)",
                    reason_preview
                ),
                "Retry with improved scenarios based on judgment feedback".to_string(),
            )
        }
        _ => {
            let reason_preview = if target_ids.len() == 1 {
                target_ids[0].clone()
            } else {
                format!("{} targets", target_ids.len())
            };
            (
                format!("run behavior verification for {reason_preview}"),
                "Run to execute behavior verification".to_string(),
            )
        }
    };

    *ctx.verification_next_action = Some(enrich::NextAction::Command {
        command: format!("bman apply --doc-pack {root}"),
        reason,
        hint: Some(hint),
        payload,
    });
}
