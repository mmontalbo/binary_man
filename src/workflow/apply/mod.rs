//! Transactional apply workflow for doc pack enrichment.
//!
//! This module executes the core enrichment loop: running scenarios, computing
//! ledgers, and optionally invoking an LM to generate new scenarios. All changes
//! are staged atomically to ensure the doc pack remains consistent.
//!
//! # Why This Exists
//!
//! Doc pack enrichment is a multi-step process that must be:
//! - **Transactional**: Partial failures shouldn't corrupt the pack
//! - **Resumable**: Can continue from where it left off
//! - **Deterministic**: Same inputs produce same outputs
//! - **LM-assisted**: Can leverage language models for semantic tasks
//!
//! # The Apply Loop
//!
//! When `max_cycles > 0`, apply runs an enrichment loop:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                    Apply Cycle N                        │
//! ├─────────────────────────────────────────────────────────┤
//! │  1. Preflight: validate → plan (if stale)               │
//! │  2. Execute planned actions (scenarios, ledgers, etc.)  │
//! │  3. Check status: what's still unverified?              │
//! │  4. If LM configured and items remain:                  │
//! │     - Build decision list with evidence                 │
//! │     - Invoke LM for scenarios/exclusions                │
//! │     - Apply LM responses to plan.json                   │
//! │  5. Repeat until complete or max_cycles reached         │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! # Submodules
//!
//! - [`cleanup`]: Transaction directory cleanup after publish
//! - [`ledgers`]: Writes coverage and verification ledgers
//! - [`rendering`]: Man page rendering and examples report
//!
//! # Transaction Model
//!
//! Apply uses a staging directory (`enrich/txn-<timestamp>/`) for all writes:
//!
//! 1. All outputs written to staging directory
//! 2. On success, atomically published to final locations
//! 3. On failure, staging directory cleaned up
//!
//! This ensures the doc pack is never left in a partially-updated state.
//!
//! # LM Integration
//!
//! When an LM command is configured (via `--lm`, config, or `BMAN_LM_COMMAND`):
//!
//! 1. Status evaluation identifies unverified surface items
//! 2. Decision list built with evidence (man excerpts, scenario outputs)
//! 3. LM invoked with structured prompt expecting JSON response
//! 4. Responses validated and applied to `scenarios/plan.json`
//! 5. Updated scenarios rerun in next cycle
//!
//! The loop terminates when all items are verified, excluded, or max cycles reached.

mod auto_verify;
mod cleanup;
mod judge;
mod ledgers;
mod lm_apply;
mod progress;
mod rendering;

// Judgment types are now in crate::verification_progress

// Re-export for tests
#[cfg(test)]
pub(super) use progress::{
    update_assertion_failed_progress_after_apply, update_outputs_equal_retry_progress_after_apply,
};

use super::lm_client::LmClientConfig;
use super::EnrichContext;
use crate::cli::ApplyArgs;
use crate::docpack::ensure_doc_pack_root;
use crate::enrich;
use crate::output::{write_outputs_staged, WriteOutputsArgs};
use crate::render;
use crate::scenarios;
use crate::semantics;
use crate::staging::publish_staging;
use crate::status::{build_status_summary, plan_status, planned_actions_from_requirements};
use crate::surface::apply_surface_discovery;
use crate::workflow::{run_plan, run_validate, status_summary_for_doc_pack};
use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use auto_verify::{auto_verification_progress, auto_verification_scenarios};
use cleanup::cleanup_txn_dirs;
use judge::{run_judgment_v2, run_post_apply_judgment, JudgmentArgs, JudgmentArgsV2};
use ledgers::{write_ledgers, LedgerArgs};
use lm_apply::{apply_lm_response, invoke_lm_and_apply};
use progress::{
    check_progress, get_excluded_count, get_unverified_count, handle_lm_no_progress_for_targets,
    process_lm_result, CycleProgress,
};
use rendering::{
    load_examples_report_optional, load_surface_for_render, resolve_pack_context_with_cwd,
    scenarios_glob, staged_help_scenario_evidence_available,
};

#[derive(Debug, Clone, Copy, Default)]
struct ApplyPreflightResult {
    ran_validate: bool,
    ran_plan: bool,
}

fn run_apply_preflight<FRefresh, FValidate, FPlan>(
    args: &ApplyArgs,
    lock_status: &enrich::LockStatus,
    plan_state: &enrich::PlanStatus,
    mut refresh: FRefresh,
    mut validate: FValidate,
    mut plan: FPlan,
) -> Result<ApplyPreflightResult>
where
    FRefresh: FnMut() -> Result<()>,
    FValidate: FnMut() -> Result<()>,
    FPlan: FnMut() -> Result<()>,
{
    let mut result = ApplyPreflightResult::default();
    if args.refresh_pack {
        refresh()?;
    }
    if args.refresh_pack || !lock_status.present || lock_status.stale {
        validate()?;
        result.ran_validate = true;
    }
    if result.ran_validate || !plan_state.present || plan_state.stale {
        plan()?;
        result.ran_plan = true;
    }
    Ok(result)
}

