mod auto;
mod inputs;
mod ledger;
mod overlays;
mod reasoning;
mod selectors;

use super::{format_preview, preview_ids, EvalState};
use crate::status::verification_policy::{
    BehaviorReasonKind, DeltaOutcomeKind, VerificationStatus, VerificationTier,
};
use anyhow::{anyhow, Result};
use auto::{eval_auto_verification, AutoVerificationContext};
use inputs::{base_evidence, ensure_verification_policy, load_verification_inputs};
use ledger::build_verification_ledger_entries;
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
use crate::verification_progress::{
    build_action_signature, get_assertion_failed_no_progress_count, is_noop_action,
    load_verification_progress, outputs_equal_delta_signature, scenario_id_from_evidence_path,
    VerificationProgress,
};

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

const BEHAVIOR_BATCH_LIMIT: usize = 10;
const BEHAVIOR_RERUN_CAP: usize = 2;
const ASSERTION_FAILED_NOOP_CAP: usize = 2;
const DELTA_PATH_FALLBACK: &str = "inventory/scenarios/<delta_variant>.json";
const STARTER_SEED_PATH_PLACEHOLDER: &str = "work/item.txt";
const STARTER_STDOUT_TOKEN_PLACEHOLDER: &str = "item.txt";
const REQUIRED_VALUE_PLACEHOLDER: &str = "__value__";

