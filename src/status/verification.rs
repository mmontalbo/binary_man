use super::verification_policy::{VerificationStatus, VerificationTier};
use crate::enrich;
use crate::scenarios;
use crate::surface;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct AutoVerificationState {
    pub(crate) targets: scenarios::AutoVerificationTargets,
    pub(crate) remaining_ids: Vec<String>,
    pub(crate) remaining_by_kind: Vec<AutoVerificationKindState>,
    pub(crate) excluded: Vec<enrich::VerificationExclusion>,
    pub(crate) excluded_count: usize,
}

pub(crate) struct AutoVerificationKindState {
    pub(crate) kind: String,
    pub(crate) target_count: usize,
    pub(crate) remaining_ids: Vec<String>,
}

pub(crate) const BEHAVIOR_SCENARIO_EDIT_STRATEGY: &str = "merge_behavior_scenarios";
const REQUIRED_VALUE_PLACEHOLDER: &str = "__value__";

#[derive(Debug, Serialize)]
struct BehaviorDefaultsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed: Option<scenarios::ScenarioSeedSpec>,
}

#[derive(Debug, Serialize)]
struct BehaviorScenarioEditPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    defaults: Option<BehaviorDefaultsPatch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    upsert_scenarios: Vec<scenarios::ScenarioSpec>,
}

pub(crate) fn auto_verification_plan_summary(
    plan: &scenarios::ScenarioPlan,
    surface: &surface::SurfaceInventory,
    ledger_entries: Option<&BTreeMap<String, scenarios::VerificationEntry>>,
    verification_tier: &str,
    semantics: &crate::semantics::Semantics,
) -> Option<enrich::VerificationPlanSummary> {
    let requested_tier = VerificationTier::from_config(Some(verification_tier));
    let (targets, status_tier) = if requested_tier.is_behavior() {
        (
            scenarios::auto_verification_targets_for_behavior(plan, surface, semantics)?,
            VerificationTier::Accepted,
        )
    } else {
        (
            scenarios::auto_verification_targets(plan, surface)?,
            requested_tier,
        )
    };
    let state = auto_verification_state_for_targets(targets, ledger_entries, status_tier);
    let remaining_preview = preview_ids(&state.remaining_ids, 10);
    let by_kind = state
        .remaining_by_kind
        .iter()
        .map(|group| enrich::VerificationKindSummary {
            kind: group.kind.clone(),
            target_count: group.target_count,
            remaining_count: group.remaining_ids.len(),
            remaining_preview: preview_ids(&group.remaining_ids, 10),
        })
        .collect();
    Some(enrich::VerificationPlanSummary {
        target_count: state.targets.target_ids.len(),
        excluded_count: state.excluded_count,
        remaining_count: state.remaining_ids.len(),
        remaining_preview,
        by_kind,
    })
}

pub(crate) fn auto_verification_state(
    plan: &scenarios::ScenarioPlan,
    surface: &surface::SurfaceInventory,
    ledger_entries: Option<&BTreeMap<String, scenarios::VerificationEntry>>,
    verification_tier: &str,
) -> Option<AutoVerificationState> {
    let targets = scenarios::auto_verification_targets(plan, surface)?;
    let tier = VerificationTier::from_config(Some(verification_tier));
    Some(auto_verification_state_for_targets(
        targets,
        ledger_entries,
        tier,
    ))
}

