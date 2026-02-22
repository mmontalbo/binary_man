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
/// Targets option-like surface items (ids starting with `-`) that aren't
/// entry points or help-output items.
pub fn auto_verification_targets_for_behavior(
    plan: &ScenarioPlan,
    surface: &SurfaceInventory,
    _semantics: &Semantics,
) -> Option<AutoVerificationTargets> {
    let policy = plan.verification.policy.as_ref()?;
    let (excluded, excluded_ids) = plan.collect_queue_exclusions();

    // Collect non-entry-point option-like surface ids
    let target_ids: Vec<String> = surface
        .items
        .iter()
        .filter(|item| {
            if is_entry_point(item) {
                return false;
            }
            // Skip help-output items (--help, --version, etc.)
            // These are already verified by the help tier.
            if item.is_help_output {
                return false;
            }
            // Only verify option-like items
            looks_like_option(&item.id)
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

/// Result of auto-verification scenario generation.
pub struct AutoVerificationResult {
    /// Generated scenarios for verification.
    pub scenarios: Vec<ScenarioSpec>,
    /// Surface IDs excluded via prereqs (e.g., interactive TTY options).
    pub prereq_excluded_ids: Vec<String>,
}

/// Build synthetic scenarios for existence verification.
///
/// When prereqs is provided, uses prereq seeds for items that have them.
/// Items with `exclude: true` in prereqs are skipped and returned in
/// `prereq_excluded_ids` for overlay generation.
pub fn auto_verification_scenarios(
    targets: &AutoVerificationTargets,
    semantics: &Semantics,
    surface: &SurfaceInventory,
    prereqs: Option<&PrereqsFile>,
) -> AutoVerificationResult {
    let mut scenarios = Vec::with_capacity(targets.target_ids.len());
    let mut prereq_excluded_ids = Vec::new();

    for surface_id in &targets.target_ids {
        let item = surface.items.iter().find(|item| &item.id == surface_id);
        let context_argv = item.map(|i| i.context_argv.as_slice()).unwrap_or(&[]);

        // Build qualified surface_id for prereq lookup (e.g., "config.--regexp")
        let qualified_id = qualify_surface_id(context_argv, surface_id);

        // Check prereqs for exclusion - try qualified ID first, fall back to simple ID
        let resolved = prereqs.map(|p| {
            let r = p.resolve(&qualified_id);
            // If no prereqs found with qualified ID, try simple surface_id
            if r.seed.is_none() && !r.exclude {
                p.resolve(surface_id)
            } else {
                r
            }
        });
        if resolved.as_ref().is_some_and(|r| r.exclude) {
            prereq_excluded_ids.push(surface_id.clone());
            continue;
        }

        let argv = existence_argv(semantics, surface_id, context_argv);

        // Use prereq seed if available, otherwise None to inherit from plan defaults
        let seed = resolved.and_then(|r| r.seed);

        scenarios.push(ScenarioSpec {
            id: auto_scenario_id(surface_id),
            kind: ScenarioKind::Behavior,
            publish: false,
            argv,
            env: std::collections::BTreeMap::new(),
            stdin: None,
            // Use prereq seed if available, or None to allow defaults inheritance
            seed,
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
    AutoVerificationResult {
        scenarios,
        prereq_excluded_ids,
    }
}

fn auto_scenario_id(surface_id: &str) -> String {
    format!("{}{}", super::AUTO_VERIFY_SCENARIO_PREFIX, surface_id)
}

/// Qualify a surface_id with context_argv to create a unique key.
fn qualify_surface_id(context_argv: &[String], surface_id: &str) -> String {
    if context_argv.is_empty() {
        surface_id.to_string()
    } else {
        format!("{}.{}", context_argv.join("."), surface_id)
    }
}

fn existence_argv(
    semantics: &Semantics,
    surface_id: &str,
    context_argv: &[String],
) -> Vec<String> {
    let (prefix, suffix) = if looks_like_option(surface_id) {
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
                    is_help_output: false,
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
                    is_help_output: false,
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

        let result_a = auto_verification_scenarios(&targets, &semantics_a, &surface, None);
        let result_b = auto_verification_scenarios(&targets, &semantics_b, &surface, None);

        assert_eq!(
            scenario_argv_for_id(&result_a.scenarios, "auto_verify::--color"),
            vec![
                "probe".to_string(),
                "--color".to_string(),
                "--usage".to_string()
            ]
        );
        assert_eq!(
            scenario_argv_for_id(&result_b.scenarios, "auto_verify::--color"),
            vec![
                "inspect".to_string(),
                "--color".to_string(),
                "--help".to_string()
            ]
        );
    }

    #[test]
    fn auto_verification_inherits_defaults_seed_when_no_prereqs() {
        let base_plan = plan_with_policy();
        let surface = surface_with_option_and_subcommand();
        let targets = auto_verification_targets(&base_plan, &surface).unwrap();
        let semantics: Semantics =
            serde_json::from_str(crate::templates::ENRICH_SEMANTICS_JSON).unwrap();
        let result = auto_verification_scenarios(&targets, &semantics, &surface, None);
        let scenario = result.scenarios
            .iter()
            .find(|candidate| candidate.id == "auto_verify::--color")
            .cloned()
            .expect("auto scenario");

        // Scenario should have seed: None (no prereqs provided)
        assert!(scenario.seed.is_none(), "auto_verify scenario should have seed: None when no prereqs");

        // Now test that effective_scenario_config inherits from defaults
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
        // Digests should DIFFER because defaults seed is now inherited
        assert_ne!(digest_a, digest_b, "digest should change when defaults seed changes");
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
                is_help_output: false,
            }],
            blockers: Vec::new(),
        };

        let plan = plan_with_policy();
        let targets = auto_verification_targets(&plan, &surface).unwrap();

        let mut semantics: Semantics =
            serde_json::from_str(crate::templates::ENRICH_SEMANTICS_JSON).unwrap();
        semantics.verification.option_existence_argv_suffix = vec!["--help".to_string()];

        let result = auto_verification_scenarios(&targets, &semantics, &surface, None);
        // Should be: prefix + context_argv + surface_id + suffix = ["config", "--global", "--help"]
        assert_eq!(
            scenario_argv_for_id(&result.scenarios, "auto_verify::--global"),
            vec![
                "config".to_string(),
                "--global".to_string(),
                "--help".to_string()
            ]
        );
    }

    #[test]
    fn auto_verification_uses_prereqs_seed() {
        use crate::enrich::{PrereqsFile, PrereqInferenceDefinition};
        use crate::scenarios::{ScenarioSeedSpec, ScenarioSeedEntry, SeedEntryKind};

        let plan = plan_with_policy();
        let surface = surface_with_option_and_subcommand();
        let targets = auto_verification_targets(&plan, &surface).unwrap();
        let semantics: Semantics =
            serde_json::from_str(crate::templates::ENRICH_SEMANTICS_JSON).unwrap();

        // Create prereqs with seed for --color
        let mut prereqs = PrereqsFile::default();
        prereqs.definitions.insert(
            "needs_setup".to_string(),
            PrereqInferenceDefinition {
                description: Some("requires setup".to_string()),
                seed: Some(ScenarioSeedSpec {
                    setup: vec![
                        vec!["git".to_string(), "init".to_string()],
                    ],
                    entries: vec![
                        ScenarioSeedEntry {
                            path: "test.txt".to_string(),
                            kind: SeedEntryKind::File,
                            contents: Some("test content".to_string()),
                            target: None,
                            mode: None,
                        },
                    ],
                }),
                exclude: false,
            },
        );
        prereqs.surface_map.insert("--color".to_string(), vec!["needs_setup".to_string()]);

        let result = auto_verification_scenarios(&targets, &semantics, &surface, Some(&prereqs));
        let scenario = result.scenarios
            .iter()
            .find(|s| s.id == "auto_verify::--color")
            .expect("auto_verify::--color scenario");

        // Scenario should have seed from prereqs
        assert!(scenario.seed.is_some(), "auto_verify scenario should have seed from prereqs");
        let seed = scenario.seed.as_ref().unwrap();
        assert_eq!(seed.setup.len(), 1, "should have one setup command");
        assert_eq!(seed.setup[0], vec!["git", "init"]);
        assert_eq!(seed.entries.len(), 1, "should have one entry");
        assert_eq!(seed.entries[0].path, "test.txt");
    }
}