#[derive(serde::Deserialize)]
struct ScenarioEvidenceId {
    scenario_id: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BehaviorMergePatchPayload {
    #[serde(default)]
    defaults: Option<serde_json::Value>,
    #[serde(default)]
    upsert_scenarios: Vec<scenarios::ScenarioSpec>,
}

#[derive(serde::Serialize)]
struct BehaviorScaffoldEditPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    defaults: Option<scenarios::ScenarioDefaults>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    upsert_scenarios: Vec<scenarios::ScenarioSpec>,
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

fn load_behavior_retry_counts(
    paths: &enrich::DocPackPaths,
    ledger_entries: &LedgerEntries,
    progress: &VerificationProgress,
    outputs_equal_surface_ids: &[String],
) -> std::collections::BTreeMap<String, usize> {
    let mut retry_counts = load_historical_behavior_retry_counts(paths, ledger_entries);
    for surface_id in normalize_target_ids(outputs_equal_surface_ids) {
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

fn action_reason_code_for_surface_id(
    surface_id: &str,
    missing_value_examples: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
) -> String {
    let reason_code =
        behavior_reason_code_for_id(surface_id, missing_value_examples, ledger_entries);
    let entry = ledger_entries.get(surface_id);
    let scenario_missing = entry.is_some_and(|entry| entry.behavior_scenario_ids.is_empty());
    // Normalize missing_value_examples to no_scenario when scenario is absent
    if scenario_missing && reason_code == "missing_value_examples" {
        "no_scenario".to_string()
    } else {
        reason_code
    }
}

fn batched_target_ids_for_reason(
    required_ids: &[String],
    remaining_set: &std::collections::BTreeSet<String>,
    missing_value_examples: &std::collections::BTreeSet<String>,
    needs_apply_ids: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
    reason_code: &str,
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
        if missing_value_examples.contains(surface_id) || needs_apply_ids.contains(surface_id) {
            continue;
        }
        if action_reason_code_for_surface_id(surface_id, missing_value_examples, ledger_entries)
            != reason_code
        {
            continue;
        }
        selected.push(surface_id.clone());
    }
    selected
}

fn render_behavior_scaffold_content(
    defaults: Option<scenarios::ScenarioDefaults>,
    mut upsert_scenarios: Vec<scenarios::ScenarioSpec>,
) -> Option<String> {
    upsert_scenarios.sort_by(|a, b| a.id.cmp(&b.id));
    upsert_scenarios.dedup_by(|a, b| a.id == b.id);
    if defaults.is_none() && upsert_scenarios.is_empty() {
        return None;
    }
    let payload = BehaviorScaffoldEditPayload {
        defaults,
        upsert_scenarios,
    };
    serde_json::to_string_pretty(&payload).ok()
}

fn merge_defaults_patch(
    plan: &mut scenarios::ScenarioPlan,
    defaults_patch: &serde_json::Value,
) -> Result<()> {
    let defaults_map = defaults_patch
        .as_object()
        .ok_or_else(|| anyhow!("merge payload defaults must be a JSON object"))?;
    let mut merged_defaults = serde_json::to_value(plan.defaults.clone().unwrap_or_default())
        .map_err(|err| anyhow!("serialize existing scenario defaults: {err}"))?;
    let merged_map = merged_defaults
        .as_object_mut()
        .ok_or_else(|| anyhow!("existing defaults must serialize as a JSON object"))?;
    for (key, value) in defaults_map {
        merged_map.insert(key.clone(), value.clone());
    }
    let parsed: scenarios::ScenarioDefaults = serde_json::from_value(merged_defaults)
        .map_err(|err| anyhow!("parse merged scenario defaults: {err}"))?;
    plan.defaults = Some(parsed);
    Ok(())
}

fn project_behavior_scaffold_merge(
    plan: &scenarios::ScenarioPlan,
    doc_pack_root: &std::path::Path,
    content: &str,
) -> Result<scenarios::ScenarioPlan> {
    let payload: BehaviorMergePatchPayload = serde_json::from_str(content).map_err(|err| {
        anyhow!("parse status next_action.content as merge_behavior_scenarios payload: {err}")
    })?;
    if payload.defaults.is_none() && payload.upsert_scenarios.is_empty() {
        return Err(anyhow!(
            "merge payload must include defaults and/or upsert_scenarios"
        ));
    }
    let mut projected = plan.clone();
    if let Some(defaults_patch) = payload.defaults.as_ref() {
        merge_defaults_patch(&mut projected, defaults_patch)?;
    }
    for mut scenario in payload.upsert_scenarios {
        let scenario_id = scenario.id.trim();
        if scenario_id.is_empty() {
            return Err(anyhow!("upsert_scenarios[].id must not be empty"));
        }
        scenario.id = scenario_id.to_string();
        if let Some(existing) = projected
            .scenarios
            .iter_mut()
            .find(|existing| existing.id == scenario.id)
        {
            *existing = scenario;
        } else {
            projected.scenarios.push(scenario);
        }
    }
    scenarios::validate_plan(&projected, doc_pack_root)?;
    Ok(projected)
}

fn content_projects_as_valid_behavior_merge(
    plan: &scenarios::ScenarioPlan,
    doc_pack_root: &std::path::Path,
    content: &str,
) -> bool {
    project_behavior_scaffold_merge(plan, doc_pack_root, content).is_ok()
}

fn first_valid_scaffold_content<I>(
    plan: &scenarios::ScenarioPlan,
    doc_pack_root: &std::path::Path,
    candidates: I,
) -> Option<String>
where
    I: IntoIterator<Item = Option<String>>,
{
    for candidate in candidates {
        let Some(content) = candidate else {
            continue;
        };
        if content_projects_as_valid_behavior_merge(plan, doc_pack_root, &content) {
            return Some(content);
        }
    }
    None
}

fn behavior_scenario_for_surface_id<'a>(
    plan: &'a scenarios::ScenarioPlan,
    ledger_entries: &LedgerEntries,
    surface_id: &str,
) -> Option<&'a scenarios::ScenarioSpec> {
    let entry = ledger_entries.get(surface_id)?;
    let scenario_id = preferred_behavior_scenario_id(entry)?;
    plan.scenarios
        .iter()
        .find(|scenario| scenario.id == scenario_id)
}

fn build_existing_behavior_scenarios_scaffold(
    plan: &scenarios::ScenarioPlan,
    ledger_entries: &LedgerEntries,
    target_ids: &[String],
) -> Option<String> {
    let mut scenarios_by_id = std::collections::BTreeMap::new();
    for surface_id in normalize_target_ids(target_ids) {
        let Some(scenario) = behavior_scenario_for_surface_id(plan, ledger_entries, &surface_id)
        else {
            continue;
        };
        scenarios_by_id.insert(scenario.id.clone(), scenario.clone());
    }
    render_behavior_scaffold_content(None, scenarios_by_id.into_values().collect())
}

fn minimal_behavior_baseline_scenario(id: &str) -> scenarios::ScenarioSpec {
    scenarios::ScenarioSpec {
        id: id.to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: vec![scenarios::DEFAULT_BEHAVIOR_SEED_DIR.to_string()],
        env: std::collections::BTreeMap::new(),
        seed_dir: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("behavior".to_string()),
        baseline_scenario_id: None,
        assertions: Vec::new(),
        covers: Vec::new(),
        coverage_ignore: true,
        expect: scenarios::ScenarioExpect::default(),
    }
}

