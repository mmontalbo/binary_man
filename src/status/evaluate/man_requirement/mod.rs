mod meta;
mod surface;

use super::super::help::{help_usage_evidence_state, HelpUsageEvidenceState};
use super::super::inputs::{load_semantics_state, SemanticsLoadError};
use super::EvalState;
use crate::enrich;
use crate::scenarios;
use crate::semantics;
use crate::surface::{load_surface_inventory, validate_surface_inventory};
use anyhow::Result;

use meta::{apply_render_summary, load_meta};
use surface::surface_is_multi_command;

pub(super) fn eval_man_page_requirement(
    state: &mut EvalState,
    req: enrich::RequirementId,
) -> Result<enrich::RequirementStatus> {
    let paths = state.paths;
    let binary_name = state.binary_name;
    let lock_status = state.lock_status;
    let missing_artifacts = &mut *state.missing_artifacts;
    let blockers = &mut *state.blockers;
    let man_semantics_next_action = &mut *state.man_semantics_next_action;
    let man_usage_next_action = &mut *state.man_usage_next_action;

    let manifest_path = paths.pack_manifest_path();
    let binary_name = match binary_name {
        Some(name) => name,
        None => {
            let evidence = paths.evidence_from_path(&manifest_path)?;
            let blocker = enrich::Blocker {
                code: "missing_manifest".to_string(),
                message: "binary name unavailable; manifest missing".to_string(),
                evidence: vec![evidence.clone()],
                next_action: None,
            };
            blockers.push(blocker.clone());
            return Ok(enrich::RequirementStatus {
                id: req,
                status: enrich::RequirementState::Blocked,
                reason: "manifest missing".to_string(),
                unverified_ids: Vec::new(),
                unverified_count: None,
                verification: None,
                evidence: vec![evidence],
                blockers: vec![blocker],
            });
        }
    };
    let semantics_state = load_semantics_state(paths, missing_artifacts)?;
    let semantics_evidence = semantics_state.evidence.clone();
    match semantics_state.error {
        Some(SemanticsLoadError::Missing) => {
            *man_semantics_next_action = Some(enrich::NextAction::Edit {
                path: "enrich/semantics.json".to_string(),
                content: semantics::semantics_stub(Some(binary_name)),
                reason: "semantics missing; add render rules".to_string(),
            });
            return Ok(enrich::RequirementStatus {
                id: req,
                status: enrich::RequirementState::Unmet,
                reason: "semantics missing".to_string(),
                unverified_ids: Vec::new(),
                unverified_count: None,
                verification: None,
                evidence: vec![semantics_evidence],
                blockers: Vec::new(),
            });
        }
        Some(SemanticsLoadError::Invalid(message)) => {
            *man_semantics_next_action = Some(enrich::NextAction::Edit {
                path: "enrich/semantics.json".to_string(),
                content: semantics::semantics_stub(Some(binary_name)),
                reason: format!("fix semantics: {message}"),
            });
            return Ok(enrich::RequirementStatus {
                id: req,
                status: enrich::RequirementState::Unmet,
                reason: "semantics invalid".to_string(),
                unverified_ids: Vec::new(),
                unverified_count: None,
                verification: None,
                evidence: vec![semantics_evidence],
                blockers: Vec::new(),
            });
        }
        None => {}
    }

    let surface_path = paths.surface_path();
    let surface_evidence = paths.evidence_from_path(&surface_path)?;
    let multi_command = if surface_path.is_file() {
        match load_surface_inventory(&surface_path) {
            Ok(surface) => {
                if validate_surface_inventory(&surface).is_ok() {
                    surface_is_multi_command(&surface)
                } else {
                    false
                }
            }
            Err(_) => false,
        }
    } else {
        false
    };

    let man_path = paths.man_page_path(binary_name);
    let evidence = paths.evidence_from_path(&man_path)?;
    if !man_path.is_file() {
        missing_artifacts.push(evidence.path.clone());
        if man_usage_next_action.is_none()
            && help_usage_evidence_state(paths) == HelpUsageEvidenceState::NoUsableHelp
        {
            let plan_content =
                scenarios::load_plan_if_exists(&paths.scenarios_plan_path(), paths.root())
                    .ok()
                    .flatten()
                    .and_then(|plan| serde_json::to_string_pretty(&plan).ok())
                    .unwrap_or_else(|| scenarios::plan_stub(Some(binary_name)));
            *man_usage_next_action = Some(enrich::NextAction::Edit {
                path: "scenarios/plan.json".to_string(),
                content: plan_content,
                reason:
                    "help scenarios produced no usable usage text; update help scenarios or semantics"
                        .to_string(),
            });
        }
        let mut evidence = vec![evidence];
        if multi_command {
            evidence.push(surface_evidence);
        }
        return Ok(enrich::RequirementStatus {
            id: req,
            status: enrich::RequirementState::Unmet,
            reason: "man page missing".to_string(),
            unverified_ids: Vec::new(),
            unverified_count: None,
            verification: None,
            evidence,
            blockers: Vec::new(),
        });
    }

    let meta_path = paths.man_dir().join("meta.json");
    let meta_evidence = paths.evidence_from_path(&meta_path)?;
    let mut requirement_evidence = vec![evidence, meta_evidence.clone(), semantics_evidence];
    if multi_command {
        requirement_evidence.push(surface_evidence);
    }

    let meta = match load_meta(
        &meta_path,
        &meta_evidence,
        blockers,
        &requirement_evidence,
        paths.root(),
    ) {
        Ok(meta) => meta,
        Err(status) => return Ok(status),
    };

    let lock_fresh = lock_status.present && !lock_status.stale;
    let (mut status, mut reason, mut local_blockers) = match meta.as_ref() {
        None => {
            missing_artifacts.push(meta_evidence.path);
            (
                enrich::RequirementState::Unmet,
                "man metadata missing".to_string(),
                Vec::new(),
            )
        }
        Some(meta) if lock_fresh => {
            let lock_hash = lock_status.inputs_hash.as_deref();
            let stale = match (meta.inputs_hash.as_deref(), lock_hash) {
                (Some(meta_hash), Some(lock_hash)) => meta_hash != lock_hash,
                (None, Some(_)) => true,
                _ => false,
            };
            if stale {
                (
                    enrich::RequirementState::Unmet,
                    "man outputs stale relative to lock".to_string(),
                    Vec::new(),
                )
            } else {
                (
                    enrich::RequirementState::Met,
                    "man page present".to_string(),
                    Vec::new(),
                )
            }
        }
        Some(_) => (
            enrich::RequirementState::Met,
            "man page present".to_string(),
            Vec::new(),
        ),
    };

    if status == enrich::RequirementState::Met && lock_fresh {
        let render_summary = meta.as_ref().and_then(|meta| meta.render_summary.as_ref());
        if let Some(summary) = render_summary {
            if !summary.semantics_unmet.is_empty()
                && man_semantics_next_action.is_none()
                && status != enrich::RequirementState::Blocked
            {
                *man_semantics_next_action = Some(enrich::NextAction::Edit {
                    path: "enrich/semantics.json".to_string(),
                    content: semantics::semantics_stub(Some(binary_name)),
                    reason: format!(
                        "update semantics (missing extractions: {})",
                        summary.semantics_unmet.join(", ")
                    ),
                });
            }
            if let Some((new_status, new_reason, blockers)) =
                apply_render_summary(summary, multi_command, &requirement_evidence, paths.root())
            {
                status = new_status;
                reason = new_reason;
                local_blockers = blockers;
            }
        } else {
            status = enrich::RequirementState::Unmet;
            reason = "man render summary missing".to_string();
        }
    }

    blockers.extend(local_blockers.clone());
    Ok(enrich::RequirementStatus {
        id: req,
        status,
        reason,
        unverified_ids: Vec::new(),
        unverified_count: None,
        verification: None,
        evidence: requirement_evidence,
        blockers: local_blockers,
    })
}
