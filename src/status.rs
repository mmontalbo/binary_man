use crate::enrich;
use crate::surface;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct PlanStatus {
    pub present: bool,
    pub stale: bool,
}

#[derive(Deserialize)]
struct RunsIndex {
    #[serde(default)]
    run_count: Option<usize>,
    #[serde(default)]
    runs: Vec<serde_json::Value>,
}

struct EvalResult {
    requirements: Vec<enrich::RequirementStatus>,
    missing_artifacts: Vec<String>,
    blockers: Vec<enrich::Blocker>,
    decision: enrich::Decision,
    decision_reason: Option<String>,
    warnings: Vec<String>,
}

pub fn build_status_summary(
    doc_pack_root: &Path,
    binary_name: Option<&str>,
    config: &enrich::EnrichConfig,
    config_exists: bool,
    lock_status: enrich::LockStatus,
    plan_status: PlanStatus,
    force_used: bool,
) -> Result<enrich::StatusSummary> {
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    let mut eval = evaluate_requirements(&paths, binary_name, config, &lock_status)?;
    if !config_exists {
        let config_rel = paths.rel_path(&paths.config_path())?;
        eval.missing_artifacts.push(config_rel);
        eval.warnings.push("enrich/config.json missing".to_string());
    }

    let missing_inputs = config_exists && enrich::resolve_inputs(config, doc_pack_root).is_err();
    let next_action = if missing_inputs {
        next_action_for_missing_inputs(&paths)
    } else {
        determine_next_action(
            doc_pack_root,
            config_exists,
            &lock_status,
            &plan_status,
            &eval.decision,
            &eval.requirements,
        )
    };

    Ok(enrich::StatusSummary {
        schema_version: 1,
        generated_at_epoch_ms: enrich::now_epoch_ms()?,
        binary_name: binary_name.map(|name| name.to_string()),
        lock: lock_status,
        requirements: eval.requirements,
        missing_artifacts: eval.missing_artifacts,
        blockers: eval.blockers,
        decision: eval.decision,
        decision_reason: eval.decision_reason,
        next_action,
        warnings: eval.warnings,
        force_used,
    })
}

