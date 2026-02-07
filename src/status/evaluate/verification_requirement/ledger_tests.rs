use super::load_or_build_verification_ledger_entries;
use crate::enrich;
use crate::scenarios;
use crate::surface;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_doc_pack_root(name: &str) -> std::path::PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("{name}-{}-{now}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create temp root");
    root
}

fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent directory");
    }
    std::fs::write(path, contents.as_bytes()).expect("write file");
}

fn minimal_surface() -> surface::SurfaceInventory {
    surface::SurfaceInventory {
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
            forms: vec!["--color[=WHEN]".to_string()],
            invocation: surface::SurfaceInvocation::default(),
            evidence: Vec::new(),
        }],
        blockers: Vec::new(),
    }
}

#[test]
fn verification_query_error_reports_missing_include_path() {
    let root = temp_doc_pack_root("bman-verification-query-missing-include");
    let paths = enrich::DocPackPaths::new(root.clone());
    let template_rel = enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL;
    let missing_rel = "queries/verification_from_scenarios/missing_section.sql";
    write_file(
        &root.join(template_rel),
        "-- @include verification_from_scenarios/missing_section.sql\n",
    );
    let template_path = root.join(template_rel);
    let template_evidence = paths
        .evidence_from_path(&template_path)
        .expect("template evidence");
    let plan: scenarios::ScenarioPlan =
        serde_json::from_str(&scenarios::plan_stub(Some("bin"))).expect("parse plan stub");
    let surface = minimal_surface();
    let lock_status = enrich::LockStatus {
        present: false,
        stale: false,
        inputs_hash: None,
    };
    let mut local_blockers = Vec::new();

    let snapshot = load_or_build_verification_ledger_entries(
        Some("bin"),
        &surface,
        &plan,
        &paths,
        &template_path,
        &lock_status,
        &mut local_blockers,
        &template_evidence,
    );
    let _ = std::fs::remove_dir_all(root);

    assert!(
        snapshot.is_none(),
        "query failure should block ledger build"
    );
    assert_eq!(local_blockers.len(), 1);
    let blocker = &local_blockers[0];
    assert_eq!(blocker.code, "verification_query_error");
    assert!(
        blocker.message.contains(missing_rel),
        "expected missing include path in blocker message: {}",
        blocker.message
    );
    assert_eq!(
        blocker.next_action.as_deref(),
        Some("fix queries/verification_from_scenarios/missing_section.sql")
    );
    assert!(
        blocker
            .evidence
            .iter()
            .any(|evidence| evidence.path == missing_rel),
        "expected missing include evidence path"
    );
}