pub(crate) fn auto_verification_state_for_targets(
    targets: scenarios::AutoVerificationTargets,
    ledger_entries: Option<&BTreeMap<String, scenarios::VerificationEntry>>,
    verification_tier: VerificationTier,
) -> AutoVerificationState {
    let mut remaining_ids = Vec::new();
    let mut remaining_by_kind = Vec::new();
    for (kind, group_ids) in &targets.targets {
        let mut group_remaining = Vec::new();
        for surface_id in group_ids {
            let status = VerificationStatus::from_entry(
                ledger_entries.and_then(|entries| entries.get(surface_id)),
                verification_tier,
            );
            if status.requires_follow_up() {
                group_remaining.push(surface_id.clone());
                remaining_ids.push(surface_id.clone());
            }
        }
        remaining_by_kind.push(AutoVerificationKindState {
            kind: kind.clone(),
            target_count: group_ids.len(),
            remaining_ids: group_remaining,
        });
    }

    let excluded: Vec<enrich::VerificationExclusion> = targets
        .excluded
        .iter()
        .map(|entry| enrich::VerificationExclusion {
            surface_id: entry.surface_id.clone(),
            reason: entry.reason.clone().unwrap_or_default(),
            prereqs: entry
                .prereqs
                .iter()
                .map(|prereq| prereq.as_str().to_string())
                .collect(),
        })
        .collect();

    AutoVerificationState {
        excluded_count: targets.excluded_ids.len(),
        targets,
        remaining_ids,
        remaining_by_kind,
        excluded,
    }
}

fn preview_ids(ids: &[String], limit: usize) -> Vec<String> {
    ids.iter().take(limit).cloned().collect()
}

pub(crate) fn find_behavior_baseline_id(plan: &scenarios::ScenarioPlan) -> Option<String> {
    if plan
        .scenarios
        .iter()
        .any(|scenario| scenario.id == "baseline")
    {
        return Some("baseline".to_string());
    }
    for scenario in &plan.scenarios {
        let Some(baseline_id) = scenario.baseline_scenario_id.as_deref() else {
            continue;
        };
        if plan
            .scenarios
            .iter()
            .any(|candidate| candidate.id == baseline_id)
        {
            return Some(baseline_id.to_string());
        }
    }
    None
}

fn behavior_defaults_patch(plan: &scenarios::ScenarioPlan) -> Option<BehaviorDefaultsPatch> {
    if plan
        .defaults
        .as_ref()
        .and_then(|defaults| defaults.seed.as_ref())
        .is_some()
    {
        return None;
    }
    Some(BehaviorDefaultsPatch {
        seed: Some(scenarios::default_behavior_seed()),
    })
}

fn render_behavior_edit_payload(
    plan: &scenarios::ScenarioPlan,
    upsert_scenarios: Vec<scenarios::ScenarioSpec>,
) -> Option<String> {
    let upsert_scenarios = normalize_behavior_upserts(upsert_scenarios);
    if upsert_scenarios.is_empty() {
        return None;
    }
    let payload = BehaviorScenarioEditPayload {
        defaults: behavior_defaults_patch(plan),
        upsert_scenarios,
    };
    serde_json::to_string_pretty(&payload).ok()
}

fn normalize_behavior_upserts(
    upsert_scenarios: Vec<scenarios::ScenarioSpec>,
) -> Vec<scenarios::ScenarioSpec> {
    let mut by_id = BTreeMap::new();
    for scenario in upsert_scenarios {
        let id = scenario.id.trim().to_string();
        if id.is_empty() {
            continue;
        }
        by_id.insert(id.clone(), scenarios::ScenarioSpec { id, ..scenario });
    }
    by_id.into_values().collect()
}

pub(crate) fn behavior_scenario_stub(
    plan: &scenarios::ScenarioPlan,
    scenario_id: &str,
) -> Option<String> {
    let scenario = plan
        .scenarios
        .iter()
        .find(|candidate| candidate.id == scenario_id)?
        .clone();
    render_behavior_edit_payload(plan, vec![scenario])
}

fn behavior_baseline_argv(plan: &scenarios::ScenarioPlan) -> Vec<String> {
    let Some(baseline_id) = find_behavior_baseline_id(plan) else {
        return vec![scenarios::DEFAULT_BEHAVIOR_SEED_DIR.to_string()];
    };
    let argv = plan
        .scenarios
        .iter()
        .find(|scenario| scenario.id == baseline_id)
        .map(|scenario| scenario.argv.clone())
        .unwrap_or_default();
    if argv.is_empty() {
        vec![scenarios::DEFAULT_BEHAVIOR_SEED_DIR.to_string()]
    } else {
        argv
    }
}

