use super::super::coverage::coverage_stub_from_plan;
use super::super::inputs::{
    load_scenario_plan_state, load_surface_inventory_state, ScenarioPlanLoadError,
    ScenarioPlanLoadResult, SurfaceLoadError, SurfaceLoadResult,
};
use super::EvalState;
use crate::enrich;
use crate::scenarios;
use crate::semantics;
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn eval_coverage_requirement(
    state: &mut EvalState,
    req: enrich::RequirementId,
) -> Result<enrich::RequirementStatus> {
    let paths = state.paths;
    let missing_artifacts = &mut *state.missing_artifacts;
    let blockers = &mut *state.blockers;
    let coverage_next_action = &mut *state.coverage_next_action;

    let SurfaceLoadResult {
        evidence: surface_evidence,
        surface,
        error,
    } = load_surface_inventory_state(paths, missing_artifacts)?;
    let ScenarioPlanLoadResult {
        evidence: scenarios_evidence,
        plan,
        error: plan_error,
    } = load_scenario_plan_state(paths, missing_artifacts)?;
    let mut evidence = vec![surface_evidence.clone(), scenarios_evidence.clone()];
    let mut local_blockers = Vec::new();
    let mut missing = Vec::new();
    let mut uncovered_ids = Vec::new();
    let mut blocked_ids = BTreeSet::new();

    let surface = match error {
        Some(SurfaceLoadError::Missing) => {
            missing.push("surface inventory missing".to_string());
            None
        }
        Some(SurfaceLoadError::Parse(message)) => {
            let blocker = enrich::Blocker {
                code: "surface_parse_error".to_string(),
                message,
                evidence: vec![surface_evidence],
                next_action: None,
            };
            local_blockers.push(blocker);
            None
        }
        Some(SurfaceLoadError::Invalid(message)) => {
            let blocker = enrich::Blocker {
                code: "surface_schema_invalid".to_string(),
                message,
                evidence: vec![surface_evidence],
                next_action: Some("fix inventory/surface.json".to_string()),
            };
            local_blockers.push(blocker);
            None
        }
        None => surface,
    };

    let plan = match plan_error {
        Some(ScenarioPlanLoadError::Missing) => {
            missing.push("scenarios plan missing".to_string());
            None
        }
        Some(ScenarioPlanLoadError::Invalid(message)) => {
            let blocker = enrich::Blocker {
                code: "scenario_plan_invalid".to_string(),
                message,
                evidence: vec![scenarios_evidence],
                next_action: Some("fix scenarios/plan.json".to_string()),
            };
            local_blockers.push(blocker);
            None
        }
        None => plan,
    };

    if let (Some(surface), Some(plan)) = (surface.as_ref(), plan.as_ref()) {
        let loaded_semantics = semantics::load_semantics(paths.root()).ok();
        let mut covered = BTreeSet::new();
        for scenario in &plan.scenarios {
            if scenario.coverage_ignore || scenario.covers.is_empty() {
                continue;
            }
            for token in &scenario.covers {
                let normalized = scenarios::normalize_surface_id(token);
                if !normalized.is_empty() {
                    covered.insert(normalized);
                }
            }
        }

        if let Some(coverage) = plan.coverage.as_ref() {
            for blocked in &coverage.blocked {
                for item_id in &blocked.item_ids {
                    let normalized = scenarios::normalize_surface_id(item_id);
                    if normalized.is_empty() {
                        continue;
                    }
                    blocked_ids.insert(normalized);
                }
            }
        }

        let mut surface_evidence_map: BTreeMap<String, Vec<enrich::EvidenceRef>> = BTreeMap::new();
        // Include all non-entry-point items for coverage tracking
        for item in surface.items.iter().filter(|item| {
            // Entry points have their own id in context_argv
            item.context_argv.last().map(|s| s.as_str()) != Some(item.id.as_str())
        }) {
            let normalized = scenarios::normalize_surface_id(&item.id);
            if normalized.is_empty() {
                continue;
            }
            let entry = surface_evidence_map.entry(normalized).or_default();
            entry.extend(item.evidence.iter().cloned());
        }

        for (id, item_evidence) in surface_evidence_map {
            if covered.contains(&id) || blocked_ids.contains(&id) {
                continue;
            }
            uncovered_ids.push(id);
            evidence.extend(item_evidence);
        }

        uncovered_ids.sort();
        if !uncovered_ids.is_empty() {
            if let Some(content) =
                coverage_stub_from_plan(plan, surface, loaded_semantics.as_ref(), &uncovered_ids)
            {
                *coverage_next_action = Some(enrich::NextAction::Edit {
                    path: "scenarios/plan.json".to_string(),
                    content,
                    reason: format!(
                        "add coverage claim (1 of {}): {}",
                        uncovered_ids.len(),
                        uncovered_ids[0]
                    ),
                    hint: Some("Add scenario to cover unclaimed surface".to_string()),
                    edit_strategy: enrich::default_edit_strategy(),
                    payload: None,
                });
            }
        }
    }

    enrich::dedupe_evidence_refs(&mut evidence);
    let (status, reason) = if !local_blockers.is_empty() {
        (
            enrich::RequirementState::Blocked,
            "coverage inputs blocked".to_string(),
        )
    } else if !missing.is_empty() {
        (
            enrich::RequirementState::Unmet,
            format!("coverage inputs missing: {}", missing.join("; ")),
        )
    } else if !uncovered_ids.is_empty() {
        (
            enrich::RequirementState::Unmet,
            format!("uncovered ids: {}", uncovered_ids.join(", ")),
        )
    } else {
        let reason = if blocked_ids.is_empty() {
            "coverage complete".to_string()
        } else {
            format!("coverage complete (blocked ids: {})", blocked_ids.len())
        };
        (enrich::RequirementState::Met, reason)
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
