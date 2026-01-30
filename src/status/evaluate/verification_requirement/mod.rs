mod actions;
mod ledger;
mod queue;
mod surface;

use super::super::inputs::{
    load_scenario_plan_state, load_surface_inventory_state, ScenarioPlanLoadError,
    ScenarioPlanLoadResult, SurfaceLoadError, SurfaceLoadResult,
};
use super::{format_preview, preview_ids, EvalState};
use crate::enrich;
use actions::{maybe_set_verification_action_from_ledger, VerificationActionArgs};
use anyhow::Result;
use ledger::{build_verification_ledger_entries, collect_unverified_from_ledger};
use queue::{
    append_missing_queue_ids_blocker, collect_discovered_untriaged_ids,
    collect_verification_queue_state, maybe_set_verification_triage_next_action,
};
use surface::collect_surface_ids;

pub(super) fn eval_verification_requirement(
    state: &mut EvalState,
    req: enrich::RequirementId,
) -> Result<enrich::RequirementStatus> {
    let paths = state.paths;
    let binary_name = state.binary_name;
    let config = state.config;
    let include_full = state.include_full;
    let missing_artifacts = &mut *state.missing_artifacts;
    let blockers = &mut *state.blockers;
    let verification_next_action = &mut *state.verification_next_action;

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

    let mut evidence = vec![
        surface_evidence.clone(),
        scenarios_evidence.clone(),
        template_evidence.clone(),
        semantics_evidence.clone(),
    ];
    let mut local_blockers = Vec::new();
    let mut missing = Vec::new();
    let mut unverified_ids = Vec::new();
    let mut triage_summary: Option<enrich::VerificationTriageSummary> = None;
    let verification_tier = config.verification_tier.as_deref().unwrap_or("accepted");
    let tier_label = if verification_tier == "behavior" {
        "behavior"
    } else {
        "acceptance"
    };

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
        missing_artifacts.push(semantics_evidence.path);
        missing.push("verification semantics missing (enrich/semantics.json)".to_string());
    }

    if let (Some(surface), Some(plan)) = (surface.as_ref(), plan.as_ref()) {
        let (surface_ids, surface_evidence_map) = collect_surface_ids(surface);
        let queue_state = collect_verification_queue_state(plan, verification_tier);
        let discovered_untriaged_ids = collect_discovered_untriaged_ids(
            &surface_ids,
            &queue_state.triaged_ids,
            &surface_evidence_map,
            &mut evidence,
        );

        append_missing_queue_ids_blocker(
            &surface_ids,
            &queue_state.queue_ids,
            &mut local_blockers,
            &surface_evidence,
            &scenarios_evidence,
        );

        maybe_set_verification_triage_next_action(
            plan,
            &discovered_untriaged_ids,
            verification_next_action,
            binary_name,
        );

        let ledger_entries = if template_path.is_file() && semantics_path.is_file() {
            build_verification_ledger_entries(
                binary_name,
                surface,
                plan,
                paths,
                &template_path,
                &mut local_blockers,
                &template_evidence,
            )
        } else {
            None
        };

        let mut triaged_unverified_ids = Vec::new();
        if let Some(ledger_entries) = ledger_entries.as_ref() {
            let (triaged_ids, _unverified) = collect_unverified_from_ledger(
                plan,
                ledger_entries,
                verification_tier,
                &mut evidence,
            );
            triaged_unverified_ids = triaged_ids;

            maybe_set_verification_action_from_ledger(VerificationActionArgs {
                plan,
                ledger_entries,
                verification_tier,
                verification_next_action,
                paths,
                binary_name,
                discovered_untriaged_empty: discovered_untriaged_ids.is_empty(),
                blockers_empty: local_blockers.is_empty(),
                missing_empty: missing.is_empty(),
            });
        }

        let discovered_preview = preview_ids(&discovered_untriaged_ids);
        let triaged_preview = preview_ids(&triaged_unverified_ids);
        let summary = enrich::VerificationTriageSummary {
            discovered_untriaged_count: discovered_untriaged_ids.len(),
            discovered_untriaged_preview: discovered_preview,
            triaged_unverified_count: triaged_unverified_ids.len(),
            triaged_unverified_preview: triaged_preview,
            excluded_count: if queue_state.excluded.is_empty() {
                None
            } else {
                Some(queue_state.excluded.len())
            },
            excluded: queue_state.excluded,
            discovered_untriaged_ids: include_full.then(|| discovered_untriaged_ids.clone()),
            triaged_unverified_ids: include_full.then(|| triaged_unverified_ids.clone()),
        };
        let summary_preview = format!(
            "triage {}: {} untriaged ({}) ; {} unverified ({})",
            tier_label,
            summary.discovered_untriaged_count,
            format_preview(
                summary.discovered_untriaged_count,
                &summary.discovered_untriaged_preview
            ),
            summary.triaged_unverified_count,
            format_preview(
                summary.triaged_unverified_count,
                &summary.triaged_unverified_preview
            )
        );
        triage_summary = Some(summary);

        if triage_summary.as_ref().is_some_and(|summary| {
            summary.discovered_untriaged_count > 0 || summary.triaged_unverified_count > 0
        }) {
            unverified_ids.clear();
            unverified_ids.push(summary_preview);
        }
    }

    enrich::dedupe_evidence_refs(&mut evidence);
    let (status, reason) = if !local_blockers.is_empty() {
        (
            enrich::RequirementState::Blocked,
            "verification inputs blocked".to_string(),
        )
    } else if !missing.is_empty() {
        (
            enrich::RequirementState::Unmet,
            format!("verification inputs missing: {}", missing.join("; ")),
        )
    } else if !unverified_ids.is_empty() {
        (
            enrich::RequirementState::Unmet,
            format!("verification {tier_label} incomplete"),
        )
    } else {
        (
            enrich::RequirementState::Met,
            format!("verification {tier_label} complete"),
        )
    };

    let unverified_count = triage_summary
        .as_ref()
        .map(|summary| summary.triaged_unverified_count);

    blockers.extend(local_blockers.clone());
    Ok(enrich::RequirementStatus {
        id: req,
        status,
        reason,
        unverified_ids,
        unverified_count,
        verification: triage_summary,
        evidence,
        blockers: local_blockers,
    })
}
