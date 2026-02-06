mod ledger;
mod overlays;
mod reasoning;
mod selectors;

use super::super::inputs::{
    load_scenario_plan_state, load_surface_inventory_state, ScenarioPlanLoadError,
    ScenarioPlanLoadResult, SurfaceLoadError, SurfaceLoadResult,
};
use super::{format_preview, preview_ids, EvalState};
use crate::status::verification_policy::{
    BehaviorReasonKind, DeltaOutcomeKind, VerificationStatus, VerificationTier,
};
use anyhow::Result;
use ledger::load_or_build_verification_ledger_entries;
use overlays::{
    build_stub_blockers_preview, surface_overlays_behavior_exclusion_stub_batch,
    surface_overlays_requires_argv_stub_batch, STUB_REASON_OUTPUTS_EQUAL_AFTER_WORKAROUND,
    STUB_REASON_OUTPUTS_EQUAL_NEEDS_WORKAROUND,
};
use reasoning::{
    behavior_reason_code_for_id, behavior_unverified_reason, build_behavior_reason_summary,
    build_behavior_unverified_preview, build_behavior_warnings, load_behavior_exclusion_state,
};
use selectors::{
    behavior_counts_for_ids, behavior_scenario_surface_ids, collect_missing_value_examples,
    first_matching_id, first_reason_id, first_reason_id_by_priority, needs_apply_ids,
    select_delta_outcome_ids_for_remaining, surface_has_requires_argv_hint,
};

use crate::enrich;
use crate::scenarios;

type LedgerEntries = std::collections::BTreeMap<String, scenarios::VerificationEntry>;

#[derive(Default)]
struct VerificationEvalOutput {
    triage_summary: Option<enrich::VerificationTriageSummary>,
    unverified_ids: Vec<String>,
    behavior_verified_count: Option<usize>,
    behavior_unverified_count: Option<usize>,
}

struct VerificationInputs {
    surface: Option<crate::surface::SurfaceInventory>,
    plan: Option<scenarios::ScenarioPlan>,
    surface_evidence: enrich::EvidenceRef,
    scenarios_evidence: enrich::EvidenceRef,
    template_path: std::path::PathBuf,
    template_evidence: enrich::EvidenceRef,
    semantics_path: std::path::PathBuf,
    semantics_evidence: enrich::EvidenceRef,
}

struct AutoVerificationContext<'a> {
    ledger_entries: Option<&'a LedgerEntries>,
    evidence: &'a mut Vec<enrich::EvidenceRef>,
    verification_next_action: &'a mut Option<enrich::NextAction>,
    local_blockers: &'a [enrich::Blocker],
    missing: &'a [String],
    paths: &'a enrich::DocPackPaths,
}

struct QueueVerificationContext<'a> {
    plan: &'a scenarios::ScenarioPlan,
    surface: &'a crate::surface::SurfaceInventory,
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

fn load_verification_inputs(
    paths: &enrich::DocPackPaths,
    missing_artifacts: &mut Vec<String>,
    missing: &mut Vec<String>,
    local_blockers: &mut Vec<enrich::Blocker>,
) -> Result<VerificationInputs> {
    let SurfaceLoadResult {
        evidence: surface_evidence,
        surface,
        error,
    } = load_surface_inventory_state(paths, missing_artifacts)?;
    let ScenarioPlanLoadResult {
        evidence: scenarios_evidence,
        plan,
        error: plan_error,
    } = load_scenario_plan_state(paths, missing_artifacts)?;
    let template_path = paths
        .root()
        .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);
    let template_evidence = paths.evidence_from_path(&template_path)?;
    let semantics_path = paths.semantics_path();
    let semantics_evidence = paths.evidence_from_path(&semantics_path)?;

    let surface = match error {
        Some(SurfaceLoadError::Missing) => {
            missing.push("surface inventory missing".to_string());
            None
        }
        Some(SurfaceLoadError::Parse(message)) => {
            let blocker = enrich::Blocker {
                code: "surface_parse_error".to_string(),
                message,
                evidence: vec![surface_evidence.clone()],
                next_action: None,
            };
            local_blockers.push(blocker);
            None
        }
        Some(SurfaceLoadError::Invalid(message)) => {
            let blocker = enrich::Blocker {
                code: "surface_schema_invalid".to_string(),
                message,
                evidence: vec![surface_evidence.clone()],
                next_action: Some("fix inventory/surface.json".to_string()),
            };
            local_blockers.push(blocker);
            None
        }
        None => surface,
    };

    let plan = match plan_error {
        Some(ScenarioPlanLoadError::Missing) => {
            missing.push("scenarios plan missing".to_string());
            None
        }
        Some(ScenarioPlanLoadError::Invalid(message)) => {
            let blocker = enrich::Blocker {
                code: "scenario_plan_invalid".to_string(),
                message,
                evidence: vec![scenarios_evidence.clone()],
                next_action: Some("fix scenarios/plan.json".to_string()),
            };
            local_blockers.push(blocker);
            None
        }
        None => plan,
    };

    if !template_path.is_file() {
        missing_artifacts.push(template_evidence.path.clone());
        missing.push(format!(
            "verification lens missing ({})",
            enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL
        ));
    }
    if !semantics_path.is_file() {
        missing_artifacts.push(semantics_evidence.path.clone());
        missing.push("verification semantics missing (enrich/semantics.json)".to_string());
    }

    Ok(VerificationInputs {
        surface,
        plan,
        surface_evidence,
        scenarios_evidence,
        template_path,
        template_evidence,
        semantics_path,
        semantics_evidence,
    })
}

