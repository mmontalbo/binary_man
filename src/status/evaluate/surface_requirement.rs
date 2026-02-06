use super::super::inputs::{load_surface_inventory_state, SurfaceLoadError};
use super::EvalState;
use crate::enrich;
use crate::surface;
use anyhow::Result;

pub(super) fn eval_surface_requirement(
    state: &mut EvalState,
    req: enrich::RequirementId,
) -> Result<enrich::RequirementStatus> {
    let paths = state.paths;
    let lock_status = state.lock_status;
    let missing_artifacts = &mut *state.missing_artifacts;
    let blockers = &mut *state.blockers;

    let surface_state = load_surface_inventory_state(paths, missing_artifacts)?;
    let evidence = surface_state.evidence.clone();
    let surface = match surface_state.error {
        Some(SurfaceLoadError::Missing) => {
            return Ok(enrich::RequirementStatus {
                id: req,
                status: enrich::RequirementState::Unmet,
                reason: "surface inventory missing".to_string(),
                verification_tier: None,
                accepted_verified_count: None,
                unverified_ids: Vec::new(),
                accepted_unverified_count: None,
                behavior_verified_count: None,
                behavior_unverified_count: None,
                verification: None,
                evidence: vec![evidence],
                blockers: Vec::new(),
            });
        }
        Some(SurfaceLoadError::Parse(message)) => {
            let blocker = enrich::Blocker {
                code: "surface_parse_error".to_string(),
                message,
                evidence: vec![evidence.clone()],
                next_action: None,
            };
            blockers.push(blocker.clone());
            return Ok(enrich::RequirementStatus {
                id: req,
                status: enrich::RequirementState::Blocked,
                reason: "surface inventory parse error".to_string(),
                verification_tier: None,
                accepted_verified_count: None,
                unverified_ids: Vec::new(),
                accepted_unverified_count: None,
                behavior_verified_count: None,
                behavior_unverified_count: None,
                verification: None,
                evidence: vec![evidence],
                blockers: vec![blocker],
            });
        }
        Some(SurfaceLoadError::Invalid(message)) => {
            let blocker = enrich::Blocker {
                code: "surface_schema_invalid".to_string(),
                message,
                evidence: vec![evidence.clone()],
                next_action: Some("fix inventory/surface.json".to_string()),
            };
            blockers.push(blocker.clone());
            return Ok(enrich::RequirementStatus {
                id: req,
                status: enrich::RequirementState::Blocked,
                reason: "surface inventory schema invalid".to_string(),
                verification_tier: None,
                accepted_verified_count: None,
                unverified_ids: Vec::new(),
                accepted_unverified_count: None,
                behavior_verified_count: None,
                behavior_unverified_count: None,
                verification: None,
                evidence: vec![evidence],
                blockers: vec![blocker],
            });
        }
        None => surface_state.surface.expect("surface inventory present"),
    };

    let meaningful_items = surface::meaningful_surface_items(&surface);
    let is_stale = lock_status.present
        && !lock_status.stale
        && lock_status.inputs_hash.is_some()
        && surface.inputs_hash.is_some()
        && surface.inputs_hash != lock_status.inputs_hash;
    let (status, reason, req_blockers) = if meaningful_items < 1 {
        (
            enrich::RequirementState::Unmet,
            "surface inventory missing items".to_string(),
            Vec::new(),
        )
    } else if is_stale {
        (
            enrich::RequirementState::Unmet,
            "surface inventory stale relative to lock".to_string(),
            Vec::new(),
        )
    } else {
        (
            enrich::RequirementState::Met,
            "surface inventory present".to_string(),
            Vec::new(),
        )
    };
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
        evidence: vec![evidence],
        blockers: req_blockers,
    })
}
