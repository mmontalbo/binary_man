//! Auto-verification helpers for surface item existence.
//!
//! These helpers expand a compact policy into synthetic scenarios using
//! pack-owned semantics for argv templates and tier targeting.
use crate::semantics::Semantics;
use crate::surface::SurfaceInventory;
use std::collections::{BTreeMap, BTreeSet};

use super::{ScenarioExpect, ScenarioKind, ScenarioPlan, ScenarioSpec, VerificationExcludedEntry};

/// Auto-verification targets derived from the surface inventory.
pub struct AutoVerificationTargets {
    pub max_new_runs_per_apply: usize,
    pub target_ids: Vec<String>,
    pub targets: Vec<(String, Vec<String>)>,
    pub excluded: Vec<VerificationExcludedEntry>,
    pub excluded_ids: BTreeSet<String>,
}

/// Collect ids eligible for auto-verification based on the policy.
pub fn auto_verification_targets(
    plan: &ScenarioPlan,
    surface: &SurfaceInventory,
) -> Option<AutoVerificationTargets> {
    let policy = plan.verification.policy.as_ref()?;
    if policy.kinds.is_empty() {
        return None;
    }
    let (excluded, excluded_ids) = plan.collect_queue_exclusions();
    let mut targets = Vec::new();
    let mut target_ids = Vec::new();
    for kind in &policy.kinds {
        let ids = collect_surface_ids(surface, kind);
        let filtered_ids: Vec<String> = ids
            .into_iter()
            .filter(|id| !excluded_ids.contains(id))
            .collect();
        target_ids.extend(filtered_ids.iter().cloned());
        targets.push((kind.clone(), filtered_ids));
    }

    Some(AutoVerificationTargets {
        max_new_runs_per_apply: policy.max_new_runs_per_apply,
        target_ids,
        targets,
        excluded,
        excluded_ids,
    })
}

/// Collect ids eligible for auto-verification in behavior tier.
/// Uses semantics.auto_scenarios to determine which kinds participate in behavior tier.
pub fn auto_verification_targets_for_behavior(
    plan: &ScenarioPlan,
    surface: &SurfaceInventory,
    semantics: &Semantics,
) -> Option<AutoVerificationTargets> {
    let policy = plan.verification.policy.as_ref()?;

    // Collect kinds that participate in behavior tier from semantics
    let behavior_kinds: BTreeSet<String> = semantics
        .verification
        .auto_scenarios
        .iter()
        .filter(|template| template.tiers.iter().any(|t| t == "behavior"))
        .map(|template| template.kind.clone())
        .collect();

    // Fall back to legacy behavior: only "option" if no auto_scenarios defined
    let behavior_kinds = if behavior_kinds.is_empty() {
        let mut fallback = BTreeSet::new();
        fallback.insert("option".to_string());
        fallback
    } else {
        behavior_kinds
    };

    let (excluded, excluded_ids) = plan.collect_queue_exclusions();
    let mut targets = Vec::new();
    let mut target_ids = Vec::new();

    for kind in &policy.kinds {
        if !behavior_kinds.contains(kind) {
            continue;
        }
        let ids = collect_surface_ids(surface, kind);
        let filtered_ids: Vec<String> = ids
            .into_iter()
            .filter(|id| !excluded_ids.contains(id))
            .collect();
        target_ids.extend(filtered_ids.iter().cloned());
        targets.push((kind.clone(), filtered_ids));
    }

    Some(AutoVerificationTargets {
        max_new_runs_per_apply: policy.max_new_runs_per_apply,
        target_ids,
        targets,
        excluded,
        excluded_ids,
    })
}

/// Build synthetic scenarios for existence verification.
pub fn auto_verification_scenarios(
    targets: &AutoVerificationTargets,
    semantics: &Semantics,
) -> Vec<ScenarioSpec> {
    let mut scenarios = Vec::with_capacity(targets.target_ids.len());
    for (kind, group_ids) in &targets.targets {
        for surface_id in group_ids {
            let argv = existence_argv(semantics, kind, surface_id);
            scenarios.push(ScenarioSpec {
                id: auto_scenario_id(kind, surface_id),
                kind: ScenarioKind::Behavior,
                publish: false,
                argv,
                env: BTreeMap::new(),
                seed_dir: None,
                // Pin inline empty seed so auto scenarios do not inherit behavior defaults.seed.
                seed: Some(super::ScenarioSeedSpec {
                    entries: Vec::new(),
                }),
                cwd: None,
                timeout_seconds: None,
                net_mode: None,
                no_sandbox: None,
                no_strace: None,
                snippet_max_lines: None,
                snippet_max_bytes: None,
                coverage_tier: Some("acceptance".to_string()),
                baseline_scenario_id: None,
                assertions: Vec::new(),
                covers: vec![surface_id.to_string()],
                coverage_ignore: false,
                expect: ScenarioExpect::default(),
            });
        }
    }
    scenarios
}