fn base_evidence(inputs: &VerificationInputs) -> Vec<enrich::EvidenceRef> {
    vec![
        inputs.surface_evidence.clone(),
        inputs.scenarios_evidence.clone(),
        inputs.template_evidence.clone(),
        inputs.semantics_evidence.clone(),
    ]
}

fn eval_auto_verification(
    auto_state: crate::status::verification::AutoVerificationState,
    ctx: &mut AutoVerificationContext<'_>,
) -> VerificationEvalOutput {
    let crate::status::verification::AutoVerificationState {
        targets,
        remaining_ids,
        remaining_by_kind,
        excluded,
        excluded_count,
        ..
    } = auto_state;
    if let Some(ledger_entries) = ctx.ledger_entries {
        for surface_id in &remaining_ids {
            if let Some(entry) = ledger_entries.get(surface_id) {
                ctx.evidence.extend(entry.evidence.iter().cloned());
            }
        }
    }
    let remaining_preview = preview_ids(&remaining_ids);
    let remaining_by_kind_summary = remaining_by_kind
        .iter()
        .map(|group| enrich::VerificationKindSummary {
            kind: group.kind.as_str().to_string(),
            target_count: group.target_count,
            remaining_count: group.remaining_ids.len(),
            remaining_preview: preview_ids(&group.remaining_ids),
        })
        .collect();
    let excluded_ids = excluded
        .iter()
        .map(|entry| entry.surface_id.clone())
        .collect::<Vec<_>>();
    let summary = enrich::VerificationTriageSummary {
        triaged_unverified_count: remaining_ids.len(),
        triaged_unverified_preview: remaining_preview,
        remaining_by_kind: remaining_by_kind_summary,
        excluded_count: (excluded_count > 0).then_some(excluded_count),
        behavior_excluded_count: excluded_count,
        behavior_excluded_preview: preview_ids(&excluded_ids),
        behavior_excluded_reasons: Vec::new(),
        excluded,
        behavior_unverified_reasons: Vec::new(),
        behavior_unverified_preview: Vec::new(),
        behavior_unverified_diagnostics: Vec::new(),
        behavior_warnings: Vec::new(),
        stub_blockers_preview: Vec::new(),
    };
    let summary_preview = format!(
        "auto verification: {} remaining ({})",
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
        behavior_verified_count: None,
        behavior_unverified_count: None,
    };
    if has_unverified {
        output.unverified_ids.push(summary_preview);
    }
    if ctx.verification_next_action.is_none()
        && !remaining_ids.is_empty()
        && ctx.local_blockers.is_empty()
        && ctx.missing.is_empty()
    {
        let remaining_total = remaining_ids.len();
        let by_kind = remaining_by_kind
            .iter()
            .map(|group| format!("{} {}", group.kind.as_str(), group.remaining_ids.len()))
            .collect::<Vec<_>>()
            .join(", ");
        let mut reason = format!("auto verification remaining: {remaining_total}");
        if !by_kind.is_empty() {
            reason.push_str(&format!(" ({by_kind})"));
        }
        reason.push_str(&format!(
            "; max_new_runs_per_apply={}",
            targets.max_new_runs_per_apply
        ));
        reason.push_str(&format!(
            "; set scenarios/plan.json.verification.policy.max_new_runs_per_apply >= {remaining_total} to finish in one apply"
        ));
        let root = ctx.paths.root().display();
        *ctx.verification_next_action = Some(enrich::NextAction::Command {
            command: format!("bman apply --doc-pack {root}"),
            reason,
            payload: None,
        });
    }

    output
}

