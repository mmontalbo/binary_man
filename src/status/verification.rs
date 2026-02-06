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
    pub(crate) kind: scenarios::VerificationTargetKind,
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
) -> Option<enrich::VerificationPlanSummary> {
    let requested_tier = VerificationTier::from_config(Some(verification_tier));
    let (targets, status_tier) = if requested_tier.is_behavior() {
        (
            scenarios::auto_verification_targets_for_behavior(plan, surface)?,
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
            kind: group.kind.as_str().to_string(),
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
            kind: *kind,
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
    scenarios::ScenarioSpec {
        id: id.to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: vec![scenarios::DEFAULT_BEHAVIOR_SEED_DIR.to_string()],
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
        assertions: Vec::new(),
        covers: Vec::new(),
        coverage_ignore: true,
        expect: scenarios::ScenarioExpect::default(),
    }
}

fn behavior_variant_spec(
    plan: &scenarios::ScenarioPlan,
    surface_id: &str,
    baseline_id: &str,
    argv: Vec<String>,
) -> scenarios::ScenarioSpec {
    let stub_id = verification_stub_id(plan, surface_id);
    scenarios::ScenarioSpec {
        id: stub_id,
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
        baseline_scenario_id: Some(baseline_id.to_string()),
        assertions: vec![scenarios::BehaviorAssertion::VariantStdoutDiffersFromBaseline {}],
        covers: vec![surface_id.to_string()],
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
mod tests {
    use super::{
        behavior_baseline_stub, behavior_scenario_stub, behavior_scenarios_batch_stub,
        build_behavior_argv, BEHAVIOR_SCENARIO_EDIT_STRATEGY, REQUIRED_VALUE_PLACEHOLDER,
    };
    use crate::scenarios;
    use crate::surface;
    use crate::surface::SurfaceInvocation;
    use serde_json::Value;
    use std::collections::BTreeMap;

    fn invocation(
        value_arity: &str,
        value_separator: &str,
        value_examples: &[&str],
    ) -> SurfaceInvocation {
        SurfaceInvocation {
            value_arity: value_arity.to_string(),
            value_separator: value_separator.to_string(),
            value_examples: value_examples
                .iter()
                .map(|value| value.to_string())
                .collect(),
            ..Default::default()
        }
    }

    fn surface_item(
        surface_id: &str,
        invocation: SurfaceInvocation,
        forms: &[&str],
    ) -> surface::SurfaceItem {
        surface::SurfaceItem {
            kind: "option".to_string(),
            id: surface_id.to_string(),
            display: surface_id.to_string(),
            description: None,
            forms: forms.iter().map(|form| (*form).to_string()).collect(),
            invocation,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn behavior_argv_required_without_examples_uses_placeholder() {
        let baseline = vec!["work".to_string()];
        let item = surface_item(
            "--classify",
            invocation("required", "equals", &[]),
            &["--classify=WHEN"],
        );
        let argv = build_behavior_argv(&item, &baseline);
        assert_eq!(
            argv.unwrap(),
            vec![
                format!("--classify={REQUIRED_VALUE_PLACEHOLDER}"),
                "work".to_string()
            ]
        );
    }

    #[test]
    fn behavior_argv_optional_without_examples_uses_bare_option() {
        let baseline = vec!["work".to_string()];
        let item = surface_item(
            "--classify",
            invocation("optional", "equals", &[]),
            &["--classify[=WHEN]"],
        );
        let argv = build_behavior_argv(&item, &baseline).unwrap();
        assert_eq!(argv, vec!["--classify".to_string(), "work".to_string()]);
    }

    #[test]
    fn behavior_argv_optional_with_example_uses_equals() {
        let baseline = vec!["work".to_string()];
        let item = surface_item(
            "--color",
            invocation("optional", "equals", &["auto"]),
            &["--color=WHEN"],
        );
        let argv = build_behavior_argv(&item, &baseline).unwrap();
        assert_eq!(argv, vec!["--color=auto".to_string(), "work".to_string()]);
    }

    #[test]
    fn behavior_argv_optional_with_example_uses_space() {
        let baseline = vec!["work".to_string()];
        let item = surface_item(
            "--color",
            invocation("optional", "space", &["auto"]),
            &["--color WHEN"],
        );
        let argv = build_behavior_argv(&item, &baseline).unwrap();
        assert_eq!(
            argv,
            vec![
                "--color".to_string(),
                "auto".to_string(),
                "work".to_string()
            ]
        );
    }

    #[test]
    fn behavior_argv_uses_form_hint_when_separator_is_either() {
        let baseline = vec!["work".to_string()];
        let item = surface_item(
            "--color",
            invocation("optional", "either", &["auto"]),
            &["--color=WHEN"],
        );
        let argv = build_behavior_argv(&item, &baseline).unwrap();
        assert_eq!(argv, vec!["--color=auto".to_string(), "work".to_string()]);
    }

    #[test]
    fn behavior_argv_for_non_option_id_stays_generic() {
        let baseline = vec!["work".to_string()];
        let mut item = surface_item("show", invocation("unknown", "unknown", &[]), &["show"]);
        item.kind = "subcommand".to_string();
        let argv = build_behavior_argv(&item, &baseline).unwrap();
        assert_eq!(argv, vec!["show".to_string(), "work".to_string()]);
    }

    fn minimal_plan() -> scenarios::ScenarioPlan {
        scenarios::ScenarioPlan {
            schema_version: 11,
            binary: Some("bin".to_string()),
            default_env: BTreeMap::new(),
            defaults: None,
            coverage: None,
            verification: scenarios::VerificationPlan::default(),
            scenarios: Vec::new(),
        }
    }

    fn minimal_surface(surface_id: &str) -> surface::SurfaceInventory {
        surface::SurfaceInventory {
            schema_version: 2,
            generated_at_epoch_ms: 0,
            binary_name: Some("bin".to_string()),
            inputs_hash: None,
            discovery: Vec::new(),
            items: vec![surface::SurfaceItem {
                kind: "option".to_string(),
                id: surface_id.to_string(),
                display: surface_id.to_string(),
                description: None,
                forms: vec![surface_id.to_string()],
                invocation: SurfaceInvocation {
                    value_arity: "optional".to_string(),
                    value_separator: "equals".to_string(),
                    value_examples: vec!["auto".to_string()],
                    ..Default::default()
                },
                evidence: Vec::new(),
            }],
            blockers: Vec::new(),
        }
    }

    #[test]
    fn behavior_baseline_stub_omits_expect_defaults() {
        let plan = minimal_plan();
        let surface = minimal_surface("--color");

        let content = behavior_baseline_stub(&plan, &surface).unwrap();
        let payload: Value = serde_json::from_str(&content).unwrap();
        let scenarios = payload
            .get("upsert_scenarios")
            .and_then(Value::as_array)
            .unwrap();
        let updated: scenarios::ScenarioSpec =
            serde_json::from_value(scenarios[0].clone()).unwrap();

        assert!(updated.expect.exit_code.is_none());
        assert_eq!(
            payload
                .get("defaults")
                .and_then(|defaults| defaults.get("seed"))
                .and_then(|seed| seed.get("entries"))
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(scenarios::default_behavior_seed().entries.len())
        );
        assert!(!content.contains("\"expect\""));
        assert!(!content.contains("\"schema_version\""));
    }

    #[test]
    fn behavior_variant_stub_omits_expect_defaults() {
        let surface = minimal_surface("--color");
        let baseline_plan = {
            let mut plan = minimal_plan();
            plan.scenarios.push(scenarios::ScenarioSpec {
                id: "baseline".to_string(),
                kind: scenarios::ScenarioKind::Behavior,
                publish: false,
                argv: vec!["work".to_string()],
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
                assertions: Vec::new(),
                covers: Vec::new(),
                coverage_ignore: true,
                expect: scenarios::ScenarioExpect::default(),
            });
            plan
        };
        let content =
            behavior_scenarios_batch_stub(&baseline_plan, &surface, &["--color".to_string()])
                .unwrap();
        let payload: Value = serde_json::from_str(&content).unwrap();
        let scenarios = payload
            .get("upsert_scenarios")
            .and_then(Value::as_array)
            .unwrap();
        let variant: scenarios::ScenarioSpec =
            serde_json::from_value(scenarios[0].clone()).unwrap();

        assert!(variant.expect.exit_code.is_none());
        assert_eq!(
            variant.assertions,
            vec![scenarios::BehaviorAssertion::VariantStdoutDiffersFromBaseline {}]
        );
        assert!(payload.get("defaults").is_some());
        assert!(!content.contains("\"expect\""));
        assert!(!content.contains("\"schema_version\""));
    }

    #[test]
    fn behavior_scenarios_batch_stub_autoincludes_baseline_when_missing() {
        let plan = minimal_plan();
        let surface = minimal_surface("--color");

        let content =
            behavior_scenarios_batch_stub(&plan, &surface, &["--color".to_string()]).unwrap();
        let payload: Value = serde_json::from_str(&content).unwrap();
        let scenarios = payload
            .get("upsert_scenarios")
            .and_then(Value::as_array)
            .unwrap();
        let ids = scenarios
            .iter()
            .filter_map(|entry| entry.get("id").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert!(ids.contains(&"baseline"));
        assert!(ids.iter().any(|id| id.starts_with("verify_")));
    }

    #[test]
    fn behavior_scenarios_batch_stub_dedupes_upserts_by_id() {
        let plan = minimal_plan();
        let surface = minimal_surface("--color");
        let content = behavior_scenarios_batch_stub(
            &plan,
            &surface,
            &["--color".to_string(), "--color".to_string()],
        )
        .unwrap();
        let payload: Value = serde_json::from_str(&content).unwrap();
        let scenarios = payload
            .get("upsert_scenarios")
            .and_then(Value::as_array)
            .unwrap();
        let color_count = scenarios
            .iter()
            .filter(|entry| {
                entry
                    .get("covers")
                    .and_then(Value::as_array)
                    .is_some_and(|covers| {
                        covers
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|cover| cover == "--color")
                    })
            })
            .count();
        assert_eq!(color_count, 1);
    }

    #[test]
    fn behavior_scenario_stub_scopes_payload_to_single_scenario() {
        let mut plan = minimal_plan();
        plan.scenarios.push(scenarios::ScenarioSpec {
            id: "verify_color".to_string(),
            kind: scenarios::ScenarioKind::Behavior,
            publish: false,
            argv: vec!["--color=auto".to_string(), "work".to_string()],
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
            baseline_scenario_id: Some("baseline".to_string()),
            assertions: Vec::new(),
            covers: vec!["--color".to_string()],
            coverage_ignore: false,
            expect: scenarios::ScenarioExpect::default(),
        });

        let content = behavior_scenario_stub(&plan, "verify_color").unwrap();
        let payload: Value = serde_json::from_str(&content).unwrap();
        assert!(payload.get("schema_version").is_none());
        let scenarios = payload
            .get("upsert_scenarios")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(scenarios.len(), 1);
        assert_eq!(
            scenarios[0].get("id").and_then(Value::as_str).unwrap(),
            "verify_color"
        );
        assert_eq!(BEHAVIOR_SCENARIO_EDIT_STRATEGY, "merge_behavior_scenarios");
    }
}
