//! Scenario plan loading and validation.
//!
//! Plans are strictly validated to keep scenario execution deterministic and
//! pack-owned.
use crate::templates;
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::Path;

use super::validate::{validate_scenario_defaults, validate_scenario_spec};
use super::SCENARIO_PLAN_SCHEMA_VERSION;
use super::{BehaviorAssertion, ScenarioPlan, ScenarioSpec, VerificationIntent};

/// Load and validate a scenario plan from disk.
pub fn load_plan(path: &Path, doc_pack_root: &Path) -> Result<ScenarioPlan> {
    let bytes =
        fs::read(path).with_context(|| format!("read scenarios plan {}", path.display()))?;
    let plan: ScenarioPlan = serde_json::from_slice(&bytes).context("parse scenarios plan JSON")?;
    validate_plan(&plan, doc_pack_root)?;
    Ok(plan)
}

pub(crate) fn load_plan_if_exists(
    path: &Path,
    doc_pack_root: &Path,
) -> Result<Option<ScenarioPlan>> {
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(load_plan(path, doc_pack_root)?))
}

/// Validate a scenario plan against schema and filesystem constraints.
pub fn validate_plan(plan: &ScenarioPlan, doc_pack_root: &Path) -> Result<()> {
    if plan.schema_version != SCENARIO_PLAN_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported scenarios plan schema_version {}",
            plan.schema_version
        ));
    }
    if plan.scenarios.is_empty() {
        return Err(anyhow!("scenarios plan contains no scenarios"));
    }
    if let Some(coverage) = plan.coverage.as_ref() {
        for blocked in &coverage.blocked {
            if blocked.item_ids.is_empty() {
                return Err(anyhow!("coverage.blocked entries must include item_ids"));
            }
            if blocked.reason.trim().is_empty() {
                return Err(anyhow!("coverage.blocked reason must not be empty"));
            }
        }
    }
    if let Some(defaults) = plan.defaults.as_ref() {
        validate_scenario_defaults(defaults, doc_pack_root)
            .context("validate scenario defaults")?;
    }
    for (idx, entry) in plan.verification.queue.iter().enumerate() {
        if entry.surface_id.trim().is_empty() {
            return Err(anyhow!(
                "verification.queue[{idx}] surface_id must not be empty"
            ));
        }
        if entry.intent == VerificationIntent::Exclude {
            if entry.prereqs.is_empty() {
                return Err(anyhow!(
                    "verification.queue[{idx}] exclude intent requires prereqs"
                ));
            }
            let reason = entry.reason.as_deref().unwrap_or("");
            if reason.trim().is_empty() {
                return Err(anyhow!(
                    "verification.queue[{idx}] exclude intent requires reason"
                ));
            }
        }
    }
    if let Some(policy) = plan.verification.policy.as_ref() {
        if policy.kinds.is_empty() {
            return Err(anyhow!(
                "verification.policy.kinds must include at least one kind"
            ));
        }
        let mut seen_kinds = std::collections::BTreeSet::new();
        for kind in &policy.kinds {
            let kind_str = kind.as_str();
            if !seen_kinds.insert(kind_str) {
                return Err(anyhow!(
                    "verification.policy.kinds contains duplicate kind {kind_str}"
                ));
            }
        }
        if policy.max_new_runs_per_apply == 0 {
            return Err(anyhow!(
                "verification.policy.max_new_runs_per_apply must be > 0"
            ));
        }
    }
    let mut scenario_ids = std::collections::BTreeSet::new();
    for scenario in &plan.scenarios {
        validate_scenario_spec(scenario)
            .with_context(|| format!("validate scenario {}", scenario.id))?;
        if !scenario_ids.insert(scenario.id.clone()) {
            return Err(anyhow!(
                "duplicate scenario.id {} in scenarios/plan.json",
                scenario.id
            ));
        }
    }
    let mut seed_paths_by_scenario: std::collections::BTreeMap<
        String,
        std::collections::BTreeSet<String>,
    > = std::collections::BTreeMap::new();
    for scenario in &plan.scenarios {
        seed_paths_by_scenario.insert(scenario.id.clone(), effective_seed_paths(plan, scenario));
    }
    for scenario in &plan.scenarios {
        if scenario.kind != super::ScenarioKind::Behavior || scenario.assertions.is_empty() {
            continue;
        }
        let baseline_id = scenario.baseline_scenario_id.as_deref().unwrap_or("");
        if baseline_id.trim().is_empty() {
            return Err(anyhow!(
                "scenario {} assertions require baseline_scenario_id",
                scenario.id
            ));
        }
        if !scenario_ids.contains(baseline_id) {
            return Err(anyhow!(
                "scenario {} baseline_scenario_id {} does not exist in plan",
                scenario.id,
                baseline_id
            ));
        }
        let scenario_seed_paths = seed_paths_by_scenario
            .get(&scenario.id)
            .cloned()
            .unwrap_or_default();
        let baseline_seed_paths = seed_paths_by_scenario
            .get(baseline_id)
            .cloned()
            .unwrap_or_default();
        let mut invalid_paths = std::collections::BTreeSet::new();
        for assertion in &scenario.assertions {
            let Some(seed_path) = assertion_seed_path(assertion) else {
                continue;
            };
            if !scenario_seed_paths.contains(seed_path) || !baseline_seed_paths.contains(seed_path)
            {
                invalid_paths.insert(seed_path.to_string());
            }
        }
        if !invalid_paths.is_empty() {
            let invalid_list = invalid_paths.into_iter().collect::<Vec<_>>().join(", ");
            return Err(anyhow!(
                "scenario {} baseline_scenario_id {} has assertion seed_path(s) not present in both baseline and variant seed entries: {}. seed_path must match a seeded entry path; use stdout_token for printed token",
                scenario.id,
                baseline_id,
                invalid_list
            ));
        }
    }
    Ok(())
}

