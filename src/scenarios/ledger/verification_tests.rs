
use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_doc_pack_root(name: &str) -> std::path::PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("{name}-{}-{now}", std::process::id()));
    std::fs::create_dir_all(root.join("inventory")).expect("create inventory dir");
    root
}

fn write_minimal_pack_inputs(root: &std::path::Path, surface: &surface::SurfaceInventory) {
    std::fs::create_dir_all(root.join("scenarios")).expect("create scenarios dir");
    std::fs::create_dir_all(root.join("enrich")).expect("create enrich dir");
    std::fs::create_dir_all(root.join("fixtures").join("empty"))
        .expect("create default fixtures dir");
    std::fs::write(
        root.join("scenarios").join("plan.json"),
        crate::scenarios::plan_stub(Some("bin")),
    )
    .expect("write plan");
    std::fs::write(
        root.join("enrich").join("semantics.json"),
        crate::templates::ENRICH_SEMANTICS_JSON,
    )
    .expect("write semantics");
    std::fs::write(
        root.join("inventory").join("surface.json"),
        serde_json::to_vec_pretty(surface).expect("serialize surface"),
    )
    .expect("write surface");
}

fn write_verification_query(
    path: &std::path::Path,
    behavior_status: &str,
    reason_code: Option<&str>,
    scenario_id: Option<&str>,
    assertion_kind: Option<&str>,
) {
    let reason_sql = reason_code
        .map(|value| format!("'{value}'"))
        .unwrap_or_else(|| "null".to_string());
    let scenario_sql = scenario_id
        .map(|value| format!("'{value}'"))
        .unwrap_or_else(|| "null".to_string());
    let assertion_kind_sql = assertion_kind
        .map(|value| format!("'{value}'"))
        .unwrap_or_else(|| "null".to_string());
    let sql = format!(
        "select
  item.id as surface_id,
  'recognized' as status,
  '{behavior_status}' as behavior_status,
  {reason_sql} as behavior_unverified_reason_code,
  {scenario_sql} as behavior_unverified_scenario_id,
  {assertion_kind_sql} as behavior_unverified_assertion_kind,
  'work/file.txt' as behavior_unverified_assertion_seed_path,
  'file.txt' as behavior_unverified_assertion_token,
  to_json([]::VARCHAR[]) as scenario_ids,
  to_json([]::VARCHAR[]) as scenario_paths,
  to_json([]::VARCHAR[]) as behavior_scenario_ids,
  to_json([]::VARCHAR[]) as behavior_assertion_scenario_ids,
  to_json([]::VARCHAR[]) as behavior_scenario_paths,
  null as delta_outcome,
  to_json([]::VARCHAR[]) as delta_evidence_paths,
  to_json([]::VARCHAR[]) as behavior_confounded_scenario_ids,
  to_json([]::VARCHAR[]) as behavior_confounded_extra_surface_ids
from read_json_auto('inventory/surface.json') as inv,
  unnest(inv.items) as t(item)
where item.kind = 'option';"
    );
    std::fs::write(path, sql).expect("write query");
}

fn sql_varchar_array(values: &[&str]) -> String {
    if values.is_empty() {
        "[]::VARCHAR[]".to_string()
    } else {
        let joined = values
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("[{joined}]::VARCHAR[]")
    }
}

fn write_verification_query_with_confounded(
    path: &std::path::Path,
    behavior_status: &str,
    reason_code: &str,
    scenario_id: &str,
    confounded_scenario_ids: &[&str],
    confounded_extra_surface_ids: &[&str],
) {
    let confounded_scenarios_sql = sql_varchar_array(confounded_scenario_ids);
    let confounded_extras_sql = sql_varchar_array(confounded_extra_surface_ids);
    let sql = format!(
        "select
  item.id as surface_id,
  'recognized' as status,
  '{behavior_status}' as behavior_status,
  '{reason_code}' as behavior_unverified_reason_code,
  '{scenario_id}' as behavior_unverified_scenario_id,
  null as behavior_unverified_assertion_kind,
  null as behavior_unverified_assertion_seed_path,
  null as behavior_unverified_assertion_token,
  to_json([]::VARCHAR[]) as scenario_ids,
  to_json([]::VARCHAR[]) as scenario_paths,
  to_json([]::VARCHAR[]) as behavior_scenario_ids,
  to_json([]::VARCHAR[]) as behavior_assertion_scenario_ids,
  to_json([]::VARCHAR[]) as behavior_scenario_paths,
  null as delta_outcome,
  to_json([]::VARCHAR[]) as delta_evidence_paths,
  to_json({confounded_scenarios_sql}) as behavior_confounded_scenario_ids,
  to_json({confounded_extras_sql}) as behavior_confounded_extra_surface_ids
from read_json_auto('inventory/surface.json') as inv,
  unnest(inv.items) as t(item)
where item.kind = 'option';"
    );
    std::fs::write(path, sql).expect("write query");
}

