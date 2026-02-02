//! Auto-verification helpers for option/subcommand existence.
//!
//! These helpers expand a compact policy into synthetic scenarios without
//! embedding CLI semantics in Rust.
use crate::semantics::Semantics;
use crate::surface::SurfaceInventory;
use std::collections::{BTreeMap, BTreeSet};

use super::{
    ScenarioExpect, ScenarioKind, ScenarioPlan, ScenarioSpec, VerificationExcludedEntry,
    VerificationTargetKind,
};

/// Auto-verification targets derived from the surface inventory.
pub struct AutoVerificationTargets {
    pub max_new_runs_per_apply: usize,
    pub target_ids: Vec<String>,
    pub targets: Vec<(VerificationTargetKind, Vec<String>)>,
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
        targets.push((*kind, filtered_ids));
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
            let argv = existence_argv(semantics, *kind, surface_id);
            scenarios.push(ScenarioSpec {
                id: auto_scenario_id(*kind, surface_id),
                kind: ScenarioKind::Behavior,
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

fn collect_surface_ids(
    surface: &SurfaceInventory,
    kind: &VerificationTargetKind,
) -> BTreeSet<String> {
    let kind_label = kind.as_str();
    let mut ids = BTreeSet::new();
    for item in surface.items.iter().filter(|item| item.kind == kind_label) {
        let id = item.id.trim();
        if id.is_empty() {
            continue;
        }
        ids.insert(id.to_string());
    }
    ids
}

fn auto_scenario_id(kind: VerificationTargetKind, surface_id: &str) -> String {
    format!(
        "{}{}::{}",
        super::AUTO_VERIFY_SCENARIO_PREFIX,
        kind.as_str(),
        surface_id
    )
}

fn existence_argv(
    semantics: &Semantics,
    kind: VerificationTargetKind,
    surface_id: &str,
) -> Vec<String> {
    let (prefix, suffix) = match kind {
        VerificationTargetKind::Option => (
            &semantics.verification.option_existence_argv_prefix,
            &semantics.verification.option_existence_argv_suffix,
        ),
        VerificationTargetKind::Subcommand => (
            &semantics.verification.subcommand_existence_argv_prefix,
            &semantics.verification.subcommand_existence_argv_suffix,
        ),
    };
    let mut argv = Vec::new();
    argv.extend(prefix.iter().cloned());
    argv.push(surface_id.to_string());
    argv.extend(suffix.iter().cloned());
    argv
}
