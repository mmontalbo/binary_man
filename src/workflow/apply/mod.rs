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
use crate::workflow::{run_plan, run_validate};
use anyhow::{anyhow, Context, Result};
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
    let (published_paths, outputs_hash) = match apply_result {
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

fn apply_plan_actions(inputs: &ApplyInputs<'_>) -> Result<(Vec<PathBuf>, Option<String>)> {
    let ctx = inputs.ctx;
    let actions = inputs.planned_actions;
    let plan = inputs.plan;
    let manifest = inputs.manifest;
    let lens_flake = inputs.lens_flake;
    let binary_name = inputs.binary_name;
    let staging_root = inputs.staging_root;
    let args = inputs.args;

    let wants_surface = actions
        .iter()
        .any(|action| matches!(action, enrich::PlannedAction::SurfaceDiscovery));
    let wants_coverage_ledger = actions
        .iter()
        .any(|action| matches!(action, enrich::PlannedAction::CoverageLedger));
    let wants_scenarios = actions
        .iter()
        .any(|action| matches!(action, enrich::PlannedAction::ScenarioRuns));
    let wants_render = actions
        .iter()
        .any(|action| matches!(action, enrich::PlannedAction::RenderManPage));
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
                ctx.paths.root(),
            ));
            extra_scenarios.extend(batch.scenarios);
        }
        examples_report = Some(scenarios::run_scenarios(&scenarios::RunScenariosArgs {
            pack_root: &pack_root,
            run_root: ctx.paths.root(),
            binary_name,
            scenarios_path: &scenarios_path,
            lens_flake,
            display_root: Some(ctx.paths.root()),
            staging_root: Some(staging_root),
            kind_filter: None,
            run_mode,
            extra_scenarios,
            auto_run_limit,
            auto_progress,
            verbose: args.verbose,
        })?);
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

    write_ledgers(&LedgerArgs {
        paths: &ctx.paths,
        staging_root,
        binary_name,
        scenarios_path: &scenarios_path,
        emit_coverage: emit_coverage_ledger,
        emit_verification: emit_verification_ledger,
    })?;

    let published_paths = publish_staging(staging_root, ctx.paths.root())?;
    let outputs_hash = (!published_paths.is_empty())
        .then(|| enrich::hash_paths(ctx.paths.root(), &published_paths))
        .transpose()?;

    Ok((published_paths, outputs_hash))
}

fn auto_verification_progress(
    plan: &enrich::EnrichPlan,
    scenario_plan: &scenarios::ScenarioPlan,
    config: &enrich::EnrichConfig,
    batch: &AutoVerificationBatch,
    doc_pack_root: &Path,
) -> scenarios::AutoVerificationProgress {
    if let Some(summary) = plan.verification_plan.as_ref() {
        return plan_summary_progress(summary, batch.targets.max_new_runs_per_apply);
    }

    let ledger_entries = scenarios::load_verification_entries(doc_pack_root);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn apply_args(refresh_pack: bool) -> ApplyArgs {
        ApplyArgs {
            doc_pack: std::path::PathBuf::from("/tmp/doc-pack"),
            refresh_pack,
            verbose: false,
            rerun_all: false,
            rerun_failed: false,
            lens_flake: "unused".to_string(),
        }
    }

    #[test]
    fn refresh_pack_runs_before_validate_and_plan_derivation() {
        let args = apply_args(true);
        let lock_status = enrich::LockStatus {
            present: true,
            stale: false,
            inputs_hash: Some("stale".to_string()),
        };
        let plan_state = enrich::PlanStatus {
            present: true,
            stale: false,
            inputs_hash: Some("stale".to_string()),
            lock_inputs_hash: Some("stale".to_string()),
        };
        let call_order = Rc::new(RefCell::new(Vec::new()));
        let input_state = Rc::new(RefCell::new("pre_refresh".to_string()));
        let plan_input_state = Rc::new(RefCell::new(None::<String>));

        let preflight = run_apply_preflight(
            &args,
            &lock_status,
            &plan_state,
            {
                let call_order = Rc::clone(&call_order);
                let input_state = Rc::clone(&input_state);
                move || {
                    call_order.borrow_mut().push("refresh");
                    *input_state.borrow_mut() = "post_refresh".to_string();
                    Ok(())
                }
            },
            {
                let call_order = Rc::clone(&call_order);
                let input_state = Rc::clone(&input_state);
                move || {
                    call_order.borrow_mut().push("validate");
                    assert_eq!(input_state.borrow().as_str(), "post_refresh");
                    Ok(())
                }
            },
            {
                let call_order = Rc::clone(&call_order);
                let input_state = Rc::clone(&input_state);
                let plan_input_state = Rc::clone(&plan_input_state);
                move || {
                    call_order.borrow_mut().push("plan");
                    *plan_input_state.borrow_mut() = Some(input_state.borrow().clone());
                    Ok(())
                }
            },
        )
        .expect("preflight should succeed");

        assert!(preflight.ran_validate);
        assert!(preflight.ran_plan);
        assert_eq!(
            call_order.borrow().as_slice(),
            &["refresh", "validate", "plan"]
        );
        assert_eq!(
            plan_input_state.borrow().as_deref(),
            Some("post_refresh"),
            "plan derivation must run against refreshed inputs"
        );
    }
}
