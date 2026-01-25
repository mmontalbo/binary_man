use crate::cli::{ApplyArgs, InitArgs, PlanArgs, StatusArgs, ValidateArgs};
use crate::docpack::{doc_pack_root_for_status, ensure_doc_pack_root};
use crate::enrich;
use crate::output::write_outputs_staged;
use crate::pack;
use crate::render;
use crate::scenarios;
use crate::staging::publish_staging;
use crate::status::{
    build_status_summary, plan_status, planned_actions_from_requirements, PlanStatus,
};
use crate::surface::{self, apply_surface_discovery};
use crate::templates;
use crate::util::resolve_flake_ref;
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub fn run_init(args: InitArgs) -> Result<()> {
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, true)?;
    let paths = enrich::DocPackPaths::new(doc_pack_root);
    let config_path = paths.config_path();
    if config_path.is_file() && !args.force {
        return Err(anyhow!(
            "config already exists at {} (use --force to overwrite)",
            config_path.display()
        ));
    }
    let manifest_path = paths.pack_manifest_path();
    if !manifest_path.is_file() {
        let mut bootstrap = None;
        let mut binary = args.binary.clone();
        if binary.is_none() {
            match enrich::load_bootstrap_optional(paths.root()) {
                Ok(Some(loaded)) => {
                    binary = Some(loaded.binary.clone());
                    bootstrap = Some(loaded);
                }
                Ok(None) => {
                    return Err(anyhow!(
                        "pack missing; provide --binary or create enrich/bootstrap.json"
                    ));
                }
                Err(err) => {
                    return Err(anyhow!(
                        "pack missing; provide --binary or create enrich/bootstrap.json ({})",
                        err
                    ));
                }
            }
        }
        let binary = binary
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow!("pack missing; provide --binary or create enrich/bootstrap.json")
            })?;
        let lens_flake_input = bootstrap
            .as_ref()
            .and_then(|loaded| loaded.lens_flake.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(args.lens_flake.as_str());
        let lens_flake = resolve_flake_ref(lens_flake_input)?;
        pack::generate_pack(binary, paths.root(), &lens_flake)?;
    }

    install_usage_lens_templates(&paths, args.force)?;
    install_probe_plan(&paths, args.force)?;

    let config = enrich::default_config();
    enrich::write_config(paths.root(), &config)?;
    println!("wrote {}", config_path.display());
    Ok(())
}

pub fn run_validate(args: ValidateArgs) -> Result<()> {
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, false)?;
    let ctx = EnrichContext::load(doc_pack_root)?;
    ctx.require_config()?;
    enrich::validate_config(&ctx.config)?;
    let lock = enrich::build_lock(ctx.paths.root(), &ctx.config, ctx.binary_name())?;
    enrich::write_lock(ctx.paths.root(), &lock)?;
    if args.verbose {
        eprintln!("wrote {}", ctx.paths.lock_path().display());
    }
    Ok(())
}