pub(crate) fn behavior_baseline_stub(
    plan: &scenarios::ScenarioPlan,
    _surface: &surface::SurfaceInventory,
) -> Option<String> {
    if find_behavior_baseline_id(plan).is_some() {
        return None;
    }
    let baseline_id = "baseline".to_string();
    render_behavior_edit_payload(plan, vec![behavior_baseline_spec(&baseline_id)])
}

pub(crate) fn behavior_scenarios_batch_stub(
    plan: &scenarios::ScenarioPlan,
    surface: &surface::SurfaceInventory,
    surface_ids: &[String],
) -> Option<String> {
    if surface_ids.is_empty() {
        return None;
    }
    let existing_baseline_id = find_behavior_baseline_id(plan);
    let baseline_id = existing_baseline_id
        .clone()
        .unwrap_or_else(|| "baseline".to_string());
    let baseline_argv = if existing_baseline_id.is_some() {
        behavior_baseline_argv(plan)
    } else {
        vec![scenarios::DEFAULT_BEHAVIOR_SEED_DIR.to_string()]
    };

    let mut updated = plan.clone();
    let mut upsert_scenarios = Vec::new();
    if existing_baseline_id.is_none() {
        let baseline = behavior_baseline_spec(&baseline_id);
        updated.scenarios.push(baseline.clone());
        upsert_scenarios.push(baseline);
    }

    let mut added_variants = 0usize;
    let mut seen_targets = BTreeSet::new();
    for surface_id in surface_ids {
        let target_id = surface_id.trim();
        if target_id.is_empty() || !seen_targets.insert(target_id.to_string()) {
            continue;
        }
        let Some(item) = surface::primary_surface_item_by_id(surface, target_id) else {
            continue;
        };
        let Some(argv) = build_behavior_argv(item, &baseline_argv) else {
            continue;
        };
        let variant = behavior_variant_spec(&updated, target_id, &baseline_id, argv);
        updated.scenarios.push(variant.clone());
        upsert_scenarios.push(variant);
        added_variants += 1;
    }
    if added_variants == 0 {
        return None;
    }
    render_behavior_edit_payload(plan, upsert_scenarios)
}

fn behavior_baseline_spec(id: &str) -> scenarios::ScenarioSpec {
    let mut spec = behavior_scenario_spec(
        id.to_string(),
        vec![scenarios::DEFAULT_BEHAVIOR_SEED_DIR.to_string()],
    );
    spec.coverage_ignore = true;
    spec
}

fn behavior_variant_spec(
    plan: &scenarios::ScenarioPlan,
    surface_id: &str,
    baseline_id: &str,
    argv: Vec<String>,
) -> scenarios::ScenarioSpec {
    let stub_id = verification_stub_id(plan, surface_id);
    let mut spec = behavior_scenario_spec(stub_id, argv);
    spec.baseline_scenario_id = Some(baseline_id.to_string());
    spec.covers = vec![surface_id.to_string()];
    // Default to outputs_differ - simplest assertion that works for any option
    spec.assertions = vec![scenarios::BehaviorAssertion::OutputsDiffer {}];
    spec
}

fn behavior_scenario_spec(id: String, argv: Vec<String>) -> scenarios::ScenarioSpec {
    scenarios::ScenarioSpec {
        id,
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv,
        env: BTreeMap::new(),
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
        // Assertions are added by behavior_variant_spec, not here
        // (baselines use this function and shouldn't have assertions)
        assertions: Vec::new(),
        covers: Vec::new(),
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect::default(),
    }
}