const BEHAVIOR_BATCH_LIMIT: usize = 10;
const BEHAVIOR_RERUN_CAP: usize = 2;
const DELTA_PATH_FALLBACK: &str = "inventory/scenarios/<delta_variant>.json";
const STARTER_SEED_PATH_PLACEHOLDER: &str = "work/item.txt";
const STARTER_STDOUT_TOKEN_PLACEHOLDER: &str = "item.txt";
const REQUIRED_VALUE_PLACEHOLDER: &str = "__value__";

#[derive(serde::Deserialize)]
struct ScenarioEvidenceId {
    scenario_id: Option<String>,
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

fn latest_delta_path_for_ids(
    target_ids: &[String],
    ledger_entries: &LedgerEntries,
) -> Option<String> {
    target_ids
        .iter()
        .find_map(|surface_id| latest_delta_path_for_entry(ledger_entries.get(surface_id)))
}

fn preferred_behavior_scenario_id(entry: &scenarios::VerificationEntry) -> Option<String> {
    entry
        .behavior_unverified_scenario_id
        .as_deref()
        .into_iter()
        .chain(entry.behavior_scenario_ids.iter().map(String::as_str))
        .map(str::trim)
        .find(|scenario_id| !scenario_id.is_empty())
        .map(str::to_string)
}

fn scenario_id_from_evidence_path(path: &str) -> Option<String> {
    let filename = std::path::Path::new(path.trim()).file_name()?.to_str()?;
    let stem = filename.strip_suffix(".json")?;
    let (scenario_id, epoch_suffix) = stem.rsplit_once('-')?;
    epoch_suffix
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| scenario_id.to_string())
}

