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
        id: surface_id.to_string(),
        display: surface_id.to_string(),
        description: None,
        parent_id: None,
        context_argv: Vec::new(),
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
    // Test with a non-dashed id that's a non-entry-point (context_argv is empty)
    let item = surface_item("show", invocation("unknown", "unknown", &[]), &["show"]);
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
            id: surface_id.to_string(),
            display: surface_id.to_string(),
            description: None,
            parent_id: None,
            context_argv: Vec::new(),
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
    let updated: scenarios::ScenarioSpec = serde_json::from_value(scenarios[0].clone()).unwrap();

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
        behavior_scenarios_batch_stub(&baseline_plan, &surface, &["--color".to_string()]).unwrap();
    let payload: Value = serde_json::from_str(&content).unwrap();
    let scenarios = payload
        .get("upsert_scenarios")
        .and_then(Value::as_array)
        .unwrap();
    let variant: scenarios::ScenarioSpec = serde_json::from_value(scenarios[0].clone()).unwrap();

    assert!(variant.expect.exit_code.is_none());
    // Variants include outputs_differ as default assertion
    assert_eq!(variant.assertions.len(), 1);
    assert!(matches!(
        variant.assertions[0],
        scenarios::BehaviorAssertion::OutputsDiffer {}
    ));
    assert!(variant.baseline_scenario_id.is_some());
    assert!(payload.get("defaults").is_some());
    assert!(!content.contains("\"expect\""));
    assert!(!content.contains("\"schema_version\""));
}

#[test]
fn behavior_scenarios_batch_stub_autoincludes_baseline_when_missing() {
    let plan = minimal_plan();
    let surface = minimal_surface("--color");

    let content = behavior_scenarios_batch_stub(&plan, &surface, &["--color".to_string()]).unwrap();
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
