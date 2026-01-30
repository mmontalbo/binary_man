//! Workflow plan step.
//!
//! Planning turns the lock snapshot into deterministic actions and a next step.
use super::EnrichContext;
use crate::cli::PlanArgs;
use crate::docpack::ensure_doc_pack_root;
use crate::enrich;
use crate::status::{build_status_summary, planned_actions_from_requirements};
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

    let lock_inputs_hash = lock.inputs_hash.clone();
    let plan = enrich::EnrichPlan {
        schema_version: enrich::PLAN_SCHEMA_VERSION,
        generated_at_epoch_ms: enrich::now_epoch_ms()?,
        binary_name: ctx.binary_name.clone(),
        lock,
        requirements,
        planned_actions,
        next_action: enrich::NextAction::Command {
            command: format!("bman apply --doc-pack {}", ctx.paths.root().display()),
            reason: "apply the planned actions".to_string(),
        },
        decision,
        decision_reason,
        force_used,
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