fn build_behavior_argv(
    item: &surface::SurfaceItem,
    baseline_argv: &[String],
) -> Option<Vec<String>> {
    let surface_id = item.id.trim();
    if surface_id.is_empty() {
        return None;
    }
    let invocation = &item.invocation;
    let mut argv = invocation.requires_argv.to_vec();
    let value_example = invocation.value_examples.first().cloned();
    let value_arity = invocation.value_arity.as_str();

    if value_arity == "required" {
        let value_example = value_example.unwrap_or_else(|| REQUIRED_VALUE_PLACEHOLDER.to_string());
        push_behavior_value_tokens(
            &mut argv,
            surface_id,
            &item.forms,
            invocation,
            &value_example,
        );
        argv.extend_from_slice(baseline_argv);
        return Some(argv);
    }

    if value_arity == "optional" {
        if let Some(value_example) = value_example {
            push_behavior_value_tokens(
                &mut argv,
                surface_id,
                &item.forms,
                invocation,
                &value_example,
            );
            argv.extend_from_slice(baseline_argv);
            return Some(argv);
        }
    }

    argv.push(surface_id.to_string());
    argv.extend_from_slice(baseline_argv);
    Some(argv)
}

fn push_behavior_value_tokens(
    argv: &mut Vec<String>,
    surface_id: &str,
    forms: &[String],
    invocation: &surface::SurfaceInvocation,
    value_example: &str,
) {
    if prefers_equals_separator(surface_id, forms, invocation) {
        argv.push(format!("{surface_id}={value_example}"));
    } else {
        argv.push(surface_id.to_string());
        argv.push(value_example.to_string());
    }
}

fn prefers_equals_separator(
    surface_id: &str,
    forms: &[String],
    invocation: &surface::SurfaceInvocation,
) -> bool {
    match invocation.value_separator.as_str() {
        "equals" => true,
        "space" => false,
        "either" => matches!(
            inferred_separator_from_forms(surface_id, forms),
            Some(ValueSeparatorHint::Equals)
        ),
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueSeparatorHint {
    Equals,
    Space,
}

fn inferred_separator_from_forms(surface_id: &str, forms: &[String]) -> Option<ValueSeparatorHint> {
    let mut saw_equals = false;
    let mut saw_space = false;
    for form in forms {
        let normalized = form.trim().replace('\t', " ");
        if normalized.is_empty() || !normalized.contains(surface_id) {
            continue;
        }
        if normalized.contains(&format!("{surface_id}="))
            || normalized.contains(&format!("{surface_id}[="))
        {
            saw_equals = true;
        }
        if normalized.contains(&format!("{surface_id} <"))
            || normalized.contains(&format!("{surface_id} ["))
            || normalized.contains(&format!("{surface_id} VALUE"))
        {
            saw_space = true;
        }
    }
    match (saw_equals, saw_space) {
        (true, false) => Some(ValueSeparatorHint::Equals),
        (false, true) => Some(ValueSeparatorHint::Space),
        _ => None,
    }
}

fn verification_stub_id(plan: &scenarios::ScenarioPlan, surface_id: &str) -> String {
    let sanitized = sanitize_scenario_id(surface_id);
    let base = format!("verify_{sanitized}");
    unique_scenario_id(plan, &base)
}

fn unique_scenario_id(plan: &scenarios::ScenarioPlan, base: &str) -> String {
    if plan.scenarios.iter().all(|scenario| scenario.id != base) {
        return base.to_string();
    }
    let mut idx = 1;
    loop {
        let candidate = format!("{base}-{idx}");
        if plan
            .scenarios
            .iter()
            .all(|scenario| scenario.id != candidate)
        {
            return candidate;
        }
        idx += 1;
    }
}

fn sanitize_scenario_id(surface_id: &str) -> String {
    let trimmed = surface_id.trim();
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let cleaned = out.trim_matches('_');
    if cleaned.is_empty() {
        "id".to_string()
    } else {
        cleaned.to_string()
    }
}

#[cfg(test)]
#[path = "verification_tests.rs"]
mod tests;
