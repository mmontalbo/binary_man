mod cleanup;
mod ledgers;
mod pack;
mod rendering;

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
use crate::workflow::{run_plan, run_validate};
use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use cleanup::cleanup_txn_dirs;
use ledgers::{write_ledgers, LedgerArgs};
use pack::refresh_pack_if_needed;
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
    // Handle LM response if provided
    if let Some(lm_response_path) = &args.lm_response {
        apply_lm_response(&args.doc_pack, lm_response_path)?;
    }

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
        apply_surface_discovery(
            ctx.paths.root(),
            staging_root,
            Some(plan.lock.inputs_hash.as_str()),
            manifest,
            lens_flake,
            args.verbose,
        )?;
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

fn auto_verification_progress(
    plan: &enrich::EnrichPlan,
    scenario_plan: &scenarios::ScenarioPlan,
    config: &enrich::EnrichConfig,
    batch: &AutoVerificationBatch,
    paths: &enrich::DocPackPaths,
) -> scenarios::AutoVerificationProgress {
    if let Some(summary) = plan.verification_plan.as_ref() {
        return plan_summary_progress(summary, batch.targets.max_new_runs_per_apply);
    }

    let binary_name = batch
        .surface
        .binary_name
        .clone()
        .unwrap_or_else(|| "<binary>".to_string());
    let template_path = paths
        .root()
        .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);
    let ledger_entries = scenarios::build_verification_ledger(
        &binary_name,
        &batch.surface,
        paths.root(),
        &paths.scenarios_plan_path(),
        &template_path,
        None,
        Some(paths.root()),
    )
    .ok()
    .map(|ledger| scenarios::verification_entries_by_surface_id(ledger.entries));
    let verification_tier = config.verification_tier.as_deref().unwrap_or("accepted");
    if let Some(summary) = crate::status::auto_verification_plan_summary(
        scenario_plan,
        &batch.surface,
        ledger_entries.as_ref(),
        verification_tier,
    ) {
        return plan_summary_progress(&summary, batch.targets.max_new_runs_per_apply);
    }

    let remaining_by_kind = batch
        .targets
        .targets
        .iter()
        .map(|(kind, ids)| scenarios::AutoVerificationKindProgress {
            kind: kind.as_str().to_string(),
            remaining_count: ids.len(),
        })
        .collect();
    scenarios::AutoVerificationProgress {
        remaining_total: Some(batch.targets.target_ids.len()),
        remaining_by_kind,
        max_new_runs_per_apply: batch.targets.max_new_runs_per_apply,
    }
}

fn plan_summary_progress(
    summary: &enrich::VerificationPlanSummary,
    max_new_runs_per_apply: usize,
) -> scenarios::AutoVerificationProgress {
    let remaining_by_kind = summary
        .by_kind
        .iter()
        .map(|group| scenarios::AutoVerificationKindProgress {
            kind: group.kind.clone(),
            remaining_count: group.remaining_count,
        })
        .collect();
    scenarios::AutoVerificationProgress {
        remaining_total: Some(summary.remaining_count),
        remaining_by_kind,
        max_new_runs_per_apply,
    }
}

struct AutoVerificationBatch {
    scenarios: Vec<scenarios::ScenarioSpec>,
    max_new_runs_per_apply: usize,
    targets: scenarios::AutoVerificationTargets,
    surface: surface::SurfaceInventory,
}

fn auto_verification_scenarios(
    plan: &scenarios::ScenarioPlan,
    doc_pack_root: &Path,
    staging_root: &Path,
    verbose: bool,
    verification_tier: &str,
) -> Result<Option<AutoVerificationBatch>> {
    let surface = match load_surface_for_auto(doc_pack_root, staging_root, verbose)? {
        Some(surface) => surface,
        None => return Ok(None),
    };
    let Some(targets) = (if verification_tier == "behavior" {
        scenarios::auto_verification_targets_for_behavior(plan, &surface)
    } else {
        scenarios::auto_verification_targets(plan, &surface)
    }) else {
        return Ok(None);
    };
    let semantics = match semantics::load_semantics(doc_pack_root) {
        Ok(semantics) => semantics,
        Err(err) => {
            if verbose {
                eprintln!("warning: skipping auto verification (load semantics failed: {err})");
            }
            return Ok(None);
        }
    };
    let scenarios = scenarios::auto_verification_scenarios(&targets, &semantics);
    Ok(Some(AutoVerificationBatch {
        scenarios,
        max_new_runs_per_apply: targets.max_new_runs_per_apply,
        targets,
        surface,
    }))
}

