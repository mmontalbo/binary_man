//! Workflow status step.
//!
//! Status summarizes the pack deterministically and provides the next action.
use super::decisions;
use super::load_manifest_optional;
use crate::cli::StatusArgs;
use crate::docpack::doc_pack_root_for_status;
use crate::enrich;
use crate::scenarios;
use crate::status::{build_status_summary, plan_status};
use crate::surface;
use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

/// Status summary plus lock hash metadata for history recording.
pub struct StatusComputation {
    pub summary: enrich::StatusSummary,
    pub lock_inputs_hash: Option<String>,
}

/// Build a status summary for a doc pack without side effects.
pub fn status_summary_for_doc_pack(
    doc_pack_root: PathBuf,
    include_full: bool,
    force: bool,
) -> Result<StatusComputation> {
    let paths = enrich::DocPackPaths::new(doc_pack_root);
    let manifest = load_manifest_optional(&paths)?;
    let binary_name = manifest.as_ref().map(|m| m.binary_name.clone());

    let config_state = load_config_state(&paths);
    let config_exists = !matches!(config_state, ConfigState::Missing);
    let scenario_plan_state = load_scenario_plan_state(&paths);

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
    let force_used = force && (!lock_status.present || lock_status.stale);
    let parse_errors_present = lock_parse_error.is_some() || plan_parse_error.is_some();

    let summary = if let ScenarioPlanState::Invalid { code, message } = &scenario_plan_state {
        build_invalid_plan_summary(
            &paths,
            binary_name.as_deref(),
            lock_status,
            plan_status,
            code,
            message.clone(),
            force_used,
        )?
    } else if let ConfigState::Invalid { code, message } = &config_state {
        build_invalid_config_summary(
            &paths,
            binary_name.as_deref(),
            lock_status,
            plan_status,
            code,
            message.clone(),
            force_used,
        )?
    } else if parse_errors_present {
        build_parse_error_summary(ParseErrorSummaryArgs {
            paths: &paths,
            binary_name: binary_name.as_deref(),
            config_state: &config_state,
            lock_status,
            plan_status,
            lock_parse_error,
            plan_parse_error,
            force_used,
        })?
    } else {
        match config_state {
            ConfigState::Valid(config) => {
                build_status_summary(crate::status::BuildStatusSummaryArgs {
                    doc_pack_root: paths.root(),
                    binary_name: binary_name.as_deref(),
                    config: &config,
                    config_exists,
                    lock_status,
                    plan_status,
                    include_full,
                    force_used,
                })?
            }
            ConfigState::Missing => {
                let config = enrich::default_config();
                build_status_summary(crate::status::BuildStatusSummaryArgs {
                    doc_pack_root: paths.root(),
                    binary_name: binary_name.as_deref(),
                    config: &config,
                    config_exists: false,
                    lock_status,
                    plan_status,
                    include_full,
                    force_used,
                })?
            }
            ConfigState::Invalid { .. } => unreachable!("config invalid handled above"),
        }
    };

    Ok(StatusComputation {
        summary,
        lock_inputs_hash: lock.as_ref().map(|lock| lock.inputs_hash.clone()),
    })
}

/// Run the status step and print a summary or JSON output.
pub fn run_status(args: &StatusArgs) -> Result<()> {
    let doc_pack_root = doc_pack_root_for_status(&args.doc_pack)?;
    let computation = status_summary_for_doc_pack(doc_pack_root.clone(), args.full, args.force)?;
    let mut summary = computation.summary;
    let paths = enrich::DocPackPaths::new(doc_pack_root);

    if args.decisions {
        return run_decisions_output(&paths, summary.binary_name.as_deref());
    }

    if args.json {
        if !args.full {
            slim_status_for_actionability(&mut summary);
        }
        let text = serde_json::to_string_pretty(&summary).context("serialize status summary")?;
        println!("{text}");
    } else {
        crate::status::print_status(paths.root(), &summary);
    }

    if summary.force_used {
        let now = enrich::now_epoch_ms()?;
        let history_entry = enrich::EnrichHistoryEntry {
            schema_version: enrich::HISTORY_SCHEMA_VERSION,
            started_at_epoch_ms: now,
            finished_at_epoch_ms: now,
            step: "status".to_string(),
            inputs_hash: computation.lock_inputs_hash,
            outputs_hash: None,
            success: true,
            message: Some("force used".to_string()),
            force_used: summary.force_used,
        };
        enrich::append_history(paths.root(), &history_entry)?;
    }

    if !args.json && (!summary.lock.present || summary.lock.stale) && !args.force {
        return Err(anyhow!(
            "missing or stale lock at {} (run `bman apply --doc-pack {}` or pass --force)",
            paths.lock_path().display(),
            paths.root().display()
        ));
    }

    Ok(())
}

