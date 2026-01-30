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
use crate::status::{build_status_summary, plan_status};
use crate::surface::apply_surface_discovery;
use crate::util::resolve_flake_ref;
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

pub(crate) fn run_apply(args: &ApplyArgs) -> Result<()> {
    let lens_flake = resolve_flake_ref(&args.lens_flake)?;
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, false)?;
    let ctx = EnrichContext::load(doc_pack_root)?;
    ctx.require_config()?;
    enrich::validate_config(&ctx.config)?;
    let mut manifest = ctx.manifest.clone();

    let (lock, lock_status, mut force_used) = ctx.lock_for_apply(args.force)?;

    let plan = ctx.require_plan()?;
    force_used |= verify_plan_lock(&plan, lock.as_ref(), &ctx.paths, args.force)?;

    if args.refresh_pack {
        manifest = refresh_pack_if_needed(&ctx, manifest.as_ref(), &lens_flake)?;
    }

    let binary_name = manifest.as_ref().map(|m| m.binary_name.clone());

    let started_at_epoch_ms = enrich::now_epoch_ms()?;
    let txn_id = format!("{started_at_epoch_ms}");
    let staging_root = ctx.paths.txn_staging_root(&txn_id);
    fs::create_dir_all(&staging_root).context("create staging dir")?;

    let apply_inputs = ApplyInputs {
        ctx: &ctx,
        plan: &plan,
        manifest: manifest.as_ref(),
        lens_flake: &lens_flake,
        binary_name: binary_name.as_deref(),
        staging_root: &staging_root,
        args,
    };
    let apply_result = apply_plan_actions(&apply_inputs);

    let finished_at_epoch_ms = enrich::now_epoch_ms()?;
    let (_published_paths, outputs_hash) = match apply_result {
        Ok(result) => result,
        Err(err) => {
            let history_entry = enrich::EnrichHistoryEntry {
                schema_version: enrich::HISTORY_SCHEMA_VERSION,
                started_at_epoch_ms,
                finished_at_epoch_ms,
                step: "apply".to_string(),
                inputs_hash: lock.as_ref().map(|l| l.inputs_hash.clone()),
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

    let plan_status = plan_status(lock.as_ref(), Some(&plan));
    let summary = build_status_summary(crate::status::BuildStatusSummaryArgs {
        doc_pack_root: ctx.paths.root(),
        binary_name: binary_name.as_deref(),
        config: &ctx.config,
        config_exists: true,
        lock_status,
        plan_status,
        include_full: false,
        force_used,
    })?;

    let last_run = enrich::EnrichRunSummary {
        step: "apply".to_string(),
        started_at_epoch_ms,
        finished_at_epoch_ms,
        success: true,
        inputs_hash: lock.as_ref().map(|l| l.inputs_hash.clone()),
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

    let report = enrich::EnrichReport {
        schema_version: enrich::REPORT_SCHEMA_VERSION,
        generated_at_epoch_ms: finished_at_epoch_ms,
        binary_name: binary_name.clone(),
        lock,
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
    plan: &'a enrich::EnrichPlan,
    manifest: Option<&'a crate::pack::PackManifest>,
    lens_flake: &'a str,
    binary_name: Option<&'a str>,
    staging_root: &'a Path,
    args: &'a ApplyArgs,
}

fn verify_plan_lock(
    plan: &enrich::EnrichPlan,
    lock: Option<&enrich::EnrichLock>,
    paths: &enrich::DocPackPaths,
    force: bool,
) -> Result<bool> {
    if let Some(lock) = lock {
        if plan.lock.inputs_hash != lock.inputs_hash {
            if !force {
                return Err(anyhow!(
                    "plan does not match lock (run `bman plan --doc-pack {}` again or pass --force)",
                    paths.root().display()
                ));
            }
            return Ok(true);
        }
        return Ok(false);
    }
    if !force {
        return Err(anyhow!(
            "missing lock for plan verification (run `bman validate --doc-pack {}` or pass --force)",
            paths.root().display()
        ));
    }
    Ok(true)
}

fn apply_plan_actions(inputs: &ApplyInputs<'_>) -> Result<(Vec<PathBuf>, Option<String>)> {
    let ctx = inputs.ctx;
    let plan = inputs.plan;
    let manifest = inputs.manifest;
    let lens_flake = inputs.lens_flake;
    let binary_name = inputs.binary_name;
    let staging_root = inputs.staging_root;
    let args = inputs.args;

    let actions = plan.planned_actions.as_slice();
    let wants_surface = actions
        .iter()
        .any(|action| matches!(action, enrich::PlannedAction::SurfaceDiscovery));
    let wants_scenarios = actions
        .iter()
        .any(|action| matches!(action, enrich::PlannedAction::ScenarioRuns));
    let wants_render = actions
        .iter()
        .any(|action| matches!(action, enrich::PlannedAction::RenderManPage));
    let requirements = enrich::normalized_requirements(&ctx.config);
    let emit_coverage_ledger = requirements
        .iter()
        .any(|req| matches!(req, enrich::RequirementId::CoverageLedger));
    let emit_verification_ledger = requirements
        .iter()
        .any(|req| matches!(req, enrich::RequirementId::Verification));

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

    write_ledgers(LedgerArgs {
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
