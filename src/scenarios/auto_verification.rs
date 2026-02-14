//! Auto-verification helpers for surface item existence.
//!
//! These helpers expand a compact policy into synthetic scenarios using
//! pack-owned semantics for argv templates.
use crate::enrich::PrereqsFile;
use crate::semantics::Semantics;
use crate::surface::SurfaceInventory;
use std::collections::BTreeSet;

use super::{ScenarioExpect, ScenarioKind, ScenarioPlan, ScenarioSpec, VerificationExcludedEntry};

/// Auto-verification targets derived from the surface inventory.
pub struct AutoVerificationTargets {
    pub max_new_runs_per_apply: usize,
    pub target_ids: Vec<String>,
    pub excluded: Vec<VerificationExcludedEntry>,
    pub excluded_ids: BTreeSet<String>,
}

/// Check if a surface item is an entry point (its id is in context_argv).
fn is_entry_point(item: &crate::surface::SurfaceItem) -> bool {
    item.context_argv.last().map(|s| s.as_str()) == Some(item.id.as_str())
}

/// Check if an id looks like an option (starts with -).
fn looks_like_option(id: &str) -> bool {
    id.starts_with('-')
}

/// Collect ids eligible for auto-verification based on the policy.
///
/// Auto-verification targets all non-entry-point surface items.
pub fn auto_verification_targets(
    plan: &ScenarioPlan,
    surface: &SurfaceInventory,
) -> Option<AutoVerificationTargets> {
    let policy = plan.verification.policy.as_ref()?;
    let (excluded, excluded_ids) = plan.collect_queue_exclusions();

    // Collect all non-entry-point surface ids
    let target_ids: Vec<String> = surface
        .items
        .iter()
        .filter(|item| !is_entry_point(item))
        .map(|item| item.id.trim())
        .filter(|id| !id.is_empty() && !excluded_ids.contains(*id))
        .map(|id| id.to_string())
        .collect();

    Some(AutoVerificationTargets {
        max_new_runs_per_apply: policy.max_new_runs_per_apply,
        target_ids,
        excluded,
        excluded_ids,
    })
}

/// Collect ids eligible for auto-verification in behavior tier.
///
/// Uses semantics.auto_scenarios to determine which item shapes participate
/// in behavior tier (based on id patterns like starting with `-` for options).
pub fn auto_verification_targets_for_behavior(
    plan: &ScenarioPlan,
    surface: &SurfaceInventory,
    semantics: &Semantics,
) -> Option<AutoVerificationTargets> {
    let policy = plan.verification.policy.as_ref()?;

    // Check if any auto_scenarios template includes behavior tier
    let has_behavior_templates = semantics
        .verification
        .auto_scenarios
        .iter()
        .any(|template| template.tiers.iter().any(|t| t == "behavior"));

    // If no behavior templates defined, default to verifying option-like items
    let verify_options = if has_behavior_templates {
        semantics
            .verification
            .auto_scenarios
            .iter()
            .filter(|template| template.tiers.iter().any(|t| t == "behavior"))
            .any(|template| template.kind == "option")
    } else {
        true // Default: verify options
    };

    let (excluded, excluded_ids) = plan.collect_queue_exclusions();

    // Collect non-entry-point surface ids that match behavior tier criteria
    let target_ids: Vec<String> = surface
        .items
        .iter()
        .filter(|item| {
            if is_entry_point(item) {
                return false;
            }
            // If we should verify options, include option-like items
            if verify_options && looks_like_option(&item.id) {
                return true;
            }
            // Otherwise check if there's a matching auto_scenario template
            semantics
                .verification
                .auto_scenarios
                .iter()
                .any(|template| {
                    template.tiers.iter().any(|t| t == "behavior")
                        && matches_template_kind(&item.id, &template.kind)
                })
        })
        .map(|item| item.id.trim())
        .filter(|id| !id.is_empty() && !excluded_ids.contains(*id))
        .map(|id| id.to_string())
        .collect();

    Some(AutoVerificationTargets {
        max_new_runs_per_apply: policy.max_new_runs_per_apply,
        target_ids,
        excluded,
        excluded_ids,
    })
}