fn slim_status_for_actionability(summary: &mut enrich::StatusSummary) {
    for requirement in &mut summary.requirements {
        requirement.evidence.clear();
        for blocker in &mut requirement.blockers {
            blocker.evidence.clear();
        }
        if let Some(verification) = requirement.verification.as_mut() {
            verification.remaining_by_kind.clear();
            verification.excluded.clear();
            verification.behavior_excluded_reasons.clear();
            verification.behavior_unverified_diagnostics.clear();
            verification.stub_blockers_preview.clear();
            if verification.triaged_unverified_preview.len() > 5 {
                verification.triaged_unverified_preview.truncate(5);
            }
            if verification.behavior_unverified_preview.len() > 5 {
                verification.behavior_unverified_preview.truncate(5);
            }
        }
    }
    for blocker in &mut summary.blockers {
        blocker.evidence.clear();
    }
    for failure in &mut summary.scenario_failures {
        failure.evidence.clear();
    }
    for lens in &mut summary.lens_summary {
        lens.evidence.clear();
    }
    match &mut summary.next_action {
        enrich::NextAction::Command { payload, .. } => slim_behavior_next_action_payload(payload),
        enrich::NextAction::Edit { payload, .. } => slim_behavior_next_action_payload(payload),
    }
}

fn slim_behavior_next_action_payload(payload: &mut Option<enrich::BehaviorNextActionPayload>) {
    let Some(value) = payload.as_mut() else {
        return;
    };
    if value.target_ids.len() > 5 {
        value.target_ids.truncate(5);
    }
    if value.suggested_overlay_keys.len() > 3 {
        value.suggested_overlay_keys.truncate(3);
    }
    if value.assertion_starters.len() > 2 {
        value.assertion_starters.truncate(2);
    }
    if value.is_empty() {
        *payload = None;
    }
}

