//! Workflow plan step.
//!
//! Planning turns the lock snapshot into deterministic actions and a next step.
use super::EnrichContext;
use crate::cli::PlanArgs;
use crate::docpack::ensure_doc_pack_root;
use crate::enrich;
use crate::scenarios;
use crate::status::{build_status_summary, planned_actions_from_requirements};
use crate::surface;
use anyhow::Result;

/// Run the plan step and write `enrich/plan.out.json`.
pub fn run_plan(args: &PlanArgs) -> Result<()> {
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, false)?;
    let ctx = EnrichContext::load(doc_pack_root)?;
    ctx.require_config()?;
    enrich::validate_config(&ctx.config)?;

    let (lock, lock_status, force_used) = ctx.lock_for_plan(args.force)?;

    let plan_status = enrich::PlanStatus {
        present: true,
        stale: false,
        inputs_hash: Some(lock.inputs_hash.clone()),
        lock_inputs_hash: Some(lock.inputs_hash.clone()),
    };
    let summary = build_status_summary(crate::status::BuildStatusSummaryArgs {
        doc_pack_root: ctx.paths.root(),
        binary_name: ctx.binary_name(),
        config: &ctx.config,
        config_exists: true,
        lock_status,
        plan_status,
        include_full: false,
        force_used,
    })?;
    let enrich::StatusSummary {
        requirements,
        decision,
        decision_reason,
        ..
    } = summary;
    let planned_actions = planned_actions_from_requirements(&requirements);
    let verification_plan = build_verification_plan_summary(&ctx, &ctx.config);

    let lock_inputs_hash = lock.inputs_hash.clone();
    let mut next_action = enrich::NextAction::Command {
        command: format!("bman apply --doc-pack {}", ctx.paths.root().display()),
        reason: "apply the planned actions".to_string(),
        payload: None,
    };
    enrich::normalize_next_action(&mut next_action);
    let plan = enrich::EnrichPlan {
        schema_version: enrich::PLAN_SCHEMA_VERSION,
        generated_at_epoch_ms: enrich::now_epoch_ms()?,
        binary_name: ctx.binary_name.clone(),
        lock,
        requirements,
        planned_actions,
        next_action,
        decision,
        decision_reason,
        force_used,
        verification_plan,
    };
    crate::status::write_plan(ctx.paths.root(), &plan)?;
    if args.verbose {
        eprintln!("wrote {}", ctx.paths.plan_path().display());
    }
    if force_used {
        let now = enrich::now_epoch_ms()?;
        let history_entry = enrich::EnrichHistoryEntry {
            schema_version: enrich::HISTORY_SCHEMA_VERSION,
            started_at_epoch_ms: now,
            finished_at_epoch_ms: now,
            step: "plan".to_string(),
            inputs_hash: Some(lock_inputs_hash),
            outputs_hash: None,
            success: true,
            message: Some("force used".to_string()),
            force_used,
        };
        enrich::append_history(ctx.paths.root(), &history_entry)?;
    }
    Ok(())
}

fn build_verification_plan_summary(
    ctx: &EnrichContext,
    config: &enrich::EnrichConfig,
) -> Option<enrich::VerificationPlanSummary> {
    if !enrich::normalized_requirements(config)
        .iter()
        .any(|req| matches!(req, enrich::RequirementId::Verification))
    {
        return None;
    }
    let plan = scenarios::load_plan(&ctx.paths.scenarios_plan_path(), ctx.paths.root()).ok()?;
    let surface_path = ctx.paths.root().join("inventory").join("surface.json");
    if !surface_path.is_file() {
        return None;
    }
    let surface = surface::load_surface_inventory(&surface_path).ok()?;
    let ledger_entries = scenarios::load_verification_entries(ctx.paths.root());
    let verification_tier = config.verification_tier.as_deref().unwrap_or("accepted");
    crate::status::auto_verification_plan_summary(
        &plan,
        &surface,
        ledger_entries.as_ref(),
        verification_tier,
    )
}