/// Check if an id matches a template kind using heuristics.
fn matches_template_kind(id: &str, template_kind: &str) -> bool {
    match template_kind {
        "option" => looks_like_option(id),
        "subcommand" | "command" => !looks_like_option(id),
        _ => false,
    }
}

/// Build synthetic scenarios for existence verification.
///
/// When prereqs is provided, uses prereq seeds for items that have them.
/// Items with `exclude: true` in prereqs are skipped.
pub fn auto_verification_scenarios(
    targets: &AutoVerificationTargets,
    semantics: &Semantics,
    surface: &SurfaceInventory,
    prereqs: Option<&PrereqsFile>,
) -> Vec<ScenarioSpec> {
    let mut scenarios = Vec::with_capacity(targets.target_ids.len());
    for surface_id in &targets.target_ids {
        // Check prereqs for exclusion
        let resolved = prereqs.map(|p| p.resolve(surface_id));
        if resolved.as_ref().is_some_and(|r| r.exclude) {
            continue;
        }

        let item = surface.items.iter().find(|item| &item.id == surface_id);
        let context_argv = item.map(|i| i.context_argv.as_slice()).unwrap_or(&[]);
        let argv = existence_argv(semantics, surface_id, context_argv);

        // Use prereq seed if available, otherwise empty seed
        let seed = resolved
            .and_then(|r| r.seed)
            .unwrap_or_else(|| super::ScenarioSeedSpec {
                entries: Vec::new(),
            });

        scenarios.push(ScenarioSpec {
            id: auto_scenario_id(surface_id),
            kind: ScenarioKind::Behavior,
            publish: false,
            argv,
            env: std::collections::BTreeMap::new(),
            // Use prereq seed or pin inline empty seed
            seed: Some(seed),
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
    scenarios
}

fn auto_scenario_id(surface_id: &str) -> String {
    format!("{}{}", super::AUTO_VERIFY_SCENARIO_PREFIX, surface_id)
}

fn existence_argv(semantics: &Semantics, surface_id: &str, context_argv: &[String]) -> Vec<String> {
    // Determine if this looks like an option based on id shape
    let is_option_like = looks_like_option(surface_id);
    let template_kind = if is_option_like {
        "option"
    } else {
        "subcommand"
    };

    // Try to find template in auto_scenarios first
    if let Some(template) = semantics
        .verification
        .auto_scenarios
        .iter()
        .find(|t| t.kind == template_kind)
    {
        let mut argv = Vec::new();
        argv.extend(template.argv_prefix.iter().cloned());
        argv.extend(context_argv.iter().cloned());
        argv.push(surface_id.to_string());
        argv.extend(template.argv_suffix.iter().cloned());
        return argv;
    }

    // Fall back to legacy fields for backward compatibility
    let (prefix, suffix) = if is_option_like {
        (
            &semantics.verification.option_existence_argv_prefix,
            &semantics.verification.option_existence_argv_suffix,
        )
    } else {
        (
            &semantics.verification.subcommand_existence_argv_prefix,
            &semantics.verification.subcommand_existence_argv_suffix,
        )
    };
    let mut argv = Vec::new();
    argv.extend(prefix.iter().cloned());
    argv.extend(context_argv.iter().cloned());
    argv.push(surface_id.to_string());
    argv.extend(suffix.iter().cloned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::{ScenarioDefaults, VerificationPlan, VerificationPolicy};
    use std::collections::BTreeMap;

    fn surface_with_option_and_subcommand() -> SurfaceInventory {
        SurfaceInventory {
            schema_version: 2,
            generated_at_epoch_ms: 0,
            binary_name: Some("bin".to_string()),
            inputs_hash: None,
            discovery: Vec::new(),
            items: vec![
                crate::surface::SurfaceItem {
                    id: "--color".to_string(),
                    display: "--color".to_string(),
                    description: None,
                    parent_id: None,
                    context_argv: Vec::new(),
                    forms: vec!["--color[=WHEN]".to_string()],
                    invocation: crate::surface::SurfaceInvocation::default(),
                    evidence: Vec::new(),
                },
                // Entry point (subcommand) - context_argv contains its own id
                crate::surface::SurfaceItem {
                    id: "show".to_string(),
                    display: "show".to_string(),
                    description: None,
                    parent_id: None,
                    context_argv: vec!["show".to_string()],
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
    fn auto_verification_targets_only_non_entry_points() {
        let plan = plan_with_policy();
        let surface = surface_with_option_and_subcommand();
        let targets = auto_verification_targets(&plan, &surface).unwrap();

        // Only --color should be targeted (show is an entry point)
        assert_eq!(targets.target_ids, vec!["--color".to_string()]);
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

        let mut semantics_b = semantics_a.clone();
        semantics_b.verification.option_existence_argv_prefix = vec!["inspect".to_string()];
        semantics_b.verification.option_existence_argv_suffix = vec!["--help".to_string()];

        let scenarios_a = auto_verification_scenarios(&targets, &semantics_a, &surface, None);
        let scenarios_b = auto_verification_scenarios(&targets, &semantics_b, &surface, None);

        assert_eq!(
            scenario_argv_for_id(&scenarios_a, "auto_verify::--color"),
            vec![
                "probe".to_string(),
                "--color".to_string(),
                "--usage".to_string()
            ]
        );
        assert_eq!(
            scenario_argv_for_id(&scenarios_b, "auto_verify::--color"),
            vec![
                "inspect".to_string(),
                "--color".to_string(),
                "--help".to_string()
            ]
        );
    }

    #[test]
    fn auto_verification_digest_ignores_behavior_defaults_seed_changes() {
        let base_plan = plan_with_policy();
        let surface = surface_with_option_and_subcommand();
        let targets = auto_verification_targets(&base_plan, &surface).unwrap();
        let semantics: Semantics =
            serde_json::from_str(crate::templates::ENRICH_SEMANTICS_JSON).unwrap();
        let scenarios = auto_verification_scenarios(&targets, &semantics, &surface, None);
        let scenario = scenarios
            .iter()
            .find(|candidate| candidate.id == "auto_verify::--color")
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

        let scenarios = auto_verification_scenarios(&targets, &semantics, &surface, None);
        assert_eq!(
            scenario_argv_for_id(&scenarios, "auto_verify::--color"),
            vec![
                "new".to_string(),
                "--color".to_string(),
                "--new".to_string()
            ]
        );
    }

    #[test]
    fn context_argv_is_included_in_generated_scenario_argv() {
        // Surface item with context_argv representing "git config --global"
        let surface = SurfaceInventory {
            schema_version: 2,
            generated_at_epoch_ms: 0,
            binary_name: Some("git".to_string()),
            inputs_hash: None,
            discovery: Vec::new(),
            items: vec![crate::surface::SurfaceItem {
                id: "--global".to_string(),
                display: "--global".to_string(),
                description: Some("use global config file".to_string()),
                parent_id: Some("config".to_string()),
                context_argv: vec!["config".to_string()],
                forms: vec!["--global".to_string()],
                invocation: crate::surface::SurfaceInvocation::default(),
                evidence: Vec::new(),
            }],
            blockers: Vec::new(),
        };

        let plan = plan_with_policy();
        let targets = auto_verification_targets(&plan, &surface).unwrap();

        let mut semantics: Semantics =
            serde_json::from_str(crate::templates::ENRICH_SEMANTICS_JSON).unwrap();
        semantics.verification.auto_scenarios = vec![crate::semantics::AutoScenarioTemplate {
            kind: "option".to_string(),
            argv_prefix: Vec::new(),
            argv_suffix: vec!["--help".to_string()],
            tiers: vec!["behavior".to_string()],
        }];

        let scenarios = auto_verification_scenarios(&targets, &semantics, &surface, None);
        // Should be: context_argv + surface_id + suffix = ["config", "--global", "--help"]
        assert_eq!(
            scenario_argv_for_id(&scenarios, "auto_verify::--global"),
            vec![
                "config".to_string(),
                "--global".to_string(),
                "--help".to_string()
            ]
        );
    }
}