enum ConfigState {
    Missing,
    Valid(enrich::EnrichConfig),
    Invalid { code: &'static str, message: String },
}

enum ScenarioPlanState {
    Missing,
    Valid,
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

fn load_scenario_plan_state(paths: &enrich::DocPackPaths) -> ScenarioPlanState {
    let plan_path = paths.scenarios_plan_path();
    if !plan_path.is_file() {
        return ScenarioPlanState::Missing;
    }
    match scenarios::load_plan(&plan_path, paths.root()) {
        Ok(_) => ScenarioPlanState::Valid,
        Err(err) => ScenarioPlanState::Invalid {
            code: "scenario_plan_invalid",
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

struct BlockedStatusSummaryArgs<'a> {
    binary_name: Option<&'a str>,
    lock_status: enrich::LockStatus,
    plan_status: enrich::PlanStatus,
    blockers: Vec<enrich::Blocker>,
    decision_reason: String,
    next_action: enrich::NextAction,
    force_used: bool,
}

fn blocked_status_summary(args: BlockedStatusSummaryArgs<'_>) -> Result<enrich::StatusSummary> {
    let BlockedStatusSummaryArgs {
        binary_name,
        lock_status,
        plan_status,
        blockers,
        decision_reason,
        next_action,
        force_used,
    } = args;
    Ok(enrich::StatusSummary {
        schema_version: 1,
        generated_at_epoch_ms: enrich::now_epoch_ms()?,
        binary_name: binary_name.map(str::to_string),
        lock: lock_status,
        plan: plan_status,
        requirements: Vec::new(),
        missing_artifacts: Vec::new(),
        blockers,
        scenario_failures: Vec::new(),
        lens_summary: Vec::new(),
        decision: enrich::Decision::Blocked,
        decision_reason: Some(decision_reason),
        focus: None,
        next_action,
        warnings: Vec::new(),
        man_warnings: Vec::new(),
        force_used,
    })
}

fn blocker(code: &str, message: String, evidence: enrich::EvidenceRef) -> enrich::Blocker {
    enrich::Blocker {
        code: code.to_string(),
        message,
        evidence: vec![evidence],
        next_action: None,
    }
}

fn apply_refresh_next_action(paths: &enrich::DocPackPaths, reason: &str) -> enrich::NextAction {
    enrich::NextAction::Command {
        command: format!("bman apply --doc-pack {}", paths.root().display()),
        reason: reason.to_string(),
        hint: Some("Run to refresh doc pack state".to_string()),
        payload: None,
    }
}

fn build_invalid_config_summary(
    paths: &enrich::DocPackPaths,
    binary_name: Option<&str>,
    lock_status: enrich::LockStatus,
    plan_status: enrich::PlanStatus,
    code: &'static str,
    message: String,
    force_used: bool,
) -> Result<enrich::StatusSummary> {
    blocked_status_summary(BlockedStatusSummaryArgs {
        binary_name,
        lock_status,
        plan_status,
        blockers: vec![blocker(
            code,
            message,
            paths.evidence_from_path(&paths.config_path())?,
        )],
        decision_reason: format!("blockers present: {code}"),
        next_action: enrich::NextAction::Edit {
            path: "enrich/config.json".to_string(),
            content: enrich::config_stub(),
            reason: "enrich/config.json invalid; replace with a minimal stub".to_string(),
            hint: Some("Fix invalid config file".to_string()),
            edit_strategy: enrich::default_edit_strategy(),
            payload: None,
        },
        force_used,
    })
}

fn build_invalid_plan_summary(
    paths: &enrich::DocPackPaths,
    binary_name: Option<&str>,
    lock_status: enrich::LockStatus,
    plan_status: enrich::PlanStatus,
    code: &'static str,
    message: String,
    force_used: bool,
) -> Result<enrich::StatusSummary> {
    blocked_status_summary(BlockedStatusSummaryArgs {
        binary_name,
        lock_status,
        plan_status,
        blockers: vec![blocker(
            code,
            message,
            paths.evidence_from_path(&paths.scenarios_plan_path())?,
        )],
        decision_reason: format!("blockers present: {code}"),
        next_action: enrich::NextAction::Edit {
            path: "scenarios/plan.json".to_string(),
            content: scenarios::plan_stub(binary_name),
            reason: "scenarios/plan.json invalid; replace with a minimal stub".to_string(),
            hint: Some("Fix invalid plan file".to_string()),
            edit_strategy: enrich::default_edit_strategy(),
            payload: None,
        },
        force_used,
    })
}

struct ParseErrorSummaryArgs<'a> {
    paths: &'a enrich::DocPackPaths,
    binary_name: Option<&'a str>,
    config_state: &'a ConfigState,
    lock_status: enrich::LockStatus,
    plan_status: enrich::PlanStatus,
    lock_parse_error: Option<String>,
    plan_parse_error: Option<String>,
    force_used: bool,
}

fn build_parse_error_summary(args: ParseErrorSummaryArgs<'_>) -> Result<enrich::StatusSummary> {
    let ParseErrorSummaryArgs {
        paths,
        binary_name,
        config_state,
        lock_status,
        plan_status,
        lock_parse_error,
        plan_parse_error,
        force_used,
    } = args;
    let lock_parse_error_present = lock_parse_error.is_some();
    let plan_parse_error_present = plan_parse_error.is_some();
    let mut blockers = Vec::new();

    if let Some(message) = lock_parse_error {
        blockers.push(blocker(
            "lock_parse_error",
            message,
            paths.evidence_from_path(&paths.lock_path())?,
        ));
    }

    if let Some(message) = plan_parse_error {
        blockers.push(blocker(
            "plan_parse_error",
            message,
            paths.evidence_from_path(&paths.plan_path())?,
        ));
    }

    let mut next_action = match config_state {
        ConfigState::Missing => {
            if paths.pack_manifest_path().is_file() {
                Some(enrich::NextAction::Command {
                    command: format!("bman init --doc-pack {}", paths.root().display()),
                    reason: "enrich/config.json missing".to_string(),
                    hint: Some("Initialize config file".to_string()),
                    payload: None,
                })
            } else {
                Some(enrich::NextAction::Command {
                    command: format!(
                        "bman init --doc-pack {} --binary <binary>",
                        paths.root().display()
                    ),
                    reason: "pack missing; init requires explicit --binary".to_string(),
                    hint: Some("Initialize doc pack".to_string()),
                    payload: None,
                })
            }
        }
        ConfigState::Valid(config) => {
            let missing_inputs = enrich::resolve_inputs(config, paths.root()).is_err();
            missing_inputs
                .then(|| crate::status::next_action_for_missing_inputs(paths, binary_name))
        }
        ConfigState::Invalid { .. } => Some(enrich::NextAction::Edit {
            path: "enrich/config.json".to_string(),
            content: enrich::config_stub(),
            reason: "enrich/config.json invalid; replace with a minimal stub".to_string(),
            hint: Some("Fix invalid config file".to_string()),
            edit_strategy: enrich::default_edit_strategy(),
            payload: None,
        }),
    };

    if next_action.is_none() {
        next_action = lock_parse_error_present
            .then(|| apply_refresh_next_action(paths, "lock parse error; apply will refresh"))
            .or_else(|| {
                plan_parse_error_present.then(|| {
                    apply_refresh_next_action(paths, "plan parse error; apply will refresh")
                })
            });
    }

    let mut next_action = next_action.unwrap_or_else(|| enrich::NextAction::Command {
        command: format!("bman status --doc-pack {}", paths.root().display()),
        reason: "status blocked; recheck when needed".to_string(),
        hint: None,
        payload: None,
    });
    enrich::normalize_next_action(&mut next_action);
    let codes: Vec<String> = blockers
        .iter()
        .map(|blocker| blocker.code.clone())
        .collect();

    blocked_status_summary(BlockedStatusSummaryArgs {
        binary_name,
        lock_status,
        plan_status,
        blockers,
        decision_reason: format!("blockers present: {}", codes.join(", ")),
        next_action,
        force_used,
    })
}

/// Generate and print the LM-friendly decisions output.
fn run_decisions_output(paths: &enrich::DocPackPaths, binary_name: Option<&str>) -> Result<()> {
    let surface_path = paths.surface_path();
    if !surface_path.is_file() {
        return Err(anyhow!(
            "surface.json not found at {}; run `bman apply` first",
            surface_path.display()
        ));
    }

    let surface_inventory: surface::SurfaceInventory = serde_json::from_str(
        &std::fs::read_to_string(&surface_path).context("read surface inventory")?,
    )
    .context("parse surface inventory")?;

    let verification_binary = binary_name
        .map(|s| s.to_string())
        .or_else(|| surface_inventory.binary_name.clone())
        .unwrap_or_else(|| "<binary>".to_string());

    // Build verification ledger on-the-fly
    let scenarios_path = paths.scenarios_plan_path();
    let template_path = paths
        .root()
        .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);
    let ledger = scenarios::build_verification_ledger(
        &verification_binary,
        &surface_inventory,
        paths.root(),
        &scenarios_path,
        &template_path,
        None,
        Some(paths.root()),
    )
    .context("compute verification ledger for decisions output")?;

    let output = decisions::build_decisions(paths, binary_name, &ledger, &surface_inventory)?;
    let text = serde_json::to_string_pretty(&output).context("serialize decisions output")?;
    println!("{text}");

    Ok(())
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
