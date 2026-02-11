use super::*;
use crate::verification_progress::load_verification_progress;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn apply_args(refresh_pack: bool) -> ApplyArgs {
    ApplyArgs {
        doc_pack: std::path::PathBuf::from("/tmp/doc-pack"),
        refresh_pack,
        verbose: false,
        rerun_all: false,
        rerun_failed: false,
        rerun_scenario_id: Vec::new(),
        lens_flake: "unused".to_string(),
        lm_response: None,
        max_cycles: 0,
        lm: None,
    }
}

fn temp_doc_pack_root(name: &str) -> std::path::PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("{name}-{}-{now}", std::process::id()));
    std::fs::create_dir_all(root.join("inventory")).expect("create inventory");
    root
}

fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, contents.as_bytes()).expect("write file");
}

fn outputs_equal_verification_entries(
    delta_rel: &str,
) -> BTreeMap<String, scenarios::VerificationEntry> {
    let entry = scenarios::VerificationEntry {
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
        behavior_scenario_paths: vec![delta_rel.to_string()],
        delta_outcome: Some("outputs_equal".to_string()),
        delta_evidence_paths: vec![delta_rel.to_string()],
        behavior_confounded_scenario_ids: Vec::new(),
        behavior_confounded_extra_surface_ids: Vec::new(),
        evidence: Vec::new(),
    };
    let mut entries = BTreeMap::new();
    entries.insert("--color".to_string(), entry);
    entries
}

struct OutputsEqualFixture {
    paths: enrich::DocPackPaths,
    entries: BTreeMap<String, scenarios::VerificationEntry>,
}

