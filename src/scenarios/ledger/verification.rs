//! Verification ledger construction from scenarios and SQL lenses.
//!
//! Verification remains pack-owned by delegating invocation matching to SQL
//! and treating Rust as a mechanical aggregator.
use super::shared::is_surface_item_kind;
use crate::enrich;
use crate::pack;
use crate::scenarios::{
    load_plan, VerificationEntry, VerificationLedger, SCENARIO_EVIDENCE_SCHEMA_VERSION,
    SCENARIO_INDEX_SCHEMA_VERSION,
};
use crate::staging::collect_files_recursive;
use crate::surface;
use crate::util::display_path;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Build the verification ledger from surface inventory and scenario evidence.
pub fn build_verification_ledger(
    binary_name: &str,
    surface: &surface::SurfaceInventory,
    doc_pack_root: &Path,
    scenarios_path: &Path,
    template_path: &Path,
    staging_root: Option<&Path>,
    display_root: Option<&Path>,
) -> Result<VerificationLedger> {
    let template_sql = fs::read_to_string(template_path)
        .with_context(|| format!("read {}", template_path.display()))?;
    let _plan = load_plan(scenarios_path, doc_pack_root)?;
    let (query_root, rows) = run_verification_query(doc_pack_root, staging_root, &template_sql)?;
    let behavior_exclusions = load_behavior_exclusions(doc_pack_root)?;
    let excluded_map = behavior_exclusion_map(surface, &rows, &behavior_exclusions)?;
    let excluded = excluded_entries_from_map(&excluded_map);

    let mut surface_evidence_map: BTreeMap<String, Vec<enrich::EvidenceRef>> = BTreeMap::new();
    for item in surface
        .items
        .iter()
        .filter(|item| is_surface_item_kind(&item.kind))
    {
        let id = item.id.trim();
        if id.is_empty() {
            continue;
        }
        surface_evidence_map
            .entry(id.to_string())
            .or_default()
            .extend(item.evidence.iter().cloned());
    }

    let surface_evidence = enrich::evidence_from_rel(&query_root.root, "inventory/surface.json")?;
    let plan_evidence = enrich::evidence_from_rel(&query_root.root, "scenarios/plan.json")?;

    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut verified_count = 0;
    let mut unverified_ids = Vec::new();
    let mut behavior_verified_count = 0;
    let mut behavior_unverified_ids = Vec::new();

    for row in rows {
        let Some(surface_id) = row.surface_id.clone() else {
            warnings.push("verification row missing surface_id".to_string());
            continue;
        };
        let status = row.status.clone().unwrap_or_else(|| "unknown".to_string());
        let excluded_entry = excluded_map.get(&surface_id);
        let (
            behavior_status,
            behavior_unverified_reason_code,
            behavior_unverified_scenario_id,
            behavior_unverified_assertion_kind,
            behavior_unverified_assertion_seed_path,
            behavior_unverified_assertion_token,
        ) = if excluded_entry.is_some() {
            ("excluded".to_string(), None, None, None, None, None)
        } else {
            (
                row.behavior_status
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                row.behavior_unverified_reason_code.clone(),
                row.behavior_unverified_scenario_id.clone(),
                row.behavior_unverified_assertion_kind.clone(),
                row.behavior_unverified_assertion_seed_path.clone(),
                row.behavior_unverified_assertion_token.clone(),
            )
        };
        if status == "verified" {
            verified_count += 1;
        } else {
            unverified_ids.push(surface_id.clone());
        }
        if behavior_status == "verified" {
            behavior_verified_count += 1;
        } else if behavior_status != "excluded" {
            behavior_unverified_ids.push(surface_id.clone());
        }

        let mut evidence = surface_evidence_map
            .get(&surface_id)
            .cloned()
            .unwrap_or_default();
        evidence.push(surface_evidence.clone());
        evidence.push(plan_evidence.clone());
        for scenario_path in row
            .scenario_paths
            .iter()
            .chain(row.behavior_scenario_paths.iter())
        {
            match enrich::evidence_from_rel(&query_root.root, scenario_path) {
                Ok(evidence_ref) => evidence.push(evidence_ref),
                Err(err) => warnings.push(err.to_string()),
            }
        }
        enrich::dedupe_evidence_refs(&mut evidence);

        entries.push(VerificationEntry {
            surface_id,
            status,
            behavior_status,
            behavior_exclusion_reason_code: excluded_entry
                .map(|entry| entry.exclusion.reason_code.as_str().to_string()),
            behavior_unverified_reason_code,
            behavior_unverified_scenario_id,
            behavior_unverified_assertion_kind,
            behavior_unverified_assertion_seed_path,
            behavior_unverified_assertion_token,
            scenario_ids: row.scenario_ids,
            scenario_paths: row.scenario_paths,
            behavior_scenario_ids: row.behavior_scenario_ids,
            behavior_assertion_scenario_ids: row.behavior_assertion_scenario_ids,
            behavior_scenario_paths: row.behavior_scenario_paths,
            delta_outcome: row.delta_outcome,
            delta_evidence_paths: row.delta_evidence_paths,
            evidence,
        });
    }

    entries.sort_by(|a, b| a.surface_id.cmp(&b.surface_id));
    unverified_ids.sort();
    unverified_ids.dedup();
    behavior_unverified_ids.sort();
    behavior_unverified_ids.dedup();

    let generated_at_epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("compute timestamp")?
        .as_millis();

    Ok(VerificationLedger {
        schema_version: 9,
        generated_at_epoch_ms,
        binary_name: binary_name.to_string(),
        scenarios_path: display_path(scenarios_path, display_root),
        surface_path: display_path(
            &doc_pack_root.join("inventory").join("surface.json"),
            display_root,
        ),
        total_count: entries.len(),
        verified_count,
        unverified_count: unverified_ids.len(),
        unverified_ids,
        behavior_verified_count,
        behavior_unverified_count: behavior_unverified_ids.len(),
        behavior_unverified_ids,
        excluded_count: excluded.len(),
        excluded,
        entries,
        warnings,
    })
}

