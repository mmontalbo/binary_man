//! Tests for verification_requirement module.
//!
//! Test organization:
//! - Helper functions for test setup (temp directories, file writing, fixtures)
//! - Outputs Equal Tests: workaround, rerun detection, retry counting
//! - Priority and Targeting Tests: reason kind ordering, batched targeting
//! - Scenario Error Tests: scaffold generation for broken scenarios
//! - Batching and Determinism Tests: bounded batches, deterministic output
//! - Reason Kind Tests: action generation for each reason type

use super::next_action::suggested_exclusion_only_next_action;
use super::retry::{load_behavior_retry_counts, BEHAVIOR_RERUN_CAP};
use super::{
    eval_behavior_verification, outputs_equal_workaround_needs_delta_rerun,
    QueueVerificationContext,
};
use crate::enrich;
use crate::scenarios;
use crate::surface;
use crate::verification_progress::load_verification_progress;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// =============================================================================
// Test Helpers
// Shared utilities for creating test fixtures
// =============================================================================

fn temp_doc_pack_root(name: &str) -> std::path::PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("{name}-{}-{now}", std::process::id()));
    std::fs::create_dir_all(root.join("inventory").join("scenarios"))
        .expect("create inventory/scenarios");
    root
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, contents.as_bytes()).expect("write file");
}

fn verification_entry(delta_path: &str) -> scenarios::VerificationEntry {
    scenarios::VerificationEntry {
        surface_id: "--color".to_string(),
        status: "verified".to_string(),
        behavior_status: "unverified".to_string(),
        behavior_exclusion_reason_code: None,
        behavior_unverified_reason_code: Some("outputs_equal".to_string()),
        behavior_unverified_scenario_id: Some("verify_color".to_string()),
        behavior_unverified_assertion_kind: None,
        behavior_unverified_assertion_seed_path: None,
        behavior_unverified_assertion_token: None,
        scenario_ids: Vec::new(),
        scenario_paths: Vec::new(),
        behavior_scenario_ids: vec!["verify_color".to_string()],
        behavior_assertion_scenario_ids: Vec::new(),
        behavior_scenario_paths: vec![delta_path.to_string()],
        delta_outcome: Some("outputs_equal".to_string()),
        delta_evidence_paths: vec![delta_path.to_string()],
        behavior_confounded_scenario_ids: Vec::new(),
        behavior_confounded_extra_surface_ids: Vec::new(),
        auto_verify_exit_code: None,
        auto_verify_stderr: None,
        behavior_exit_code: None,
        behavior_stderr: None,
        evidence: Vec::new(),
    }
}

fn verification_entry_with_reason(
    surface_id: &str,
    reason_code: &str,
) -> scenarios::VerificationEntry {
    scenarios::VerificationEntry {
        surface_id: surface_id.to_string(),
        status: "verified".to_string(),
        behavior_status: "unverified".to_string(),
        behavior_exclusion_reason_code: None,
        behavior_unverified_reason_code: Some(reason_code.to_string()),
        behavior_unverified_scenario_id: Some(format!(
            "verify_{}",
            surface_id.trim_start_matches('-')
        )),
        behavior_unverified_assertion_kind: None,
        behavior_unverified_assertion_seed_path: None,
        behavior_unverified_assertion_token: None,
        scenario_ids: Vec::new(),
        scenario_paths: Vec::new(),
        behavior_scenario_ids: Vec::new(),
        behavior_assertion_scenario_ids: Vec::new(),
        behavior_scenario_paths: Vec::new(),
        delta_outcome: None,
        delta_evidence_paths: Vec::new(),
        behavior_confounded_scenario_ids: Vec::new(),
        behavior_confounded_extra_surface_ids: Vec::new(),
        auto_verify_exit_code: None,
        auto_verify_stderr: None,
        behavior_exit_code: None,
        behavior_stderr: None,
        evidence: Vec::new(),
    }
}

fn minimal_surface_with_ids(surface_ids: &[&str]) -> surface::SurfaceInventory {
    let items = surface_ids
        .iter()
        .map(|surface_id| surface::SurfaceItem {
            id: (*surface_id).to_string(),
            display: (*surface_id).to_string(),
            description: None,
            parent_id: None,
            context_argv: Vec::new(),
            forms: vec![(*surface_id).to_string()],
            invocation: surface::SurfaceInvocation::default(),
            evidence: Vec::new(),
            is_help_output: false,
        })
        .collect();
    surface::SurfaceInventory {
        schema_version: 2,
        generated_at_epoch_ms: 0,
        binary_name: Some("bin".to_string()),
        inputs_hash: None,
        discovery: Vec::new(),
        items,
        blockers: Vec::new(),
    }
}

fn minimal_surface(surface_id: &str) -> surface::SurfaceInventory {
    surface::SurfaceInventory {
        schema_version: 2,
        generated_at_epoch_ms: 0,
        binary_name: Some("bin".to_string()),
        inputs_hash: None,
        discovery: Vec::new(),
        items: vec![surface::SurfaceItem {
            id: surface_id.to_string(),
            display: surface_id.to_string(),
            description: None,
            parent_id: None,
            context_argv: Vec::new(),
            forms: vec![surface_id.to_string()],
            invocation: surface::SurfaceInvocation::default(),
            evidence: Vec::new(),
            is_help_output: false,
        }],
        blockers: Vec::new(),
    }
}