pub(crate) fn run_apply(args: &ApplyArgs) -> Result<()> {
    // If max_cycles > 0, run in LM-assisted loop mode
    if args.max_cycles > 0 {
        return run_apply_with_lm_loop(args);
    }

    run_apply_single(args)
}

/// Write an auto-exclude overlay file.
fn write_auto_exclude(
    paths: &enrich::DocPackPaths,
    path: &str,
    content: &str,
    verbose: bool,
) -> Result<()> {
    let overlay_path = paths.root().join(path);
    if let Some(parent) = overlay_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&overlay_path, content)?;
    if verbose {
        eprintln!("apply: wrote exclusion to {}", overlay_path.display());
    }
    Ok(())
}

/// Result of a single LM cycle iteration
enum LmCycleResult {
    /// Continue to next cycle
    Continue,
    /// Continue with updated state
    ContinueWithUpdates {
        rerun_scenario_ids: Vec<String>,
        processed_surfaces: Vec<String>,
        increment_no_progress: bool,
    },
    /// Stop the loop (no LM configured for edit action)
    Stop,
}

/// Process a single LM cycle given the summary and payload.
#[allow(clippy::too_many_arguments)]
fn run_lm_cycle(
    doc_pack_root: &Path,
    paths: &enrich::DocPackPaths,
    lm_config: Option<&LmClientConfig>,
    summary: &enrich::StatusSummary,
    payload: &enrich::BehaviorNextActionPayload,
    lm_processed_surfaces: &mut BTreeSet<String>,
    max_lm_failures: usize,
    max_lm_no_progress: usize,
    verbose: bool,
) -> Result<LmCycleResult> {
    let current_targets: BTreeSet<String> = payload
        .target_ids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // Find surfaces that LM has previously processed but are still unverified
    let still_unverified_after_lm: Vec<String> = lm_processed_surfaces
        .intersection(&current_targets)
        .cloned()
        .collect();

    if !still_unverified_after_lm.is_empty() {
        let auto_excluded = handle_lm_no_progress_for_targets(
            paths,
            &still_unverified_after_lm,
            max_lm_no_progress,
            verbose,
        );
        if auto_excluded > 0 {
            if verbose {
                eprintln!(
                    "apply: auto-excluded {} surface(s) after repeated LM targeting without progress",
                    auto_excluded
                );
            }
            for s in &still_unverified_after_lm {
                lm_processed_surfaces.remove(s);
            }
            return Ok(LmCycleResult::Continue);
        }
    }

    // Check if LM is configured
    let lm_config = match lm_config {
        Some(cfg) => cfg,
        None => {
            if verbose {
                eprintln!("apply: edit action requires LM, but no LM configured, stopping");
            }
            return Ok(LmCycleResult::Stop);
        }
    };

    if verbose {
        eprintln!(
            "apply: invoking LM for {} targets (reason: {})",
            payload.target_ids.len(),
            payload.reason_code.as_deref().unwrap_or("unknown")
        );
    }

    let lm_result = invoke_lm_and_apply(doc_pack_root, lm_config, summary, payload, verbose);
    let processing = process_lm_result(
        paths,
        lm_result,
        &payload.target_ids,
        &current_targets,
        max_lm_failures,
        verbose,
    );

    Ok(LmCycleResult::ContinueWithUpdates {
        rerun_scenario_ids: processing.updated_scenario_ids,
        processed_surfaces: processing.processed_surfaces,
        increment_no_progress: processing.increment_no_progress,
    })
}