fn scenario_id_from_evidence_file(paths: &enrich::DocPackPaths, rel_path: &str) -> Option<String> {
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

fn retry_count_for_entry(
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

fn load_behavior_retry_counts(
    paths: &enrich::DocPackPaths,
    ledger_entries: &LedgerEntries,
) -> std::collections::BTreeMap<String, usize> {
    let mut retry_counts = std::collections::BTreeMap::new();
    for (surface_id, entry) in ledger_entries {
        if let Some(retry_count) = retry_count_for_entry(paths, entry) {
            retry_counts.insert(surface_id.clone(), retry_count);
        }
    }
    retry_counts
}

fn max_retry_count(
    target_ids: &[String],
    retry_counts: &std::collections::BTreeMap<String, usize>,
) -> Option<usize> {
    target_ids
        .iter()
        .filter_map(|surface_id| retry_counts.get(surface_id).copied())
        .max()
}

fn behavior_payload(
    target_ids: &[String],
    reason_code: Option<&str>,
    retry_counts: &std::collections::BTreeMap<String, usize>,
    ledger_entries: &LedgerEntries,
    suggested_overlay_keys: &[&str],
    assertion_starters: Vec<enrich::BehaviorAssertionStarter>,
    suggested_exclusion_payload: Option<enrich::SuggestedBehaviorExclusionPayload>,
) -> Option<enrich::BehaviorNextActionPayload> {
    let target_ids = normalize_target_ids(target_ids);
    let reason_code = reason_code
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .map(str::to_string);
    let retry_count = max_retry_count(&target_ids, retry_counts);
    let mut latest_delta_path = latest_delta_path_for_ids(&target_ids, ledger_entries);
    if latest_delta_path.is_none()
        && reason_code
            .as_deref()
            .is_some_and(|code| matches!(code, "outputs_equal" | "missing_delta_assertion"))
    {
        latest_delta_path = Some(DELTA_PATH_FALLBACK.to_string());
    }
    let suggested_overlay_keys = suggested_overlay_keys
        .iter()
        .map(|key| key.to_string())
        .collect();
    let payload = enrich::BehaviorNextActionPayload {
        target_ids,
        reason_code,
        retry_count,
        latest_delta_path,
        suggested_overlay_keys,
        assertion_starters,
        suggested_exclusion_payload,
    };
    (!payload.is_empty()).then_some(payload)
}

fn assertion_starters_for_missing_assertions(
    entry: Option<&scenarios::VerificationEntry>,
    include_full: bool,
) -> Vec<enrich::BehaviorAssertionStarter> {
    let seed_path = entry
        .and_then(|entry| entry.behavior_unverified_assertion_seed_path.clone())
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty());
    let mut starters = if let Some(seed_path) = seed_path {
        let stdout_token = entry
            .and_then(|entry| entry.behavior_unverified_assertion_token.clone())
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
            .or_else(|| seed_path.rsplit('/').next().map(str::to_string));
        vec![
            enrich::BehaviorAssertionStarter {
                kind: "baseline_stdout_not_contains_seed_path".to_string(),
                seed_path: Some(seed_path.clone()),
                stdout_token: stdout_token.clone(),
            },
            enrich::BehaviorAssertionStarter {
                kind: "variant_stdout_contains_seed_path".to_string(),
                seed_path: Some(seed_path.clone()),
                stdout_token: stdout_token.clone(),
            },
            enrich::BehaviorAssertionStarter {
                kind: "baseline_stdout_contains_seed_path".to_string(),
                seed_path: Some(seed_path.clone()),
                stdout_token: stdout_token.clone(),
            },
            enrich::BehaviorAssertionStarter {
                kind: "variant_stdout_not_contains_seed_path".to_string(),
                seed_path: Some(seed_path),
                stdout_token,
            },
        ]
    } else {
        vec![
            enrich::BehaviorAssertionStarter {
                kind: "baseline_stdout_not_contains_seed_path".to_string(),
                seed_path: Some(STARTER_SEED_PATH_PLACEHOLDER.to_string()),
                stdout_token: Some(STARTER_STDOUT_TOKEN_PLACEHOLDER.to_string()),
            },
            enrich::BehaviorAssertionStarter {
                kind: "variant_stdout_contains_seed_path".to_string(),
                seed_path: Some(STARTER_SEED_PATH_PLACEHOLDER.to_string()),
                stdout_token: Some(STARTER_STDOUT_TOKEN_PLACEHOLDER.to_string()),
            },
            enrich::BehaviorAssertionStarter {
                kind: "baseline_stdout_contains_seed_path".to_string(),
                seed_path: Some(STARTER_SEED_PATH_PLACEHOLDER.to_string()),
                stdout_token: Some(STARTER_STDOUT_TOKEN_PLACEHOLDER.to_string()),
            },
            enrich::BehaviorAssertionStarter {
                kind: "variant_stdout_not_contains_seed_path".to_string(),
                seed_path: Some(STARTER_SEED_PATH_PLACEHOLDER.to_string()),
                stdout_token: Some(STARTER_STDOUT_TOKEN_PLACEHOLDER.to_string()),
            },
        ]
    };
    if !include_full {
        starters.truncate(2);
    }
    starters
}

fn preferred_required_value_token(
    surface: &crate::surface::SurfaceInventory,
    surface_id: &str,
) -> String {
    let Some(item) = crate::surface::primary_surface_item_by_id(surface, surface_id) else {
        return REQUIRED_VALUE_PLACEHOLDER.to_string();
    };
    item.invocation
        .value_examples
        .iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            item.invocation
                .value_placeholder
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| REQUIRED_VALUE_PLACEHOLDER.to_string())
}

fn required_value_argv_rewrite_hint(
    surface: &crate::surface::SurfaceInventory,
    surface_id: &str,
) -> String {
    let value_token = preferred_required_value_token(surface, surface_id);
    let separator = crate::surface::primary_surface_item_by_id(surface, surface_id)
        .map(|item| item.invocation.value_separator.as_str())
        .unwrap_or("unknown");
    let argv_fragment = match separator {
        "equals" => format!("{surface_id}={value_token}"),
        "space" => format!("{surface_id} {value_token}"),
        _ => format!("{surface_id}={value_token} or {surface_id} {value_token}"),
    };
    format!("scenario.argv should include `{argv_fragment}`")
}

