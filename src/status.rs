//! Status summary generation for doc packs.
//!
//! This module computes a deterministic status summary from pack-owned artifacts.
//! The status drives the enrichment loop by providing a `next_action` that tells
//! the caller exactly what to do next.
//!
//! # Why Determinism Matters
//!
//! Status evaluation avoids heuristics because:
//! - Same inputs must produce same `next_action` (reproducible builds)
//! - External tools (LMs, scripts) can rely on predictable behavior
//! - Debugging is tractable when outputs are reproducible
//!
//! # Status Structure
//!
//! ```text
//! StatusSummary
//! ├── decision: Complete | NeedsAction | Blocked
//! ├── requirements: [RequirementStatus, ...]
//! │   ├── surface: Met/Unmet (CLI options discovered?)
//! │   ├── coverage: Met/Unmet (all options documented?)
//! │   ├── verification: Met/Unmet (all options tested?)
//! │   └── man_page: Met/Unmet (man page rendered?)
//! ├── next_action: Command | Edit | None
//! └── blockers: [Blocker, ...] (what's preventing progress)
//! ```
//!
//! # Submodules
//!
//! - [`coverage`]: Coverage requirement evaluation
//! - [`evaluate`]: Requirement evaluation framework and implementations
//! - [`help`]: Help text extraction from binary
//! - [`inputs`]: Input loading for status computation
//! - [`lens`]: binary_lens integration
//! - [`plan`]: Plan status and action computation
//! - [`scenario_failures`]: Scenario failure analysis
//! - [`verification`]: Verification status computation
//! - [`verification_policy`]: Policy rules for verification tiers
//!
//! # Next Action Protocol
//!
//! The `next_action` field tells callers what to do:
//!
//! - `Command { command, reason }`: Run this shell command
//! - `Edit { path, content, edit_strategy }`: Write this content to this file
//! - `None`: Status is complete or blocked
//!
//! External tools can implement the protocol without understanding internals.
use crate::enrich;
use crate::scenarios;
use anyhow::Result;
use std::path::Path;

mod coverage;
mod evaluate;
mod help;
mod inputs;
mod lens;
mod plan;
mod scenario_failures;
mod verification;
mod verification_policy;
pub use plan::{load_plan, plan_status, planned_actions_from_requirements, write_plan};
pub(crate) use verification::auto_verification_plan_summary;

/// Inputs required to build a full status summary.
pub struct BuildStatusSummaryArgs<'a> {
    pub doc_pack_root: &'a Path,
    pub binary_name: Option<&'a str>,
    pub config: &'a enrich::EnrichConfig,
    pub config_exists: bool,
    pub lock_status: enrich::LockStatus,
    pub plan_status: enrich::PlanStatus,
    pub include_full: bool,
    pub force_used: bool,
}

