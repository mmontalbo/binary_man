//! Auto-verification helpers for option existence.
//!
//! These helpers expand a compact policy into synthetic scenarios without
//! embedding CLI semantics in Rust.
use crate::semantics::Semantics;
use crate::surface::SurfaceInventory;
use std::collections::{BTreeMap, BTreeSet};

use super::{
    ScenarioExpect, ScenarioKind, ScenarioPlan, ScenarioSpec, VerificationExcludedEntry,
    VerificationPolicyMode,
};

/// Auto-verification targets derived from the surface inventory.
pub struct AutoVerificationTargets {
    pub max_new_runs_per_apply: usize,
    pub target_ids: Vec<String>,
    pub excluded: Vec<VerificationExcludedEntry>,
    pub excluded_ids: BTreeSet<String>,
}

/// Collect option ids eligible for auto-verification based on the policy.
pub fn auto_verification_targets(
    plan: &ScenarioPlan,
    surface: &SurfaceInventory,
) -> Option<AutoVerificationTargets> {
    let policy = plan.verification.policy.as_ref()?;
    if policy.mode != VerificationPolicyMode::VerifyAllOptions {
        return None;
    }
    let mut option_ids = BTreeSet::new();
    for item in surface.items.iter().filter(|item| item.kind == "option") {
        let id = item.id.trim();
        if id.is_empty() {
            continue;
        }
        option_ids.insert(id.to_string());
    }

    let mut excluded_ids = BTreeSet::new();
    let mut excluded = Vec::new();
    for entry in &policy.excludes {
        let surface_id = entry.surface_id.trim();
        if surface_id.is_empty() {
            continue;
        }
        excluded_ids.insert(surface_id.to_string());
        excluded.push(VerificationExcludedEntry {
            surface_id: surface_id.to_string(),
            prereqs: entry.prereqs.clone(),
            reason: Some(entry.reason.trim().to_string()),
        });
    }

    let target_ids: Vec<String> = option_ids
        .into_iter()
        .filter(|id| !excluded_ids.contains(id))
        .collect();

    Some(AutoVerificationTargets {
        max_new_runs_per_apply: policy.max_new_runs_per_apply,
        target_ids,
        excluded,
        excluded_ids,
    })
}

/// Build synthetic scenarios for option existence verification.
pub fn auto_verification_scenarios(
    targets: &AutoVerificationTargets,
    semantics: &Semantics,
) -> Vec<ScenarioSpec> {
    let mut scenarios = Vec::with_capacity(targets.target_ids.len());
    for surface_id in &targets.target_ids {
        let argv = option_existence_argv(semantics, surface_id);
        scenarios.push(ScenarioSpec {
            id: format!("{}{}", super::AUTO_VERIFY_SCENARIO_PREFIX, surface_id),
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
            covers: vec![surface_id.to_string()],
            coverage_ignore: false,
            expect: ScenarioExpect::default(),
        });
    }
    scenarios
}

fn option_existence_argv(semantics: &Semantics, surface_id: &str) -> Vec<String> {
    let mut argv = Vec::new();
    argv.extend(
        semantics
            .verification
            .option_existence_argv_prefix
            .iter()
            .cloned(),
    );
    argv.push(surface_id.to_string());
    argv.extend(
        semantics
            .verification
            .option_existence_argv_suffix
            .iter()
            .cloned(),
    );
    argv
}