fn load_surface_for_auto(
    doc_pack_root: &Path,
    staging_root: &Path,
    verbose: bool,
) -> Result<Option<surface::SurfaceInventory>> {
    let staged_surface = staging_root.join("inventory").join("surface.json");
    let surface_path = if staged_surface.is_file() {
        staged_surface
    } else {
        doc_pack_root.join("inventory").join("surface.json")
    };
    if !surface_path.is_file() {
        return Ok(None);
    }
    match surface::load_surface_inventory(&surface_path) {
        Ok(surface) => Ok(Some(surface)),
        Err(err) => {
            if verbose {
                eprintln!("warning: skipping auto verification (invalid surface inventory: {err})");
            }
            Ok(None)
        }
    }
}

/// Apply LM responses to the doc pack before running normal apply.
fn apply_lm_response(doc_pack: &Path, lm_response_path: &Path) -> Result<()> {
    use super::lm_response::{load_lm_response, validate_responses};

    let doc_pack_root = crate::docpack::ensure_doc_pack_root(doc_pack, false)?;
    let paths = enrich::DocPackPaths::new(doc_pack_root);

    // Load LM response
    let batch = load_lm_response(lm_response_path)?;
    eprintln!(
        "lm-response: loaded {} responses from {}",
        batch.responses.len(),
        lm_response_path.display()
    );

    // Load surface inventory
    let surface_path = paths.surface_path();
    if !surface_path.is_file() {
        return Err(anyhow!(
            "surface.json not found; run `bman apply` first to generate surface inventory"
        ));
    }
    let surface_inventory: surface::SurfaceInventory =
        serde_json::from_str(&fs::read_to_string(&surface_path).context("read surface inventory")?)
            .context("parse surface inventory")?;
    let binary_name = surface_inventory
        .binary_name
        .clone()
        .unwrap_or_else(|| "<binary>".to_string());

    // Load scenarios plan
    let scenarios_path = paths.scenarios_plan_path();
    if !scenarios_path.is_file() {
        return Err(anyhow!(
            "scenarios/plan.json not found; run `bman apply` first"
        ));
    }

    // Build verification ledger on-the-fly
    let template_path = paths
        .root()
        .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);
    let ledger = scenarios::build_verification_ledger(
        &binary_name,
        &surface_inventory,
        paths.root(),
        &scenarios_path,
        &template_path,
        None,
        Some(paths.root()),
    )
    .context("compute verification ledger for LM response validation")?;

    // Build set of valid unverified surface_ids
    let valid_surface_ids: BTreeSet<String> = ledger
        .entries
        .iter()
        .filter(|e| e.behavior_status != "verified" && e.behavior_status != "excluded")
        .map(|e| e.surface_id.clone())
        .collect();

    // Validate responses
    let (validated, result) = validate_responses(&batch, &valid_surface_ids);

    eprintln!(
        "lm-response: validated {} responses ({} skipped, {} errors)",
        result.valid_count,
        result.skipped_count,
        result.errors.len()
    );

    for error in &result.errors {
        eprintln!("  error: {}: {}", error.surface_id, error.message);
    }

    if result.valid_count == 0 {
        if result.errors.is_empty() {
            eprintln!("lm-response: no actionable responses to apply");
            return Ok(());
        }
        return Err(anyhow!(
            "all {} responses failed validation",
            result.errors.len()
        ));
    }

    // Apply scenarios to plan.json
    if !validated.scenarios_to_upsert.is_empty() {
        let plan_path = paths.scenarios_plan_path();
        let mut plan = scenarios::load_plan(&plan_path, paths.root())?;

        for scenario in &validated.scenarios_to_upsert {
            // Upsert: replace existing or add new
            if let Some(existing) = plan.scenarios.iter_mut().find(|s| s.id == scenario.id) {
                *existing = scenario.clone();
                eprintln!("  updated scenario: {}", scenario.id);
            } else {
                plan.scenarios.push(scenario.clone());
                eprintln!("  added scenario: {}", scenario.id);
            }
        }

        let plan_json = serde_json::to_string_pretty(&plan).context("serialize plan")?;
        fs::write(&plan_path, plan_json.as_bytes()).context("write plan.json")?;
        eprintln!(
            "lm-response: wrote {} scenario(s) to {}",
            validated.scenarios_to_upsert.len(),
            plan_path.display()
        );
    }

    // Apply assertion fixes to existing scenarios
    if !validated.assertion_fixes.is_empty() {
        let plan_path = paths.scenarios_plan_path();
        let mut plan = scenarios::load_plan(&plan_path, paths.root())?;

        for (scenario_id, assertions) in &validated.assertion_fixes {
            if let Some(scenario) = plan.scenarios.iter_mut().find(|s| s.id == *scenario_id) {
                scenario.assertions = assertions.clone();
                eprintln!("  fixed assertions in scenario: {}", scenario_id);
            } else {
                eprintln!(
                    "  warning: scenario {} not found for assertion fix",
                    scenario_id
                );
            }
        }

        let plan_json = serde_json::to_string_pretty(&plan).context("serialize plan")?;
        fs::write(&plan_path, plan_json.as_bytes()).context("write plan.json")?;
    }

    // Apply overlays (value_examples, requires_argv, exclusions)
    let has_overlays = !validated.value_examples.is_empty()
        || !validated.requires_argv.is_empty()
        || !validated.exclusions.is_empty();

    if has_overlays {
        apply_lm_overlays(&paths, &validated, &ledger)?;
    }

    Ok(())
}