pub fn run_plan(args: PlanArgs) -> Result<()> {
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, false)?;
    let ctx = EnrichContext::load(doc_pack_root)?;
    ctx.require_config()?;
    enrich::validate_config(&ctx.config)?;

    let (lock, lock_status, force_used) = ctx.lock_for_plan(args.force)?;

    let plan_status = PlanStatus {
        present: true,
        stale: false,
    };
    let summary = build_status_summary(
        ctx.paths.root(),
        ctx.binary_name(),
        &ctx.config,
        true,
        lock_status,
        plan_status,
        force_used,
    )?;
    let planned_actions = planned_actions_from_requirements(&summary.requirements);

    let lock_inputs_hash = lock.inputs_hash.clone();
    let plan = enrich::EnrichPlan {
        schema_version: enrich::PLAN_SCHEMA_VERSION,
        generated_at_epoch_ms: enrich::now_epoch_ms()?,
        binary_name: ctx.binary_name.clone(),
        lock,
        requirements: summary.requirements.clone(),
        planned_actions,
        next_action: enrich::NextAction::Command {
            command: format!("bman apply --doc-pack {}", ctx.paths.root().display()),
            reason: "apply the planned actions".to_string(),
        },
        decision: summary.decision.clone(),
        decision_reason: summary.decision_reason.clone(),
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

pub fn run_apply(args: ApplyArgs) -> Result<()> {
    let lens_flake = resolve_flake_ref(&args.lens_flake)?;
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, false)?;
    let ctx = EnrichContext::load(doc_pack_root)?;
    ctx.require_config()?;
    enrich::validate_config(&ctx.config)?;
    let mut manifest = ctx.manifest.clone();

    let (lock, lock_status, mut force_used) = ctx.lock_for_apply(args.force)?;

    let plan = ctx.require_plan()?;
    if let Some(lock) = lock.as_ref() {
        if plan.lock.inputs_hash != lock.inputs_hash {
            if !args.force {
                return Err(anyhow!(
                    "plan does not match lock (run `bman plan --doc-pack {}` again or pass --force)",
                    ctx.paths.root().display()
                ));
            }
            force_used = true;
        }
    } else if !args.force {
        return Err(anyhow!(
            "missing lock for plan verification (run `bman validate --doc-pack {}` or pass --force)",
            ctx.paths.root().display()
        ));
    } else {
        force_used = true;
    }

    if args.refresh_pack {
        let binary_path = manifest
            .as_ref()
            .map(|m| m.binary_path.as_str())
            .ok_or_else(|| anyhow!("manifest missing; cannot refresh pack"))?;
        pack::generate_pack(binary_path, ctx.paths.root(), &lens_flake)?;
        manifest = load_manifest_optional(&ctx.paths)?;
    }

    let binary_name = manifest.as_ref().map(|m| m.binary_name.as_str());

    let started_at_epoch_ms = enrich::now_epoch_ms()?;
    let txn_id = format!("{started_at_epoch_ms}");
    let staging_root = ctx.paths.txn_staging_root(&txn_id);
    fs::create_dir_all(&staging_root).context("create staging dir")?;

    let actions = plan.planned_actions.as_slice();
    let wants_surface = actions
        .iter()
        .any(|action| matches!(action, enrich::PlannedAction::SurfaceDiscovery));
    let wants_scenarios = actions
        .iter()
        .any(|action| matches!(action, enrich::PlannedAction::ScenarioRuns));
    let wants_coverage = actions
        .iter()
        .any(|action| matches!(action, enrich::PlannedAction::CoverageLedger));
    let wants_render = actions
        .iter()
        .any(|action| matches!(action, enrich::PlannedAction::RenderManPage));

    let apply_result: Result<(Vec<PathBuf>, Option<String>)> = (|| {
        let pack_root = ctx.paths.pack_root();
        let pack_root_exists = pack_root.is_dir();
        let requires_pack = wants_scenarios || wants_render;
        if requires_pack && !pack_root_exists {
            return Err(anyhow!(
                "pack root missing at {} (run `bman {} --doc-pack {}` first)",
                pack_root.display(),
                manifest
                    .as_ref()
                    .map(|m| m.binary_name.as_str())
                    .unwrap_or("<binary>"),
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

        let templates = compute_lens_templates(ctx.paths.root(), &ctx.config);
        let mut examples_report = None;
        let scenarios_path = ctx
            .config
            .scenario_catalogs
            .first()
            .map(|rel| ctx.paths.root().join(rel));

        if wants_surface {
            apply_surface_discovery(
                ctx.paths.root(),
                &staging_root,
                Some(plan.lock.inputs_hash.as_str()),
                manifest.as_ref(),
                args.verbose,
            )?;
        }

        if wants_scenarios {
            let scenarios_path = scenarios_path
                .clone()
                .ok_or_else(|| anyhow!("scenario_catalogs missing for planned scenario runs"))?;
            let binary_name = ensure_manifest_binary_name(manifest.as_ref(), binary_name)?;
            examples_report = Some(scenarios::run_scenarios(
                &pack_root,
                ctx.paths.root(),
                &binary_name,
                &scenarios_path,
                &lens_flake,
                Some(ctx.paths.root()),
                args.verbose,
            )?);
        } else if wants_render {
            examples_report = load_examples_report_optional(&ctx.paths)?;
        }

        let context = if wants_render {
            Some(resolve_pack_context_for_templates(&pack_root, &templates)?)
        } else {
            None
        };
        let surface_for_render = if wants_render {
            load_surface_for_render(&staging_root, &ctx.paths)?
        } else {
            None
        };

        if wants_render {
            let context = context
                .as_ref()
                .ok_or_else(|| anyhow!("pack context required for man rendering"))?;
            let man_page = render::render_man_page(
                context,
                examples_report.as_ref(),
                surface_for_render.as_ref(),
            );
            write_outputs_staged(
                &staging_root,
                ctx.paths.root(),
                context,
                &pack_root,
                Some(plan.lock.inputs_hash.as_str()),
                Some(&man_page),
                examples_report.as_ref(),
            )?;
        }

        if wants_coverage {
            let scenarios_path = scenarios_path
                .clone()
                .ok_or_else(|| anyhow!("scenario_catalogs missing for coverage ledger"))?;
            let staged_surface = staging_root.join("inventory").join("surface.json");
            let surface_path = if staged_surface.is_file() {
                staged_surface
            } else {
                ctx.paths.surface_path()
            };
            let surface = crate::surface::load_surface_inventory(&surface_path)?;
            let coverage_binary = binary_name
                .map(|name| name.to_string())
                .or_else(|| surface.binary_name.clone())
                .ok_or_else(|| anyhow!("binary name unavailable for coverage ledger"))?;
            let ledger = scenarios::build_coverage_ledger(
                &coverage_binary,
                &surface,
                ctx.paths.root(),
                &scenarios_path,
                Some(ctx.paths.root()),
            )?;
            crate::staging::write_staged_json(&staging_root, "coverage_ledger.json", &ledger)?;
        }

        let published_paths = publish_staging(&staging_root, ctx.paths.root())?;
        let outputs_hash = if !published_paths.is_empty() {
            Some(enrich::hash_paths(ctx.paths.root(), &published_paths)?)
        } else {
            None
        };

        Ok((published_paths, outputs_hash))
    })();

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

    // Successful publish: discard staging/backups to keep doc packs lean.
    let txn_root = ctx.paths.txn_root(&txn_id);
    if txn_root.is_dir() {
        if let Err(err) = fs::remove_dir_all(&txn_root) {
            if args.verbose {
                eprintln!(
                    "warning: failed to clean txn dir {}: {err}",
                    txn_root.display()
                );
            }
        }
    }
    let txns_root = ctx.paths.txns_root();
    if txns_root.is_dir() {
        match fs::read_dir(&txns_root) {
            Ok(mut entries) => {
                if entries.next().is_none() {
                    let _ = fs::remove_dir(&txns_root);
                }
            }
            Err(err) => {
                if args.verbose {
                    eprintln!(
                        "warning: failed to read txns dir {}: {err}",
                        txns_root.display()
                    );
                }
            }
        }
    }
    let plan_status = plan_status(lock.as_ref(), Some(&plan));
    let summary = build_status_summary(
        ctx.paths.root(),
        binary_name,
        &ctx.config,
        true,
        lock_status,
        plan_status,
        force_used,
    )?;

    let last_run = enrich::EnrichRunSummary {
        step: "apply".to_string(),
        started_at_epoch_ms,
        finished_at_epoch_ms,
        success: true,
        inputs_hash: lock.as_ref().map(|l| l.inputs_hash.clone()),
        outputs_hash,
        message: None,
    };

    let report = enrich::EnrichReport {
        schema_version: enrich::REPORT_SCHEMA_VERSION,
        generated_at_epoch_ms: finished_at_epoch_ms,
        binary_name: binary_name.map(|name| name.to_string()),
        lock,
        requirements: summary.requirements.clone(),
        blockers: summary.blockers.clone(),
        missing_artifacts: summary.missing_artifacts.clone(),
        decision: summary.decision.clone(),
        decision_reason: summary.decision_reason.clone(),
        next_action: summary.next_action.clone(),
        last_run: Some(last_run.clone()),
        force_used,
    };
    enrich::write_report(ctx.paths.root(), &report)?;

    let history_entry = enrich::EnrichHistoryEntry {
        schema_version: enrich::HISTORY_SCHEMA_VERSION,
        started_at_epoch_ms,
        finished_at_epoch_ms,
        step: "apply".to_string(),
        inputs_hash: last_run.inputs_hash.clone(),
        outputs_hash: last_run.outputs_hash.clone(),
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

pub fn run_status(args: StatusArgs) -> Result<()> {
    let doc_pack_root = doc_pack_root_for_status(&args.doc_pack)?;
    let paths = enrich::DocPackPaths::new(doc_pack_root);
    let manifest = load_manifest_optional(&paths)?;
    let binary_name = manifest.as_ref().map(|m| m.binary_name.clone());

    let config_state = load_config_state(&paths);
    let config_exists = !matches!(config_state, ConfigState::Missing);

    let (lock, lock_parse_error) = if paths.lock_path().is_file() {
        match enrich::load_lock(paths.root()) {
            Ok(lock) => (Some(lock), None),
            Err(err) => (None, Some(error_chain_message(&err))),
        }
    } else {
        (None, None)
    };
    let lock_status = if let Some(lock) = lock.as_ref() {
        enrich::lock_status(paths.root(), Some(lock))?
    } else if lock_parse_error.is_some() {
        enrich::LockStatus {
            present: true,
            stale: true,
            inputs_hash: None,
        }
    } else {
        enrich::LockStatus {
            present: false,
            stale: false,
            inputs_hash: None,
        }
    };
    let (plan, plan_parse_error) = if paths.plan_path().is_file() {
        match crate::status::load_plan(paths.root()) {
            Ok(plan) => (Some(plan), None),
            Err(err) => (None, Some(error_chain_message(&err))),
        }
    } else {
        (None, None)
    };
    let plan_status = plan_status(lock.as_ref(), plan.as_ref());
    let force_used = args.force && (!lock_status.present || lock_status.stale);
    let parse_errors_present = lock_parse_error.is_some() || plan_parse_error.is_some();

    let summary = if let ConfigState::Invalid { code, message } = &config_state {
        build_invalid_config_summary(
            &paths,
            binary_name.as_deref(),
            lock_status,
            code,
            message.clone(),
            force_used,
        )?
    } else if parse_errors_present {
        build_parse_error_summary(
            &paths,
            binary_name.as_deref(),
            &config_state,
            lock_status,
            lock_parse_error,
            plan_parse_error,
            force_used,
        )?
    } else {
        match config_state {
            ConfigState::Valid(config) => build_status_summary(
                paths.root(),
                binary_name.as_deref(),
                &config,
                config_exists,
                lock_status,
                plan_status,
                force_used,
            )?,
            ConfigState::Missing => {
                let config = enrich::default_config();
                build_status_summary(
                    paths.root(),
                    binary_name.as_deref(),
                    &config,
                    false,
                    lock_status,
                    plan_status,
                    force_used,
                )?
            }
            ConfigState::Invalid { .. } => unreachable!("config invalid handled above"),
        }
    };

    if args.json {
        let text = serde_json::to_string_pretty(&summary).context("serialize status summary")?;
        println!("{text}");
    } else {
        crate::status::print_status(paths.root(), &summary);
    }

    if force_used {
        let now = enrich::now_epoch_ms()?;
        let history_entry = enrich::EnrichHistoryEntry {
            schema_version: enrich::HISTORY_SCHEMA_VERSION,
            started_at_epoch_ms: now,
            finished_at_epoch_ms: now,
            step: "status".to_string(),
            inputs_hash: lock.as_ref().map(|lock| lock.inputs_hash.clone()),
            outputs_hash: None,
            success: true,
            message: Some("force used".to_string()),
            force_used,
        };
        enrich::append_history(paths.root(), &history_entry)?;
    }

    if !args.json && (!summary.lock.present || summary.lock.stale) && !args.force {
        return Err(anyhow!(
            "missing or stale lock at {} (run `bman validate --doc-pack {}` or pass --force)",
            paths.lock_path().display(),
            paths.root().display()
        ));
    }

    Ok(())
}

enum ConfigState {
    Missing,
    Valid(enrich::EnrichConfig),
    Invalid { code: &'static str, message: String },
}

fn load_config_state(paths: &enrich::DocPackPaths) -> ConfigState {
    let config_path = paths.config_path();
    if !config_path.is_file() {
        return ConfigState::Missing;
    }
    match enrich::load_config(paths.root()) {
        Ok(config) => match enrich::validate_config(&config) {
            Ok(()) => ConfigState::Valid(config),
            Err(err) => ConfigState::Invalid {
                code: "config_invalid",
                message: error_chain_message(&err),
            },
        },
        Err(err) => ConfigState::Invalid {
            code: "config_parse_error",
            message: error_chain_message(&err),
        },
    }
}

fn error_chain_message(err: &anyhow::Error) -> String {
    err.chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ")
}

fn build_invalid_config_summary(
    paths: &enrich::DocPackPaths,
    binary_name: Option<&str>,
    lock_status: enrich::LockStatus,
    code: &'static str,
    message: String,
    force_used: bool,
) -> Result<enrich::StatusSummary> {
    let evidence = paths.evidence_from_path(&paths.config_path())?;
    let blocker = enrich::Blocker {
        code: code.to_string(),
        message,
        evidence: vec![evidence],
        next_action: None,
    };
    let stub = enrich::config_stub();
    Ok(enrich::StatusSummary {
        schema_version: 1,
        generated_at_epoch_ms: enrich::now_epoch_ms()?,
        binary_name: binary_name.map(|name| name.to_string()),
        lock: lock_status,
        requirements: Vec::new(),
        missing_artifacts: Vec::new(),
        blockers: vec![blocker],
        decision: enrich::Decision::Blocked,
        decision_reason: Some(format!("blockers present: {}", code)),
        next_action: enrich::NextAction::Edit {
            path: "enrich/config.json".to_string(),
            content: stub,
            reason: "enrich/config.json invalid; replace with a minimal stub".to_string(),
        },
        warnings: Vec::new(),
        force_used,
    })
}

fn build_parse_error_summary(
    paths: &enrich::DocPackPaths,
    binary_name: Option<&str>,
    config_state: &ConfigState,
    lock_status: enrich::LockStatus,
    lock_parse_error: Option<String>,
    plan_parse_error: Option<String>,
    force_used: bool,
) -> Result<enrich::StatusSummary> {
    let lock_parse_error_present = lock_parse_error.is_some();
    let plan_parse_error_present = plan_parse_error.is_some();
    let mut blockers = Vec::new();

    if let Some(message) = lock_parse_error {
        let evidence = paths.evidence_from_path(&paths.lock_path())?;
        blockers.push(enrich::Blocker {
            code: "lock_parse_error".to_string(),
            message,
            evidence: vec![evidence],
            next_action: None,
        });
    }

    if let Some(message) = plan_parse_error {
        let evidence = paths.evidence_from_path(&paths.plan_path())?;
        blockers.push(enrich::Blocker {
            code: "plan_parse_error".to_string(),
            message,
            evidence: vec![evidence],
            next_action: None,
        });
    }

    let mut next_action = match config_state {
        ConfigState::Missing => {
            let bootstrap_ok = enrich::load_bootstrap_optional(paths.root())
                .ok()
                .flatten()
                .is_some();
            if paths.pack_manifest_path().is_file() || bootstrap_ok {
                Some(enrich::NextAction::Command {
                    command: format!("bman init --doc-pack {}", paths.root().display()),
                    reason: "enrich/config.json missing".to_string(),
                })
            } else {
                Some(enrich::NextAction::Edit {
                    path: "enrich/bootstrap.json".to_string(),
                    content: enrich::bootstrap_stub(),
                    reason: "pack missing; init requires binary; set enrich/bootstrap.json"
                        .to_string(),
                })
            }
        }
        ConfigState::Valid(config) => {
            let missing_inputs = enrich::resolve_inputs(config, paths.root()).is_err();
            if missing_inputs {
                if !paths.pack_manifest_path().is_file() {
                    Some(enrich::NextAction::Edit {
                        path: "enrich/bootstrap.json".to_string(),
                        content: enrich::bootstrap_stub(),
                        reason: "pack missing; init requires binary; set enrich/bootstrap.json"
                            .to_string(),
                    })
                } else {
                    Some(enrich::NextAction::Edit {
                        path: "enrich/config.json".to_string(),
                        content: enrich::config_stub(),
                        reason: "config inputs missing; replace with a minimal stub".to_string(),
                    })
                }
            } else {
                None
            }
        }
        ConfigState::Invalid { .. } => Some(enrich::NextAction::Edit {
            path: "enrich/config.json".to_string(),
            content: enrich::config_stub(),
            reason: "enrich/config.json invalid; replace with a minimal stub".to_string(),
        }),
    };

    if next_action.is_none() {
        if lock_parse_error_present {
            next_action = Some(enrich::NextAction::Command {
                command: format!("bman validate --doc-pack {}", paths.root().display()),
                reason: "lock parse error; regenerate via validate".to_string(),
            });
        } else if plan_parse_error_present {
            let (command, reason) = if lock_status.present && !lock_status.stale {
                (
                    format!("bman plan --doc-pack {}", paths.root().display()),
                    "plan parse error; regenerate via plan".to_string(),
                )
            } else {
                (
                    format!("bman validate --doc-pack {}", paths.root().display()),
                    "plan parse error; lock missing or stale".to_string(),
                )
            };
            next_action = Some(enrich::NextAction::Command { command, reason });
        }
    }

    let next_action = next_action.unwrap_or_else(|| enrich::NextAction::Command {
        command: format!("bman status --doc-pack {}", paths.root().display()),
        reason: "status blocked; recheck when needed".to_string(),
    });
    let codes: Vec<String> = blockers
        .iter()
        .map(|blocker| blocker.code.clone())
        .collect();

    Ok(enrich::StatusSummary {
        schema_version: 1,
        generated_at_epoch_ms: enrich::now_epoch_ms()?,
        binary_name: binary_name.map(|name| name.to_string()),
        lock: lock_status,
        requirements: Vec::new(),
        missing_artifacts: Vec::new(),
        blockers,
        decision: enrich::Decision::Blocked,
        decision_reason: Some(format!("blockers present: {}", codes.join(", "))),
        next_action,
        warnings: Vec::new(),
        force_used,
    })
}

struct EnrichContext {
    paths: enrich::DocPackPaths,
    manifest: Option<pack::PackManifest>,
    binary_name: Option<String>,
    config: enrich::EnrichConfig,
    config_exists: bool,
    lock: Option<enrich::EnrichLock>,
    lock_status: enrich::LockStatus,
    plan: Option<enrich::EnrichPlan>,
}

impl EnrichContext {
    fn load(doc_pack_root: PathBuf) -> Result<Self> {
        let paths = enrich::DocPackPaths::new(doc_pack_root);
        let manifest = load_manifest_optional(&paths)?;
        let binary_name = manifest.as_ref().map(|m| m.binary_name.clone());

        let config_exists = paths.config_path().is_file();
        let config = if config_exists {
            enrich::load_config(paths.root())?
        } else {
            enrich::default_config()
        };

        let lock = if paths.lock_path().is_file() {
            Some(enrich::load_lock(paths.root())?)
        } else {
            None
        };
        let lock_status = enrich::lock_status(paths.root(), lock.as_ref())?;

        let plan = if paths.plan_path().is_file() {
            Some(crate::status::load_plan(paths.root())?)
        } else {
            None
        };

        Ok(Self {
            paths,
            manifest,
            binary_name,
            config,
            config_exists,
            lock,
            lock_status,
            plan,
        })
    }

    fn binary_name(&self) -> Option<&str> {
        self.binary_name.as_deref()
    }

    fn require_config(&self) -> Result<()> {
        if self.config_exists {
            return Ok(());
        }
        Err(anyhow!(
            "missing enrich config at {} (run `bman init --doc-pack {}` first)",
            self.paths.config_path().display(),
            self.paths.root().display()
        ))
    }

    fn lock_for_plan(&self, force: bool) -> Result<(enrich::EnrichLock, enrich::LockStatus, bool)> {
        let mut force_used = false;
        let lock = match self.lock.as_ref() {
            Some(lock) => lock.clone(),
            None => {
                if !force {
                    return Err(anyhow!(
                        "missing lock at {} (run `bman validate --doc-pack {}` or pass --force)",
                        self.paths.lock_path().display(),
                        self.paths.root().display()
                    ));
                }
                force_used = true;
                enrich::build_lock(self.paths.root(), &self.config, self.binary_name())?
            }
        };

        let lock_status = enrich::lock_status(self.paths.root(), Some(&lock))?;
        if lock_status.stale && !force {
            return Err(anyhow!(
                "stale lock at {} (run `bman validate --doc-pack {}` or pass --force)",
                self.paths.lock_path().display(),
                self.paths.root().display()
            ));
        }
        if lock_status.stale {
            force_used = true;
        }
        Ok((lock, lock_status, force_used))
    }

    fn lock_for_apply(
        &self,
        force: bool,
    ) -> Result<(Option<enrich::EnrichLock>, enrich::LockStatus, bool)> {
        let lock_status = self.lock_status.clone();
        let force_used = force && (!lock_status.present || lock_status.stale);
        if (!lock_status.present || lock_status.stale) && !force {
            return Err(anyhow!(
                "missing or stale lock at {} (run `bman validate --doc-pack {}` or pass --force)",
                self.paths.lock_path().display(),
                self.paths.root().display()
            ));
        }
        Ok((self.lock.clone(), lock_status, force_used))
    }

    fn require_plan(&self) -> Result<enrich::EnrichPlan> {
        self.plan.clone().ok_or_else(|| {
            anyhow!(
                "missing plan at {} (run `bman plan --doc-pack {}` first)",
                self.paths.plan_path().display(),
                self.paths.root().display()
            )
        })
    }
}

fn load_manifest_optional(paths: &enrich::DocPackPaths) -> Result<Option<pack::PackManifest>> {
    let pack_root = paths.pack_root();
    let manifest_path = paths.pack_manifest_path();
    if !manifest_path.is_file() {
        return Ok(None);
    }
    Ok(Some(pack::load_manifest(&pack_root)?))
}

fn load_examples_report_optional(
    paths: &enrich::DocPackPaths,
) -> Result<Option<scenarios::ExamplesReport>> {
    let path = paths.examples_report_path();
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let report: scenarios::ExamplesReport =
        serde_json::from_slice(&bytes).context("parse examples report")?;
    Ok(Some(report))
}

fn ensure_manifest_binary_name(
    manifest: Option<&pack::PackManifest>,
    binary_name: Option<&str>,
) -> Result<String> {
    if let Some(name) = binary_name {
        return Ok(name.to_string());
    }
    if let Some(manifest) = manifest {
        return Ok(manifest.binary_name.clone());
    }
    Err(anyhow!("binary name unavailable; manifest missing"))
}

fn compute_lens_templates(doc_pack_root: &Path, config: &enrich::EnrichConfig) -> Vec<PathBuf> {
    config
        .usage_lens_templates
        .iter()
        .map(|rel| doc_pack_root.join(rel))
        .collect()
}

fn resolve_pack_context_for_templates(
    pack_root: &Path,
    templates: &[PathBuf],
) -> Result<pack::PackContext> {
    let mut errors = Vec::new();
    for template in templates {
        match pack::load_pack_context_with_template(pack_root, template) {
            Ok(context) => return Ok(context),
            Err(err) => errors.push(format!("{}: {}", template.display(), err)),
        }
    }
    Err(anyhow!(
        "all usage lens templates failed: {}",
        errors.join("; ")
    ))
}

fn load_surface_for_render(
    staging_root: &Path,
    paths: &enrich::DocPackPaths,
) -> Result<Option<surface::SurfaceInventory>> {
    let staged_surface = staging_root.join("inventory").join("surface.json");
    let surface_path = if staged_surface.is_file() {
        staged_surface
    } else {
        paths.surface_path()
    };
    if !surface_path.is_file() {
        return Ok(None);
    }
    Ok(Some(surface::load_surface_inventory(&surface_path)?))
}

fn install_usage_lens_templates(paths: &enrich::DocPackPaths, force: bool) -> Result<()> {
    write_usage_lens_template(
        paths.root(),
        enrich::PROBE_LENS_TEMPLATE_REL,
        templates::USAGE_FROM_PROBES_SQL,
        force,
    )?;
    write_usage_lens_template(
        paths.root(),
        enrich::SCOPED_USAGE_LENS_TEMPLATE_REL,
        templates::USAGE_FROM_SCOPED_USAGE_FUNCTIONS_SQL,
        force,
    )?;
    write_usage_lens_template(
        paths.root(),
        enrich::SUBCOMMANDS_FROM_PROBES_TEMPLATE_REL,
        templates::SUBCOMMANDS_FROM_PROBES_SQL,
        force,
    )?;
    Ok(())
}

fn install_probe_plan(paths: &enrich::DocPackPaths, force: bool) -> Result<()> {
    let path = paths.probes_plan_path();
    if path.is_file() && !force {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let plan = surface::default_probe_plan();
    let text = serde_json::to_string_pretty(&plan).context("serialize probe plan")?;
    fs::write(&path, text.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn write_usage_lens_template(
    doc_pack_root: &Path,
    rel_path: &str,
    contents: &str,
    force: bool,
) -> Result<()> {
    let path = doc_pack_root.join(rel_path);
    if path.is_file() && !force {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&path, contents.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}