fn load_behavior_exclusions(
    doc_pack_root: &Path,
) -> Result<Vec<surface::SurfaceBehaviorExclusion>> {
    let overlays_path = doc_pack_root
        .join("inventory")
        .join("surface.overlays.json");
    Ok(surface::load_surface_overlays_if_exists(&overlays_path)?
        .map(|overlays| surface::collect_behavior_exclusions(&overlays))
        .unwrap_or_default())
}

fn behavior_exclusion_map(
    surface: &surface::SurfaceInventory,
    rows: &[VerificationRow],
    exclusions: &[surface::SurfaceBehaviorExclusion],
) -> Result<BTreeMap<String, surface::SurfaceBehaviorExclusion>> {
    let option_ids: BTreeSet<String> = surface
        .items
        .iter()
        .filter(|item| item.kind == "option")
        .map(|item| item.id.trim())
        .filter(|id| !id.is_empty())
        .map(|id| id.to_string())
        .collect();
    let mut row_by_surface_id = BTreeMap::new();
    for row in rows {
        let Some(surface_id) = row.surface_id.as_ref() else {
            continue;
        };
        row_by_surface_id.insert(
            surface_id.clone(),
            surface::BehaviorExclusionLedgerEntry {
                delta_outcome: row.delta_outcome.clone(),
                delta_evidence_paths: row.delta_evidence_paths.clone(),
            },
        );
    }
    surface::validate_behavior_exclusions(
        exclusions,
        &option_ids,
        &row_by_surface_id,
        "missing from verification rows",
        "requires delta_outcome evidence",
    )
}

fn excluded_entries_from_map(
    excluded_map: &BTreeMap<String, surface::SurfaceBehaviorExclusion>,
) -> Vec<crate::scenarios::VerificationExcludedEntry> {
    excluded_map
        .values()
        .map(|entry| {
            let reason_code = entry.exclusion.reason_code.as_str().to_string();
            crate::scenarios::VerificationExcludedEntry {
                surface_id: entry.surface_id.clone(),
                reason_code: Some(reason_code.clone()),
                note: entry.exclusion.note.clone(),
                prereqs: Vec::new(),
                reason: Some(reason_code),
            }
        })
        .collect()
}

fn run_verification_query(
    doc_pack_root: &Path,
    staging_root: Option<&Path>,
    template_sql: &str,
) -> Result<(VerificationQueryRoot, Vec<VerificationRow>)> {
    let query_root = prepare_verification_root(doc_pack_root, staging_root)?;
    let output = pack::run_duckdb_query(template_sql, &query_root.root)?;
    let rows: Vec<VerificationRow> =
        if output.is_empty() || output.iter().all(|byte| byte.is_ascii_whitespace()) {
            Vec::new()
        } else {
            serde_json::from_slice(&output).context("parse verification query output")?
        };
    Ok((query_root, rows))
}