fn suggested_exclusion_payload(
    surface_kind: &str,
    surface_id: &str,
    reason_code: &str,
    retry_count: usize,
    delta_variant_path_after: Option<&str>,
) -> enrich::SuggestedBehaviorExclusionPayload {
    let (exclusion_reason_code, workaround_kind, ref_path) = match reason_code {
        "missing_delta_assertion" => ("assertion_gap", "other", "scenarios/plan.json"),
        _ => (
            "fixture_gap",
            "added_requires_argv",
            "inventory/surface.overlays.json",
        ),
    };
    let note = format!(
        "reason_code={reason_code}; rerun cap reached after {retry_count} retries; exclude only if behavior remains unverifiable"
    );
    let delta_variant_path_after = delta_variant_path_after
        .unwrap_or(DELTA_PATH_FALLBACK)
        .to_string();
    let fallback_workaround = enrich::SuggestedBehaviorExclusionWorkaround {
        kind: workaround_kind.to_string(),
        ref_path: ref_path.to_string(),
        delta_variant_path_after,
    };
    enrich::SuggestedBehaviorExclusionPayload {
        kind: surface_kind.to_string(),
        id: surface_id.to_string(),
        behavior_exclusion: enrich::SuggestedBehaviorExclusion {
            reason_code: exclusion_reason_code.to_string(),
            note: Some(note),
            evidence: enrich::SuggestedBehaviorExclusionEvidence {
                attempted_workarounds: vec![fallback_workaround],
            },
        },
    }
}

fn suggested_exclusion_only_next_action(
    ctx: &QueueVerificationContext<'_>,
    target_ids: &[String],
    reason_code: &str,
    retry_counts: &std::collections::BTreeMap<String, usize>,
    ledger_entries: &LedgerEntries,
) -> enrich::NextAction {
    let next_id = target_ids.first().cloned().unwrap_or_default();
    let retry_count = retry_counts.get(&next_id).copied().unwrap_or(0);
    let suggested = suggested_exclusion_payload(
        &selectors::surface_kind_for_id(ctx.surface, &next_id, "option"),
        &next_id,
        reason_code,
        retry_count,
        latest_delta_path_for_entry(ledger_entries.get(&next_id)).as_deref(),
    );
    let payload = behavior_payload(
        target_ids,
        Some(reason_code),
        retry_counts,
        ledger_entries,
        &["overlays[].behavior_exclusion"],
        Vec::new(),
        Some(suggested),
    );
    let root = ctx.paths.root().display();
    enrich::NextAction::Command {
        command: format!("bman status --doc-pack {root}"),
        reason: format!(
            "rerun cap reached for {reason_code}; review next_action.payload.suggested_exclusion_payload and apply exclusion manually if justified"
        ),
        payload,
    }
}

fn partition_cap_hit(
    surface_ids: Vec<String>,
    retry_counts: &std::collections::BTreeMap<String, usize>,
) -> (Vec<String>, Vec<String>) {
    surface_ids.into_iter().partition(|surface_id| {
        retry_counts.get(surface_id).copied().unwrap_or(0) >= BEHAVIOR_RERUN_CAP
    })
}

fn set_outputs_equal_cap_hit_next_action(
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
    *ctx.verification_next_action = Some(suggested_exclusion_only_next_action(
        ctx,
        cap_hit,
        "outputs_equal",
        retry_counts,
        ledger_entries,
    ));
    true
}