#[test]
fn ledger_adapter_rejects_duplicate_behavior_exclusions() {
    let root = temp_doc_pack_root("bman-ledger-dup");

    let overlays = serde_json::json!({
        "schema_version": 3,
        "items": [],
        "overlays": [
            {
                "kind": "option",
                "id": "--color",
                "invocation": {},
                "behavior_exclusion": {
                    "reason_code": "assertion_gap",
                    "note": "first",
                    "evidence": {
                        "delta_variant_path": "inventory/scenarios/color-after-1.json"
                    }
                }
            },
            {
                "kind": "option",
                "id": "--color",
                "invocation": {},
                "behavior_exclusion": {
                    "reason_code": "assertion_gap",
                    "note": "second",
                    "evidence": {
                        "delta_variant_path": "inventory/scenarios/color-after-2.json"
                    }
                }
            }
        ]
    });
    std::fs::write(
        root.join("inventory").join("surface.overlays.json"),
        serde_json::to_vec_pretty(&overlays).expect("serialize overlays"),
    )
    .expect("write overlays");

    let exclusions = load_behavior_exclusions(&root).expect("load exclusions");
    let rows = vec![VerificationRow {
        surface_id: Some("--color".to_string()),
        status: Some("verified".to_string()),
        behavior_status: Some("verified".to_string()),
        behavior_unverified_reason_code: None,
        behavior_unverified_scenario_id: None,
        behavior_unverified_assertion_kind: None,
        behavior_unverified_assertion_seed_path: None,
        behavior_unverified_assertion_token: None,
        scenario_ids: Vec::new(),
        scenario_paths: Vec::new(),
        behavior_scenario_ids: Vec::new(),
        behavior_assertion_scenario_ids: Vec::new(),
        behavior_scenario_paths: Vec::new(),
        delta_outcome: Some("not_applicable".to_string()),
        delta_evidence_paths: Vec::new(),
        behavior_confounded_scenario_ids: Vec::new(),
        behavior_confounded_extra_surface_ids: Vec::new(),
    }];
    let surface = surface::SurfaceInventory {
        schema_version: 2,
        generated_at_epoch_ms: 0,
        binary_name: Some("ls".to_string()),
        inputs_hash: None,
        discovery: Vec::new(),
        items: vec![surface::SurfaceItem {
            kind: "option".to_string(),
            id: "--color".to_string(),
            display: "--color".to_string(),
            description: None,
            forms: Vec::new(),
            invocation: surface::SurfaceInvocation::default(),
            evidence: Vec::new(),
        }],
        blockers: Vec::new(),
    };

    let err = behavior_exclusion_map(&surface, &rows, &exclusions)
        .expect_err("ledger adapter should reject duplicates");
    let _ = std::fs::remove_dir_all(&root);

    assert!(err
        .to_string()
        .contains("duplicate behavior_exclusion entries for surface_id --color"));
}

#[test]
fn verification_ledger_changes_when_query_template_changes() {
    let root = temp_doc_pack_root("bman-ledger-sql-edit");
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
            forms: vec!["--color[=WHEN]".to_string()],
            invocation: surface::SurfaceInvocation::default(),
            evidence: Vec::new(),
        }],
        blockers: Vec::new(),
    };
    write_minimal_pack_inputs(&root, &surface);
    let query_a = root.join("query-a.sql");
    let query_b = root.join("query-b.sql");
    write_verification_query(&query_a, "verified", None, None, None);
    write_verification_query(
        &query_b,
        "rejected",
        Some("assertion_failed"),
        Some("verify_color"),
        Some("variant_stdout_has_line"),
    );

    let ledger_a = build_verification_ledger(
        "bin",
        &surface,
        &root,
        &root.join("scenarios").join("plan.json"),
        &query_a,
        None,
        Some(&root),
    )
    .expect("build ledger from query-a");
    let ledger_b = build_verification_ledger(
        "bin",
        &surface,
        &root,
        &root.join("scenarios").join("plan.json"),
        &query_b,
        None,
        Some(&root),
    )
    .expect("build ledger from query-b");
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(ledger_a.entries.len(), 1);
    assert_eq!(ledger_b.entries.len(), 1);
    assert_eq!(ledger_a.entries[0].behavior_status, "verified");
    assert_eq!(ledger_a.entries[0].behavior_unverified_reason_code, None);
    assert_eq!(ledger_b.entries[0].behavior_status, "rejected");
    assert_eq!(
        ledger_b.entries[0]
            .behavior_unverified_reason_code
            .as_deref(),
        Some("assertion_failed")
    );
    assert_eq!(
        ledger_b.entries[0]
            .behavior_unverified_scenario_id
            .as_deref(),
        Some("verify_color")
    );
    assert_eq!(
        ledger_b.entries[0]
            .behavior_unverified_assertion_kind
            .as_deref(),
        Some("variant_stdout_has_line")
    );
}

#[test]
fn verification_ledger_maps_required_value_reason_and_confounded_columns() {
    let root = temp_doc_pack_root("bman-ledger-required-value");
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
            forms: vec!["--color[=WHEN]".to_string()],
            invocation: surface::SurfaceInvocation::default(),
            evidence: Vec::new(),
        }],
        blockers: Vec::new(),
    };
    write_minimal_pack_inputs(&root, &surface);
    let query = root.join("query-required-value.sql");
    write_verification_query_with_confounded(
        &query,
        "rejected",
        "required_value_missing",
        "verify_color",
        &["verify_color"],
        &["--group-directories-first"],
    );

    let ledger = build_verification_ledger(
        "bin",
        &surface,
        &root,
        &root.join("scenarios").join("plan.json"),
        &query,
        None,
        Some(&root),
    )
    .expect("build ledger");
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(ledger.entries.len(), 1);
    let entry = &ledger.entries[0];
    assert_eq!(
        entry.behavior_unverified_reason_code.as_deref(),
        Some("required_value_missing")
    );
    assert_eq!(
        entry.behavior_confounded_scenario_ids,
        vec!["verify_color".to_string()]
    );
    assert_eq!(
        entry.behavior_confounded_extra_surface_ids,
        vec!["--group-directories-first".to_string()]
    );
}