fn evaluate_requirements(
    paths: &enrich::DocPackPaths,
    binary_name: Option<&str>,
    config: &enrich::EnrichConfig,
    lock_status: &enrich::LockStatus,
) -> Result<EvalResult> {
    let mut requirements = Vec::new();
    let mut missing_artifacts = Vec::new();
    let mut blockers = Vec::new();
    let warnings = Vec::new();

    for req in enrich::normalized_requirements(config) {
        match req {
            enrich::RequirementId::Surface => {
                let surface_path = paths.surface_path();
                let evidence = paths.evidence_from_path(&surface_path)?;
                if !surface_path.is_file() {
                    missing_artifacts.push(evidence.path.clone());
                    requirements.push(enrich::RequirementStatus {
                        id: req.clone(),
                        status: enrich::RequirementState::Unmet,
                        reason: "surface inventory missing".to_string(),
                        evidence: vec![evidence],
                        blockers: Vec::new(),
                    });
                    continue;
                }

                let surface = match surface::load_surface_inventory(&surface_path) {
                    Ok(surface) => surface,
                    Err(err) => {
                        let blocker = enrich::Blocker {
                            code: "surface_parse_error".to_string(),
                            message: err.to_string(),
                            evidence: vec![evidence.clone()],
                            next_action: None,
                        };
                        blockers.push(blocker.clone());
                        requirements.push(enrich::RequirementStatus {
                            id: req.clone(),
                            status: enrich::RequirementState::Blocked,
                            reason: "surface inventory parse error".to_string(),
                            evidence: vec![evidence],
                            blockers: vec![blocker],
                        });
                        continue;
                    }
                };

                if let Err(err) = surface::validate_surface_inventory(&surface) {
                    let blocker = enrich::Blocker {
                        code: "surface_schema_invalid".to_string(),
                        message: err.to_string(),
                        evidence: vec![evidence.clone()],
                        next_action: Some("fix inventory/surface.json".to_string()),
                    };
                    blockers.push(blocker.clone());
                    requirements.push(enrich::RequirementStatus {
                        id: req.clone(),
                        status: enrich::RequirementState::Blocked,
                        reason: "surface inventory schema invalid".to_string(),
                        evidence: vec![evidence],
                        blockers: vec![blocker],
                    });
                    continue;
                }

                let local_blockers = surface.blockers.clone();
                blockers.extend(local_blockers.clone());
                let meaningful_items = surface::meaningful_surface_items(&surface);
                let is_stale = lock_status.present
                    && !lock_status.stale
                    && match (
                        surface.inputs_hash.as_deref(),
                        lock_status.inputs_hash.as_deref(),
                    ) {
                        (Some(surface_hash), Some(lock_hash)) => surface_hash != lock_hash,
                        (None, Some(_)) => true,
                        _ => false,
                    };
                let (status, reason, req_blockers) = if !local_blockers.is_empty() {
                    (
                        enrich::RequirementState::Blocked,
                        "surface blockers present".to_string(),
                        local_blockers,
                    )
                } else if meaningful_items < 1 {
                    (
                        enrich::RequirementState::Unmet,
                        "surface inventory missing items".to_string(),
                        Vec::new(),
                    )
                } else if is_stale {
                    (
                        enrich::RequirementState::Unmet,
                        "surface inventory stale relative to lock".to_string(),
                        Vec::new(),
                    )
                } else {
                    (
                        enrich::RequirementState::Met,
                        "surface inventory present".to_string(),
                        Vec::new(),
                    )
                };
                requirements.push(enrich::RequirementStatus {
                    id: req.clone(),
                    status,
                    reason,
                    evidence: vec![evidence],
                    blockers: req_blockers,
                });
            }
            enrich::RequirementId::CoverageLedger => {
                let surface_path = paths.surface_path();
                let surface_evidence = paths.evidence_from_path(&surface_path)?;
                let mut evidence = vec![surface_evidence.clone()];
                let mut local_blockers = Vec::new();
                let mut unmet = Vec::new();

                if !surface_path.is_file() {
                    missing_artifacts.push(surface_evidence.path.clone());
                    unmet.push("surface inventory missing".to_string());
                } else {
                    match surface::load_surface_inventory(&surface_path) {
                        Ok(surface) => {
                            if let Err(err) = surface::validate_surface_inventory(&surface) {
                                let blocker = enrich::Blocker {
                                    code: "surface_schema_invalid".to_string(),
                                    message: err.to_string(),
                                    evidence: vec![surface_evidence.clone()],
                                    next_action: Some("fix inventory/surface.json".to_string()),
                                };
                                local_blockers.push(blocker);
                            } else if surface::meaningful_surface_items(&surface) < 1 {
                                unmet.push("surface inventory missing items".to_string());
                            }
                        }
                        Err(err) => {
                            let blocker = enrich::Blocker {
                                code: "surface_parse_error".to_string(),
                                message: err.to_string(),
                                evidence: vec![surface_evidence.clone()],
                                next_action: None,
                            };
                            local_blockers.push(blocker);
                        }
                    }
                }

                match config.scenario_catalogs.first() {
                    Some(rel) => {
                        let scenario_path = paths.root().join(rel);
                        let scenario_evidence = paths.evidence_from_path(&scenario_path)?;
                        evidence.push(scenario_evidence.clone());
                        if !scenario_path.is_file() {
                            missing_artifacts.push(scenario_evidence.path.clone());
                            unmet.push("scenario catalog missing".to_string());
                        }
                    }
                    None => {
                        let config_evidence = paths.evidence_from_path(&paths.config_path())?;
                        evidence.push(config_evidence);
                        unmet.push("scenario catalog missing".to_string());
                    }
                }

                let (status, reason) = if !local_blockers.is_empty() {
                    (
                        enrich::RequirementState::Blocked,
                        "coverage inputs blocked".to_string(),
                    )
                } else if !unmet.is_empty() {
                    (
                        enrich::RequirementState::Unmet,
                        format!("coverage inputs missing: {}", unmet.join("; ")),
                    )
                } else {
                    (
                        enrich::RequirementState::Met,
                        "coverage inputs present".to_string(),
                    )
                };
                blockers.extend(local_blockers.clone());
                requirements.push(enrich::RequirementStatus {
                    id: req.clone(),
                    status,
                    reason,
                    evidence,
                    blockers: local_blockers,
                });
            }
            enrich::RequirementId::ExamplesReport => {
                let runs_index_path = paths.pack_root().join("runs").join("index.json");
                let runs_evidence = paths.evidence_from_path(&runs_index_path)?;
                let mut evidence = vec![runs_evidence.clone()];
                let mut local_blockers = Vec::new();
                let mut unmet = Vec::new();

                match config.scenario_catalogs.first() {
                    Some(rel) => {
                        let scenario_path = paths.root().join(rel);
                        let scenario_evidence = paths.evidence_from_path(&scenario_path)?;
                        evidence.push(scenario_evidence.clone());
                        if !scenario_path.is_file() {
                            missing_artifacts.push(scenario_evidence.path.clone());
                            unmet.push("scenario catalog missing".to_string());
                        }
                    }
                    None => {
                        let config_evidence = paths.evidence_from_path(&paths.config_path())?;
                        evidence.push(config_evidence);
                        unmet.push("scenario catalog missing".to_string());
                    }
                }

                if !runs_index_path.is_file() {
                    missing_artifacts.push(runs_evidence.path.clone());
                    unmet.push("scenario runs index missing".to_string());
                } else {
                    let bytes = std::fs::read(&runs_index_path)
                        .with_context(|| format!("read {}", runs_index_path.display()))?;
                    let index: RunsIndex = match serde_json::from_slice(&bytes) {
                        Ok(index) => index,
                        Err(err) => {
                            let blocker = enrich::Blocker {
                                code: "runs_index_parse_error".to_string(),
                                message: err.to_string(),
                                evidence: vec![runs_evidence.clone()],
                                next_action: None,
                            };
                            local_blockers.push(blocker);
                            RunsIndex {
                                run_count: Some(0),
                                runs: Vec::new(),
                            }
                        }
                    };
                    let count = index.run_count.unwrap_or(index.runs.len());
                    if count == 0 {
                        unmet.push("no scenario runs recorded".to_string());
                    }
                }

                let (status, reason) = if !local_blockers.is_empty() {
                    (
                        enrich::RequirementState::Blocked,
                        "scenario runs blocked".to_string(),
                    )
                } else if !unmet.is_empty() {
                    (
                        enrich::RequirementState::Unmet,
                        format!("scenario runs missing: {}", unmet.join("; ")),
                    )
                } else {
                    (
                        enrich::RequirementState::Met,
                        "scenario runs present".to_string(),
                    )
                };
                blockers.extend(local_blockers.clone());
                requirements.push(enrich::RequirementStatus {
                    id: req.clone(),
                    status,
                    reason,
                    evidence,
                    blockers: local_blockers,
                });
            }
            enrich::RequirementId::ManPage => {
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
                        requirements.push(enrich::RequirementStatus {
                            id: req.clone(),
                            status: enrich::RequirementState::Blocked,
                            reason: "manifest missing".to_string(),
                            evidence: vec![evidence],
                            blockers: vec![blocker],
                        });
                        continue;
                    }
                };
                let surface_path = paths.surface_path();
                let surface_evidence = paths.evidence_from_path(&surface_path)?;
                let multi_command = if surface_path.is_file() {
                    match surface::load_surface_inventory(&surface_path) {
                        Ok(surface) => {
                            if surface::validate_surface_inventory(&surface).is_ok() {
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
                    let mut evidence = vec![evidence];
                    if multi_command {
                        evidence.push(surface_evidence.clone());
                    }
                    requirements.push(enrich::RequirementStatus {
                        id: req.clone(),
                        status: enrich::RequirementState::Unmet,
                        reason: "man page missing".to_string(),
                        evidence,
                        blockers: Vec::new(),
                    });
                } else {
                    let meta_path = paths.man_dir().join("meta.json");
                    let meta_evidence = paths.evidence_from_path(&meta_path)?;
                    let mut requirement_evidence = vec![evidence.clone(), meta_evidence.clone()];
                    if multi_command {
                        requirement_evidence.push(surface_evidence.clone());
                    }
                    let (status, reason, local_blockers) =
                        if lock_status.present && !lock_status.stale {
                            if !meta_path.is_file() {
                                missing_artifacts.push(meta_evidence.path.clone());
                                (
                                    enrich::RequirementState::Unmet,
                                    "man metadata missing".to_string(),
                                    Vec::new(),
                                )
                            } else {
                                #[derive(Deserialize)]
                                struct ManMetaInputs {
                                    #[serde(default)]
                                    inputs_hash: Option<String>,
                                }
                                let bytes = std::fs::read(&meta_path)
                                    .with_context(|| format!("read {}", meta_path.display()))?;
                                match serde_json::from_slice::<ManMetaInputs>(&bytes) {
                                    Ok(meta) => {
                                        let lock_hash = lock_status.inputs_hash.as_deref();
                                        let stale = match (meta.inputs_hash.as_deref(), lock_hash) {
                                            (Some(meta_hash), Some(lock_hash)) => {
                                                meta_hash != lock_hash
                                            }
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
                                    Err(err) => {
                                        let blocker = enrich::Blocker {
                                            code: "man_meta_parse_error".to_string(),
                                            message: err.to_string(),
                                            evidence: vec![meta_evidence.clone()],
                                            next_action: Some(format!(
                                                "bman apply --doc-pack {}",
                                                paths.root().display()
                                            )),
                                        };
                                        (
                                            enrich::RequirementState::Blocked,
                                            "man metadata parse error".to_string(),
                                            vec![blocker],
                                        )
                                    }
                                }
                            }
                        } else {
                            (
                                enrich::RequirementState::Met,
                                "man page present".to_string(),
                                Vec::new(),
                            )
                        };
                    let (status, reason, local_blockers) =
                        if status == enrich::RequirementState::Met && multi_command {
                            match std::fs::read_to_string(&man_path) {
                                Ok(text) => {
                                    if man_has_commands_section(&text) {
                                        (status, reason, local_blockers)
                                    } else {
                                        let blocker = enrich::Blocker {
                                            code: "man_commands_missing".to_string(),
                                            message:
                                                "man page missing COMMANDS section for subcommands"
                                                    .to_string(),
                                            evidence: requirement_evidence.clone(),
                                            next_action: Some(format!(
                                                "bman apply --doc-pack {}",
                                                paths.root().display()
                                            )),
                                        };
                                        (
                                            enrich::RequirementState::Blocked,
                                            "man page missing COMMANDS section".to_string(),
                                            vec![blocker],
                                        )
                                    }
                                }
                                Err(err) => {
                                    let blocker = enrich::Blocker {
                                        code: "man_read_error".to_string(),
                                        message: err.to_string(),
                                        evidence: requirement_evidence.clone(),
                                        next_action: Some(format!(
                                            "bman apply --doc-pack {}",
                                            paths.root().display()
                                        )),
                                    };
                                    (
                                        enrich::RequirementState::Blocked,
                                        "man page read error".to_string(),
                                        vec![blocker],
                                    )
                                }
                            }
                        } else {
                            (status, reason, local_blockers)
                        };
                    blockers.extend(local_blockers.clone());
                    requirements.push(enrich::RequirementStatus {
                        id: req.clone(),
                        status,
                        reason,
                        evidence: requirement_evidence,
                        blockers: local_blockers,
                    });
                }
            }
        }
    }

    let unmet: Vec<String> = requirements
        .iter()
        .filter(|req| req.status != enrich::RequirementState::Met)
        .map(|req| req.id.to_string())
        .collect();
    let decision = if !blockers.is_empty() {
        enrich::Decision::Blocked
    } else if unmet.is_empty() {
        enrich::Decision::Complete
    } else {
        enrich::Decision::Incomplete
    };
    let decision_reason = if !blockers.is_empty() {
        let codes: Vec<String> = blockers.iter().map(|b| b.code.clone()).collect();
        Some(format!("blockers present: {}", codes.join(", ")))
    } else if unmet.is_empty() {
        None
    } else {
        Some(format!("unmet requirements: {}", unmet.join(", ")))
    };

    Ok(EvalResult {
        requirements,
        missing_artifacts,
        blockers,
        decision,
        decision_reason,
        warnings,
    })
}

fn determine_next_action(
    doc_pack_root: &Path,
    config_exists: bool,
    lock_status: &enrich::LockStatus,
    plan_status: &PlanStatus,
    decision: &enrich::Decision,
    requirements: &[enrich::RequirementStatus],
) -> enrich::NextAction {
    if !config_exists {
        let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
        if paths.pack_manifest_path().is_file() {
            return enrich::NextAction::Command {
                command: format!("bman init --doc-pack {}", doc_pack_root.display()),
                reason: "enrich/config.json missing".to_string(),
            };
        }
        let bootstrap_ok = enrich::load_bootstrap_optional(doc_pack_root)
            .ok()
            .and_then(|bootstrap| bootstrap)
            .is_some();
        if bootstrap_ok {
            return enrich::NextAction::Command {
                command: format!("bman init --doc-pack {}", doc_pack_root.display()),
                reason: "enrich/config.json missing".to_string(),
            };
        }
        return enrich::NextAction::Edit {
            path: "enrich/bootstrap.json".to_string(),
            content: enrich::bootstrap_stub(),
            reason: "pack missing; init requires binary; set enrich/bootstrap.json".to_string(),
        };
    }
    if !lock_status.present || lock_status.stale {
        return enrich::NextAction::Command {
            command: format!("bman validate --doc-pack {}", doc_pack_root.display()),
            reason: "lock missing or stale".to_string(),
        };
    }
    if !plan_status.present || plan_status.stale {
        return enrich::NextAction::Command {
            command: format!("bman plan --doc-pack {}", doc_pack_root.display()),
            reason: "plan missing or stale".to_string(),
        };
    }
    if *decision != enrich::Decision::Complete {
        let reason = requirements
            .iter()
            .find(|req| req.status != enrich::RequirementState::Met)
            .map(|req| format!("address {}: {}", req.id, req.reason))
            .unwrap_or_else(|| "apply planned actions".to_string());
        return enrich::NextAction::Command {
            command: format!("bman apply --doc-pack {}", doc_pack_root.display()),
            reason,
        };
    }
    enrich::NextAction::Command {
        command: format!("bman status --doc-pack {}", doc_pack_root.display()),
        reason: "requirements met; recheck when needed".to_string(),
    }
}

fn next_action_for_missing_inputs(paths: &enrich::DocPackPaths) -> enrich::NextAction {
    if !paths.pack_manifest_path().is_file() {
        return enrich::NextAction::Edit {
            path: "enrich/bootstrap.json".to_string(),
            content: enrich::bootstrap_stub(),
            reason: "pack missing; init requires binary; set enrich/bootstrap.json".to_string(),
        };
    }
    enrich::NextAction::Edit {
        path: "enrich/config.json".to_string(),
        content: enrich::config_stub(),
        reason: "config inputs missing; replace with a minimal stub".to_string(),
    }
}

pub fn planned_actions_from_requirements(
    requirements: &[enrich::RequirementStatus],
) -> Vec<enrich::PlannedAction> {
    let mut actions = std::collections::BTreeSet::new();
    for req in requirements {
        if req.status == enrich::RequirementState::Met {
            continue;
        }
        actions.insert(req.id.planned_action());
    }
    actions.into_iter().collect()
}

pub fn plan_status(
    lock: Option<&enrich::EnrichLock>,
    plan: Option<&enrich::EnrichPlan>,
) -> PlanStatus {
    let Some(plan) = plan else {
        return PlanStatus {
            present: false,
            stale: false,
        };
    };
    let stale = match lock {
        Some(lock) => plan.lock.inputs_hash != lock.inputs_hash,
        None => true,
    };
    PlanStatus {
        present: true,
        stale,
    }
}

fn surface_is_multi_command(surface: &surface::SurfaceInventory) -> bool {
    surface.items.iter().any(|item| item.kind == "subcommand")
        || surface
            .blockers
            .iter()
            .any(|blocker| blocker.code == "surface_subcommands_missing")
}

fn man_has_commands_section(text: &str) -> bool {
    text.contains(".SH COMMANDS")
}

pub fn load_plan(doc_pack_root: &Path) -> Result<enrich::EnrichPlan> {
    let path = enrich::plan_path(doc_pack_root);
    if !path.is_file() {
        return Err(anyhow!(
            "missing plan at {} (run `bman plan --doc-pack {}` first)",
            path.display(),
            doc_pack_root.display()
        ));
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let plan: enrich::EnrichPlan = serde_json::from_slice(&bytes).context("parse plan JSON")?;
    if plan.schema_version != enrich::PLAN_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported plan schema_version {}",
            plan.schema_version
        ));
    }
    Ok(plan)
}

pub fn write_plan(doc_pack_root: &Path, plan: &enrich::EnrichPlan) -> Result<()> {
    let path = enrich::plan_path(doc_pack_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create enrich dir")?;
    }
    let text = serde_json::to_string_pretty(plan).context("serialize plan")?;
    std::fs::write(&path, text.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn print_status(doc_pack_root: &Path, summary: &enrich::StatusSummary) {
    println!("doc pack: {}", doc_pack_root.display());
    if let Some(binary) = summary.binary_name.as_ref() {
        println!("binary: {binary}");
    }
    let lock_state = if !summary.lock.present {
        "missing"
    } else if summary.lock.stale {
        "stale"
    } else {
        "fresh"
    };
    println!("lock: {lock_state}");
    println!("decision: {}", summary.decision);
    if let Some(reason) = summary.decision_reason.as_ref() {
        println!("decision detail: {reason}");
    }
    println!("requirements:");
    for req in &summary.requirements {
        println!("  - {}: {} ({})", req.id, req.status, req.reason);
    }
    if !summary.blockers.is_empty() {
        println!("blockers:");
        for blocker in &summary.blockers {
            println!("  - {}: {}", blocker.code, blocker.message);
        }
    }
    if !summary.missing_artifacts.is_empty() {
        println!("missing: {}", summary.missing_artifacts.join(", "));
    }
    match &summary.next_action {
        enrich::NextAction::Command { command, reason } => {
            println!("next: {}", command);
            println!("next detail: {reason}");
        }
        enrich::NextAction::Edit { path, reason, .. } => {
            println!("next edit: {}", path);
            println!("next detail: {reason}");
        }
    }
    if !summary.warnings.is_empty() {
        println!("warnings: {}", summary.warnings.join("; "));
    }
}