/// Apply overlay changes from LM responses.
fn apply_lm_overlays(
    paths: &enrich::DocPackPaths,
    validated: &super::lm_response::ValidatedResponses,
    ledger: &scenarios::VerificationLedger,
) -> Result<()> {
    use super::lm_response::ExclusionReasonCode;

    let overlays_path = paths.surface_overlays_path();

    // Load existing overlays or create new structure
    let mut overlays: serde_json::Value = if overlays_path.is_file() {
        serde_json::from_str(&fs::read_to_string(&overlays_path)?)?
    } else {
        serde_json::json!({
            "schema_version": 3,
            "items": [],
            "overlays": []
        })
    };

    let overlays_array = overlays["overlays"]
        .as_array_mut()
        .ok_or_else(|| anyhow!("overlays must be an array"))?;

    // Helper to find or create overlay for a surface_id
    let find_or_create_overlay = |arr: &mut Vec<serde_json::Value>, surface_id: &str| -> usize {
        if let Some(idx) = arr
            .iter()
            .position(|o| o["id"].as_str() == Some(surface_id))
        {
            idx
        } else {
            arr.push(serde_json::json!({
                "id": surface_id,
                "kind": "option",
                "invocation": {}
            }));
            arr.len() - 1
        }
    };

    // Apply value_examples
    for (surface_id, examples) in &validated.value_examples {
        let idx = find_or_create_overlay(overlays_array, surface_id);
        overlays_array[idx]["invocation"]["value_examples"] = serde_json::json!(examples);
        eprintln!("  added value_examples for {}: {:?}", surface_id, examples);
    }

    // Apply requires_argv
    for (surface_id, argv) in &validated.requires_argv {
        let idx = find_or_create_overlay(overlays_array, surface_id);
        overlays_array[idx]["invocation"]["requires_argv"] = serde_json::json!(argv);
        eprintln!("  added requires_argv for {}: {:?}", surface_id, argv);
    }

    // Apply exclusions
    for (surface_id, (reason_code, note)) in &validated.exclusions {
        let idx = find_or_create_overlay(overlays_array, surface_id);

        // Get delta evidence from ledger for this surface_id
        let ledger_entry = ledger.entries.iter().find(|e| e.surface_id == *surface_id);
        let delta_variant_path = ledger_entry
            .and_then(|e| e.delta_evidence_paths.first().cloned())
            .unwrap_or_default();

        let reason_code_str = match reason_code {
            ExclusionReasonCode::FixtureGap => "fixture_gap",
            ExclusionReasonCode::AssertionGap => "assertion_gap",
            ExclusionReasonCode::Nondeterministic => "nondeterministic",
            ExclusionReasonCode::RequiresInteractiveTty => "requires_interactive_tty",
            ExclusionReasonCode::UnsafeSideEffects => "unsafe_side_effects",
        };

        overlays_array[idx]["behavior_exclusion"] = serde_json::json!({
            "reason_code": reason_code_str,
            "note": note,
            "evidence": {
                "delta_variant_path": delta_variant_path
            }
        });
        eprintln!(
            "  added exclusion for {}: {} ({})",
            surface_id, reason_code_str, note
        );
    }

    // Write updated overlays
    let overlays_json = serde_json::to_string_pretty(&overlays)?;
    fs::write(&overlays_path, overlays_json.as_bytes())?;
    eprintln!("lm-response: wrote overlays to {}", overlays_path.display());

    Ok(())
}

#[cfg(test)]
mod tests;