fn prepare_verification_root(
    doc_pack_root: &Path,
    staging_root: Option<&Path>,
) -> Result<VerificationQueryRoot> {
    let (root, cleanup) = if let Some(staging_root) = staging_root {
        let txn_root = staging_root
            .parent()
            .ok_or_else(|| anyhow!("staging root has no parent"))?;
        let now = enrich::now_epoch_ms()?;
        (
            txn_root
                .join("scratch")
                .join(format!("verification_root-{now}")),
            false,
        )
    } else {
        let now = enrich::now_epoch_ms()?;
        (
            std::env::temp_dir().join(format!("bman-verification-{now}")),
            true,
        )
    };
    fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;

    let inventory_root = root.join("inventory");
    let scenarios_root = root.join("scenarios");
    let enrich_root = root.join("enrich");
    fs::create_dir_all(inventory_root.join("scenarios"))
        .with_context(|| format!("create {}", inventory_root.display()))?;
    fs::create_dir_all(&scenarios_root)
        .with_context(|| format!("create {}", scenarios_root.display()))?;
    fs::create_dir_all(&enrich_root)
        .with_context(|| format!("create {}", enrich_root.display()))?;

    let plan_src = doc_pack_root.join("scenarios").join("plan.json");
    let plan_dest = scenarios_root.join("plan.json");
    fs::copy(&plan_src, &plan_dest).with_context(|| format!("copy {}", plan_src.display()))?;

    let semantics_src = doc_pack_root.join("enrich").join("semantics.json");
    let semantics_dest = enrich_root.join("semantics.json");
    fs::copy(&semantics_src, &semantics_dest)
        .with_context(|| format!("copy {}", semantics_src.display()))?;

    let staged_surface = staging_root
        .map(|root| root.join("inventory").join("surface.json"))
        .filter(|path| path.is_file());
    let surface_src =
        staged_surface.unwrap_or_else(|| doc_pack_root.join("inventory").join("surface.json"));
    let surface_dest = inventory_root.join("surface.json");
    fs::copy(&surface_src, &surface_dest)
        .with_context(|| format!("copy {}", surface_src.display()))?;

    copy_scenario_evidence(
        &doc_pack_root.join("inventory").join("scenarios"),
        &inventory_root.join("scenarios"),
    )?;
    if let Some(staging_root) = staging_root {
        copy_scenario_evidence(
            &staging_root.join("inventory").join("scenarios"),
            &inventory_root.join("scenarios"),
        )?;
    }

    let placeholder = format!(
        concat!(
            "{{\"schema_version\":{schema},",
            "\"generated_at_epoch_ms\":0,",
            "\"scenario_id\":null,",
            "\"argv\":[],",
            "\"env\":{{}},",
            "\"seed_dir\":null,",
            "\"cwd\":null,",
            "\"timeout_seconds\":null,",
            "\"net_mode\":null,",
            "\"no_sandbox\":null,",
            "\"no_strace\":null,",
            "\"snippet_max_lines\":0,",
            "\"snippet_max_bytes\":0,",
            "\"exit_code\":null,",
            "\"exit_signal\":null,",
            "\"timed_out\":false,",
            "\"duration_ms\":0,",
            "\"stdout\":\"\",",
            "\"stderr\":\"\"}}"
        ),
        schema = SCENARIO_EVIDENCE_SCHEMA_VERSION
    );
    let placeholder_path = inventory_root.join("scenarios").join("schema.json");
    fs::write(&placeholder_path, placeholder.as_bytes())
        .with_context(|| format!("write placeholder {}", placeholder_path.display()))?;

    let index_path = inventory_root.join("scenarios").join("index.json");
    if !index_path.is_file() {
        let placeholder = format!(
            "{{\"schema_version\":{},\"scenarios\":[]}}",
            SCENARIO_INDEX_SCHEMA_VERSION
        );
        fs::write(&index_path, placeholder.as_bytes())
            .with_context(|| format!("write placeholder {}", index_path.display()))?;
    }

    Ok(VerificationQueryRoot { root, cleanup })
}

fn copy_scenario_evidence(src_root: &Path, dest_root: &Path) -> Result<()> {
    if !src_root.is_dir() {
        return Ok(());
    }
    for file in collect_files_recursive(src_root)? {
        if file.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let rel = file
            .strip_prefix(src_root)
            .context("strip scenario evidence prefix")?;
        let dest = dest_root.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        fs::copy(&file, &dest)
            .with_context(|| format!("copy {} to {}", file.display(), dest.display()))?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct VerificationRow {
    #[serde(default)]
    surface_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    behavior_status: Option<String>,
    #[serde(default)]
    behavior_unverified_reason_code: Option<String>,
    #[serde(default)]
    behavior_unverified_scenario_id: Option<String>,
    #[serde(default)]
    behavior_unverified_assertion_kind: Option<String>,
    #[serde(default)]
    behavior_unverified_assertion_seed_path: Option<String>,
    #[serde(default)]
    behavior_unverified_assertion_token: Option<String>,
    #[serde(default)]
    scenario_ids: Vec<String>,
    #[serde(default)]
    scenario_paths: Vec<String>,
    #[serde(default)]
    behavior_scenario_ids: Vec<String>,
    #[serde(default)]
    behavior_assertion_scenario_ids: Vec<String>,
    #[serde(default)]
    behavior_scenario_paths: Vec<String>,
    #[serde(default)]
    delta_outcome: Option<String>,
    #[serde(default)]
    delta_evidence_paths: Vec<String>,
}

struct VerificationQueryRoot {
    root: PathBuf,
    cleanup: bool,
}

impl Drop for VerificationQueryRoot {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

#[cfg(test)]
mod tests {
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
  to_json([]::VARCHAR[]) as delta_evidence_paths
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
}
