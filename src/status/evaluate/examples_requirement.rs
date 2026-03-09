//! Examples report requirement evaluation.
//!
//! Checks if scenario evidence exists in the inventory.

use super::EvalState;
use crate::enrich;
use crate::scenarios;
use anyhow::Result;

pub(super) fn eval_examples_report_requirement(
    state: &mut EvalState,
    req: enrich::RequirementId,
) -> Result<enrich::RequirementStatus> {
    let paths = state.paths;
    let missing_artifacts = &mut *state.missing_artifacts;
    let blockers = &mut *state.blockers;

    // Check for scenario evidence in inventory/scenarios/index.json
    let scenarios_index_path = paths.inventory_scenarios_dir().join("index.json");
    let scenarios_index_evidence = paths.evidence_from_path(&scenarios_index_path)?;
    let mut evidence = vec![scenarios_index_evidence.clone()];
    let local_blockers: Vec<enrich::Blocker> = Vec::new();
    let mut unmet = Vec::new();

    // Check scenarios plan exists
    let scenarios_path = paths.scenarios_plan_path();
    let scenarios_evidence = paths.evidence_from_path(&scenarios_path)?;
    evidence.push(scenarios_evidence.clone());
    if !scenarios_path.is_file() {
        missing_artifacts.push(scenarios_evidence.path);
        unmet.push("scenarios plan missing".to_string());
    }

    // Check if scenario evidence index exists
    if let Ok(Some(index)) = scenarios::read_scenario_index(&scenarios_index_path) {
        if index.scenarios.is_empty() {
            unmet.push("no scenario evidence recorded".to_string());
        }
    } else if !scenarios_index_path.is_file() {
        missing_artifacts.push(scenarios_index_evidence.path);
        unmet.push("scenario evidence index missing".to_string());
    }

    let (status, reason) = if !local_blockers.is_empty() {
        (
            enrich::RequirementState::Blocked,
            "scenario runs blocked".to_string(),
        )
    } else if !unmet.is_empty() {
        (
            enrich::RequirementState::Unmet,
            format!("scenario runs missing: {}", unmet.join("; ")),
        )
    } else {
        (
            enrich::RequirementState::Met,
            "scenario evidence present".to_string(),
        )
    };
    blockers.extend(local_blockers);
    Ok(enrich::RequirementStatus {
        id: req,
        status,
        reason,
        verification_tier: None,
        accepted_verified_count: None,
        unverified_ids: Vec::new(),
        accepted_unverified_count: None,
        behavior_verified_count: None,
        behavior_unverified_count: None,
        verification: None,
        evidence,
        blockers: Vec::new(),
    })
}