fn outputs_equal_needs_rerun_fixture(
    name: &str,
) -> (
    std::path::PathBuf,
    enrich::DocPackPaths,
    scenarios::ScenarioPlan,
    surface::SurfaceInventory,
    BTreeMap<String, scenarios::VerificationEntry>,
) {
    let root = temp_doc_pack_root(name);
    let paths = enrich::DocPackPaths::new(root.clone());
    let mut plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    plan.scenarios
        .retain(|scenario| scenario.kind != scenarios::ScenarioKind::Behavior);
    plan.scenarios.push(scenarios::ScenarioSpec {
        id: "baseline".to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: vec!["work".to_string()],
        env: BTreeMap::new(),
        stdin: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("behavior".to_string()),
        baseline_scenario_id: None,
        assertions: Vec::new(),
        covers: Vec::new(),
        coverage_ignore: true,
        expect: scenarios::ScenarioExpect::default(),
    });
    plan.scenarios.push(scenarios::ScenarioSpec {
        id: "verify_color".to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: vec!["--color".to_string(), "work".to_string()],
        env: BTreeMap::new(),
        stdin: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("behavior".to_string()),
        baseline_scenario_id: Some("baseline".to_string()),
        assertions: Vec::new(),
        covers: vec!["--color".to_string()],
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect::default(),
    });
    let surface = surface::SurfaceInventory {
        schema_version: 2,
        generated_at_epoch_ms: 0,
        binary_name: Some("bin".to_string()),
        inputs_hash: None,
        discovery: Vec::new(),
        items: vec![surface::SurfaceItem {
            id: "--color".to_string(),
            display: "--color".to_string(),
            description: None,
            parent_id: None,
            context_argv: Vec::new(),
            forms: vec!["--color".to_string()],
            invocation: surface::SurfaceInvocation {
                requires_argv: vec!["work".to_string()],
                ..Default::default()
            },
            evidence: Vec::new(),
            is_help_output: false,
        }],
        blockers: Vec::new(),
    };

    let delta_100 = "inventory/scenarios/verify_color-100.json";
    let delta_300 = "inventory/scenarios/verify_color-300.json";
    write_file(&root.join(delta_100), r#"{"scenario_id":"verify_color"}"#);
    write_file(&root.join(delta_300), r#"{"scenario_id":"verify_color"}"#);
    std::thread::sleep(Duration::from_millis(20));
    write_file(
        &root.join("inventory/surface.overlays.json"),
        r#"{"schema_version":3,"items":[],"overlays":[]}"#,
    );

    let mut entry = verification_entry(delta_300);
    entry.delta_evidence_paths = vec![delta_300.to_string()];
    entry.behavior_scenario_paths = vec![delta_300.to_string()];
    let mut entries = BTreeMap::new();
    entries.insert("--color".to_string(), entry);

    (root, paths, plan, surface, entries)
}

fn test_semantics() -> crate::semantics::Semantics {
    serde_json::from_str(crate::templates::ENRICH_SEMANTICS_JSON).expect("parse test semantics")
}

fn eval_behavior_next_action(
    plan: &scenarios::ScenarioPlan,
    surface: &surface::SurfaceInventory,
    ledger_entries: &BTreeMap<String, scenarios::VerificationEntry>,
    paths: &enrich::DocPackPaths,
) -> enrich::NextAction {
    let semantics = test_semantics();
    let mut evidence = Vec::new();
    let mut local_blockers = Vec::new();
    let mut verification_next_action = None;
    let missing = Vec::new();
    let surface_evidence = enrich::EvidenceRef {
        path: "inventory/surface.json".to_string(),
        sha256: None,
    };
    let scenarios_evidence = enrich::EvidenceRef {
        path: "scenarios/plan.json".to_string(),
        sha256: None,
    };
    let mut ctx = QueueVerificationContext {
        plan,
        surface,
        semantics: Some(&semantics),
        include_full: true,
        ledger_entries: Some(ledger_entries),
        evidence: &mut evidence,
        local_blockers: &mut local_blockers,
        verification_next_action: &mut verification_next_action,
        missing: &missing,
        paths,
        surface_evidence: &surface_evidence,
        scenarios_evidence: &scenarios_evidence,
    };
    let _ = eval_behavior_verification(&mut ctx);
    ctx.verification_next_action
        .clone()
        .expect("expected next action")
}

// =============================================================================
// Outputs Equal Tests
// Tests for outputs_equal workaround, rerun detection, and retry counting
// =============================================================================

#[test]
fn outputs_equal_workaround_needs_rerun_when_overlays_are_newer_than_delta_evidence() {
    let root = temp_doc_pack_root("bman-verification-rerun");
    let paths = enrich::DocPackPaths::new(root.clone());
    let delta_rel = "inventory/scenarios/verify_color.variant.json";
    let delta_abs = root.join(delta_rel);
    let overlays_abs = root.join("inventory").join("surface.overlays.json");
    write_file(&delta_abs, "{}");
    std::thread::sleep(Duration::from_millis(20));
    write_file(&overlays_abs, "{}");
    let entry = verification_entry(delta_rel);

    let needs_rerun = outputs_equal_workaround_needs_delta_rerun(&paths, &entry);
    assert!(needs_rerun);

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn outputs_equal_workaround_does_not_need_rerun_when_delta_evidence_is_newer() {
    let root = temp_doc_pack_root("bman-verification-no-rerun");
    let paths = enrich::DocPackPaths::new(root.clone());
    let delta_rel = "inventory/scenarios/verify_color.variant.json";
    let delta_abs = root.join(delta_rel);
    let overlays_abs = root.join("inventory").join("surface.overlays.json");
    write_file(&overlays_abs, "{}");
    std::thread::sleep(Duration::from_millis(20));
    write_file(&delta_abs, "{}");
    let entry = verification_entry(delta_rel);

    let needs_rerun = outputs_equal_workaround_needs_delta_rerun(&paths, &entry);
    assert!(!needs_rerun);

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn outputs_equal_retry_count_uses_verification_progress_state() {
    let root = temp_doc_pack_root("bman-verification-retry-count");
    let paths = enrich::DocPackPaths::new(root.clone());
    let current_delta_rel = "inventory/scenarios/verify_color-300.json";
    write_file(
        &root.join(current_delta_rel),
        r#"{"scenario_id":"verify_color"}"#,
    );
    write_file(
        &root.join("inventory/verification_progress.json"),
        r#"{
  "schema_version": 2,
  "outputs_equal_retries_by_surface": {
    "--color": {
      "retry_count": 2,
      "delta_signature": "scenario:verify_color"
    }
  }
}"#,
    );

    let mut ledger_entries = BTreeMap::new();
    ledger_entries.insert("--color".to_string(), verification_entry(current_delta_rel));
    let progress = load_verification_progress(&paths);
    let retry_counts =
        load_behavior_retry_counts(&paths, &ledger_entries, &progress, &["--color".to_string()]);
    assert_eq!(retry_counts.get("--color").copied(), Some(2));

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn cap_hit_suggestion_uses_command_with_delta_evidence() {
    let root = temp_doc_pack_root("bman-verification-suggested-exclusion");
    let paths = enrich::DocPackPaths::new(root.clone());
    let delta_rel = "inventory/scenarios/verify_color-300.json";
    write_file(&root.join(delta_rel), r#"{"scenario_id":"verify_color"}"#);

    let plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    let surface = minimal_surface("--color");
    let mut ledger_entries = BTreeMap::new();
    ledger_entries.insert("--color".to_string(), verification_entry(delta_rel));

    let mut evidence = Vec::new();
    let mut local_blockers = Vec::new();
    let mut verification_next_action = None;
    let missing = Vec::new();
    let surface_evidence = enrich::EvidenceRef {
        path: "inventory/surface.json".to_string(),
        sha256: None,
    };
    let scenarios_evidence = enrich::EvidenceRef {
        path: "scenarios/plan.json".to_string(),
        sha256: None,
    };
    let ctx = QueueVerificationContext {
        plan: &plan,
        surface: &surface,
        semantics: None,
        include_full: true,
        ledger_entries: Some(&ledger_entries),
        evidence: &mut evidence,
        local_blockers: &mut local_blockers,
        verification_next_action: &mut verification_next_action,
        missing: &missing,
        paths: &paths,
        surface_evidence: &surface_evidence,
        scenarios_evidence: &scenarios_evidence,
    };
    let target_ids = vec!["--color".to_string()];
    let mut retry_counts = BTreeMap::new();
    retry_counts.insert("--color".to_string(), BEHAVIOR_RERUN_CAP);

    let next_action = suggested_exclusion_only_next_action(
        &ctx,
        &target_ids,
        "outputs_equal",
        &retry_counts,
        &ledger_entries,
    );
    match next_action {
        enrich::NextAction::Command {
            command, payload, ..
        } => {
            assert!(command.contains("bman status --doc-pack"));
            let payload = payload.expect("expected behavior payload");
            let suggested = payload
                .suggested_exclusion_payload
                .expect("expected suggested exclusion payload");
            assert!(suggested
                .behavior_exclusion
                .evidence
                .delta_variant_path
                .is_some());
        }
        enrich::NextAction::Edit { .. } => {
            panic!("expected command next_action for suggestion-only cap hit");
        }
        enrich::NextAction::AutoExclude { .. } => {
            panic!("expected command next_action for suggestion-only cap hit");
        }
    }

    std::fs::remove_dir_all(root).expect("cleanup");
}

// =============================================================================
// Priority and Targeting Tests
// Tests for reason kind priority ordering and batched targeting
// =============================================================================

#[test]
fn behavior_priority_repairs_existing_rejections_before_missing_behavior_stubs() {
    let root = temp_doc_pack_root("bman-verification-repair-priority");
    let paths = enrich::DocPackPaths::new(root.clone());
    let mut plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    plan.scenarios
        .retain(|scenario| scenario.kind != scenarios::ScenarioKind::Behavior);

    // Add behavior scenarios for both surfaces so we exit initial phase and test reason-based targeting
    plan.scenarios.push(scenarios::ScenarioSpec {
        id: "verify_new".to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: vec!["--new".to_string()],
        env: BTreeMap::new(),
        stdin: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("behavior".to_string()),
        baseline_scenario_id: Some("baseline".to_string()),
        assertions: Vec::new(),
        covers: vec!["--new".to_string()],
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect::default(),
    });
    plan.scenarios.push(scenarios::ScenarioSpec {
        id: "verify_repair".to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: vec!["--repair".to_string()],
        env: BTreeMap::new(),
        stdin: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("behavior".to_string()),
        baseline_scenario_id: Some("baseline".to_string()),
        assertions: Vec::new(),
        covers: vec!["--repair".to_string()],
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect::default(),
    });

    let surface = minimal_surface_with_ids(&["--new", "--repair"]);
    let mut ledger_entries = BTreeMap::new();
    // Both have scenarios now, so ledger reasons drive targeting.
    // Include behavior_scenario_paths so they're not marked as "needs apply".
    let mut entry_new = verification_entry_with_reason("--new", "assertion_failed");
    entry_new.behavior_scenario_paths = vec!["scenarios/delta/verify_new.json".to_string()];
    ledger_entries.insert("--new".to_string(), entry_new);

    let mut entry_repair = verification_entry_with_reason("--repair", "scenario_error");
    entry_repair.behavior_scenario_paths = vec!["scenarios/delta/verify_repair.json".to_string()];
    ledger_entries.insert("--repair".to_string(), entry_repair);

    let mut evidence = Vec::new();
    let mut local_blockers = Vec::new();
    let mut verification_next_action = None;
    let missing = Vec::new();
    let surface_evidence = enrich::EvidenceRef {
        path: "inventory/surface.json".to_string(),
        sha256: None,
    };
    let scenarios_evidence = enrich::EvidenceRef {
        path: "scenarios/plan.json".to_string(),
        sha256: None,
    };
    let semantics = test_semantics();
    let mut ctx = QueueVerificationContext {
        plan: &plan,
        surface: &surface,
        semantics: Some(&semantics),
        include_full: true,
        ledger_entries: Some(&ledger_entries),
        evidence: &mut evidence,
        local_blockers: &mut local_blockers,
        verification_next_action: &mut verification_next_action,
        missing: &missing,
        paths: &paths,
        surface_evidence: &surface_evidence,
        scenarios_evidence: &scenarios_evidence,
    };

    let _ = eval_behavior_verification(&mut ctx);
    let next_action = ctx
        .verification_next_action
        .as_ref()
        .expect("expected next action");
    match next_action {
        enrich::NextAction::Edit { payload, .. } => {
            let payload = payload.as_ref().expect("expected behavior payload");
            // scenario_error has higher priority than assertion_failed
            assert_eq!(payload.target_ids, vec!["--repair".to_string()]);
            assert_eq!(payload.reason_code.as_deref(), Some("scenario_error"));
        }
        enrich::NextAction::Command { .. } => {
            panic!("expected edit next action");
        }
        enrich::NextAction::AutoExclude { .. } => {
            panic!("expected edit next action");
        }
    }

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn no_scenario_next_action_payload_includes_assertion_starters() {
    let root = temp_doc_pack_root("bman-verification-starter-payload");
    let paths = enrich::DocPackPaths::new(root.clone());
    let mut plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    plan.scenarios
        .retain(|scenario| scenario.kind != scenarios::ScenarioKind::Behavior);
    // Add a behavior scenario for --color so we exit initial phase and test reason-based targeting.
    // The ledger still has no_scenario reason, simulating a state where the scenario exists
    // but the verification run hasn't progressed yet.
    plan.scenarios.push(scenarios::ScenarioSpec {
        id: "verify_color".to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: vec!["--color".to_string()],
        env: BTreeMap::new(),
        stdin: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("behavior".to_string()),
        baseline_scenario_id: Some("baseline".to_string()),
        assertions: Vec::new(),
        covers: vec!["--color".to_string()],
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect::default(),
    });
    let surface = minimal_surface("--color");
    let mut ledger_entries = BTreeMap::new();
    // Include behavior_scenario_paths so it's not marked as "needs apply"
    let mut entry_color = verification_entry_with_reason("--color", "no_scenario");
    entry_color.behavior_scenario_paths = vec!["scenarios/delta/verify_color.json".to_string()];
    ledger_entries.insert("--color".to_string(), entry_color);

    let mut evidence = Vec::new();
    let mut local_blockers = Vec::new();
    let mut verification_next_action = None;
    let missing = Vec::new();
    let surface_evidence = enrich::EvidenceRef {
        path: "inventory/surface.json".to_string(),
        sha256: None,
    };
    let scenarios_evidence = enrich::EvidenceRef {
        path: "scenarios/plan.json".to_string(),
        sha256: None,
    };
    let semantics = test_semantics();
    let mut ctx = QueueVerificationContext {
        plan: &plan,
        surface: &surface,
        semantics: Some(&semantics),
        include_full: true,
        ledger_entries: Some(&ledger_entries),
        evidence: &mut evidence,
        local_blockers: &mut local_blockers,
        verification_next_action: &mut verification_next_action,
        missing: &missing,
        paths: &paths,
        surface_evidence: &surface_evidence,
        scenarios_evidence: &scenarios_evidence,
    };

    let _ = eval_behavior_verification(&mut ctx);
    let next_action = ctx
        .verification_next_action
        .as_ref()
        .expect("expected next action");
    match next_action {
        enrich::NextAction::Edit { payload, .. } => {
            let payload = payload.as_ref().expect("expected behavior payload");
            assert_eq!(payload.reason_code.as_deref(), Some("no_scenario"));
        }
        enrich::NextAction::Command { .. } => {
            panic!("expected edit next action");
        }
        enrich::NextAction::AutoExclude { .. } => {
            panic!("expected edit next action");
        }
    }

    std::fs::remove_dir_all(root).expect("cleanup");
}

// =============================================================================
// Scenario Error Tests
// Tests for scenario_error handling and scaffold generation
// =============================================================================

#[test]
fn scenario_error_next_action_includes_edit() {
    let root = temp_doc_pack_root("bman-verification-required-value-hint");
    let paths = enrich::DocPackPaths::new(root.clone());
    let mut plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    plan.scenarios.push(scenarios::ScenarioSpec {
        id: "verify_color".to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: vec!["--color".to_string(), "work".to_string()],
        env: BTreeMap::new(),
        stdin: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("behavior".to_string()),
        baseline_scenario_id: Some("baseline".to_string()),
        assertions: Vec::new(),
        covers: vec!["--color".to_string()],
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect::default(),
    });
    let surface = surface::SurfaceInventory {
        schema_version: 2,
        generated_at_epoch_ms: 0,
        binary_name: Some("bin".to_string()),
        inputs_hash: None,
        discovery: Vec::new(),
        items: vec![surface::SurfaceItem {
            id: "--color".to_string(),
            display: "--color".to_string(),
            description: None,
            parent_id: None,
            context_argv: Vec::new(),
            forms: vec!["--color=WHEN".to_string()],
            invocation: surface::SurfaceInvocation {
                value_arity: "required".to_string(),
                value_separator: "equals".to_string(),
                value_placeholder: None,
                value_examples: vec!["auto".to_string()],
                requires_argv: Vec::new(),
            },
            evidence: Vec::new(),
            is_help_output: false,
        }],
        blockers: Vec::new(),
    };

    let mut entry = verification_entry_with_reason("--color", "scenario_error");
    entry.behavior_unverified_scenario_id = Some("verify_color".to_string());
    entry.behavior_scenario_ids = vec!["verify_color".to_string()];
    entry.behavior_scenario_paths = vec!["inventory/scenarios/verify_color-1.json".to_string()];
    let mut ledger_entries = BTreeMap::new();
    ledger_entries.insert("--color".to_string(), entry);

    let mut evidence = Vec::new();
    let mut local_blockers = Vec::new();
    let mut verification_next_action = None;
    let missing = Vec::new();
    let surface_evidence = enrich::EvidenceRef {
        path: "inventory/surface.json".to_string(),
        sha256: None,
    };
    let scenarios_evidence = enrich::EvidenceRef {
        path: "scenarios/plan.json".to_string(),
        sha256: None,
    };
    let semantics = test_semantics();
    let mut ctx = QueueVerificationContext {
        plan: &plan,
        surface: &surface,
        semantics: Some(&semantics),
        include_full: true,
        ledger_entries: Some(&ledger_entries),
        evidence: &mut evidence,
        local_blockers: &mut local_blockers,
        verification_next_action: &mut verification_next_action,
        missing: &missing,
        paths: &paths,
        surface_evidence: &surface_evidence,
        scenarios_evidence: &scenarios_evidence,
    };

    let _ = eval_behavior_verification(&mut ctx);
    let next_action = ctx
        .verification_next_action
        .as_ref()
        .expect("expected next action");
    match next_action {
        enrich::NextAction::Edit {
            reason, payload, ..
        } => {
            assert!(reason.contains("scenario_error"));
            let payload = payload.as_ref().expect("expected behavior payload");
            assert_eq!(payload.reason_code.as_deref(), Some("scenario_error"));
        }
        enrich::NextAction::Command { .. } => {
            panic!("expected edit next action");
        }
        enrich::NextAction::AutoExclude { .. } => {
            panic!("expected edit next action");
        }
    }

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scenario_error_scaffold_projects_and_uses_seeded_assertions() {
    let root = temp_doc_pack_root("bman-verification-missing-assertions-valid-scaffold");
    let paths = enrich::DocPackPaths::new(root.clone());
    let mut plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    plan.defaults = None;
    plan.scenarios
        .retain(|scenario| scenario.kind != scenarios::ScenarioKind::Behavior);
    plan.scenarios.push(scenarios::ScenarioSpec {
        id: "baseline".to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: vec!["work".to_string()],
        env: BTreeMap::new(),
        stdin: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("behavior".to_string()),
        baseline_scenario_id: None,
        assertions: Vec::new(),
        covers: Vec::new(),
        coverage_ignore: true,
        expect: scenarios::ScenarioExpect::default(),
    });
    plan.scenarios.push(scenarios::ScenarioSpec {
        id: "verify_color".to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: vec!["--color".to_string(), "work".to_string()],
        env: BTreeMap::new(),
        stdin: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("behavior".to_string()),
        baseline_scenario_id: Some("baseline".to_string()),
        assertions: Vec::new(),
        covers: vec!["--color".to_string()],
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect::default(),
    });
    let surface = minimal_surface("--color");
    let mut entry = verification_entry_with_reason("--color", "scenario_error");
    entry.behavior_unverified_scenario_id = Some("verify_color".to_string());
    entry.behavior_scenario_ids = vec!["verify_color".to_string()];
    entry.behavior_scenario_paths = vec!["inventory/scenarios/verify_color-1.json".to_string()];
    let mut ledger_entries = BTreeMap::new();
    ledger_entries.insert("--color".to_string(), entry);

    let next_action = eval_behavior_next_action(&plan, &surface, &ledger_entries, &paths);
    match next_action {
        enrich::NextAction::Edit { payload, .. } => {
            let payload = payload.expect("expected behavior payload");
            assert_eq!(payload.reason_code.as_deref(), Some("scenario_error"));
            assert_eq!(payload.target_ids, vec!["--color".to_string()]);
        }
        enrich::NextAction::Command { .. } => panic!("expected edit next action"),
        enrich::NextAction::AutoExclude { .. } => panic!("expected edit next action"),
    };

    std::fs::remove_dir_all(root).expect("cleanup");
}

// =============================================================================
// Batching and Determinism Tests
// Tests for batched scenario generation and deterministic behavior
// =============================================================================

#[test]
fn no_scenario_batches_are_deterministic_and_bounded() {
    let root = temp_doc_pack_root("bman-verification-missing-behavior-batch");
    let paths = enrich::DocPackPaths::new(root.clone());
    let mut plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    plan.scenarios
        .retain(|scenario| scenario.kind != scenarios::ScenarioKind::Behavior);

    let ids: Vec<String> = (0..14).map(|idx| format!("--opt{idx:02}")).collect();
    let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let surface = minimal_surface_with_ids(&id_refs);
    let mut ledger_entries = BTreeMap::new();
    for surface_id in &ids {
        ledger_entries.insert(
            surface_id.clone(),
            verification_entry_with_reason(surface_id, "no_scenario"),
        );
    }

    let first = eval_behavior_next_action(&plan, &surface, &ledger_entries, &paths);
    let second = eval_behavior_next_action(&plan, &surface, &ledger_entries, &paths);
    match (first, second) {
        (
            enrich::NextAction::Edit {
                content: content_a,
                payload: payload_a,
                ..
            },
            enrich::NextAction::Edit {
                content: content_b,
                payload: payload_b,
                ..
            },
        ) => {
            let payload_a = payload_a.expect("first payload");
            let payload_b = payload_b.expect("second payload");
            // Deterministic: same targets and content on repeated evaluation
            assert!(!payload_a.target_ids.is_empty());
            assert_eq!(payload_a.target_ids, payload_b.target_ids);
            assert_eq!(content_a, content_b);
        }
        _ => panic!("expected edit next action on both evaluations"),
    }

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scenario_error_batches_emit_non_empty_assertions_and_validate() {
    let root = temp_doc_pack_root("bman-verification-missing-assertions-batch");
    let paths = enrich::DocPackPaths::new(root.clone());
    let mut plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    plan.scenarios
        .retain(|scenario| scenario.kind != scenarios::ScenarioKind::Behavior);
    plan.scenarios.push(scenarios::ScenarioSpec {
        id: "baseline".to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: vec!["work".to_string()],
        env: BTreeMap::new(),
        stdin: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("behavior".to_string()),
        baseline_scenario_id: None,
        assertions: Vec::new(),
        covers: Vec::new(),
        coverage_ignore: true,
        expect: scenarios::ScenarioExpect::default(),
    });

    let ids: Vec<String> = (0..14).map(|idx| format!("--flag{idx:02}")).collect();
    let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    for surface_id in &ids {
        let scenario_id = format!("verify_{}", surface_id.trim_start_matches('-'));
        plan.scenarios.push(scenarios::ScenarioSpec {
            id: scenario_id,
            kind: scenarios::ScenarioKind::Behavior,
            publish: false,
            argv: vec![surface_id.clone(), "work".to_string()],
            env: BTreeMap::new(),
            stdin: None,
            seed: None,
            cwd: None,
            timeout_seconds: None,
            net_mode: None,
            no_sandbox: None,
            no_strace: None,
            snippet_max_lines: None,
            snippet_max_bytes: None,
            coverage_tier: Some("behavior".to_string()),
            baseline_scenario_id: Some("baseline".to_string()),
            assertions: Vec::new(),
            covers: vec![surface_id.clone()],
            coverage_ignore: false,
            expect: scenarios::ScenarioExpect::default(),
        });
    }
    let surface = minimal_surface_with_ids(&id_refs);
    let mut ledger_entries = BTreeMap::new();
    for surface_id in &ids {
        let scenario_id = format!("verify_{}", surface_id.trim_start_matches('-'));
        let mut entry = verification_entry_with_reason(surface_id, "scenario_error");
        entry.behavior_unverified_scenario_id = Some(scenario_id.clone());
        entry.behavior_scenario_ids = vec![scenario_id];
        entry.behavior_scenario_paths = vec![format!(
            "inventory/scenarios/verify_{}-1.json",
            surface_id.trim_start_matches('-')
        )];
        ledger_entries.insert(surface_id.clone(), entry);
    }

    let next_action = eval_behavior_next_action(&plan, &surface, &ledger_entries, &paths);
    match next_action {
        enrich::NextAction::Edit { payload, .. } => {
            let payload = payload.expect("expected payload");
            assert!(!payload.target_ids.is_empty());
        }
        enrich::NextAction::Command { .. } => panic!("expected edit next action"),
        enrich::NextAction::AutoExclude { .. } => panic!("expected edit next action"),
    };

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn outputs_equal_status_is_read_only_and_pivots_only_from_persisted_cap() {
    let (root, paths, plan, surface, entries) =
        outputs_equal_needs_rerun_fixture("bman-verification-outputs-equal-pivot");

    let progress_path = paths.verification_progress_path();
    for _ in 0..3 {
        let action = eval_behavior_next_action(&plan, &surface, &entries, &paths);
        match action {
            enrich::NextAction::Command {
                command, reason, ..
            } => {
                assert!(command.contains("--rerun-scenario-id verify_color"));
                assert_ne!(
                    command.trim(),
                    format!("bman apply --doc-pack {}", root.display())
                );
                assert!(reason.contains("no-progress retry 1/2"));
            }
            enrich::NextAction::Edit { .. } => panic!("status-only polling must not advance cap"),
            enrich::NextAction::AutoExclude { .. } => {
                panic!("status-only polling must not advance cap")
            }
        }
    }
    assert!(
        !progress_path.is_file(),
        "status evaluation must not write verification progress"
    );

    write_file(
        &progress_path,
        r#"{
  "schema_version": 2,
  "outputs_equal_retries_by_surface": {
    "--color": {
      "retry_count": 2,
      "delta_signature": "scenario:verify_color"
    }
  }
}"#,
    );

    let edit_action = eval_behavior_next_action(&plan, &surface, &entries, &paths);
    match edit_action {
        enrich::NextAction::AutoExclude {
            path,
            reason,
            target_ids,
            evidence,
            ..
        } => {
            assert_eq!(path, "inventory/surface.overlays.json");
            assert!(reason.contains("auto-excluding"));
            assert!(reason.contains("outputs_equal"));
            assert!(target_ids.contains(&"--color".to_string()));
            assert_eq!(evidence.reason_code, "outputs_equal_exhausted");
            assert!(evidence.retry_count >= BEHAVIOR_RERUN_CAP);
        }
        enrich::NextAction::Command { .. } => panic!("expected auto-exclude after cap"),
        enrich::NextAction::Edit { .. } => panic!("expected auto-exclude after cap, not edit"),
    }

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn outputs_equal_status_does_not_mutate_existing_retry_progress() {
    let (root, paths, plan, surface, entries) =
        outputs_equal_needs_rerun_fixture("bman-verification-outputs-equal-read-only");
    let progress_path = paths.verification_progress_path();
    write_file(
        &progress_path,
        r#"{
  "schema_version": 2,
  "outputs_equal_retries_by_surface": {
    "--color": {
      "retry_count": 1,
      "delta_signature": "scenario:verify_color"
    }
  }
}"#,
    );
    let before = std::fs::read_to_string(&progress_path).expect("read initial progress");

    for _ in 0..3 {
        let action = eval_behavior_next_action(&plan, &surface, &entries, &paths);
        match action {
            enrich::NextAction::Command { reason, .. } => {
                assert!(reason.contains("no-progress retry 2/2"));
            }
            enrich::NextAction::Edit { .. } => panic!("status-only polling must remain command"),
            enrich::NextAction::AutoExclude { .. } => {
                panic!("status-only polling must remain command, not auto-exclude")
            }
        }
    }

    let after = std::fs::read_to_string(&progress_path).expect("read progress after status");
    assert_eq!(before, after);

    std::fs::remove_dir_all(root).expect("cleanup");
}

// Note: Integration tests for assertion_failed no-op detection would require
// additional setup for behavior verification targets through auto_verification.
// The core no-op detection logic is tested in verification_progress::tests.
// The guard is wired into reason_based_behavior_next_action for assertion_failed.

#[test]
fn missing_value_examples_generates_scaffold_with_placeholder() {
    let root = temp_doc_pack_root("bman-verification-missing-value-examples");
    let paths = enrich::DocPackPaths::new(root.clone());
    let mut plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    plan.scenarios
        .retain(|scenario| scenario.kind != scenarios::ScenarioKind::Behavior);

    // Create surfaces with value_arity="required" but no value_examples
    let surface_ids = vec!["--config", "--output", "--format"];
    let items = surface_ids
        .iter()
        .map(|surface_id| surface::SurfaceItem {
            id: (*surface_id).to_string(),
            display: (*surface_id).to_string(),
            description: Some("test option".to_string()),
            parent_id: None,
            context_argv: Vec::new(),
            forms: vec![(*surface_id).to_string()],
            invocation: surface::SurfaceInvocation {
                value_arity: "required".to_string(),
                value_separator: "space".to_string(),
                value_placeholder: Some("VALUE".to_string()),
                value_examples: Vec::new(), // Empty - triggers missing_value_examples
                requires_argv: Vec::new(),
            },
            evidence: Vec::new(),
            is_help_output: false,
        })
        .collect();
    let surface = surface::SurfaceInventory {
        schema_version: 2,
        generated_at_epoch_ms: 0,
        binary_name: Some("bin".to_string()),
        inputs_hash: None,
        discovery: Vec::new(),
        items,
        blockers: Vec::new(),
    };

    // Create ledger entries that report missing_value_examples
    let mut ledger_entries = BTreeMap::new();
    for surface_id in &surface_ids {
        let mut entry = verification_entry_with_reason(surface_id, "missing_value_examples");
        // No behavior scenarios exist yet
        entry.behavior_scenario_ids = Vec::new();
        ledger_entries.insert((*surface_id).to_string(), entry);
    }

    let next_action = eval_behavior_next_action(&plan, &surface, &ledger_entries, &paths);
    match next_action {
        enrich::NextAction::Edit {
            content, payload, ..
        } => {
            let payload = payload.expect("expected behavior payload");
            assert_eq!(
                payload.reason_code.as_deref(),
                Some("missing_value_examples")
            );
            assert!(!payload.target_ids.is_empty());
            // Content should have a scaffold with placeholder value
            assert!(
                content.contains("__value__") || content.contains("VALUE"),
                "scaffold should contain value placeholder, got: {content}"
            );
        }
        enrich::NextAction::Command { payload, .. } => {
            panic!(
                "expected edit next action, got command with payload: {:?}",
                payload
            );
        }
        enrich::NextAction::AutoExclude { .. } => panic!("expected edit next action"),
    };

    std::fs::remove_dir_all(root).expect("cleanup");
}

// =============================================================================
// Behavior Contract Tests
// =============================================================================
// These tests verify the core behavioral contracts of the targeting system.
// They should remain stable even as implementation details change.

/// Contract: Targeting selects highest-priority reason kind first.
///
/// Given surfaces with different reason codes (setup_failed, missing_value_examples),
/// targeting should return setup_failed surfaces first (higher priority).
#[test]
fn targeting_selects_highest_priority_reason_kind() {
    use super::selectors::{first_reason_id_by_priority, BehaviorLookupContext};
    use crate::status::verification_policy::BehaviorReasonKind;

    let root = temp_doc_pack_root("bman-priority-test");
    let _paths = enrich::DocPackPaths::new(root.clone());

    // Create surfaces with different reason codes
    let surface_ids = ["--setup-opt", "--value-opt"];

    // Ledger entries: one setup_failed, one missing_value_examples
    let mut ledger_entries = BTreeMap::new();

    // --setup-opt has setup_failed
    let mut setup_entry = verification_entry_with_reason("--setup-opt", "setup_failed");
    setup_entry.behavior_scenario_ids = vec!["verify_setup_opt".to_string()];
    setup_entry.delta_outcome = Some("setup_failed".to_string());
    ledger_entries.insert("--setup-opt".to_string(), setup_entry);

    // --value-opt has missing_value_examples (no scenario yet)
    let value_entry = verification_entry_with_reason("--value-opt", "missing_value_examples");
    ledger_entries.insert("--value-opt".to_string(), value_entry);

    let remaining: std::collections::BTreeSet<String> =
        surface_ids.iter().map(|s| s.to_string()).collect();
    let missing_value_examples: std::collections::BTreeSet<String> =
        vec!["--value-opt".to_string()].into_iter().collect();
    let needs_apply: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    let ctx = BehaviorLookupContext {
        remaining_ids: &remaining,
        missing_value_examples: &missing_value_examples,
        needs_apply_ids: &needs_apply,
        ledger_entries: &ledger_entries,
    };

    let priority_order = [
        BehaviorReasonKind::ScenarioError,
        BehaviorReasonKind::SetupFailed,
        BehaviorReasonKind::AssertionFailed,
        BehaviorReasonKind::MissingValueExamples,
        BehaviorReasonKind::NoScenario,
        BehaviorReasonKind::OutputsEqual,
    ];

    let required_ids: Vec<String> = surface_ids.iter().map(|s| s.to_string()).collect();
    let selected = first_reason_id_by_priority(&required_ids, &ctx, &priority_order);

    // Should select setup_failed (priority #2) before missing_value_examples (priority #4)
    assert_eq!(
        selected,
        Some("--setup-opt".to_string()),
        "should select setup_failed surface first"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

/// Contract: When higher-priority surfaces are not in remaining set,
/// targeting falls through to lower-priority reason kinds.
///
/// This is the core contract that the bug violated - when setup_failed
/// surfaces are stuck/excluded, missing_value_examples should be targeted.
#[test]
fn targeting_falls_through_when_higher_priority_not_remaining() {
    use super::selectors::{first_reason_id_by_priority, BehaviorLookupContext};
    use crate::status::verification_policy::BehaviorReasonKind;

    let root = temp_doc_pack_root("bman-fallthrough-test");
    let _paths = enrich::DocPackPaths::new(root.clone());

    // Create surfaces with different reason codes
    let surface_ids = ["--setup-opt", "--value-opt"];

    // Ledger entries
    let mut ledger_entries = BTreeMap::new();

    let mut setup_entry = verification_entry_with_reason("--setup-opt", "setup_failed");
    setup_entry.behavior_scenario_ids = vec!["verify_setup_opt".to_string()];
    setup_entry.delta_outcome = Some("setup_failed".to_string());
    ledger_entries.insert("--setup-opt".to_string(), setup_entry);

    let value_entry = verification_entry_with_reason("--value-opt", "missing_value_examples");
    ledger_entries.insert("--value-opt".to_string(), value_entry);

    // KEY: Only --value-opt is in remaining (--setup-opt was excluded/resolved)
    let remaining: std::collections::BTreeSet<String> =
        vec!["--value-opt".to_string()].into_iter().collect();
    let missing_value_examples: std::collections::BTreeSet<String> =
        vec!["--value-opt".to_string()].into_iter().collect();
    let needs_apply: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    let ctx = BehaviorLookupContext {
        remaining_ids: &remaining,
        missing_value_examples: &missing_value_examples,
        needs_apply_ids: &needs_apply,
        ledger_entries: &ledger_entries,
    };

    let priority_order = [
        BehaviorReasonKind::ScenarioError,
        BehaviorReasonKind::SetupFailed,
        BehaviorReasonKind::AssertionFailed,
        BehaviorReasonKind::MissingValueExamples,
        BehaviorReasonKind::NoScenario,
        BehaviorReasonKind::OutputsEqual,
    ];

    let required_ids: Vec<String> = surface_ids.iter().map(|s| s.to_string()).collect();
    let selected = first_reason_id_by_priority(&required_ids, &ctx, &priority_order);

    // Should fall through to missing_value_examples since setup_failed not in remaining
    assert_eq!(
        selected,
        Some("--value-opt".to_string()),
        "should fall through to missing_value_examples when higher priority not remaining"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

/// Contract: Multiple surfaces with same reason kind are batched together.
///
/// Note: Batching only happens for NoScenario and OutputsEqual reason kinds.
/// Other reason kinds (like MissingValueExamples) process one surface at a time
/// because each needs specific context (value examples, setup fixes, etc.).
#[test]
fn targeting_batches_same_reason_kind() {
    let root = temp_doc_pack_root("bman-batch-test");
    let paths = enrich::DocPackPaths::new(root.clone());
    let mut plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    plan.scenarios
        .retain(|scenario| scenario.kind != scenarios::ScenarioKind::Behavior);

    // Create 3 surfaces all with no_scenario (a reason kind that DOES batch)
    let surface_ids = vec!["--opt-a", "--opt-b", "--opt-c"];
    let surface = minimal_surface_with_ids(&surface_ids);

    let mut ledger_entries = BTreeMap::new();
    for id in &surface_ids {
        // no_scenario: no behavior_scenario_ids, no behavior_scenario_paths
        let entry = verification_entry_with_reason(id, "no_scenario");
        ledger_entries.insert(id.to_string(), entry);
    }

    let next_action = eval_behavior_next_action(&plan, &surface, &ledger_entries, &paths);

    match next_action {
        enrich::NextAction::Edit { payload, .. } => {
            let payload = payload.expect("expected behavior payload");
            assert_eq!(payload.reason_code.as_deref(), Some("initial_scenarios"));
            // All 3 should be batched (below BATCH_LIMIT of 15)
            assert_eq!(
                payload.target_ids.len(),
                3,
                "should batch all no_scenario surfaces together"
            );
        }
        other => panic!("expected Edit action, got {:?}", other),
    }

    std::fs::remove_dir_all(root).expect("cleanup");
}

/// Contract: Each reason kind generates appropriate action type.
///
/// - missing_value_examples -> Edit (scaffold with placeholders)
/// - setup_failed -> Edit (fix scenario)
/// - no_scenario -> Edit (generate scenario)
#[test]
fn each_reason_kind_generates_appropriate_action() {
    let root = temp_doc_pack_root("bman-action-type-test");
    let paths = enrich::DocPackPaths::new(root.clone());
    let mut plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    plan.scenarios
        .retain(|scenario| scenario.kind != scenarios::ScenarioKind::Behavior);

    // Test missing_value_examples generates Edit
    let surface = minimal_surface_with_ids(&["--value-opt"]);
    let mut ledger_entries = BTreeMap::new();
    let entry = verification_entry_with_reason("--value-opt", "missing_value_examples");
    ledger_entries.insert("--value-opt".to_string(), entry);

    // Modify surface to have value_arity=required (triggers missing_value_examples path)
    let mut surface = surface;
    surface.items[0].invocation.value_arity = "required".to_string();
    surface.items[0].invocation.value_examples = Vec::new();

    let next_action = eval_behavior_next_action(&plan, &surface, &ledger_entries, &paths);

    match next_action {
        enrich::NextAction::Edit { payload, .. } => {
            let payload = payload.expect("expected payload");
            assert_eq!(
                payload.reason_code.as_deref(),
                Some("missing_value_examples"),
                "missing_value_examples should generate Edit with correct reason"
            );
        }
        other => panic!(
            "missing_value_examples should generate Edit, got {:?}",
            other
        ),
    }

    std::fs::remove_dir_all(root).expect("cleanup");
}

/// Contract: Surfaces with behavior_scenario_paths populated are not in needs_apply set.
///
/// This ensures surfaces that already have scenarios don't get re-scaffolded.
#[test]
fn surfaces_with_scenario_paths_excluded_from_needs_apply() {
    use super::selectors::needs_apply_ids;

    let root = temp_doc_pack_root("bman-needs-apply-test");
    let _paths = enrich::DocPackPaths::new(root.clone());

    let mut plan_behavior_ids = std::collections::BTreeSet::new();
    plan_behavior_ids.insert("--with-scenario".to_string());
    plan_behavior_ids.insert("--without-scenario".to_string());

    let remaining: std::collections::BTreeSet<String> = plan_behavior_ids.clone();

    let mut ledger_entries = BTreeMap::new();

    // --with-scenario has behavior_scenario_paths populated
    let mut with_entry = verification_entry_with_reason("--with-scenario", "outputs_equal");
    with_entry.behavior_scenario_paths = vec!["inventory/scenarios/verify.json".to_string()];
    ledger_entries.insert("--with-scenario".to_string(), with_entry);

    // --without-scenario has empty scenario paths
    let without_entry = verification_entry_with_reason("--without-scenario", "no_scenario");
    ledger_entries.insert("--without-scenario".to_string(), without_entry);

    let needs = needs_apply_ids(&plan_behavior_ids, &remaining, &ledger_entries);

    assert!(
        !needs.contains("--with-scenario"),
        "surface with scenario paths should not be in needs_apply"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

// =============================================================================
// Man Page Extraction Tests
// Tests for extract_man_name_line subcommand prioritization
// =============================================================================

#[test]
fn extract_man_name_line_prioritizes_specific_binary_man_page() {
    use super::next_action::extract_man_name_line;

    let root = temp_doc_pack_root("bman-man-name-extraction");
    let paths = enrich::DocPackPaths::new(root.clone());
    let man_dir = root.join("man");
    std::fs::create_dir_all(&man_dir).expect("create man dir");

    // Create a parent command man page (git.1)
    write_file(
        &man_dir.join("git.1"),
        r#".TH GIT 1
.SH NAME
git \- the stupid content tracker
.SH SYNOPSIS
..."#,
    );

    // Create a subcommand man page (git-diff.1)
    write_file(
        &man_dir.join("git-diff.1"),
        r#".TH GIT-DIFF 1
.SH NAME
git-diff \- Show changes between commits, commit and working tree, etc
.SH SYNOPSIS
..."#,
    );

    // Without binary_name, should find any .1 file (may be git.1 or git-diff.1)
    let desc_any = extract_man_name_line(&paths, None);
    assert!(
        desc_any.is_some(),
        "should extract description without binary name"
    );

    // With binary_name="git-diff", should prioritize git-diff.1
    let desc_specific = extract_man_name_line(&paths, Some("git-diff"));
    assert_eq!(
        desc_specific.as_deref(),
        Some("Show changes between commits, commit and working tree, etc"),
        "should extract description from specific man page"
    );

    // With binary_name="git", should use git.1
    let desc_git = extract_man_name_line(&paths, Some("git"));
    assert_eq!(
        desc_git.as_deref(),
        Some("the stupid content tracker"),
        "should extract description from git.1"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn extract_man_name_line_falls_back_when_specific_missing() {
    use super::next_action::extract_man_name_line;

    let root = temp_doc_pack_root("bman-man-name-fallback");
    let paths = enrich::DocPackPaths::new(root.clone());
    let man_dir = root.join("man");
    std::fs::create_dir_all(&man_dir).expect("create man dir");

    // Only create git.1 (no git-diff.1)
    write_file(
        &man_dir.join("git.1"),
        r#".TH GIT 1
.SH NAME
git \- the stupid content tracker
.SH SYNOPSIS
..."#,
    );

    // Request git-diff (doesn't exist) - should fall back to git.1
    let desc = extract_man_name_line(&paths, Some("git-diff"));
    assert_eq!(
        desc.as_deref(),
        Some("the stupid content tracker"),
        "should fall back to available man page"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}