fn collect_surface_ids(surface: &SurfaceInventory, kind: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for item in surface.items.iter().filter(|item| item.kind == kind) {
        let id = item.id.trim();
        if id.is_empty() {
            continue;
        }
        ids.insert(id.to_string());
    }
    ids
}

fn auto_scenario_id(kind: &str, surface_id: &str) -> String {
    format!("{}{}::{}", super::AUTO_VERIFY_SCENARIO_PREFIX, kind, surface_id)
}

fn existence_argv(semantics: &Semantics, kind: &str, surface_id: &str) -> Vec<String> {
    // Try to find template in auto_scenarios first
    if let Some(template) = semantics
        .verification
        .auto_scenarios
        .iter()
        .find(|t| t.kind == kind)
    {
        let mut argv = Vec::new();
        argv.extend(template.argv_prefix.iter().cloned());
        argv.push(surface_id.to_string());
        argv.extend(template.argv_suffix.iter().cloned());
        return argv;
    }

    // Fall back to legacy fields for backward compatibility
    let (prefix, suffix) = match kind {
        "option" => (
            &semantics.verification.option_existence_argv_prefix,
            &semantics.verification.option_existence_argv_suffix,
        ),
        "subcommand" => (
            &semantics.verification.subcommand_existence_argv_prefix,
            &semantics.verification.subcommand_existence_argv_suffix,
        ),
        _ => {
            // Unknown kind with no template: just use the surface_id
            return vec![surface_id.to_string()];
        }
    };
    let mut argv = Vec::new();
    argv.extend(prefix.iter().cloned());
    argv.push(surface_id.to_string());
    argv.extend(suffix.iter().cloned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::{ScenarioDefaults, VerificationPlan, VerificationPolicy};

    fn surface_with_option_and_subcommand() -> SurfaceInventory {
        SurfaceInventory {
            schema_version: 2,
            generated_at_epoch_ms: 0,
            binary_name: Some("bin".to_string()),
            inputs_hash: None,
            discovery: Vec::new(),
            items: vec![
                crate::surface::SurfaceItem {
                    kind: "option".to_string(),
                    id: "--color".to_string(),
                    display: "--color".to_string(),
                    description: None,
                    parent_id: None,
                    context_argv: Vec::new(),
                    forms: vec!["--color[=WHEN]".to_string()],
                    invocation: crate::surface::SurfaceInvocation::default(),
                    evidence: Vec::new(),
                },
                crate::surface::SurfaceItem {
                    kind: "subcommand".to_string(),
                    id: "show".to_string(),
                    display: "show".to_string(),
                    description: None,
                    parent_id: None,
                    context_argv: Vec::new(),
                    forms: vec!["show".to_string()],
                    invocation: crate::surface::SurfaceInvocation::default(),
                    evidence: Vec::new(),
                },
            ],
            blockers: Vec::new(),
        }
    }

    fn plan_with_policy() -> ScenarioPlan {
        ScenarioPlan {
            schema_version: crate::scenarios::SCENARIO_PLAN_SCHEMA_VERSION,
            binary: Some("bin".to_string()),
            default_env: BTreeMap::new(),
            defaults: None,
            coverage: None,
            verification: VerificationPlan {
                queue: Vec::new(),
                policy: Some(VerificationPolicy {
                    kinds: vec!["option".to_string(), "subcommand".to_string()],
                    max_new_runs_per_apply: 3,
                }),
            },
            scenarios: Vec::new(),
        }
    }

    fn scenario_argv_for_id(scenarios: &[ScenarioSpec], id: &str) -> Vec<String> {
        scenarios
            .iter()
            .find(|scenario| scenario.id == id)
            .map(|scenario| scenario.argv.clone())
            .unwrap()
    }

    #[test]
    fn auto_verification_argv_is_semantics_driven() {
        let plan = plan_with_policy();
        let surface = surface_with_option_and_subcommand();
        let targets = auto_verification_targets(&plan, &surface).unwrap();

        let mut semantics_a: Semantics =
            serde_json::from_str(crate::templates::ENRICH_SEMANTICS_JSON).unwrap();
        semantics_a.verification.option_existence_argv_prefix = vec!["probe".to_string()];
        semantics_a.verification.option_existence_argv_suffix = vec!["--usage".to_string()];
        semantics_a.verification.subcommand_existence_argv_prefix = vec!["help".to_string()];
        semantics_a.verification.subcommand_existence_argv_suffix = vec!["--json".to_string()];

        let mut semantics_b = semantics_a.clone();
        semantics_b.verification.option_existence_argv_prefix = vec!["inspect".to_string()];
        semantics_b.verification.option_existence_argv_suffix = vec!["--help".to_string()];
        semantics_b.verification.subcommand_existence_argv_prefix = Vec::new();
        semantics_b.verification.subcommand_existence_argv_suffix = vec!["--help".to_string()];

        let scenarios_a = auto_verification_scenarios(&targets, &semantics_a);
        let scenarios_b = auto_verification_scenarios(&targets, &semantics_b);

        assert_eq!(
            scenario_argv_for_id(&scenarios_a, "auto_verify::option::--color"),
            vec![
                "probe".to_string(),
                "--color".to_string(),
                "--usage".to_string()
            ]
        );
        assert_eq!(
            scenario_argv_for_id(&scenarios_b, "auto_verify::option::--color"),
            vec![
                "inspect".to_string(),
                "--color".to_string(),
                "--help".to_string()
            ]
        );
        assert_eq!(
            scenario_argv_for_id(&scenarios_a, "auto_verify::subcommand::show"),
            vec!["help".to_string(), "show".to_string(), "--json".to_string()]
        );
        assert_eq!(
            scenario_argv_for_id(&scenarios_b, "auto_verify::subcommand::show"),
            vec!["show".to_string(), "--help".to_string()]
        );
    }

    #[test]
    fn auto_verification_digest_ignores_behavior_defaults_seed_changes() {
        let base_plan = plan_with_policy();
        let surface = surface_with_option_and_subcommand();
        let targets = auto_verification_targets(&base_plan, &surface).unwrap();
        let semantics: Semantics =
            serde_json::from_str(crate::templates::ENRICH_SEMANTICS_JSON).unwrap();
        let scenarios = auto_verification_scenarios(&targets, &semantics);
        let scenario = scenarios
            .iter()
            .find(|candidate| candidate.id == "auto_verify::option::--color")
            .cloned()
            .expect("auto scenario");

        let mut plan_a = base_plan.clone();
        plan_a.defaults = Some(ScenarioDefaults {
            seed: Some(super::super::default_behavior_seed()),
            ..ScenarioDefaults::default()
        });
        let mut changed_seed = super::super::default_behavior_seed();
        changed_seed.entries[0].contents = Some("changed\n".to_string());
        let mut plan_b = base_plan;
        plan_b.defaults = Some(ScenarioDefaults {
            seed: Some(changed_seed),
            ..ScenarioDefaults::default()
        });

        let digest_a = super::super::config::effective_scenario_config(&plan_a, &scenario)
            .expect("digest A")
            .scenario_digest;
        let digest_b = super::super::config::effective_scenario_config(&plan_b, &scenario)
            .expect("digest B")
            .scenario_digest;
        assert_eq!(digest_a, digest_b);
    }

    #[test]
    fn auto_scenarios_template_overrides_legacy_fields() {
        let plan = plan_with_policy();
        let surface = surface_with_option_and_subcommand();
        let targets = auto_verification_targets(&plan, &surface).unwrap();

        let mut semantics: Semantics =
            serde_json::from_str(crate::templates::ENRICH_SEMANTICS_JSON).unwrap();
        // Set legacy fields
        semantics.verification.option_existence_argv_prefix = vec!["legacy".to_string()];
        semantics.verification.option_existence_argv_suffix = vec!["--legacy".to_string()];
        // Set auto_scenarios which should take precedence
        semantics.verification.auto_scenarios = vec![crate::semantics::AutoScenarioTemplate {
            kind: "option".to_string(),
            argv_prefix: vec!["new".to_string()],
            argv_suffix: vec!["--new".to_string()],
            tiers: vec!["accepted".to_string(), "behavior".to_string()],
        }];

        let scenarios = auto_verification_scenarios(&targets, &semantics);
        assert_eq!(
            scenario_argv_for_id(&scenarios, "auto_verify::option::--color"),
            vec!["new".to_string(), "--color".to_string(), "--new".to_string()]
        );
    }

    #[test]
    fn behavior_tier_targeting_uses_semantics() {
        let plan = plan_with_policy();
        let surface = surface_with_option_and_subcommand();

        let mut semantics: Semantics =
            serde_json::from_str(crate::templates::ENRICH_SEMANTICS_JSON).unwrap();
        // Only subcommand participates in behavior tier
        semantics.verification.auto_scenarios = vec![
            crate::semantics::AutoScenarioTemplate {
                kind: "option".to_string(),
                argv_prefix: Vec::new(),
                argv_suffix: Vec::new(),
                tiers: vec!["accepted".to_string()], // NOT in behavior
            },
            crate::semantics::AutoScenarioTemplate {
                kind: "subcommand".to_string(),
                argv_prefix: Vec::new(),
                argv_suffix: vec!["--help".to_string()],
                tiers: vec!["accepted".to_string(), "behavior".to_string()],
            },
        ];

        let targets =
            auto_verification_targets_for_behavior(&plan, &surface, &semantics).unwrap();
        // Only subcommand should be in targets
        assert_eq!(targets.target_ids, vec!["show".to_string()]);
        assert_eq!(targets.targets.len(), 1);
        assert_eq!(targets.targets[0].0, "subcommand");
    }
}
