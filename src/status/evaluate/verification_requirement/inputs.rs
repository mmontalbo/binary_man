use super::super::super::inputs::{
    load_scenario_plan_state, load_surface_inventory_state, ScenarioPlanLoadError,
    ScenarioPlanLoadResult, SurfaceLoadError, SurfaceLoadResult,
};
use crate::enrich;
use crate::scenarios;
use anyhow::Result;

pub(super) struct VerificationInputs {
    pub(super) surface: Option<crate::surface::SurfaceInventory>,
    pub(super) plan: Option<scenarios::ScenarioPlan>,
    pub(super) surface_evidence: enrich::EvidenceRef,
    pub(super) scenarios_evidence: enrich::EvidenceRef,
    pub(super) template_path: std::path::PathBuf,
    pub(super) template_evidence: enrich::EvidenceRef,
    pub(super) semantics_path: std::path::PathBuf,
    pub(super) semantics_evidence: enrich::EvidenceRef,
}

pub(super) fn load_verification_inputs(
    paths: &enrich::DocPackPaths,
    missing_artifacts: &mut Vec<String>,
    missing: &mut Vec<String>,
    local_blockers: &mut Vec<enrich::Blocker>,
) -> Result<VerificationInputs> {
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
    let template_path = paths
        .root()
        .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);
    let template_evidence = paths.evidence_from_path(&template_path)?;
    let semantics_path = paths.semantics_path();
    let semantics_evidence = paths.evidence_from_path(&semantics_path)?;

    let surface = match error {
        Some(SurfaceLoadError::Missing) => {
            missing.push("surface inventory missing".to_string());
            None
        }
        Some(SurfaceLoadError::Parse(message)) => {
            let blocker = enrich::Blocker {
                code: "surface_parse_error".to_string(),
                message,
                evidence: vec![surface_evidence.clone()],
                next_action: None,
            };
            local_blockers.push(blocker);
            None
        }
        Some(SurfaceLoadError::Invalid(message)) => {
            let blocker = enrich::Blocker {
                code: "surface_schema_invalid".to_string(),
                message,
                evidence: vec![surface_evidence.clone()],
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
                evidence: vec![scenarios_evidence.clone()],
                next_action: Some("fix scenarios/plan.json".to_string()),
            };
            local_blockers.push(blocker);
            None
        }
        None => plan,
    };

    if !template_path.is_file() {
        missing_artifacts.push(template_evidence.path.clone());
        missing.push(format!(
            "verification lens missing ({})",
            enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL
        ));
    }
    if !semantics_path.is_file() {
        missing_artifacts.push(semantics_evidence.path.clone());
        missing.push("verification semantics missing (enrich/semantics.json)".to_string());
    }

    Ok(VerificationInputs {
        surface,
        plan,
        surface_evidence,
        scenarios_evidence,
        template_path,
        template_evidence,
        semantics_path,
        semantics_evidence,
    })
}

pub(super) fn base_evidence(inputs: &VerificationInputs) -> Vec<enrich::EvidenceRef> {
    vec![
        inputs.surface_evidence.clone(),
        inputs.scenarios_evidence.clone(),
        inputs.template_evidence.clone(),
        inputs.semantics_evidence.clone(),
    ]
}

pub(super) fn ensure_verification_policy(
    plan: &scenarios::ScenarioPlan,
    missing: &mut Vec<String>,
    verification_next_action: &mut Option<enrich::NextAction>,
    binary_name: Option<&str>,
) {
    if plan.verification.policy.is_some() {
        return;
    }
    missing.push("verification policy missing (scenarios/plan.json)".to_string());
    let content =
        serde_json::to_string_pretty(plan).unwrap_or_else(|_| scenarios::plan_stub(binary_name));
    *verification_next_action = Some(enrich::NextAction::Edit {
        path: "scenarios/plan.json".to_string(),
        content,
        reason: "add verification policy in scenarios/plan.json".to_string(),
        hint: Some("Add verification policy to plan".to_string()),
        edit_strategy: enrich::default_edit_strategy(),
        payload: None,
    });
}