fn scenario_with_assertion_starters(
    plan: &scenarios::ScenarioPlan,
    ledger_entries: &LedgerEntries,
    surface_id: &str,
    baseline_by_id: &mut std::collections::BTreeMap<String, scenarios::ScenarioSpec>,
    upsert_by_id: &mut std::collections::BTreeMap<String, scenarios::ScenarioSpec>,
) {
    let Some(entry) = ledger_entries.get(surface_id) else {
        return;
    };
    let Some(scenario_id) = preferred_behavior_scenario_id(entry) else {
        return;
    };
    let Some(mut scenario) = plan
        .scenarios
        .iter()
        .find(|candidate| candidate.id == scenario_id)
        .cloned()
    else {
        return;
    };

    // Ensure scenario has a baseline
    let baseline_id = scenario
        .baseline_scenario_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .or_else(|| crate::status::verification::find_behavior_baseline_id(plan))
        .unwrap_or_else(|| "baseline".to_string());
    scenario.baseline_scenario_id = Some(baseline_id.clone());
    scenario.coverage_tier = Some("behavior".to_string());

    // Ensure baseline exists in the scaffold
    baseline_by_id.entry(baseline_id).or_insert_with_key(|id| {
        plan.scenarios
            .iter()
            .find(|candidate| candidate.id == *id)
            .cloned()
            .unwrap_or_else(|| minimal_behavior_baseline_scenario(id))
    });

    // Default to outputs_differ - simplest assertion that works for any option
    scenario.assertions = vec![scenarios::BehaviorAssertion::OutputsDiffer {}];
    upsert_by_id.insert(scenario.id.clone(), scenario);
}

fn build_missing_assertions_scaffold_content(
    plan: &scenarios::ScenarioPlan,
    ledger_entries: &LedgerEntries,
    target_ids: &[String],
) -> Option<String> {
    let mut baseline_by_id = std::collections::BTreeMap::new();
    let mut upsert_by_id = std::collections::BTreeMap::new();
    for surface_id in normalize_target_ids(target_ids) {
        scenario_with_assertion_starters(
            plan,
            ledger_entries,
            &surface_id,
            &mut baseline_by_id,
            &mut upsert_by_id,
        );
    }
    for baseline in baseline_by_id.into_values() {
        upsert_by_id.insert(baseline.id.clone(), baseline);
    }
    render_behavior_scaffold_content(None, upsert_by_id.into_values().collect())
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
        hint: Some("Review suggested exclusion and apply if justified".to_string()),
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
    let payload = behavior_payload(
        cap_hit,
        Some("outputs_equal"),
        retry_counts,
        ledger_entries,
        &["overlays[].behavior_exclusion"],
        Vec::new(),
        None,
    );
    *ctx.verification_next_action = Some(enrich::NextAction::Edit {
        path: "inventory/surface.overlays.json".to_string(),
        content,
        reason: format!(
            "stopped outputs_equal retries after {BEHAVIOR_RERUN_CAP} no-progress attempts; add behavior_exclusion stubs in inventory/surface.overlays.json"
        ),
        hint: Some("Add exclusion stubs after max retries".to_string()),
        edit_strategy: enrich::default_edit_strategy(),
        payload,
    });
    true
}

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