/// Run apply in a loop with LM assistance.
///
/// This is the main enrichment loop that:
/// 1. Runs a single apply (scenarios, ledgers, etc.)
/// 2. Checks status to see what's still unverified
/// 3. If LM is configured and can help, invokes LM
/// 4. Applies LM responses and repeats
#[allow(clippy::cognitive_complexity)]
fn run_apply_with_lm_loop(args: &ApplyArgs) -> Result<()> {
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, false)?;

    // Resolve LM command: CLI flag > config > env var
    let ctx = EnrichContext::load(doc_pack_root.clone())?;
    let lm_command = args
        .lm
        .clone()
        .or_else(|| enrich::resolve_lm_command(&ctx.config));

    let lm_config = lm_command.as_ref().map(|cmd| LmClientConfig {
        command: cmd.clone(),
    });

    if args.verbose {
        if let Some(ref cmd) = lm_command {
            eprintln!("apply: LM command configured: {}", cmd);
        } else {
            eprintln!("apply: no LM configured (set lm_command in config or BMAN_LM_COMMAND env)");
        }
    }

    let mut cycle = 0;
    let mut last_unverified_count: Option<usize> = None;
    let mut no_progress_count = 0;
    const MAX_NO_PROGRESS: usize = 3;
    const MAX_LM_FAILURES_PER_SURFACE: usize = 2;
    const MAX_LM_NO_PROGRESS_PER_SURFACE: usize = 3;
    let mut rerun_scenario_ids: Vec<String> = Vec::new();
    let paths = enrich::DocPackPaths::new(doc_pack_root.clone());
    // Track surfaces that LM has worked on - only increment no-progress after LM processes them
    let mut lm_processed_surfaces: BTreeSet<String> = BTreeSet::new();
    // Track last reason kind to reset no_progress_count on reason kind rotation
    let mut last_reason_kind: Option<String> = None;

    loop {
        cycle += 1;
        if args.verbose {
            eprintln!("\n=== Apply cycle {}/{} ===", cycle, args.max_cycles);
        }

        // Combine user-specified reruns with LM-updated scenarios
        let mut cycle_rerun_ids = args.rerun_scenario_id.clone();
        cycle_rerun_ids.append(&mut rerun_scenario_ids);

        // Run single apply (with max_cycles=0 to avoid recursion)
        let single_apply_args = ApplyArgs {
            doc_pack: args.doc_pack.clone(),
            refresh_pack: args.refresh_pack,
            verbose: args.verbose,
            rerun_all: args.rerun_all,
            rerun_failed: args.rerun_failed,
            rerun_scenario_id: cycle_rerun_ids,
            lm_response: args.lm_response.clone(),
            max_cycles: 0,
            lm: args.lm.clone(),
            explore: args.explore.clone(),
            context: args.context.clone(),
        };
        run_apply_single(&single_apply_args)?;

        // Run post-apply judgment (V2: outputs scenario specs directly)
        if let Some(ref lm_cmd) = lm_command {
            let judgment_args = JudgmentArgsV2 {
                paths: &paths,
                lm_command: lm_cmd,
                verbose: args.verbose,
            };
            match run_judgment_v2(&judgment_args) {
                Ok(judgment_result) => {
                    if args.verbose && !judgment_result.passed.is_empty() {
                        eprintln!(
                            "apply: {} scenario(s) passed judgment",
                            judgment_result.passed.len()
                        );
                    }
                    if !judgment_result.scenarios_to_run.is_empty() {
                        if args.verbose {
                            eprintln!(
                                "apply: judgment generated {} scenario(s) to run",
                                judgment_result.scenarios_to_run.len()
                            );
                        }
                        // Append scenarios to plan.json
                        match append_judgment_scenarios(
                            &paths,
                            &judgment_result.scenarios_to_run,
                            args.verbose,
                        ) {
                            Ok(ids) => {
                                // Add to rerun list for next cycle
                                rerun_scenario_ids.extend(ids);
                            }
                            Err(e) => {
                                if args.verbose {
                                    eprintln!("apply: failed to append judgment scenarios: {}", e);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    if args.verbose {
                        eprintln!("apply: judgment error (non-fatal): {}", e);
                    }
                    // Fall back to V1 judgment
                    let judgment_args_v1 = JudgmentArgs {
                        paths: &paths,
                        lm_command: lm_cmd,
                        verbose: args.verbose,
                    };
                    let _ = run_post_apply_judgment(&judgment_args_v1);
                }
            }
        }

        // Check status
        let computation = status_summary_for_doc_pack(doc_pack_root.clone(), false, false)?;
        let summary = &computation.summary;

        // Check if complete
        if summary.decision == enrich::Decision::Complete {
            if args.verbose {
                eprintln!("apply: verification complete!");
            }
            break;
        }

        // Get unverified count and check progress
        let unverified_count = get_unverified_count(summary);
        if args.verbose {
            eprintln!("apply: {} options still unverified", unverified_count);
        }

        match check_progress(
            unverified_count,
            last_unverified_count,
            no_progress_count,
            MAX_NO_PROGRESS,
        ) {
            CycleProgress::Advanced => {
                no_progress_count = 0;
            }
            CycleProgress::Stalled { count } => {
                no_progress_count = count;
                if args.verbose {
                    eprintln!("apply: no progress ({}/{})", count, MAX_NO_PROGRESS);
                }
            }
            CycleProgress::HitLimit { count } => {
                eprintln!("apply: stopping after {} cycles with no progress", count);
                break;
            }
        }
        last_unverified_count = Some(unverified_count);

        // Check if we've hit max cycles
        if cycle >= args.max_cycles {
            if args.verbose {
                eprintln!("apply: reached max cycles ({})", args.max_cycles);
            }
            break;
        }

        // Handle AutoExclude action type
        if let enrich::NextAction::AutoExclude {
            path,
            content,
            reason,
            target_ids,
            evidence,
        } = &summary.next_action
        {
            if args.verbose {
                eprintln!(
                    "apply: auto-excluding {} surface(s): {}",
                    target_ids.len(),
                    reason
                );
                eprintln!(
                    "apply: evidence: reason_code={}, retry_count={}",
                    evidence.reason_code, evidence.retry_count
                );
            }
            write_auto_exclude(&paths, path, content, args.verbose)?;
            continue;
        }

        // Extract action kind and payload
        let (action_kind, payload) = match &summary.next_action {
            enrich::NextAction::Edit { payload, .. } => ("edit", payload.clone()),
            enrich::NextAction::Command { payload, .. } => ("command", payload.clone()),
            enrich::NextAction::AutoExclude { .. } => unreachable!("handled above"),
        };

        // Check for payload and early-exit conditions
        let Some(payload) = payload else {
            if args.verbose {
                eprintln!(
                    "apply: next action is {} with no payload, continuing",
                    action_kind
                );
            }
            continue;
        };

        if payload.target_ids.is_empty() {
            if args.verbose {
                eprintln!("apply: no target IDs in payload, continuing");
            }
            continue;
        }

        // Reset no_progress_count when reason kind changes (enables reason kind rotation)
        let current_reason_kind = payload.reason_code.clone();
        if current_reason_kind != last_reason_kind {
            if args.verbose && last_reason_kind.is_some() && no_progress_count > 0 {
                eprintln!(
                    "apply: reason kind changed from {:?} to {:?}, resetting no-progress counter",
                    last_reason_kind, current_reason_kind
                );
            }
            no_progress_count = 0;
            last_reason_kind = current_reason_kind;
        }

        // For command actions without LM, just continue
        if action_kind == "command" && lm_config.is_none() {
            continue;
        }

        // Run LM cycle with payload
        match run_lm_cycle(
            &doc_pack_root,
            &paths,
            lm_config.as_ref(),
            summary,
            &payload,
            &mut lm_processed_surfaces,
            MAX_LM_FAILURES_PER_SURFACE,
            MAX_LM_NO_PROGRESS_PER_SURFACE,
            args.verbose,
        )? {
            LmCycleResult::Continue => continue,
            LmCycleResult::Stop => break,
            LmCycleResult::ContinueWithUpdates {
                rerun_scenario_ids: new_ids,
                processed_surfaces,
                increment_no_progress,
            } => {
                if increment_no_progress {
                    no_progress_count += 1;
                }
                for surface in processed_surfaces {
                    lm_processed_surfaces.insert(surface);
                }
                rerun_scenario_ids.extend(new_ids);
            }
        }
    }

    // Final status
    let final_computation = status_summary_for_doc_pack(doc_pack_root.clone(), false, false)?;
    let final_summary = &final_computation.summary;
    let unverified = get_unverified_count(final_summary);
    let excluded = get_excluded_count(final_summary);

    // Record learned hints from successful verifications
    if let Err(e) = record_learned_hints(&paths, args.verbose) {
        if args.verbose {
            eprintln!("apply: warning: failed to record hints: {}", e);
        }
    }

    eprintln!(
        "apply: finished after {} cycles ({} unverified, {} excluded)",
        cycle, unverified, excluded
    );

    Ok(())
}

/// Internal single-apply without the loop.
fn run_apply_single(args: &ApplyArgs) -> Result<()> {
    // Handle LM response if provided
    if let Some(lm_response_path) = &args.lm_response {
        apply_lm_response(&args.doc_pack, lm_response_path)?;
    }

    run_apply_core(args)
}

/// Core apply logic (extracted from original run_apply).
fn run_apply_core(args: &ApplyArgs) -> Result<()> {
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, false)?;
    let mut ctx = EnrichContext::load(doc_pack_root)?;
    ctx.require_config()?;
    enrich::validate_config(&ctx.config)?;
    let manifest = ctx.manifest.clone();
    let mut lock_status = ctx.lock_status.clone();
    let plan_state = plan_status(ctx.lock.as_ref(), ctx.plan.as_ref());
    let preflight = run_apply_preflight(
        args,
        &lock_status,
        &plan_state,
        || {
            // Pack refresh is no longer needed - direct scenario execution
            Ok(())
        },
        || {
            let validate_args = crate::cli::ValidateArgs {
                doc_pack: ctx.paths.root().to_path_buf(),
                verbose: args.verbose,
            };
            run_validate(&validate_args)
        },
        || {
            let plan_args = crate::cli::PlanArgs {
                doc_pack: ctx.paths.root().to_path_buf(),
                force: false,
                verbose: args.verbose,
            };
            run_plan(&plan_args)
        },
    )?;

    if preflight.ran_validate || preflight.ran_plan {
        ctx = EnrichContext::load(ctx.paths.root().to_path_buf())?;
        lock_status = ctx.lock_status.clone();
    }

    let lock = ctx
        .lock
        .clone()
        .ok_or_else(|| anyhow!("missing lock at {}", ctx.paths.lock_path().display()))?;
    let plan = ctx
        .plan
        .clone()
        .ok_or_else(|| anyhow!("missing plan at {}", ctx.paths.plan_path().display()))?;
    let force_used = false;
    let initial_plan_state = plan_status(Some(&lock), Some(&plan));
    let initial_summary = build_status_summary(crate::status::BuildStatusSummaryArgs {
        doc_pack_root: ctx.paths.root(),
        binary_name: ctx.binary_name(),
        config: &ctx.config,
        config_exists: true,
        lock_status: lock_status.clone(),
        plan_status: initial_plan_state,
        include_full: false,
        force_used,
    })?;
    let planned_actions = planned_actions_from_requirements(&initial_summary.requirements);

    let binary_name = manifest.as_ref().map(|m| m.binary_name.clone());

    let started_at_epoch_ms = enrich::now_epoch_ms()?;
    let txn_id = format!("{started_at_epoch_ms}");
    let staging_root = ctx.paths.txn_staging_root(&txn_id);
    fs::create_dir_all(&staging_root).context("create staging dir")?;

    let apply_inputs = ApplyInputs {
        ctx: &ctx,
        planned_actions: planned_actions.as_slice(),
        plan: &plan,
        manifest: manifest.as_ref(),
        binary_name: binary_name.as_deref(),
        staging_root: &staging_root,
        args,
    };
    let apply_result = apply_plan_actions(&apply_inputs);

    let finished_at_epoch_ms = enrich::now_epoch_ms()?;
    let ApplyPlanActionsResult {
        published_paths,
        outputs_hash,
        executed_forced_rerun_scenario_ids,
        verification_entries,
        skipped_scenarios,
    } = match apply_result {
        Ok(result) => result,
        Err(err) => {
            let history_entry = enrich::EnrichHistoryEntry {
                schema_version: enrich::HISTORY_SCHEMA_VERSION,
                started_at_epoch_ms,
                finished_at_epoch_ms,
                step: "apply".to_string(),
                inputs_hash: Some(lock.inputs_hash),
                outputs_hash: None,
                success: false,
                message: Some(err.to_string()),
                force_used,
            };
            let _ = enrich::append_history(ctx.paths.root(), &history_entry);
            return Err(err);
        }
    };

    cleanup_txn_dirs(&ctx.paths, &txn_id, args.verbose);

    if let Some(ref entries) = verification_entries {
        if let Err(err) = progress::update_outputs_equal_retry_progress_after_apply(
            &ctx.paths,
            &executed_forced_rerun_scenario_ids,
            entries,
        ) {
            eprintln!("warning: failed to persist outputs_equal verification progress: {err}");
        }

        if let Err(err) = progress::update_assertion_failed_progress_after_apply(
            &ctx.paths,
            &executed_forced_rerun_scenario_ids,
            entries,
        ) {
            eprintln!("warning: failed to persist assertion_failed verification progress: {err}");
        }
    }

    let summary = if planned_actions.is_empty() && !args.refresh_pack && published_paths.is_empty()
    {
        initial_summary
    } else {
        let plan_state = plan_status(Some(&lock), Some(&plan));
        build_status_summary(crate::status::BuildStatusSummaryArgs {
            doc_pack_root: ctx.paths.root(),
            binary_name: binary_name.as_deref(),
            config: &ctx.config,
            config_exists: true,
            lock_status,
            plan_status: plan_state,
            include_full: false,
            force_used,
        })?
    };

    let last_run = enrich::EnrichRunSummary {
        step: "apply".to_string(),
        started_at_epoch_ms,
        finished_at_epoch_ms,
        success: true,
        inputs_hash: Some(lock.inputs_hash.clone()),
        outputs_hash,
        message: None,
    };

    let enrich::StatusSummary {
        requirements,
        blockers,
        missing_artifacts,
        decision,
        decision_reason,
        next_action,
        ..
    } = summary;
    let mut next_action = next_action;
    enrich::normalize_next_action(&mut next_action);

    let report = enrich::EnrichReport {
        schema_version: enrich::REPORT_SCHEMA_VERSION,
        generated_at_epoch_ms: finished_at_epoch_ms,
        binary_name: binary_name.clone(),
        lock: Some(lock),
        requirements,
        blockers,
        missing_artifacts,
        decision,
        decision_reason,
        next_action,
        last_run: Some(last_run.clone()),
        force_used,
        skipped_scenarios,
    };
    enrich::write_report(ctx.paths.root(), &report)?;

    let enrich::EnrichRunSummary {
        inputs_hash,
        outputs_hash,
        ..
    } = last_run;

    let history_entry = enrich::EnrichHistoryEntry {
        schema_version: enrich::HISTORY_SCHEMA_VERSION,
        started_at_epoch_ms,
        finished_at_epoch_ms,
        step: "apply".to_string(),
        inputs_hash,
        outputs_hash,
        success: true,
        message: None,
        force_used,
    };
    enrich::append_history(ctx.paths.root(), &history_entry)?;

    if args.verbose {
        eprintln!(
            "apply completed; wrote {}",
            ctx.paths.report_path().display()
        );
    }
    Ok(())
}

struct ApplyInputs<'a> {
    ctx: &'a EnrichContext,
    planned_actions: &'a [enrich::PlannedAction],
    plan: &'a enrich::EnrichPlan,
    manifest: Option<&'a crate::pack::PackManifest>,
    binary_name: Option<&'a str>,
    staging_root: &'a Path,
    args: &'a ApplyArgs,
}

#[derive(Debug, Default)]
struct ApplyPlanActionsResult {
    published_paths: Vec<PathBuf>,
    outputs_hash: Option<String>,
    executed_forced_rerun_scenario_ids: Vec<String>,
    verification_entries: Option<BTreeMap<String, scenarios::VerificationEntry>>,
    skipped_scenarios: Vec<enrich::SkippedScenario>,
}

fn apply_plan_actions(inputs: &ApplyInputs<'_>) -> Result<ApplyPlanActionsResult> {
    let ctx = inputs.ctx;
    let actions = inputs.planned_actions;
    let plan = inputs.plan;
    let manifest = inputs.manifest;
    let binary_name = inputs.binary_name;
    let staging_root = inputs.staging_root;
    let args = inputs.args;
    let (wants_surface, wants_coverage_ledger, wants_scenarios, wants_render) = actions
        .iter()
        .fold((false, false, false, false), |flags, action| match action {
            enrich::PlannedAction::SurfaceDiscovery => (true, flags.1, flags.2, flags.3),
            enrich::PlannedAction::CoverageLedger => (flags.0, true, flags.2, flags.3),
            enrich::PlannedAction::ScenarioRuns => (flags.0, flags.1, true, flags.3),
            enrich::PlannedAction::RenderManPage => (flags.0, flags.1, flags.2, true),
        });

    let requirements = enrich::normalized_requirements(&ctx.config);
    let emit_coverage_ledger = requirements
        .iter()
        .any(|req| matches!(req, enrich::RequirementId::CoverageLedger))
        && (wants_coverage_ledger || wants_scenarios || wants_surface);
    let emit_verification_ledger = requirements
        .iter()
        .any(|req| matches!(req, enrich::RequirementId::Verification))
        && (wants_scenarios || wants_surface);

    let pack_root = ctx.paths.pack_root();
    let pack_root_exists = pack_root.is_dir();
    let requires_pack = wants_scenarios || wants_render;
    if requires_pack && !pack_root_exists {
        return Err(anyhow!(
            "pack root missing at {} (run `bman {} --doc-pack {}` first)",
            pack_root.display(),
            binary_name.unwrap_or("<binary>"),
            ctx.paths.root().display()
        ));
    }

    let pack_root = if pack_root_exists {
        pack_root
            .canonicalize()
            .with_context(|| format!("resolve pack root {}", pack_root.display()))?
    } else {
        pack_root
    };

    let mut examples_report = None;
    let mut executed_forced_rerun_scenario_ids = Vec::new();
    let mut skipped_scenarios = Vec::new();
    let scenarios_path = ctx.paths.scenarios_plan_path();

    if wants_surface {
        apply_surface_discovery(&crate::surface::SurfaceDiscoveryArgs {
            doc_pack_root: ctx.paths.root(),
            staging_root,
            inputs_hash: Some(plan.lock.inputs_hash.as_str()),
            manifest,
            verbose: args.verbose,
            explore_hints: &args.explore,
            scope_context: &args.context,
        })?;

        // Prereq inference now happens via LM actions (define_prereq, set_prereq, exclude_from_verify)
        // during behavior response processing, not as a separate LM call.
    }

    if wants_scenarios {
        let binary_name =
            binary_name.ok_or_else(|| anyhow!("binary name unavailable; manifest missing"))?;
        let run_mode = if args.rerun_all {
            scenarios::ScenarioRunMode::RerunAll
        } else if args.rerun_failed {
            scenarios::ScenarioRunMode::RerunFailed
        } else {
            scenarios::ScenarioRunMode::Default
        };
        let forced_rerun_scenario_ids = normalize_rerun_scenario_ids(&args.rerun_scenario_id);
        let verification_tier = ctx
            .config
            .verification_tier
            .as_deref()
            .unwrap_or("accepted");
        let mut extra_scenarios = Vec::new();
        let mut auto_run_limit = None;
        let mut auto_progress = None;
        let plan = scenarios::load_plan(&scenarios_path, ctx.paths.root())?;
        if let Some(batch) = auto_verification_scenarios(
            &plan,
            ctx.paths.root(),
            staging_root,
            args.verbose,
            verification_tier,
            &args.context,
        )? {
            auto_run_limit = Some(batch.max_new_runs_per_apply);
            auto_progress = Some(auto_verification_progress(
                inputs.plan,
                &plan,
                &ctx.config,
                &batch,
                &ctx.paths,
            ));
            extra_scenarios.extend(batch.scenarios);
        }
        let binary_path = manifest
            .map(|m| m.binary_path.as_str())
            .ok_or_else(|| anyhow!("binary path unavailable; manifest missing"))?;
        let run_result = scenarios::run_scenarios(&scenarios::RunScenariosArgs {
            pack_root: &pack_root,
            run_root: ctx.paths.root(),
            binary_name,
            binary_path,
            scenarios_path: &scenarios_path,
            display_root: Some(ctx.paths.root()),
            staging_root: Some(staging_root),
            kind_filter: None,
            run_mode,
            forced_rerun_scenario_ids,
            extra_scenarios,
            auto_run_limit,
            auto_progress,
            verbose: args.verbose,
        })?;
        executed_forced_rerun_scenario_ids = run_result.executed_forced_rerun_scenario_ids;
        skipped_scenarios = run_result.skipped_scenarios;
        examples_report = Some(run_result.report);
    } else if wants_render {
        examples_report = load_examples_report_optional(&ctx.paths)?;
    }
    examples_report = examples_report.and_then(scenarios::publishable_examples_report);

    let scenarios_glob = wants_render.then(|| {
        let scenarios_root = if staged_help_scenario_evidence_available(staging_root) {
            staging_root
        } else {
            ctx.paths.root()
        };
        scenarios_glob(scenarios_root)
    });
    let context = if wants_render {
        let scenarios_glob = scenarios_glob
            .as_deref()
            .ok_or_else(|| anyhow!("scenarios_glob required for render"))?;
        Some(resolve_pack_context_with_cwd(
            &pack_root,
            ctx.paths.root(),
            &pack_root,
            &ctx.config.usage_lens_template,
            scenarios_glob,
        )?)
    } else {
        None
    };
    let semantics = wants_render
        .then(|| semantics::load_semantics(ctx.paths.root()))
        .transpose()?;
    let surface_for_render = if wants_render {
        load_surface_for_render(staging_root, &ctx.paths)?
    } else {
        None
    };

    if wants_render {
        let context = context
            .as_ref()
            .ok_or_else(|| anyhow!("pack context required for man rendering"))?;
        let semantics = semantics
            .as_ref()
            .ok_or_else(|| anyhow!("semantics required for man rendering"))?;
        let rendered = render::render_man_page(
            context,
            semantics,
            examples_report.as_ref(),
            surface_for_render.as_ref(),
        )?;
        write_outputs_staged(&WriteOutputsArgs {
            staging_root,
            doc_pack_root: ctx.paths.root(),
            context,
            pack_root: &pack_root,
            inputs_hash: Some(plan.lock.inputs_hash.as_str()),
            man_page: Some(&rendered.man_page),
            render_summary: Some(&rendered.summary),
            examples_report: examples_report.as_ref(),
        })?;
    }

    let ledger_result = write_ledgers(&LedgerArgs {
        paths: &ctx.paths,
        staging_root,
        binary_name,
        scenarios_path: &scenarios_path,
        emit_coverage: emit_coverage_ledger,
        compute_verification: emit_verification_ledger,
    })?;

    let published_paths = publish_staging(staging_root, ctx.paths.root())?;
    let outputs_hash = (!published_paths.is_empty())
        .then(|| enrich::hash_paths(ctx.paths.root(), &published_paths))
        .transpose()?;

    Ok(ApplyPlanActionsResult {
        published_paths,
        outputs_hash,
        executed_forced_rerun_scenario_ids,
        verification_entries: ledger_result.verification_entries,
        skipped_scenarios,
    })
}

fn normalize_rerun_scenario_ids(raw: &[String]) -> Vec<String> {
    let mut ids = raw
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

/// Append judgment-generated scenarios to plan.json.
///
/// Returns the list of scenario IDs that were added (for forced rerun).
fn append_judgment_scenarios(
    paths: &enrich::DocPackPaths,
    scenarios_to_add: &[scenarios::ScenarioSpec],
    verbose: bool,
) -> Result<Vec<String>> {
    if scenarios_to_add.is_empty() {
        return Ok(Vec::new());
    }

    let plan_path = paths.scenarios_plan_path();
    let mut plan = scenarios::load_plan(&plan_path, paths.root())?;

    let mut added_ids = Vec::new();
    for scenario in scenarios_to_add {
        // Check if scenario already exists (by ID)
        if let Some(existing) = plan.scenarios.iter_mut().find(|s| s.id == scenario.id) {
            // Update existing scenario
            *existing = scenario.clone();
            if verbose {
                eprintln!("  judgment: updated scenario {}", scenario.id);
            }
        } else {
            // Add new scenario
            plan.scenarios.push(scenario.clone());
            if verbose {
                eprintln!("  judgment: added scenario {}", scenario.id);
            }
        }
        added_ids.push(scenario.id.clone());
    }

    // Write updated plan
    let plan_json = serde_json::to_string_pretty(&plan)?;
    fs::write(&plan_path, plan_json.as_bytes())?;

    Ok(added_ids)
}

/// Record learned hints from successful verifications.
///
/// Extracts working argvs from scenarios that produced `delta_seen` and
/// stores them in `enrich/learned_hints.json` for use in future LM prompts.
fn record_learned_hints(paths: &enrich::DocPackPaths, verbose: bool) -> Result<()> {
    // Load verification cache
    let cache_path = paths.root().join("inventory/verification_cache.json");
    if !cache_path.exists() {
        return Ok(());
    }

    #[derive(serde::Deserialize)]
    struct Cache {
        ledger: scenarios::VerificationLedger,
    }

    let content = fs::read_to_string(&cache_path)
        .with_context(|| format!("read verification cache: {}", cache_path.display()))?;
    let cache: Cache = serde_json::from_str(&content)
        .with_context(|| format!("parse verification cache: {}", cache_path.display()))?;

    // Load scenario plan to get argvs
    let scenarios_path = paths.scenarios_plan_path();
    if !scenarios_path.exists() {
        return Ok(());
    }
    let plan = scenarios::load_plan(&scenarios_path, paths.root())?;

    // Build surface_id -> argv map from behavior scenarios
    let mut scenario_argvs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for scenario in &plan.scenarios {
        if scenario.covers.is_empty() {
            continue;
        }
        // Only use scenarios that are likely behavior scenarios (have argv)
        if scenario.argv.is_empty() {
            continue;
        }
        // Use first covered surface as key
        let surface_id = &scenario.covers[0];
        // Prefer scenarios with "verify_" prefix as they're behavior scenarios
        if scenario.id.starts_with("verify_") || !scenario_argvs.contains_key(surface_id) {
            scenario_argvs.insert(surface_id.clone(), scenario.argv.clone());
        }
    }

    // Load existing hints
    let hints_path = paths.learned_hints_path();
    let mut hints = enrich::load_learned_hints(&hints_path)?;

    // Record working argvs for delta_seen entries
    // Always overwrite - verified argv is authoritative
    let mut new_hints = 0;
    for entry in &cache.ledger.entries {
        if entry.delta_outcome.as_deref() == Some("delta_seen") {
            if let Some(argv) = scenario_argvs.get(&entry.surface_id) {
                let existing = hints.working_argvs.get(&entry.surface_id);
                if existing != Some(argv) {
                    hints.record_working_argv(&entry.surface_id, argv.clone());
                    new_hints += 1;
                }
            }
        }
    }

    // Only write if we have hints
    if !hints.is_empty() && new_hints > 0 {
        enrich::write_learned_hints(&hints_path, &hints)?;
        if verbose {
            eprintln!(
                "apply: recorded {} new hints ({} total working argvs)",
                new_hints,
                hints.working_argv_count()
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;