fn setup_outputs_equal_retry_fixture(root: &std::path::Path) -> OutputsEqualFixture {
    let paths = enrich::DocPackPaths::new(root.to_path_buf());
    let surface = crate::surface::SurfaceInventory {
        schema_version: 2,
        generated_at_epoch_ms: 0,
        binary_name: Some("bin".to_string()),
        inputs_hash: None,
        discovery: Vec::new(),
        items: vec![crate::surface::SurfaceItem {
            kind: "option".to_string(),
            id: "--color".to_string(),
            display: "--color".to_string(),
            description: None,
            forms: vec!["--color".to_string()],
            invocation: crate::surface::SurfaceInvocation {
                requires_argv: vec!["work".to_string()],
                ..Default::default()
            },
            evidence: Vec::new(),
        }],
        blockers: Vec::new(),
    };
    write_file(
        &paths.surface_path(),
        &serde_json::to_string_pretty(&surface).expect("serialize surface"),
    );

    let delta_rel = "inventory/scenarios/verify_color-300.json";
    write_file(
        &root.join(delta_rel),
        r#"{"scenario_id":"verify_color","schema_version":3}"#,
    );
    std::thread::sleep(Duration::from_millis(20));
    write_file(
        &paths.surface_overlays_path(),
        r#"{"schema_version":3,"items":[],"overlays":[]}"#,
    );

    let entries = outputs_equal_verification_entries(delta_rel);
    OutputsEqualFixture { paths, entries }
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

#[test]
fn executed_targeted_reruns_increment_outputs_equal_retry_progress() {
    let root = temp_doc_pack_root("bman-apply-progress-increment");
    let fixture = setup_outputs_equal_retry_fixture(&root);

    update_outputs_equal_retry_progress_after_apply(
        &fixture.paths,
        &[String::from("verify_color")],
        &fixture.entries,
    )
    .expect("first retry increment");
    let first = load_verification_progress(&fixture.paths);
    let first_retry = first
        .outputs_equal_retries_by_surface
        .get("--color")
        .map(|entry| entry.retry_count);
    assert_eq!(first_retry, Some(1));

    update_outputs_equal_retry_progress_after_apply(
        &fixture.paths,
        &[String::from("verify_color")],
        &fixture.entries,
    )
    .expect("second retry increment");
    let second = load_verification_progress(&fixture.paths);
    let second_retry = second
        .outputs_equal_retries_by_surface
        .get("--color")
        .map(|entry| entry.retry_count);
    assert_eq!(second_retry, Some(2));

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn unknown_or_empty_forced_reruns_do_not_increment_retry_progress() {
    let root = temp_doc_pack_root("bman-apply-progress-unknown");
    let fixture = setup_outputs_equal_retry_fixture(&root);

    update_outputs_equal_retry_progress_after_apply(
        &fixture.paths,
        &[String::from("unknown_scenario")],
        &fixture.entries,
    )
    .expect("unknown forced rerun should not fail");
    let after_unknown = load_verification_progress(&fixture.paths);
    assert!(after_unknown.outputs_equal_retries_by_surface.is_empty());

    update_outputs_equal_retry_progress_after_apply(&fixture.paths, &[], &fixture.entries)
        .expect("empty forced rerun set should not fail");
    let after_empty = load_verification_progress(&fixture.paths);
    assert!(after_empty.outputs_equal_retries_by_surface.is_empty());

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn outputs_equal_retry_counts_accumulate_across_delta_path_churn() {
    let root = temp_doc_pack_root("bman-apply-progress-delta-churn");
    let fixture = setup_outputs_equal_retry_fixture(&root);

    update_outputs_equal_retry_progress_after_apply(
        &fixture.paths,
        &[String::from("verify_color")],
        &fixture.entries,
    )
    .expect("first retry increment");
    let first = load_verification_progress(&fixture.paths);
    assert_eq!(
        first
            .outputs_equal_retries_by_surface
            .get("--color")
            .map(|entry| entry.retry_count),
        Some(1)
    );

    let next_delta_rel = "inventory/scenarios/verify_color-400.json";
    write_file(
        &root.join(next_delta_rel),
        r#"{"scenario_id":"verify_color","schema_version":3}"#,
    );
    let next_entries = outputs_equal_verification_entries(next_delta_rel);
    std::thread::sleep(Duration::from_millis(20));
    write_file(
        &fixture.paths.surface_overlays_path(),
        r#"{"schema_version":3,"items":[],"overlays":[]}"#,
    );

    update_outputs_equal_retry_progress_after_apply(
        &fixture.paths,
        &[String::from("verify_color")],
        &next_entries,
    )
    .expect("second retry increment");
    let second = load_verification_progress(&fixture.paths);
    assert_eq!(
        second
            .outputs_equal_retries_by_surface
            .get("--color")
            .map(|entry| entry.retry_count),
        Some(2)
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

fn assertion_failed_verification_entries(
    delta_rel: &str,
) -> BTreeMap<String, scenarios::VerificationEntry> {
    let entry = scenarios::VerificationEntry {
        surface_id: "--color".to_string(),
        status: "verified".to_string(),
        behavior_status: "unverified".to_string(),
        behavior_exclusion_reason_code: None,
        behavior_unverified_reason_code: Some("assertion_failed".to_string()),
        behavior_unverified_scenario_id: Some("verify_color".to_string()),
        behavior_unverified_assertion_kind: Some("variant_stdout_contains_seed_path".to_string()),
        behavior_unverified_assertion_seed_path: Some("work/item.txt".to_string()),
        behavior_unverified_assertion_token: Some("item.txt".to_string()),
        scenario_ids: Vec::new(),
        scenario_paths: Vec::new(),
        behavior_scenario_ids: vec!["verify_color".to_string()],
        behavior_assertion_scenario_ids: Vec::new(),
        behavior_scenario_paths: vec![delta_rel.to_string()],
        delta_outcome: Some("outputs_differ".to_string()),
        delta_evidence_paths: vec![delta_rel.to_string()],
        behavior_confounded_scenario_ids: Vec::new(),
        behavior_confounded_extra_surface_ids: Vec::new(),
        evidence: Vec::new(),
    };
    let mut entries = BTreeMap::new();
    entries.insert("--color".to_string(), entry);
    entries
}

struct AssertionFailedFixture {
    paths: enrich::DocPackPaths,
    entries: BTreeMap<String, scenarios::VerificationEntry>,
}

fn setup_assertion_failed_retry_fixture(root: &std::path::Path) -> AssertionFailedFixture {
    let paths = enrich::DocPackPaths::new(root.to_path_buf());
    let surface = crate::surface::SurfaceInventory {
        schema_version: 2,
        generated_at_epoch_ms: 0,
        binary_name: Some("bin".to_string()),
        inputs_hash: None,
        discovery: Vec::new(),
        items: vec![crate::surface::SurfaceItem {
            kind: "option".to_string(),
            id: "--color".to_string(),
            display: "--color".to_string(),
            description: None,
            forms: vec!["--color".to_string()],
            invocation: crate::surface::SurfaceInvocation::default(),
            evidence: Vec::new(),
        }],
        blockers: Vec::new(),
    };
    write_file(
        &paths.surface_path(),
        &serde_json::to_string_pretty(&surface).expect("serialize surface"),
    );

    let delta_rel = "inventory/scenarios/verify_color-300.json";
    write_file(
        &root.join(delta_rel),
        r#"{"scenario_id":"verify_color","schema_version":3}"#,
    );

    let entries = assertion_failed_verification_entries(delta_rel);
    AssertionFailedFixture { paths, entries }
}

#[test]
fn assertion_failed_progress_increments_on_forced_rerun_with_no_evidence_change() {
    let root = temp_doc_pack_root("bman-apply-assertion-failed-increment");
    let fixture = setup_assertion_failed_retry_fixture(&root);

    // Set up initial progress with a fingerprint
    let mut progress = crate::verification_progress::VerificationProgress::default();
    progress.assertion_failed_by_surface.insert(
        "--color".to_string(),
        crate::verification_progress::AssertionFailedProgressEntry {
            no_progress_count: 0,
            last_signature: crate::verification_progress::ActionSignature {
                reason_code: Some("assertion_failed".to_string()),
                target_id: Some("--color".to_string()),
                content_hash: Some("abc123".to_string()),
                evidence_fingerprint: Some(
                    "assertion_kind:variant_stdout_contains_seed_path|reason:assertion_failed|scenario:verify_color".to_string(),
                ),
            },
        },
    );
    crate::verification_progress::write_verification_progress(&fixture.paths, &progress)
        .expect("write initial progress");

    // Execute forced rerun - evidence fingerprint matches, so no progress
    update_assertion_failed_progress_after_apply(
        &fixture.paths,
        &[String::from("verify_color")],
        &fixture.entries,
    )
    .expect("first forced rerun");
    let after = load_verification_progress(&fixture.paths);
    let entry = after
        .assertion_failed_by_surface
        .get("--color")
        .expect("entry should exist");
    assert_eq!(entry.no_progress_count, 1);

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn assertion_failed_progress_resets_on_evidence_change() {
    let root = temp_doc_pack_root("bman-apply-assertion-failed-reset");
    let fixture = setup_assertion_failed_retry_fixture(&root);

    // Set up initial progress with a DIFFERENT fingerprint (evidence has changed)
    let mut progress = crate::verification_progress::VerificationProgress::default();
    progress.assertion_failed_by_surface.insert(
        "--color".to_string(),
        crate::verification_progress::AssertionFailedProgressEntry {
            no_progress_count: 1,
            last_signature: crate::verification_progress::ActionSignature {
                reason_code: Some("assertion_failed".to_string()),
                target_id: Some("--color".to_string()),
                content_hash: Some("abc123".to_string()),
                evidence_fingerprint: Some("old_different_fingerprint".to_string()),
            },
        },
    );
    crate::verification_progress::write_verification_progress(&fixture.paths, &progress)
        .expect("write initial progress");

    // Execute forced rerun - evidence fingerprint changed, so progress reset
    update_assertion_failed_progress_after_apply(
        &fixture.paths,
        &[String::from("verify_color")],
        &fixture.entries,
    )
    .expect("forced rerun");
    let after = load_verification_progress(&fixture.paths);
    let entry = after
        .assertion_failed_by_surface
        .get("--color")
        .expect("entry should exist");
    assert_eq!(entry.no_progress_count, 0);

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn assertion_failed_progress_transitions_to_exclusion_at_cap() {
    let root = temp_doc_pack_root("bman-apply-assertion-failed-cap");
    let fixture = setup_assertion_failed_retry_fixture(&root);

    // Set up initial progress near cap
    let mut progress = crate::verification_progress::VerificationProgress::default();
    progress.assertion_failed_by_surface.insert(
        "--color".to_string(),
        crate::verification_progress::AssertionFailedProgressEntry {
            no_progress_count: ASSERTION_FAILED_NOOP_CAP - 1,
            last_signature: crate::verification_progress::ActionSignature {
                reason_code: Some("assertion_failed".to_string()),
                target_id: Some("--color".to_string()),
                content_hash: Some("abc123".to_string()),
                evidence_fingerprint: Some(
                    "assertion_kind:variant_stdout_contains_seed_path|reason:assertion_failed|scenario:verify_color".to_string(),
                ),
            },
        },
    );
    crate::verification_progress::write_verification_progress(&fixture.paths, &progress)
        .expect("write initial progress");

    // Execute forced rerun - should hit cap
    update_assertion_failed_progress_after_apply(
        &fixture.paths,
        &[String::from("verify_color")],
        &fixture.entries,
    )
    .expect("forced rerun");
    let after = load_verification_progress(&fixture.paths);
    let entry = after
        .assertion_failed_by_surface
        .get("--color")
        .expect("entry should exist");
    assert_eq!(entry.no_progress_count, ASSERTION_FAILED_NOOP_CAP);

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn assertion_failed_progress_cleans_up_entries_for_non_assertion_failed_surfaces() {
    let root = temp_doc_pack_root("bman-apply-assertion-failed-cleanup");
    let fixture = setup_assertion_failed_retry_fixture(&root);

    // Set up progress with an entry for a surface that is no longer assertion_failed
    let mut progress = crate::verification_progress::VerificationProgress::default();
    progress.assertion_failed_by_surface.insert(
        "--other".to_string(), // This surface is not in the ledger as assertion_failed
        crate::verification_progress::AssertionFailedProgressEntry {
            no_progress_count: 1,
            last_signature: crate::verification_progress::ActionSignature::default(),
        },
    );
    crate::verification_progress::write_verification_progress(&fixture.paths, &progress)
        .expect("write initial progress");

    // Execute any apply - should clean up the stale entry
    update_assertion_failed_progress_after_apply(&fixture.paths, &[], &fixture.entries)
        .expect("apply");
    let after = load_verification_progress(&fixture.paths);
    assert!(
        !after.assertion_failed_by_surface.contains_key("--other"),
        "stale entry should be removed"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}
