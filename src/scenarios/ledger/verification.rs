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
use std::collections::BTreeMap;
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
    let plan = load_plan(scenarios_path, doc_pack_root)?;
    let (query_root, rows) = run_verification_query(doc_pack_root, staging_root, &template_sql)?;

    let (excluded, excluded_ids) = plan.collect_queue_exclusions();

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
        let behavior_status = row
            .behavior_status
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        if status == "verified" {
            verified_count += 1;
        } else if !excluded_ids.contains(&surface_id) {
            unverified_ids.push(surface_id.clone());
        }
        if behavior_status == "verified" {
            behavior_verified_count += 1;
        } else if !excluded_ids.contains(&surface_id) {
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
            behavior_unverified_reason_code: row.behavior_unverified_reason_code,
            scenario_ids: row.scenario_ids,
            scenario_paths: row.scenario_paths,
            behavior_scenario_ids: row.behavior_scenario_ids,
            behavior_assertion_scenario_ids: row.behavior_assertion_scenario_ids,
            behavior_scenario_paths: row.behavior_scenario_paths,
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
        schema_version: 6,
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

fn copy_scenario_evidence(src_root: &Path, dest_root: &Path) -> Result<usize> {
    if !src_root.is_dir() {
        return Ok(0);
    }
    let mut copied = 0usize;
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
        copied += 1;
    }
    Ok(copied)
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
    scenario_ids: Vec<String>,
    #[serde(default)]
    scenario_paths: Vec<String>,
    #[serde(default)]
    behavior_scenario_ids: Vec<String>,
    #[serde(default)]
    behavior_assertion_scenario_ids: Vec<String>,
    #[serde(default)]
    behavior_scenario_paths: Vec<String>,
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
