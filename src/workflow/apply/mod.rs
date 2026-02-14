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
//! - [`pack`]: Pack refresh via binary_lens
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
mod ledgers;
mod lm_apply;
mod pack;
mod progress;
mod rendering;

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
use crate::surface::{self, apply_surface_discovery};
use crate::util::resolve_flake_ref;
use crate::verification_progress::{
    load_verification_progress, outputs_equal_delta_signature, write_verification_progress,
};
use crate::workflow::{run_plan, run_validate, status_summary_for_doc_pack};
use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use auto_verify::{auto_verification_progress, auto_verification_scenarios};
use cleanup::cleanup_txn_dirs;
use ledgers::{write_ledgers, LedgerArgs};
use lm_apply::{apply_lm_response, invoke_lm_and_apply};
use pack::refresh_pack_if_needed;
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

/// Run apply in a loop with LM assistance.
///
/// This is the main enrichment loop that:
/// 1. Runs a single apply (scenarios, ledgers, etc.)
/// 2. Checks status to see what's still unverified
/// 3. If LM is configured and can help, invokes LM
/// 4. Applies LM responses and repeats
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
            lens_flake: args.lens_flake.clone(),
            lm_response: args.lm_response.clone(),
            max_cycles: 0,
            lm: args.lm.clone(),
            explore: args.explore.clone(),
            context: args.context.clone(),
        };
        run_apply_single(&single_apply_args)?;

        // Check status
        let computation =
            status_summary_for_doc_pack(doc_pack_root.clone(), false, false)?;
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

        match check_progress(unverified_count, last_unverified_count, no_progress_count, MAX_NO_PROGRESS) {
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

        // Check next action type
        let (action_kind, payload) = match &summary.next_action {
            enrich::NextAction::Edit { payload, .. } => ("edit", payload.clone()),
            enrich::NextAction::Command { payload, .. } => ("command", payload.clone()),
        };

        // If next action is a command with no LM payload, just continue the loop
        // (it will run more scenarios on the next cycle)
        let Some(payload) = payload else {
            if args.verbose {
                eprintln!("apply: next action is {} with no payload, continuing", action_kind);
            }
            continue;
        };

        if payload.target_ids.is_empty() {
            if args.verbose {
                eprintln!("apply: no target IDs in payload, continuing");
            }
            continue;
        }

        // Track surfaces that have been processed by LM but remain unverified
        // Only increment counter for surfaces that LM has ALREADY worked on
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
                &paths,
                &still_unverified_after_lm,
                MAX_LM_NO_PROGRESS_PER_SURFACE,
                args.verbose,
            );
            if auto_excluded > 0 {
                if args.verbose {
                    eprintln!(
                        "apply: auto-excluded {} surface(s) after repeated LM targeting without progress",
                        auto_excluded
                    );
                }
                // Remove excluded surfaces from tracking
                for s in &still_unverified_after_lm {
                    lm_processed_surfaces.remove(s);
                }
                // Recalculate status and continue to next cycle
                continue;
            }
        }

        // Check if LM is configured for edit actions
        let lm_config = match &lm_config {
            Some(cfg) => cfg,
            None => {
                if action_kind == "edit" {
                    if args.verbose {
                        eprintln!("apply: edit action requires LM, but no LM configured, stopping");
                    }
                    break;
                }
                // For command actions, just continue to run more scenarios
                continue;
            }
        };

        // Invoke LM for actions with payload
        if args.verbose {
            eprintln!(
                "apply: invoking LM for {} targets (reason: {})",
                payload.target_ids.len(),
                payload.reason_code.as_deref().unwrap_or("unknown")
            );
        }

        let lm_result = invoke_lm_and_apply(&doc_pack_root, lm_config, summary, &payload, args.verbose);
        let processing = process_lm_result(
            &paths,
            lm_result,
            &payload.target_ids,
            &current_targets,
            MAX_LM_FAILURES_PER_SURFACE,
            args.verbose,
        );

        if processing.increment_no_progress {
            no_progress_count += 1;
        }
        for surface in processing.processed_surfaces {
            lm_processed_surfaces.insert(surface);
        }
        rerun_scenario_ids.extend(processing.updated_scenario_ids);
    }

    // Final status
    let final_computation = status_summary_for_doc_pack(doc_pack_root, false, false)?;
    let final_summary = &final_computation.summary;
    let unverified = get_unverified_count(final_summary);
    let excluded = get_excluded_count(final_summary);

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
    let lens_flake = resolve_flake_ref(&args.lens_flake)?;
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, false)?;
    let mut ctx = EnrichContext::load(doc_pack_root)?;
    ctx.require_config()?;
    enrich::validate_config(&ctx.config)?;
    let mut manifest = ctx.manifest.clone();
    let mut lock_status = ctx.lock_status.clone();
    let plan_state = plan_status(ctx.lock.as_ref(), ctx.plan.as_ref());
    let preflight = run_apply_preflight(
        args,
        &lock_status,
        &plan_state,
        || {
            manifest = refresh_pack_if_needed(&ctx, manifest.as_ref(), &lens_flake)?;
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
        lens_flake: &lens_flake,
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
        if let Err(err) = update_outputs_equal_retry_progress_after_apply(
            &ctx.paths,
            &executed_forced_rerun_scenario_ids,
            entries,
        ) {
            eprintln!("warning: failed to persist outputs_equal verification progress: {err}");
        }

        if let Err(err) = update_assertion_failed_progress_after_apply(
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
    lens_flake: &'a str,
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
}

fn apply_plan_actions(inputs: &ApplyInputs<'_>) -> Result<ApplyPlanActionsResult> {
    let ctx = inputs.ctx;
    let actions = inputs.planned_actions;
    let plan = inputs.plan;
    let manifest = inputs.manifest;
    let lens_flake = inputs.lens_flake;
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
    let scenarios_path = ctx.paths.scenarios_plan_path();

    if wants_surface {
        apply_surface_discovery(&crate::surface::SurfaceDiscoveryArgs {
            doc_pack_root: ctx.paths.root(),
            staging_root,
            inputs_hash: Some(plan.lock.inputs_hash.as_str()),
            manifest,
            lens_flake,
            verbose: args.verbose,
            explore_hints: &args.explore,
            scope_context: &args.context,
        })?;
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
        let run_result = scenarios::run_scenarios(&scenarios::RunScenariosArgs {
            pack_root: &pack_root,
            run_root: ctx.paths.root(),
            binary_name,
            scenarios_path: &scenarios_path,
            lens_flake,
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
    })
}

fn modified_epoch_ms(path: &Path) -> Option<u128> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let duration = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(duration.as_millis())
}

fn outputs_equal_workaround_needs_delta_rerun(
    paths: &enrich::DocPackPaths,
    entry: &scenarios::VerificationEntry,
) -> bool {
    let overlays_path = paths.surface_overlays_path();
    let Some(overlays_modified_ms) = modified_epoch_ms(&overlays_path) else {
        return false;
    };
    let latest_delta_modified_ms = entry
        .delta_evidence_paths
        .iter()
        .filter_map(|rel| {
            let rel = rel.trim();
            if rel.is_empty() {
                return None;
            }
            let abs = paths.root().join(rel);
            modified_epoch_ms(&abs)
        })
        .max();
    match latest_delta_modified_ms {
        Some(delta_modified_ms) => delta_modified_ms <= overlays_modified_ms,
        None => true,
    }
}

fn surface_has_requires_argv_hint(surface: &surface::SurfaceInventory, surface_id: &str) -> bool {
    surface::primary_surface_item_by_id(surface, surface_id)
        .is_some_and(|item| !item.invocation.requires_argv.is_empty())
}

fn fallback_behavior_scenario_id_for_surface_id(surface_id: &str) -> String {
    format!(
        "verify_{}",
        surface_id.trim_start_matches('-').trim().replace('-', "_")
    )
}

fn behavior_scenario_ids_for_entry(
    surface_id: &str,
    entry: &scenarios::VerificationEntry,
) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if let Some(scenario_id) = entry.behavior_unverified_scenario_id.as_deref() {
        let scenario_id = scenario_id.trim();
        if !scenario_id.is_empty() {
            ids.insert(scenario_id.to_string());
        }
    }
    for scenario_id in &entry.behavior_scenario_ids {
        let scenario_id = scenario_id.trim();
        if scenario_id.is_empty() {
            continue;
        }
        ids.insert(scenario_id.to_string());
    }
    if ids.is_empty() {
        ids.insert(fallback_behavior_scenario_id_for_surface_id(surface_id));
    }
    ids
}

fn normalize_rerun_ids(ids: &[String]) -> BTreeSet<String> {
    ids.iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

fn update_outputs_equal_retry_progress_after_apply(
    paths: &enrich::DocPackPaths,
    executed_forced_rerun_scenario_ids: &[String],
    ledger_entries: &BTreeMap<String, scenarios::VerificationEntry>,
) -> Result<()> {
    if !paths.surface_path().is_file() {
        return Ok(());
    }

    let surface = surface::load_surface_inventory(&paths.surface_path())
        .with_context(|| format!("load {}", paths.surface_path().display()))?;
    let executed_forced_rerun_ids = normalize_rerun_ids(executed_forced_rerun_scenario_ids);
    let mut progress = load_verification_progress(paths);

    let active_outputs_equal_surface_ids: BTreeSet<String> = ledger_entries
        .iter()
        .filter(|(surface_id, entry)| {
            entry.delta_outcome.as_deref() == Some("outputs_equal")
                && surface_has_requires_argv_hint(&surface, surface_id)
                && outputs_equal_workaround_needs_delta_rerun(paths, entry)
        })
        .map(|(surface_id, _)| surface_id.clone())
        .collect();

    let before_len = progress.outputs_equal_retries_by_surface.len();
    progress
        .outputs_equal_retries_by_surface
        .retain(|surface_id, _| active_outputs_equal_surface_ids.contains(surface_id));
    let mut changed = progress.outputs_equal_retries_by_surface.len() != before_len;

    for surface_id in &active_outputs_equal_surface_ids {
        let Some(entry) = ledger_entries.get(surface_id.as_str()) else {
            continue;
        };
        let scenario_ids = behavior_scenario_ids_for_entry(surface_id, entry);
        let was_forced_rerun_executed = scenario_ids
            .iter()
            .any(|scenario_id| executed_forced_rerun_ids.contains(scenario_id));
        let delta_signature = outputs_equal_delta_signature(Some(entry));

        if !was_forced_rerun_executed {
            if let Some(progress_entry) = progress
                .outputs_equal_retries_by_surface
                .get_mut(surface_id)
            {
                if progress_entry.delta_signature.as_deref() != Some(delta_signature.as_str()) {
                    progress_entry.retry_count = 0;
                    progress_entry.delta_signature = Some(delta_signature);
                    changed = true;
                }
            }
            continue;
        }

        let progress_entry = progress
            .outputs_equal_retries_by_surface
            .entry(surface_id.clone())
            .or_default();
        if progress_entry.delta_signature.as_deref() != Some(delta_signature.as_str()) {
            progress_entry.retry_count = 0;
        }
        progress_entry.retry_count = progress_entry.retry_count.saturating_add(1);
        progress_entry.delta_signature = Some(delta_signature);
        changed = true;
    }

    if changed {
        write_verification_progress(paths, &progress)?;
    }

    Ok(())
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

#[cfg(test)]
const ASSERTION_FAILED_NOOP_CAP: usize = 2;

/// Update assertion_failed loop progress after scenario executions.
/// Advances loop state and no_progress_count when targeted reruns are executed.
fn update_assertion_failed_progress_after_apply(
    paths: &enrich::DocPackPaths,
    executed_forced_rerun_scenario_ids: &[String],
    ledger_entries: &BTreeMap<String, scenarios::VerificationEntry>,
) -> Result<()> {
    let executed_forced_rerun_ids = normalize_rerun_ids(executed_forced_rerun_scenario_ids);
    let mut progress = load_verification_progress(paths);

    // Find surfaces with assertion_failed that had forced reruns executed
    let assertion_failed_surface_ids: BTreeSet<String> = ledger_entries
        .iter()
        .filter(|(_, entry)| {
            entry.behavior_unverified_reason_code.as_deref() == Some("assertion_failed")
        })
        .map(|(surface_id, _)| surface_id.clone())
        .collect();

    let before_len = progress.assertion_failed_by_surface.len();
    // Remove entries for surfaces no longer in assertion_failed state
    progress
        .assertion_failed_by_surface
        .retain(|surface_id, _| assertion_failed_surface_ids.contains(surface_id));
    let mut changed = progress.assertion_failed_by_surface.len() != before_len;

    for surface_id in &assertion_failed_surface_ids {
        let Some(entry) = ledger_entries.get(surface_id.as_str()) else {
            continue;
        };
        let scenario_ids = behavior_scenario_ids_for_entry(surface_id, entry);
        let was_forced_rerun_executed = scenario_ids
            .iter()
            .any(|scenario_id| executed_forced_rerun_ids.contains(scenario_id));

        if !was_forced_rerun_executed {
            continue;
        }

        // Compute current evidence fingerprint
        let current_fingerprint = crate::verification_progress::evidence_fingerprint(Some(entry));

        let progress_entry = progress
            .assertion_failed_by_surface
            .entry(surface_id.clone())
            .or_default();

        // Check if evidence has changed
        let evidence_changed = progress_entry
            .last_signature
            .evidence_fingerprint
            .as_deref()
            != Some(current_fingerprint.as_str());

        if evidence_changed {
            // Evidence changed - this is progress, reset counter
            progress_entry.no_progress_count = 0;
            progress_entry.last_signature.evidence_fingerprint = Some(current_fingerprint);
            changed = true;
        } else {
            // Evidence unchanged after rerun - no progress made
            progress_entry.no_progress_count = progress_entry.no_progress_count.saturating_add(1);
            changed = true;
        }
    }

    if changed {
        write_verification_progress(paths, &progress)?;
    }

    Ok(())
}


#[cfg(test)]
mod tests;