fn first_behavior_reason_target(
    required_ids: &[String],
    remaining_set: &std::collections::BTreeSet<String>,
    needs_apply_ids: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
) -> Option<String> {
    let empty = std::collections::BTreeSet::new();
    // Priority: scenario_error > assertion_failed > no_scenario > outputs_equal
    // NoScenario before OutputsEqual so we scaffold new scenarios first,
    // then deal with outputs_equal (which often just need exclusion)
    first_reason_id_by_priority(
        required_ids,
        remaining_set,
        &empty,
        needs_apply_ids,
        ledger_entries,
        &[
            BehaviorReasonKind::ScenarioError,
            BehaviorReasonKind::AssertionFailed,
            BehaviorReasonKind::NoScenario,
            BehaviorReasonKind::OutputsEqual,
        ],
    )
    .or_else(|| first_reason_id(required_ids, remaining_set, &empty, needs_apply_ids))
}

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
    let reason_code = behavior_reason_code_for_id(&next_id, missing_value_examples, ledger_entries);
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
    // Normalize missing_value_examples to no_scenario when scenario is absent
    let action_reason_code = if scenario_missing && reason_code == "missing_value_examples" {
        "no_scenario".to_string()
    } else {
        reason_code.clone()
    };
    let retry_count = retry_counts.get(&next_id).copied().unwrap_or(0);
    if reason_code == "missing_delta_assertion" && retry_count >= BEHAVIOR_RERUN_CAP {
        return Some(suggested_exclusion_only_next_action(
            ctx,
            &[next_id],
            "missing_delta_assertion",
            retry_counts,
            ledger_entries,
        ));
    }

    let scaffold_candidates = if scenario_missing {
        summary.stub_blockers_preview = build_stub_blockers_preview(
            ctx,
            &ordered_target_ids,
            ledger_entries,
            &reason_code,
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
        let assertion_repair_reason = matches!(
            action_reason_code.as_str(),
            "assertion_seed_path_not_seeded"
                | "seed_signature_mismatch"
                | "seed_mismatch"
                | "assertion_failed"
                | "missing_delta_assertion"
        );
        // For assertion repair, prioritize scaffold that adds assertions
        let mut candidates = Vec::new();
        if assertion_repair_reason {
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
    if action_reason_code == "assertion_failed" {
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
            let payload = behavior_payload(
                std::slice::from_ref(&next_id),
                Some("assertion_failed"),
                retry_counts,
                ledger_entries,
                &[],
                Vec::new(),
                None,
            );
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
    if ordered_target_ids.len() > 1 {
        reason.push_str(&format!(
            "; batched deterministic scaffold for {} targets",
            ordered_target_ids.len()
        ));
    }
    reason.push_str("; apply patch as merge/upsert by scenario.id");
    let assertion_starters = if action_reason_code == "no_scenario" {
        assertion_starters_for_missing_assertions(entry, ctx.include_full)
    } else {
        Vec::new()
    };
    let payload = behavior_payload(
        &ordered_target_ids,
        Some(&action_reason_code),
        retry_counts,
        ledger_entries,
        &[],
        assertion_starters,
        None,
    );
    Some(enrich::NextAction::Edit {
        path: "scenarios/plan.json".to_string(),
        content,
        reason,
        hint: Some("Add or fix behavior scenario assertions".to_string()),
        edit_strategy: crate::status::verification::BEHAVIOR_SCENARIO_EDIT_STRATEGY.to_string(),
        payload,
    })
}

#[allow(clippy::too_many_arguments)]
fn maybe_set_behavior_next_action(
    ctx: &mut QueueVerificationContext<'_>,
    summary: &mut enrich::VerificationTriageSummary,
    required_ids: &[String],
    remaining_set: &std::collections::BTreeSet<String>,
    missing_value_examples: &std::collections::BTreeSet<String>,
    needs_apply_ids: &std::collections::BTreeSet<String>,
    outputs_equal_without_workaround: &[String],
    outputs_equal_with_workaround_needs_rerun: &[String],
    outputs_equal_with_workaround_ready_for_exclusion: &[String],
    retry_counts: &std::collections::BTreeMap<String, usize>,
    ledger_entries: &LedgerEntries,
) {
    let can_set_next_action = ctx.verification_next_action.is_none()
        && ctx.missing.is_empty()
        && ctx.local_blockers.is_empty();
    if !can_set_next_action {
        return;
    }

    if !outputs_equal_without_workaround.is_empty() {
        let content = surface_overlays_requires_argv_stub_batch(
            ctx.paths,
            ctx.surface,
            outputs_equal_without_workaround,
        );
        summary.stub_blockers_preview = build_stub_blockers_preview(
            ctx,
            outputs_equal_without_workaround,
            ledger_entries,
            STUB_REASON_OUTPUTS_EQUAL_NEEDS_WORKAROUND,
            true,
        );
        let payload = behavior_payload(
            outputs_equal_without_workaround,
            Some("outputs_equal"),
            retry_counts,
            ledger_entries,
            &["overlays[].invocation.requires_argv"],
            Vec::new(),
            None,
        );
        *ctx.verification_next_action = Some(enrich::NextAction::Edit {
            path: "inventory/surface.overlays.json".to_string(),
            content,
            reason: "add requires_argv workaround overlays in inventory/surface.overlays.json; see verification.stub_blockers_preview".to_string(),
            hint: Some("Add requires_argv workaround overlays".to_string()),
            edit_strategy: enrich::default_edit_strategy(),
            payload,
        });
        return;
    }

    if !outputs_equal_with_workaround_needs_rerun.is_empty() {
        let (cap_hit, needs_rerun) = partition_cap_hit(
            outputs_equal_with_workaround_needs_rerun.to_vec(),
            retry_counts,
        );
        if !set_outputs_equal_plateau_next_action(
            ctx,
            summary,
            &cap_hit,
            retry_counts,
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
            let scenario_ids = {
                let ids = rerun_scenario_ids_for_surface_ids(&needs_rerun, ledger_entries);
                if ids.is_empty() {
                    normalize_target_ids(&needs_rerun)
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
            let payload = behavior_payload(
                &needs_rerun,
                Some("outputs_equal"),
                retry_counts,
                ledger_entries,
                &["overlays[].behavior_exclusion"],
                Vec::new(),
                None,
            );
            let retry = max_retry_count(&needs_rerun, retry_counts).unwrap_or(0);
            *ctx.verification_next_action = Some(enrich::NextAction::Command {
                command,
                reason: format!(
                    "requires_argv workaround is present but outputs_equal evidence has not progressed; rerun targeted behavior delta checks for {} scenario ids ({} targets, no-progress retry {}/{})",
                    scenario_ids.len(),
                    needs_rerun.len(),
                    retry.saturating_add(1),
                    BEHAVIOR_RERUN_CAP
                ),
                hint: Some("Rerun to verify workaround effect".to_string()),
                payload,
            });
        }
        return;
    }

    if !outputs_equal_with_workaround_ready_for_exclusion.is_empty() {
        let (cap_hit, ready_for_exclusion) = partition_cap_hit(
            outputs_equal_with_workaround_ready_for_exclusion.to_vec(),
            retry_counts,
        );
        if !set_outputs_equal_plateau_next_action(
            ctx,
            summary,
            &cap_hit,
            retry_counts,
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
                retry_counts,
                ledger_entries,
                &["overlays[].behavior_exclusion"],
                Vec::new(),
                None,
            );
            *ctx.verification_next_action = Some(enrich::NextAction::Edit {
                path: "inventory/surface.overlays.json".to_string(),
                content,
                reason: "record behavior exclusions in inventory/surface.overlays.json; see verification.stub_blockers_preview".to_string(),
                hint: Some("Add behavior exclusion overlays".to_string()),
                edit_strategy: enrich::default_edit_strategy(),
                payload,
            });
        }
        return;
    }

    if let Some(next_id) =
        first_behavior_reason_target(required_ids, remaining_set, needs_apply_ids, ledger_entries)
    {
        let action_reason_code =
            action_reason_code_for_surface_id(&next_id, missing_value_examples, ledger_entries);
        let target_ids = if matches!(
            action_reason_code.as_str(),
            "no_scenario" | "outputs_equal"
        ) {
            let batched = batched_target_ids_for_reason(
                required_ids,
                remaining_set,
                missing_value_examples,
                needs_apply_ids,
                ledger_entries,
                &action_reason_code,
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
        if let Some(action) = reason_based_behavior_next_action(
            ctx,
            summary,
            &target_ids,
            missing_value_examples,
            retry_counts,
            ledger_entries,
        ) {
            *ctx.verification_next_action = Some(action);
        }
        return;
    }

    if let Some(next_id) = first_matching_id(required_ids, needs_apply_ids) {
        let root = ctx.paths.root().display();
        let payload = behavior_payload(
            std::slice::from_ref(&next_id),
            Some("needs_apply"),
            retry_counts,
            ledger_entries,
            &[],
            Vec::new(),
            None,
        );
        *ctx.verification_next_action = Some(enrich::NextAction::Command {
            command: format!("bman apply --doc-pack {root}"),
            reason: format!("run behavior verification for {next_id}"),
            hint: Some("Run to execute behavior verification".to_string()),
            payload,
        });
    }
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

    maybe_set_behavior_next_action(
        ctx,
        &mut summary,
        required_ids,
        &remaining_set,
        &missing_value_examples,
        &needs_apply_ids,
        &outputs_equal_without_workaround,
        &outputs_equal_with_workaround_needs_rerun,
        &outputs_equal_with_workaround_ready_for_exclusion,
        &retry_counts,
        ledger_entries,
    );

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

    if let (Some(surface), Some(plan)) = (inputs.surface.as_ref(), inputs.plan.as_ref()) {
        if inputs.template_path.is_file() && inputs.semantics_path.is_file() {
            ledger_snapshot = build_verification_ledger_entries(
                binary_name,
                surface,
                plan,
                paths,
                &inputs.template_path,
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
