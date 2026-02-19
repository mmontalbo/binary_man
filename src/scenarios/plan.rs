//! Scenario plan loading and validation.
//!
//! Plans are strictly validated to keep scenario execution deterministic and
//! pack-owned. Invalid scenarios (e.g., with absolute seed paths) are skipped
//! rather than failing the entire load.
use crate::enrich::SkippedScenario;
use crate::templates;
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::Path;

use super::validate::{validate_scenario_defaults, validate_scenario_spec};
use super::SCENARIO_PLAN_SCHEMA_VERSION;
use super::{ScenarioPlan, VerificationIntent};

/// Result of loading a plan with potential skipped scenarios.
#[derive(Debug)]
pub struct LoadedPlan {
    pub plan: ScenarioPlan,
    pub skipped: Vec<SkippedScenario>,
}

/// Load and validate a scenario plan from disk.
pub fn load_plan(path: &Path, doc_pack_root: &Path) -> Result<ScenarioPlan> {
    let loaded = load_plan_with_filtering(path, doc_pack_root)?;
    Ok(loaded.plan)
}

/// Load a scenario plan, filtering out invalid scenarios instead of failing.
///
/// Returns the valid plan and a list of skipped scenarios with reasons.
pub fn load_plan_with_filtering(path: &Path, doc_pack_root: &Path) -> Result<LoadedPlan> {
    let bytes =
        fs::read(path).with_context(|| format!("read scenarios plan {}", path.display()))?;
    let mut plan: ScenarioPlan =
        serde_json::from_slice(&bytes).context("parse scenarios plan JSON")?;

    // Validate plan-level constraints (these are fatal)
    validate_plan_structure(&plan, doc_pack_root)?;

    // Filter scenarios, collecting skipped ones
    let mut skipped = Vec::new();
    let mut valid_scenarios = Vec::new();

    for scenario in plan.scenarios.drain(..) {
        match validate_scenario_spec(&scenario) {
            Ok(()) => valid_scenarios.push(scenario),
            Err(err) => {
                eprintln!(
                    "warning: skipping scenario {} ({})",
                    scenario.id,
                    err.root_cause()
                );
                skipped.push(SkippedScenario {
                    id: scenario.id.clone(),
                    reason: format!("{:#}", err),
                });
            }
        }
    }

    plan.scenarios = valid_scenarios;

    // Validate cross-scenario constraints (baseline refs, etc.)
    validate_plan_scenarios(&plan)?;

    Ok(LoadedPlan { plan, skipped })
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
    validate_plan_structure(plan, doc_pack_root)?;
    // Validate each scenario individually
    for scenario in &plan.scenarios {
        validate_scenario_spec(scenario)
            .with_context(|| format!("validate scenario {}", scenario.id))?;
    }
    validate_plan_scenarios(plan)?;
    Ok(())
}

/// Validate plan-level structure (schema version, coverage, verification queue, defaults).
/// These are fatal errors that prevent plan loading.
fn validate_plan_structure(plan: &ScenarioPlan, doc_pack_root: &Path) -> Result<()> {
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
        if policy.max_new_runs_per_apply == 0 {
            return Err(anyhow!(
                "verification.policy.max_new_runs_per_apply must be > 0"
            ));
        }
    }
    Ok(())
}

/// Validate cross-scenario constraints (duplicate IDs, baseline refs, assertion seed paths).
fn validate_plan_scenarios(plan: &ScenarioPlan) -> Result<()> {
    let mut scenario_ids = std::collections::BTreeSet::new();
    for scenario in &plan.scenarios {
        if !scenario_ids.insert(scenario.id.clone()) {
            return Err(anyhow!(
                "duplicate scenario.id {} in scenarios/plan.json",
                scenario.id
            ));
        }
    }
    for scenario in &plan.scenarios {
        if scenario.kind != super::ScenarioKind::Behavior || scenario.assertions.is_empty() {
            continue;
        }
        // Check if any assertion requires a baseline (file assertions don't)
        let needs_baseline = scenario.assertions.iter().any(|a| a.requires_baseline());
        let baseline_id = scenario.baseline_scenario_id.as_deref().unwrap_or("");
        if needs_baseline && baseline_id.trim().is_empty() {
            return Err(anyhow!(
                "scenario {} assertions require baseline_scenario_id",
                scenario.id
            ));
        }
        if !baseline_id.trim().is_empty() && !scenario_ids.contains(baseline_id) {
            return Err(anyhow!(
                "scenario {} baseline_scenario_id {} does not exist in plan",
                scenario.id,
                baseline_id
            ));
        }
        // Note: seed_path validation is intentionally deferred to run-time.
        // The SQL verification query detects missing seed_paths and marks scenarios
        // as `scenario_error`, allowing the LM to self-correct instead of blocking
        // the entire pack at load time. See 10_behavior_assertion_eval.sql.
    }
    Ok(())
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
            stdin: None,
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
            stdin: None,
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
                setup: Vec::new(),
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
    fn behavior_assertions_with_unseeded_path_are_deferred_to_runtime() {
        // Unseeded seed_path validation was moved from load-time to run-time.
        // The SQL verification query now detects missing seed_paths and marks
        // scenarios as `scenario_error`. This allows LM self-correction instead
        // of blocking the entire pack. See 10_behavior_assertion_eval.sql.
        let plan = plan_with(vec![
            baseline_scenario(),
            behavior_scenario(Some("baseline"), "."),
        ]);
        validate_plan(&plan, Path::new(".")).expect("unseeded seed_path deferred to run-time");
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