#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
fn eval_behavior_verification(ctx: &mut QueueVerificationContext<'_>) -> VerificationEvalOutput {
    let Some(targets) = scenarios::auto_verification_targets_for_behavior(ctx.plan, ctx.surface)
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

    let mut remaining_ids = Vec::new();
    let mut behavior_verified_count = 0;
    for surface_id in required_ids {
        if excluded_set.contains(surface_id) {
            continue;
        }
        let status = VerificationStatus::from_entry(
            ledger_entries.get(surface_id),
            VerificationTier::Behavior,
        );
        if status == VerificationStatus::Verified {
            behavior_verified_count += 1;
        } else {
            remaining_ids.push(surface_id.clone());
            if let Some(entry) = ledger_entries.get(surface_id) {
                ctx.evidence.extend(entry.evidence.iter().cloned());
            }
        }
    }
    remaining_ids.sort();
    remaining_ids.dedup();

    let remaining_set: std::collections::BTreeSet<String> = remaining_ids.iter().cloned().collect();
    let remaining_preview = preview_ids(&remaining_ids);
    let missing_value_examples =
        collect_missing_value_examples(ctx.surface, &remaining_ids, ledger_entries);
    let needs_apply_ids = needs_apply_ids(&plan_behavior_ids, &remaining_set, ledger_entries);
    let retry_counts = load_behavior_retry_counts(ctx.paths, ledger_entries);
    let outputs_equal_ids = select_delta_outcome_ids_for_remaining(
        required_ids,
        &remaining_set,
        &missing_value_examples,
        ledger_entries,
        DeltaOutcomeKind::OutputsEqual,
        BEHAVIOR_BATCH_LIMIT,
    );
    let (outputs_equal_with_workaround, outputs_equal_without_workaround): (Vec<_>, Vec<_>) =
        outputs_equal_ids
            .into_iter()
            .partition(|surface_id| surface_has_requires_argv_hint(ctx.surface, surface_id));
    let (
        outputs_equal_with_workaround_needs_rerun,
        outputs_equal_with_workaround_ready_for_exclusion,
    ): (Vec<_>, Vec<_>) = outputs_equal_with_workaround
        .into_iter()
        .partition(|surface_id| {
            ledger_entries
                .get(surface_id.as_str())
                .is_some_and(|entry| outputs_equal_workaround_needs_delta_rerun(ctx.paths, entry))
        });
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

    let can_set_next_action = ctx.verification_next_action.is_none()
        && ctx.missing.is_empty()
        && ctx.local_blockers.is_empty();

    if can_set_next_action {
        let non_blocking_missing_value_examples = std::collections::BTreeSet::new();
        if !outputs_equal_without_workaround.is_empty() {
            let content = surface_overlays_requires_argv_stub_batch(
                ctx.paths,
                ctx.surface,
                &outputs_equal_without_workaround,
            );
            summary.stub_blockers_preview = build_stub_blockers_preview(
                ctx,
                &outputs_equal_without_workaround,
                ledger_entries,
                STUB_REASON_OUTPUTS_EQUAL_NEEDS_WORKAROUND,
                true,
            );
            let payload = behavior_payload(
                &outputs_equal_without_workaround,
                Some("outputs_equal"),
                &retry_counts,
                ledger_entries,
                &["overlays[].invocation.requires_argv"],
                Vec::new(),
                None,
            );
            *ctx.verification_next_action = Some(enrich::NextAction::Edit {
                path: "inventory/surface.overlays.json".to_string(),
                content,
                reason: "add requires_argv workaround overlays in inventory/surface.overlays.json; see verification.stub_blockers_preview".to_string(),
                edit_strategy: enrich::default_edit_strategy(),
                payload,
            });
        } else if !outputs_equal_with_workaround_needs_rerun.is_empty() {
            let (cap_hit, needs_rerun) =
                partition_cap_hit(outputs_equal_with_workaround_needs_rerun, &retry_counts);
            if !set_outputs_equal_cap_hit_next_action(
                ctx,
                &mut summary,
                &cap_hit,
                &retry_counts,
                ledger_entries,
            ) && !needs_rerun.is_empty()
            {
                summary.stub_blockers_preview = build_stub_blockers_preview(
                    ctx,
                    &needs_rerun,
                    ledger_entries,
                    STUB_REASON_OUTPUTS_EQUAL_AFTER_WORKAROUND,
                    true,
                );
                let root = ctx.paths.root().display();
                let next_id = needs_rerun[0].clone();
                let payload = behavior_payload(
                    &needs_rerun,
                    Some("outputs_equal"),
                    &retry_counts,
                    ledger_entries,
                    &["overlays[].behavior_exclusion"],
                    Vec::new(),
                    None,
                );
                *ctx.verification_next_action = Some(enrich::NextAction::Command {
                    command: format!("bman apply --doc-pack {root}"),
                    reason: format!(
                        "rerun behavior delta checks after requires_argv workaround for {next_id} ({} targets)",
                        needs_rerun.len()
                    ),
                    payload,
                });
            }
        } else if !outputs_equal_with_workaround_ready_for_exclusion.is_empty() {
            let (cap_hit, ready_for_exclusion) = partition_cap_hit(
                outputs_equal_with_workaround_ready_for_exclusion,
                &retry_counts,
            );
            if !set_outputs_equal_cap_hit_next_action(
                ctx,
                &mut summary,
                &cap_hit,
                &retry_counts,
                ledger_entries,
            ) && !ready_for_exclusion.is_empty()
            {
                let content = surface_overlays_behavior_exclusion_stub_batch(
                    ctx.paths,
                    ctx.surface,
                    &ready_for_exclusion,
                    ledger_entries,
                );
                summary.stub_blockers_preview = build_stub_blockers_preview(
                    ctx,
                    &ready_for_exclusion,
                    ledger_entries,
                    STUB_REASON_OUTPUTS_EQUAL_AFTER_WORKAROUND,
                    true,
                );
                let payload = behavior_payload(
                    &ready_for_exclusion,
                    Some("outputs_equal"),
                    &retry_counts,
                    ledger_entries,
                    &["overlays[].behavior_exclusion"],
                    Vec::new(),
                    None,
                );
                *ctx.verification_next_action = Some(enrich::NextAction::Edit {
                    path: "inventory/surface.overlays.json".to_string(),
                    content,
                    reason: "record behavior exclusions in inventory/surface.overlays.json; see verification.stub_blockers_preview".to_string(),
                    edit_strategy: enrich::default_edit_strategy(),
                    payload,
                });
            }
        } else if let Some(next_id) = first_reason_id_by_priority(
            required_ids,
            &remaining_set,
            &non_blocking_missing_value_examples,
            &needs_apply_ids,
            ledger_entries,
            &[
                BehaviorReasonKind::RequiredValueMissing,
                BehaviorReasonKind::AssertionSeedPathNotSeeded,
                BehaviorReasonKind::SeedSignatureMismatch,
                BehaviorReasonKind::SeedMismatch,
                BehaviorReasonKind::AssertionFailed,
                BehaviorReasonKind::ScenarioFailed,
                BehaviorReasonKind::MissingAssertions,
            ],
        )
        .or_else(|| {
            first_reason_id_by_priority(
                required_ids,
                &remaining_set,
                &non_blocking_missing_value_examples,
                &needs_apply_ids,
                ledger_entries,
                &[
                    BehaviorReasonKind::MissingDeltaAssertion,
                    BehaviorReasonKind::MissingSemanticPredicate,
                ],
            )
        })
        .or_else(|| {
            first_reason_id_by_priority(
                required_ids,
                &remaining_set,
                &non_blocking_missing_value_examples,
                &needs_apply_ids,
                ledger_entries,
                &[BehaviorReasonKind::MissingBehaviorScenario],
            )
        })
        .or_else(|| {
            first_reason_id(
                required_ids,
                &remaining_set,
                &non_blocking_missing_value_examples,
                &needs_apply_ids,
            )
        }) {
            let reason_code =
                behavior_reason_code_for_id(&next_id, &missing_value_examples, ledger_entries);
            let entry = ledger_entries.get(&next_id);
            let scenario_missing =
                entry.is_some_and(|entry| entry.behavior_scenario_ids.is_empty());
            let scenario_id = entry
                .and_then(|entry| {
                    entry
                        .behavior_unverified_scenario_id
                        .as_deref()
                        .or_else(|| entry.behavior_scenario_ids.first().map(String::as_str))
                })
                .map(str::to_string)
                .unwrap_or_else(|| next_id.clone());
            let assertion_kind =
                entry.and_then(|entry| entry.behavior_unverified_assertion_kind.as_deref());
            let assertion_seed_path =
                entry.and_then(|entry| entry.behavior_unverified_assertion_seed_path.as_deref());
            let action_reason_code = if scenario_missing && reason_code == "missing_value_examples"
            {
                "missing_behavior_scenario".to_string()
            } else {
                reason_code.clone()
            };
            let retry_count = retry_counts.get(&next_id).copied().unwrap_or(0);
            if reason_code == "missing_delta_assertion" && retry_count >= BEHAVIOR_RERUN_CAP {
                *ctx.verification_next_action = Some(suggested_exclusion_only_next_action(
                    ctx,
                    std::slice::from_ref(&next_id),
                    "missing_delta_assertion",
                    &retry_counts,
                    ledger_entries,
                ));
            } else {
                let content = if scenario_missing {
                    let target_ids = vec![next_id.clone()];
                    summary.stub_blockers_preview = build_stub_blockers_preview(
                        ctx,
                        &target_ids,
                        ledger_entries,
                        &reason_code,
                        false,
                    );
                    crate::status::verification::behavior_scenarios_batch_stub(
                        ctx.plan,
                        ctx.surface,
                        &target_ids,
                    )
                    .or_else(|| {
                        crate::status::verification::behavior_baseline_stub(ctx.plan, ctx.surface)
                    })
                } else {
                    crate::status::verification::behavior_scenario_stub(ctx.plan, &scenario_id)
                        .or_else(|| {
                            crate::status::verification::behavior_scenarios_batch_stub(
                                ctx.plan,
                                ctx.surface,
                                std::slice::from_ref(&next_id),
                            )
                        })
                };
                if let Some(content) = content {
                    let mut reason = behavior_unverified_reason(
                        Some(&action_reason_code),
                        &scenario_id,
                        &next_id,
                        assertion_kind,
                        assertion_seed_path,
                    );
                    if action_reason_code == "required_value_missing" {
                        reason.push_str("; ");
                        reason.push_str(&required_value_argv_rewrite_hint(ctx.surface, &next_id));
                    }
                    if scenario_missing && reason_code == "missing_value_examples" {
                        reason.push_str(
                            "; scaffold argv uses a placeholder value token (optional: add value_examples overlay later)",
                        );
                    }
                    reason.push_str("; apply patch as merge/upsert by scenario.id");
                    let assertion_starters = if matches!(
                        action_reason_code.as_str(),
                        "missing_assertions" | "missing_behavior_scenario"
                    ) {
                        assertion_starters_for_missing_assertions(entry, ctx.include_full)
                    } else {
                        Vec::new()
                    };
                    let payload = behavior_payload(
                        std::slice::from_ref(&next_id),
                        Some(&action_reason_code),
                        &retry_counts,
                        ledger_entries,
                        &[],
                        assertion_starters,
                        None,
                    );
                    *ctx.verification_next_action = Some(enrich::NextAction::Edit {
                        path: "scenarios/plan.json".to_string(),
                        content,
                        reason,
                        edit_strategy: crate::status::verification::BEHAVIOR_SCENARIO_EDIT_STRATEGY
                            .to_string(),
                        payload,
                    });
                }
            }
        } else if let Some(next_id) = first_matching_id(required_ids, &needs_apply_ids) {
            let root = ctx.paths.root().display();
            let payload = behavior_payload(
                std::slice::from_ref(&next_id),
                Some("needs_apply"),
                &retry_counts,
                ledger_entries,
                &[],
                Vec::new(),
                None,
            );
            *ctx.verification_next_action = Some(enrich::NextAction::Command {
                command: format!("bman apply --doc-pack {root}"),
                reason: format!("run behavior verification for {next_id}"),
                payload,
            });
        }
    }

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

fn ensure_verification_policy(
    plan: &scenarios::ScenarioPlan,
    missing: &mut Vec<String>,
    verification_next_action: &mut Option<enrich::NextAction>,
    binary_name: Option<&str>,
) {
    if plan.verification.policy.is_some() {
        return;
    }
    missing.push("verification policy missing (scenarios/plan.json)".to_string());
    let content =
        serde_json::to_string_pretty(plan).unwrap_or_else(|_| scenarios::plan_stub(binary_name));
    *verification_next_action = Some(enrich::NextAction::Edit {
        path: "scenarios/plan.json".to_string(),
        content,
        reason: "add verification policy in scenarios/plan.json".to_string(),
        edit_strategy: enrich::default_edit_strategy(),
        payload: None,
    });
}

pub(super) fn eval_verification_requirement(
    state: &mut EvalState,
    req: enrich::RequirementId,
) -> Result<enrich::RequirementStatus> {
    let paths = state.paths;
    let binary_name = state.binary_name;
    let config = state.config;
    let lock_status = state.lock_status;
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

    if let (Some(surface), Some(plan)) = (inputs.surface.as_ref(), inputs.plan.as_ref()) {
        if inputs.template_path.is_file() && inputs.semantics_path.is_file() {
            ledger_snapshot = load_or_build_verification_ledger_entries(
                binary_name,
                surface,
                plan,
                paths,
                &inputs.template_path,
                lock_status,
                &mut local_blockers,
                &inputs.template_evidence,
            );
        }
        let ledger_entries = ledger_snapshot.as_ref().map(|snapshot| &snapshot.entries);

        ensure_verification_policy(plan, &mut missing, verification_next_action, binary_name);

        let behavior_targets = if verification_tier.is_behavior() {
            scenarios::auto_verification_targets_for_behavior(plan, surface)
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
                    existence_output = Some(output);
                }
            }

            if let Some(output) = existence_output {
                output
            } else {
                let mut ctx = QueueVerificationContext {
                    plan,
                    surface,
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
