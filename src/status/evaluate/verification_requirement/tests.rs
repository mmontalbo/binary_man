
use super::{
    eval_behavior_verification, load_behavior_retry_counts,
    outputs_equal_workaround_needs_delta_rerun, suggested_exclusion_only_next_action,
    QueueVerificationContext, BEHAVIOR_RERUN_CAP,
};
use crate::enrich;
use crate::scenarios;
use crate::surface;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        evidence: Vec::new(),
    }
}

fn minimal_surface_with_ids(surface_ids: &[&str]) -> surface::SurfaceInventory {
    let items = surface_ids
        .iter()
        .map(|surface_id| surface::SurfaceItem {
            kind: "option".to_string(),
            id: (*surface_id).to_string(),
            display: (*surface_id).to_string(),
            description: None,
            forms: vec![(*surface_id).to_string()],
            invocation: surface::SurfaceInvocation::default(),
            evidence: Vec::new(),
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
            kind: "option".to_string(),
            id: surface_id.to_string(),
            display: surface_id.to_string(),
            description: None,
            forms: vec![surface_id.to_string()],
            invocation: surface::SurfaceInvocation::default(),
            evidence: Vec::new(),
        }],
        blockers: Vec::new(),
    }
}

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
fn retry_count_does_not_overcount_from_unrelated_historical_files() {
    let root = temp_doc_pack_root("bman-verification-retry-count");
    let paths = enrich::DocPackPaths::new(root.clone());
    let current_delta_rel = "inventory/scenarios/verify_color-300.json";
    write_file(
        &root.join(current_delta_rel),
        r#"{"scenario_id":"verify_color"}"#,
    );
    write_file(
        &root.join("inventory/scenarios/verify_color-100.json"),
        r#"{"scenario_id":"verify_color"}"#,
    );
    write_file(
        &root.join("inventory/scenarios/verify_color-200.json"),
        r#"{"scenario_id":"verify_color"}"#,
    );
    write_file(
        &root.join("inventory/scenarios/unrelated-999.json"),
        r#"{"scenario_id":"unrelated"}"#,
    );

    let mut ledger_entries = BTreeMap::new();
    ledger_entries.insert("--color".to_string(), verification_entry(current_delta_rel));
    let retry_counts = load_behavior_retry_counts(&paths, &ledger_entries);
    assert_eq!(retry_counts.get("--color").copied(), Some(0));

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn cap_hit_suggestion_uses_command_and_keeps_attempted_workarounds_non_empty() {
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
            assert!(!suggested
                .behavior_exclusion
                .evidence
                .attempted_workarounds
                .is_empty());
        }
        enrich::NextAction::Edit { .. } => {
            panic!("expected command next_action for suggestion-only cap hit");
        }
    }

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn behavior_priority_repairs_existing_rejections_before_missing_behavior_stubs() {
    let root = temp_doc_pack_root("bman-verification-repair-priority");
    let paths = enrich::DocPackPaths::new(root.clone());
    let mut plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    plan.scenarios
        .retain(|scenario| scenario.kind != scenarios::ScenarioKind::Behavior);

    let surface = minimal_surface_with_ids(&["--new", "--repair"]);
    let mut ledger_entries = BTreeMap::new();
    ledger_entries.insert(
        "--new".to_string(),
        verification_entry_with_reason("--new", "missing_behavior_scenario"),
    );
    ledger_entries.insert(
        "--repair".to_string(),
        verification_entry_with_reason("--repair", "seed_mismatch"),
    );

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
        plan: &plan,
        surface: &surface,
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
            assert_eq!(payload.target_ids, vec!["--repair".to_string()]);
            assert_eq!(payload.reason_code.as_deref(), Some("seed_mismatch"));
        }
        enrich::NextAction::Command { .. } => {
            panic!("expected edit next action");
        }
    }

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn missing_behavior_scenario_next_action_payload_includes_assertion_starters() {
    let root = temp_doc_pack_root("bman-verification-starter-payload");
    let paths = enrich::DocPackPaths::new(root.clone());
    let mut plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    plan.scenarios
        .retain(|scenario| scenario.kind != scenarios::ScenarioKind::Behavior);
    let surface = minimal_surface("--color");
    let mut ledger_entries = BTreeMap::new();
    ledger_entries.insert(
        "--color".to_string(),
        verification_entry_with_reason("--color", "missing_behavior_scenario"),
    );

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
        plan: &plan,
        surface: &surface,
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
            assert_eq!(
                payload.reason_code.as_deref(),
                Some("missing_behavior_scenario")
            );
            assert!(!payload.assertion_starters.is_empty());
            assert!(payload
                .assertion_starters
                .iter()
                .all(|starter| starter.kind != "variant_stdout_differs_from_baseline"));
        }
        enrich::NextAction::Command { .. } => {
            panic!("expected edit next action");
        }
    }

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn required_value_missing_next_action_includes_argv_rewrite_hint() {
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
        seed_dir: None,
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
            kind: "option".to_string(),
            id: "--color".to_string(),
            display: "--color".to_string(),
            description: None,
            forms: vec!["--color=WHEN".to_string()],
            invocation: surface::SurfaceInvocation {
                value_arity: "required".to_string(),
                value_separator: "equals".to_string(),
                value_placeholder: None,
                value_examples: vec!["auto".to_string()],
                requires_argv: Vec::new(),
            },
            evidence: Vec::new(),
        }],
        blockers: Vec::new(),
    };

    let mut entry = verification_entry_with_reason("--color", "required_value_missing");
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
    let mut ctx = QueueVerificationContext {
        plan: &plan,
        surface: &surface,
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
            assert!(reason.contains("required_value_missing"));
            assert!(reason.contains("scenario.argv should include `--color=auto`"));
            let payload = payload.as_ref().expect("expected behavior payload");
            assert_eq!(
                payload.reason_code.as_deref(),
                Some("required_value_missing")
            );
        }
        enrich::NextAction::Command { .. } => {
            panic!("expected edit next action");
        }
    }

    std::fs::remove_dir_all(root).expect("cleanup");
}