/// Build the status summary for a doc pack.
pub fn build_status_summary(args: BuildStatusSummaryArgs<'_>) -> Result<enrich::StatusSummary> {
    let BuildStatusSummaryArgs {
        doc_pack_root,
        binary_name,
        config,
        config_exists,
        lock_status,
        plan_status,
        include_full,
        force_used,
    } = args;
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    let mut eval =
        evaluate::evaluate_requirements(&paths, binary_name, config, &lock_status, include_full)?;
    let mut warnings = Vec::new();
    if !config_exists {
        let config_rel = paths.rel_path(&paths.config_path())?;
        eval.missing_artifacts.push(config_rel);
        warnings.push("enrich/config.json missing".to_string());
    }
    if !paths.scenarios_plan_path().is_file() {
        let plan_rel = paths.rel_path(&paths.scenarios_plan_path())?;
        eval.missing_artifacts.push(plan_rel);
        warnings.push("scenarios/plan.json missing".to_string());
    }
    let scenario_failures = scenario_failures::load_scenario_failures(&paths, &mut warnings)?;
    let missing_inputs = config_exists && enrich::resolve_inputs(config, doc_pack_root).is_err();
    let gating_ok = config_exists
        && lock_status.present
        && !lock_status.stale
        && plan_status.present
        && !plan_status.stale;
    let first_unmet = eval
        .requirements
        .iter()
        .find(|req| req.status != enrich::RequirementState::Met)
        .map(|req| req.id.clone());
    let first_unmet_is_scenarios = matches!(
        first_unmet.as_ref(),
        Some(
            enrich::RequirementId::Coverage
                | enrich::RequirementId::CoverageLedger
                | enrich::RequirementId::Verification
                | enrich::RequirementId::ExamplesReport
        )
    );
    let scenario_failure_next_action =
        (gating_ok && first_unmet_is_scenarios && !scenario_failures.is_empty()).then(|| {
            let plan_content =
                scenarios::load_plan_if_exists(&paths.scenarios_plan_path(), paths.root())
                    .ok()
                    .flatten()
                    .and_then(|plan| serde_json::to_string_pretty(&plan).ok())
                    .unwrap_or_else(|| scenarios::plan_stub(binary_name));
            enrich::NextAction::Edit {
                path: "scenarios/plan.json".to_string(),
                content: plan_content,
                reason: format!("edit scenario {}", scenario_failures[0].scenario_id),
                hint: Some("Fix failing scenario assertions".to_string()),
                edit_strategy: enrich::default_edit_strategy(),
                payload: None,
            }
        });
    let mut next_action = if missing_inputs {
        next_action_for_missing_inputs(&paths, binary_name)
    } else if config_exists && eval.man_semantics_next_action.is_some() {
        eval.man_semantics_next_action.clone().unwrap()
    } else if gating_ok
        && matches!(first_unmet.as_ref(), Some(enrich::RequirementId::ManPage))
        && eval.man_usage_next_action.is_some()
    {
        eval.man_usage_next_action.clone().unwrap()
    } else if gating_ok
        && matches!(first_unmet.as_ref(), Some(enrich::RequirementId::Coverage))
        && eval.coverage_next_action.is_some()
    {
        eval.coverage_next_action.clone().unwrap()
    } else if gating_ok
        && matches!(
            first_unmet.as_ref(),
            Some(enrich::RequirementId::Verification)
        )
        && eval.verification_next_action.is_some()
    {
        eval.verification_next_action.clone().unwrap()
    } else if gating_ok {
        if let Some(action) = scenario_failure_next_action {
            action
        } else {
            determine_next_action(
                doc_pack_root,
                config_exists,
                &lock_status,
                &plan_status,
                &eval.decision,
                &eval.requirements,
            )
        }
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
    enrich::normalize_next_action(&mut next_action);
    let man_meta = lens::read_man_meta(&paths);
    let man_warnings = man_meta
        .as_ref()
        .map(|meta| meta.warnings.clone())
        .unwrap_or_default();
    let lens_summary = lens::build_lens_summary(&paths, config, &mut warnings, man_meta.as_ref());

    let focus = build_action_focus(&eval.requirements, &next_action);

    Ok(enrich::StatusSummary {
        schema_version: 1,
        generated_at_epoch_ms: enrich::now_epoch_ms()?,
        binary_name: binary_name.map(|name| name.to_string()),
        lock: lock_status,
        plan: plan_status,
        requirements: eval.requirements,
        missing_artifacts: eval.missing_artifacts,
        blockers: eval.blockers,
        scenario_failures,
        decision: eval.decision,
        decision_reason: eval.decision_reason,
        focus,
        next_action,
        warnings,
        man_warnings,
        lens_summary,
        force_used,
    })
}

fn determine_next_action(
    doc_pack_root: &Path,
    config_exists: bool,
    lock_status: &enrich::LockStatus,
    plan_status: &enrich::PlanStatus,
    decision: &enrich::Decision,
    requirements: &[enrich::RequirementStatus],
) -> enrich::NextAction {
    if !config_exists {
        let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
        if paths.pack_manifest_path().is_file() {
            return enrich::NextAction::Command {
                command: format!("bman init --doc-pack {}", doc_pack_root.display()),
                reason: "enrich/config.json missing".to_string(),
                hint: Some("Initialize doc pack configuration".to_string()),
                payload: None,
            };
        }
        return enrich::NextAction::Command {
            command: format!(
                "bman init --doc-pack {} --binary <binary>",
                doc_pack_root.display()
            ),
            reason: "pack missing; init requires explicit --binary".to_string(),
            hint: Some("Initialize new doc pack with binary name".to_string()),
            payload: None,
        };
    }
    if !lock_status.present || lock_status.stale {
        return enrich::NextAction::Command {
            command: format!("bman apply --doc-pack {}", doc_pack_root.display()),
            reason: "lock missing or stale; apply will refresh".to_string(),
            hint: Some("Run to refresh lock state".to_string()),
            payload: None,
        };
    }
    if !plan_status.present || plan_status.stale {
        return enrich::NextAction::Command {
            command: format!("bman apply --doc-pack {}", doc_pack_root.display()),
            reason: "plan missing or stale; apply will refresh".to_string(),
            hint: Some("Run to refresh plan state".to_string()),
            payload: None,
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
            hint: Some("Run to execute pending actions".to_string()),
            payload: None,
        };
    }
    enrich::NextAction::Command {
        command: format!("bman status --doc-pack {}", doc_pack_root.display()),
        reason: "requirements met; recheck when needed".to_string(),
        hint: None,
        payload: None,
    }
}

pub(crate) fn next_action_for_missing_inputs(
    paths: &enrich::DocPackPaths,
    binary_name: Option<&str>,
) -> enrich::NextAction {
    if !paths.scenarios_plan_path().is_file() {
        return enrich::NextAction::Edit {
            path: "scenarios/plan.json".to_string(),
            content: scenarios::plan_stub(binary_name),
            reason: "scenarios/plan.json missing; create a minimal stub".to_string(),
            hint: Some("Create scenarios plan file".to_string()),
            edit_strategy: enrich::default_edit_strategy(),
            payload: None,
        };
    }
    if !paths.pack_manifest_path().is_file() {
        return enrich::NextAction::Command {
            command: format!(
                "bman init --doc-pack {} --binary <binary>",
                paths.root().display()
            ),
            reason: "pack missing; init requires explicit --binary".to_string(),
            hint: Some("Initialize doc pack".to_string()),
            payload: None,
        };
    }
    enrich::NextAction::Edit {
        path: "enrich/config.json".to_string(),
        content: enrich::config_stub(),
        reason: "config inputs missing; replace with a minimal stub".to_string(),
        hint: Some("Create config file".to_string()),
        edit_strategy: enrich::default_edit_strategy(),
        payload: None,
    }
}

/// Build an ActionFocus summarizing the primary focus area for the next action.
fn build_action_focus(
    requirements: &[enrich::RequirementStatus],
    next_action: &enrich::NextAction,
) -> Option<enrich::ActionFocus> {
    // Extract reason_code from next_action payload if present
    let payload_reason_code = match next_action {
        enrich::NextAction::Command { payload, .. } => {
            payload.as_ref().and_then(|p| p.reason_code.clone())
        }
        enrich::NextAction::Edit { payload, .. } => {
            payload.as_ref().and_then(|p| p.reason_code.clone())
        }
    };
    let payload_target_ids = match next_action {
        enrich::NextAction::Command { payload, .. } => payload
            .as_ref()
            .map(|p| p.target_ids.clone())
            .unwrap_or_default(),
        enrich::NextAction::Edit { payload, .. } => payload
            .as_ref()
            .map(|p| p.target_ids.clone())
            .unwrap_or_default(),
    };

    // Find the first unmet requirement
    let unmet = requirements
        .iter()
        .find(|r| r.status == enrich::RequirementState::Unmet)?;

    // Determine affected count and sample IDs
    let (affected_count, sample_ids) = if !payload_target_ids.is_empty() {
        let count = payload_target_ids.len();
        let sample: Vec<String> = payload_target_ids.into_iter().take(3).collect();
        (count, sample)
    } else if !unmet.unverified_ids.is_empty() {
        let count = unmet.unverified_ids.len();
        let sample: Vec<String> = unmet.unverified_ids.iter().take(3).cloned().collect();
        (count, sample)
    } else {
        (0, Vec::new())
    };

    Some(enrich::ActionFocus {
        requirement: unmet.id.to_string(),
        reason_code: payload_reason_code,
        affected_count,
        sample_ids,
    })
}

/// Print a human-readable status summary to stdout.
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
        enrich::NextAction::Command {
            command, reason, ..
        } => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::evidence::ScenarioEvidence;
    use crate::scenarios::{ScenarioIndex, ScenarioIndexEntry};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        path.push(format!("{prefix}-{nanos}-{}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn status_includes_scenario_failures_and_next_action() {
        let root = temp_dir("bman-status-failure");
        fs::create_dir_all(root.join("enrich")).unwrap();
        fs::create_dir_all(root.join("scenarios")).unwrap();
        fs::create_dir_all(root.join("inventory").join("scenarios")).unwrap();
        fs::create_dir_all(root.join("binary.lens").join("runs")).unwrap();
        fs::create_dir_all(root.join("binary_lens")).unwrap();
        fs::create_dir_all(root.join("queries")).unwrap();
        fs::write(
            root.join("queries").join("usage_from_scenarios.sql"),
            "-- test\n".as_bytes(),
        )
        .unwrap();
        fs::write(
            root.join("queries").join("surface_from_scenarios.sql"),
            "-- test\n".as_bytes(),
        )
        .unwrap();
        fs::write(
            root.join("queries").join("verification_from_scenarios.sql"),
            "-- test\n".as_bytes(),
        )
        .unwrap();
        fs::create_dir_all(root.join("queries").join("verification_from_scenarios")).unwrap();
        fs::write(
            root.join(enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[0]),
            "-- test section\n".as_bytes(),
        )
        .unwrap();
        fs::write(
            root.join(enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[1]),
            "-- test section\n".as_bytes(),
        )
        .unwrap();
        fs::write(
            root.join(enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[2]),
            "-- test section\n".as_bytes(),
        )
        .unwrap();
        fs::write(
            root.join(enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[3]),
            "-- test section\n".as_bytes(),
        )
        .unwrap();
        fs::write(
            root.join("binary_lens").join("export_plan.json"),
            "{}".as_bytes(),
        )
        .unwrap();

        let config = enrich::EnrichConfig {
            schema_version: enrich::CONFIG_SCHEMA_VERSION,
            usage_lens_template: enrich::SCENARIO_USAGE_LENS_TEMPLATE_REL.to_string(),
            requirements: vec![enrich::RequirementId::ExamplesReport],
            verification_tier: None,
            lm_command: None,
        };
        enrich::write_config(&root, &config).unwrap();

        let scenario = scenarios::ScenarioSpec {
            id: "fail".to_string(),
            kind: scenarios::ScenarioKind::Behavior,
            publish: false,
            argv: vec!["--help".to_string()],
            env: BTreeMap::new(),
            seed: None,
            cwd: None,
            timeout_seconds: None,
            net_mode: None,
            no_sandbox: None,
            no_strace: None,
            snippet_max_lines: None,
            snippet_max_bytes: None,
            coverage_tier: None,
            baseline_scenario_id: None,
            assertions: Vec::new(),
            covers: Vec::new(),
            coverage_ignore: true,
            expect: scenarios::ScenarioExpect {
                exit_code: Some(0),
                exit_signal: None,
                stdout_contains_all: Vec::new(),
                stdout_contains_any: Vec::new(),
                stdout_regex_all: Vec::new(),
                stdout_regex_any: Vec::new(),
                stderr_contains_all: Vec::new(),
                stderr_contains_any: Vec::new(),
                stderr_regex_all: Vec::new(),
                stderr_regex_any: Vec::new(),
            },
        };
        let plan = scenarios::ScenarioPlan {
            schema_version: 11,
            binary: None,
            default_env: BTreeMap::new(),
            defaults: None,
            coverage: None,
            verification: scenarios::VerificationPlan::default(),
            scenarios: vec![scenario],
        };
        let plan_text = serde_json::to_string_pretty(&plan).unwrap();
        fs::write(
            root.join("scenarios").join("plan.json"),
            plan_text.as_bytes(),
        )
        .unwrap();

        let evidence = ScenarioEvidence {
            schema_version: 3,
            generated_at_epoch_ms: 1,
            scenario_id: "fail".to_string(),
            argv: vec!["bin".to_string(), "--help".to_string()],
            env: BTreeMap::new(),
            cwd: None,
            timeout_seconds: None,
            net_mode: None,
            no_sandbox: None,
            no_strace: None,
            snippet_max_lines: 1,
            snippet_max_bytes: 1,
            exit_code: Some(1),
            exit_signal: None,
            timed_out: false,
            duration_ms: 1,
            stdout: String::new(),
            stderr: String::new(),
        };
        let evidence_path = root.join("inventory").join("scenarios").join("fail-1.json");
        fs::write(
            &evidence_path,
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();

        let index = ScenarioIndex {
            schema_version: 1,
            scenarios: vec![ScenarioIndexEntry {
                scenario_id: "fail".to_string(),
                scenario_digest: "abc".to_string(),
                last_run_epoch_ms: Some(1),
                last_pass: Some(false),
                failures: vec!["expected exit_code 0".to_string()],
                evidence_paths: vec!["inventory/scenarios/fail-1.json".to_string()],
            }],
        };
        fs::write(
            root.join("inventory").join("scenarios").join("index.json"),
            serde_json::to_vec_pretty(&index).unwrap(),
        )
        .unwrap();

        fs::write(
            root.join("binary.lens").join("runs").join("index.json"),
            br#"{"run_count":1,"runs":[]}"#,
        )
        .unwrap();

        let summary = build_status_summary(BuildStatusSummaryArgs {
            doc_pack_root: &root,
            binary_name: Some("bin"),
            config: &config,
            config_exists: true,
            lock_status: enrich::LockStatus {
                present: true,
                stale: false,
                inputs_hash: Some("hash".to_string()),
            },
            plan_status: enrich::PlanStatus {
                present: true,
                stale: false,
                inputs_hash: Some("hash".to_string()),
                lock_inputs_hash: Some("hash".to_string()),
            },
            include_full: false,
            force_used: false,
        })
        .unwrap();

        assert_eq!(summary.scenario_failures.len(), 1);
        assert_eq!(summary.scenario_failures[0].scenario_id, "fail");
        assert_eq!(
            summary.scenario_failures[0].evidence[0].path,
            "inventory/scenarios/fail-1.json"
        );
        match summary.next_action {
            enrich::NextAction::Command { command, .. } => {
                assert!(command.contains("bman status"));
            }
            _ => panic!("expected command next action"),
        }
        assert_eq!(summary.decision, enrich::Decision::Complete);

        let _ = fs::remove_dir_all(&root);
    }
}
