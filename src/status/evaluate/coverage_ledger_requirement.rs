use super::super::inputs::{load_surface_inventory_state, SurfaceLoadError, SurfaceLoadResult};
use super::EvalState;
use crate::enrich;
use crate::surface;
use anyhow::Result;

pub(super) fn eval_coverage_ledger_requirement(
    state: &mut EvalState,
    req: enrich::RequirementId,
) -> Result<enrich::RequirementStatus> {
    let paths = state.paths;
    let missing_artifacts = &mut *state.missing_artifacts;
    let blockers = &mut *state.blockers;

    let SurfaceLoadResult {
        evidence: surface_evidence,
        surface,
        error,
    } = load_surface_inventory_state(paths, missing_artifacts)?;
    let mut evidence = vec![surface_evidence.clone()];
    let mut local_blockers = Vec::new();
    let mut unmet = Vec::new();

    match error {
        Some(SurfaceLoadError::Missing) => {
            unmet.push("surface inventory missing".to_string());
        }
        Some(SurfaceLoadError::Parse(message)) => {
            let blocker = enrich::Blocker {
                code: "surface_parse_error".to_string(),
                message,
                evidence: vec![surface_evidence],
                next_action: None,
            };
            local_blockers.push(blocker);
        }
        Some(SurfaceLoadError::Invalid(message)) => {
            let blocker = enrich::Blocker {
                code: "surface_schema_invalid".to_string(),
                message,
                evidence: vec![surface_evidence],
                next_action: Some("fix inventory/surface.json".to_string()),
            };
            local_blockers.push(blocker);
        }
        None => {
            let surface = surface.expect("surface inventory present");
            if surface::meaningful_surface_items(&surface) < 1 {
                unmet.push("surface inventory missing items".to_string());
            }
        }
    }

    let scenarios_path = paths.scenarios_plan_path();
    let scenarios_evidence = paths.evidence_from_path(&scenarios_path)?;
    evidence.push(scenarios_evidence.clone());
    if !scenarios_path.is_file() {
        missing_artifacts.push(scenarios_evidence.path);
        unmet.push("scenarios plan missing".to_string());
    }

    let (status, reason) = if !local_blockers.is_empty() {
        (
            enrich::RequirementState::Blocked,
            "coverage inputs blocked".to_string(),
        )
    } else if !unmet.is_empty() {
        (
            enrich::RequirementState::Unmet,
            format!("coverage inputs missing: {}", unmet.join("; ")),
        )
    } else {
        (
            enrich::RequirementState::Met,
            "coverage inputs present".to_string(),
        )
    };
    blockers.extend(local_blockers.clone());
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
        blockers: local_blockers,
    })
}