fn effective_seed_paths(
    plan: &ScenarioPlan,
    scenario: &ScenarioSpec,
) -> std::collections::BTreeSet<String> {
    let defaults = plan.defaults.as_ref();
    let seed = if scenario.seed.is_some() {
        scenario.seed.as_ref()
    } else if scenario.seed_dir.is_some() {
        None
    } else {
        defaults.and_then(|value| value.seed.as_ref())
    };
    let mut paths = std::collections::BTreeSet::new();
    if let Some(seed) = seed {
        for entry in &seed.entries {
            if entry.path.trim().is_empty() {
                continue;
            }
            paths.insert(entry.path.clone());
        }
    }
    paths
}

fn assertion_seed_path(assertion: &BehaviorAssertion) -> Option<&str> {
    assertion.seed_path()
}

/// Render a minimal scenario plan stub for edit suggestions.
pub fn plan_stub(binary_name: Option<&str>) -> String {
    let mut plan: ScenarioPlan = serde_json::from_str(templates::SCENARIOS_PLAN_JSON)
        .expect("parse scenarios plan template");
    if let Some(binary) = binary_name {
        plan.binary = Some(binary.to_string());
    }
    serde_json::to_string_pretty(&plan).expect("serialize scenarios plan stub")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::{
        BehaviorAssertion, RunTarget, ScenarioExpect, ScenarioKind, ScenarioSpec, VerificationPlan,
    };
    use std::collections::BTreeMap;
    use std::path::Path;

    fn baseline_scenario() -> ScenarioSpec {
        ScenarioSpec {
            id: "baseline".to_string(),
            kind: ScenarioKind::Behavior,
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
            expect: ScenarioExpect {
                exit_code: Some(0),
                ..Default::default()
            },
        }
    }

    fn behavior_scenario(baseline_id: Option<&str>, seed_path: &str) -> ScenarioSpec {
        ScenarioSpec {
            id: "verify".to_string(),
            kind: ScenarioKind::Behavior,
            publish: false,
            argv: vec!["-a".to_string()],
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
            baseline_scenario_id: baseline_id.map(|value| value.to_string()),
            assertions: vec![BehaviorAssertion::StdoutContains {
                run: RunTarget::Variant,
                seed_path: seed_path.to_string(),
                token: None,
                exact_line: false,
            }],
            covers: vec!["-a".to_string()],
            coverage_ignore: false,
            expect: ScenarioExpect::default(),
        }
    }

    fn defaults_with_seed() -> crate::scenarios::ScenarioDefaults {
        crate::scenarios::ScenarioDefaults {
            seed: Some(crate::scenarios::ScenarioSeedSpec {
                entries: vec![crate::scenarios::ScenarioSeedEntry {
                    path: "seed.txt".to_string(),
                    kind: crate::scenarios::SeedEntryKind::File,
                    contents: Some("seed\n".to_string()),
                    target: None,
                    mode: None,
                }],
            }),
            ..Default::default()
        }
    }

    fn plan_with(scenarios: Vec<ScenarioSpec>) -> ScenarioPlan {
        ScenarioPlan {
            schema_version: SCENARIO_PLAN_SCHEMA_VERSION,
            binary: None,
            default_env: BTreeMap::new(),
            defaults: Some(defaults_with_seed()),
            coverage: None,
            verification: VerificationPlan::default(),
            scenarios,
        }
    }

    #[test]
    fn behavior_assertions_require_baseline_reference() {
        let plan = plan_with(vec![behavior_scenario(None, "seed.txt")]);
        let err = validate_plan(&plan, Path::new(".")).expect_err("missing baseline");
        assert!(err.to_string().contains("baseline_scenario_id"));
    }

    #[test]
    fn behavior_assertions_accept_existing_baseline() {
        let plan = plan_with(vec![
            baseline_scenario(),
            behavior_scenario(Some("baseline"), "seed.txt"),
        ]);
        validate_plan(&plan, Path::new(".")).expect("baseline referenced");
    }

    #[test]
    fn behavior_assertions_reject_unseeded_path() {
        let plan = plan_with(vec![
            baseline_scenario(),
            behavior_scenario(Some("baseline"), "."),
        ]);
        let err = validate_plan(&plan, Path::new(".")).expect_err("unseeded seed_path");
        let message = err.to_string();
        assert!(message.contains("scenario verify"));
        assert!(message.contains("baseline_scenario_id baseline"));
        assert!(message.contains("seed_path"));
    }

    #[test]
    fn rejects_duplicate_scenario_ids() {
        let plan = plan_with(vec![
            baseline_scenario(),
            ScenarioSpec {
                id: "baseline".to_string(),
                ..behavior_scenario(Some("baseline"), "seed.txt")
            },
        ]);
        let err = validate_plan(&plan, Path::new(".")).expect_err("duplicate scenario id");
        assert!(err
            .to_string()
            .contains("duplicate scenario.id baseline in scenarios/plan.json"));
    }
}
