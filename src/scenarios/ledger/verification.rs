//! Verification ledger construction from scenarios and SQL lenses.
//!
//! Verification remains pack-owned by delegating invocation matching to SQL
//! and treating Rust as a mechanical aggregator.
use super::shared::is_entry_point;
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
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Cache entry for verification ledger results.
#[derive(Debug, Serialize, Deserialize)]
struct VerificationCache {
    inputs_hash: String,
    computed_at_epoch_ms: u128,
    ledger: VerificationLedger,
}

static VERIFICATION_ROOT_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) struct VerificationQueryTemplateReadError {
    path: PathBuf,
    source: std::io::Error,
}

impl std::fmt::Display for VerificationQueryTemplateReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "verification query template read failed for {}: {}",
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for VerificationQueryTemplateReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub(crate) fn verification_query_template_failure_path(err: &anyhow::Error) -> Option<&Path> {
    err.chain().find_map(|cause| {
        cause
            .downcast_ref::<VerificationQueryTemplateReadError>()
            .map(|read_err| read_err.path.as_path())
    })
}

/// Compute a hash of all inputs that affect verification ledger output.
fn compute_verification_inputs_hash(
    doc_pack_root: &Path,
    template_path: &Path,
    surface_path: &Path,
) -> Result<String> {
    let mut hasher = Sha256::new();

    // Hash template SQL content
    if template_path.exists() {
        let sql = fs::read_to_string(template_path)
            .with_context(|| format!("read template: {}", template_path.display()))?;
        hasher.update(b"template:");
        hasher.update(sql.as_bytes());
    }

    // Hash surface inventory
    if surface_path.exists() {
        let surface = fs::read_to_string(surface_path)
            .with_context(|| format!("read surface: {}", surface_path.display()))?;
        hasher.update(b"surface:");
        hasher.update(surface.as_bytes());
    }

    // Hash all scenario evidence files
    let scenarios_dir = doc_pack_root.join("inventory/scenarios");
    if scenarios_dir.is_dir() {
        if let Ok(files) = collect_files_recursive(&scenarios_dir) {
            let mut paths: Vec<_> = files
                .into_iter()
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .collect();
            paths.sort();
            for path in paths {
                if let Ok(content) = fs::read_to_string(&path) {
                    let rel = path.strip_prefix(doc_pack_root).unwrap_or(&path);
                    hasher.update(b"scenario:");
                    hasher.update(rel.to_string_lossy().as_bytes());
                    hasher.update(content.as_bytes());
                }
            }
        }
    }

    let digest = hasher.finalize();
    Ok(format!("{:x}", digest))
}

/// Path to the verification cache file.
fn verification_cache_path(doc_pack_root: &Path) -> PathBuf {
    doc_pack_root.join("inventory/verification_cache.json")
}

/// Try to load cached verification ledger if inputs haven't changed.
fn try_load_verification_cache(
    doc_pack_root: &Path,
    inputs_hash: &str,
) -> Option<VerificationLedger> {
    let cache_path = verification_cache_path(doc_pack_root);
    let content = fs::read_to_string(&cache_path).ok()?;
    let cache: VerificationCache = serde_json::from_str(&content).ok()?;
    if cache.inputs_hash == inputs_hash {
        tracing::info!("verification cache hit");
        Some(cache.ledger)
    } else {
        tracing::debug!("verification cache stale");
        None
    }
}

/// Save verification ledger to cache.
fn save_verification_cache(
    doc_pack_root: &Path,
    inputs_hash: &str,
    ledger: &VerificationLedger,
) -> Result<()> {
    let cache_path = verification_cache_path(doc_pack_root);
    let cache = VerificationCache {
        inputs_hash: inputs_hash.to_string(),
        computed_at_epoch_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        ledger: ledger.clone(),
    };
    let content = serde_json::to_string_pretty(&cache).context("serialize cache")?;
    fs::write(&cache_path, content)
        .with_context(|| format!("write cache: {}", cache_path.display()))?;
    Ok(())
}

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
    let surface_path = doc_pack_root.join("inventory/surface.json");

    // Try cache first (only when not using staging_root, which indicates test/temp context)
    if staging_root.is_none() {
        if let Ok(inputs_hash) =
            compute_verification_inputs_hash(doc_pack_root, template_path, &surface_path)
        {
            if let Some(cached) = try_load_verification_cache(doc_pack_root, &inputs_hash) {
                return Ok(cached);
            }
        }
    }

    let start = Instant::now();
    let template_sql = load_verification_query_template(template_path)?;
    let _plan = load_plan(scenarios_path, doc_pack_root)?;
    let (query_root, rows) = run_verification_query(doc_pack_root, staging_root, &template_sql)?;
    tracing::debug!(
        elapsed_ms = start.elapsed().as_millis(),
        "verification query executed"
    );
    let behavior_exclusions = load_behavior_exclusions(doc_pack_root)?;
    let excluded_map = behavior_exclusion_map(surface, &rows, &behavior_exclusions)?;
    let excluded = excluded_entries_from_map(&excluded_map);

    let mut surface_evidence_map: BTreeMap<String, Vec<enrich::EvidenceRef>> = BTreeMap::new();
    // Include all non-entry-point items for verification tracking
    for item in surface.items.iter().filter(|item| !is_entry_point(item)) {
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
            behavior_confounded_scenario_ids: row.behavior_confounded_scenario_ids,
            behavior_confounded_extra_surface_ids: row.behavior_confounded_extra_surface_ids,
            evidence,
            auto_verify_exit_code: row.auto_verify_exit_code,
            auto_verify_stderr: row.auto_verify_stderr,
            behavior_exit_code: row.behavior_exit_code,
            behavior_stderr: row.behavior_stderr,
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

    let ledger = VerificationLedger {
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
    };

    // Save to cache (only when not using staging_root)
    if staging_root.is_none() {
        if let Ok(inputs_hash) =
            compute_verification_inputs_hash(doc_pack_root, template_path, &surface_path)
        {
            let _ = save_verification_cache(doc_pack_root, &inputs_hash, &ledger);
        }
    }

    Ok(ledger)
}

fn load_verification_query_template(template_path: &Path) -> Result<String> {
    let mut include_stack = Vec::new();
    load_verification_query_template_inner(template_path, &mut include_stack)
}

fn load_verification_query_template_inner(
    template_path: &Path,
    include_stack: &mut Vec<PathBuf>,
) -> Result<String> {
    let stack_path =
        fs::canonicalize(template_path).unwrap_or_else(|_| template_path.to_path_buf());
    if include_stack.iter().any(|existing| existing == &stack_path) {
        let cycle = include_stack
            .iter()
            .chain(std::iter::once(&stack_path))
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(anyhow!(
            "verification query template include cycle: {cycle}"
        ));
    }
    include_stack.push(stack_path);

    let rendered = (|| -> Result<String> {
        let source = fs::read_to_string(template_path).map_err(|source| {
            anyhow!(VerificationQueryTemplateReadError {
                path: template_path.to_path_buf(),
                source,
            })
        })?;
        let base_dir = template_path.parent().unwrap_or_else(|| Path::new("."));
        let mut output = String::new();
        for line in source.lines() {
            if let Some(include_path) = verification_query_include_path(line) {
                let include_abs = base_dir.join(include_path);
                let included = load_verification_query_template_inner(&include_abs, include_stack)?;
                output.push_str(&included);
                if !included.ends_with('\n') {
                    output.push('\n');
                }
                continue;
            }
            output.push_str(line);
            output.push('\n');
        }
        Ok(output)
    })();

    include_stack.pop();
    rendered
}

fn verification_query_include_path(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("-- @include ")
        .map(str::trim)
        .filter(|path| !path.is_empty())
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
    _rows: &[VerificationRow],
    exclusions: &[surface::SurfaceBehaviorExclusion],
) -> Result<BTreeMap<String, surface::SurfaceBehaviorExclusion>> {
    // Behavior exclusions apply to non-entry-point items (options, flags, etc.)
    let surface_ids: BTreeSet<String> = surface
        .items
        .iter()
        .filter(|item| !is_entry_point(item))
        .map(|item| item.id.trim())
        .filter(|id| !id.is_empty())
        .map(|id| id.to_string())
        .collect();
    surface::validate_behavior_exclusions(exclusions, &surface_ids)
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
    let root_suffix = verification_root_suffix()?;
    let (root, cleanup) = if let Some(staging_root) = staging_root {
        let txn_root = staging_root
            .parent()
            .ok_or_else(|| anyhow!("staging root has no parent"))?;
        (
            txn_root
                .join("scratch")
                .join(format!("verification_root-{root_suffix}")),
            false,
        )
    } else {
        (
            std::env::temp_dir().join(format!("bman-verification-{root_suffix}")),
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

fn verification_root_suffix() -> Result<String> {
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("compute verification root timestamp")?
        .as_nanos();
    let seq = VERIFICATION_ROOT_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(format!("{now_ns}-{}-{seq}", std::process::id()))
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
    #[serde(default)]
    behavior_confounded_scenario_ids: Vec<String>,
    #[serde(default)]
    behavior_confounded_extra_surface_ids: Vec<String>,
    #[serde(default)]
    auto_verify_exit_code: Option<i64>,
    #[serde(default)]
    auto_verify_stderr: Option<String>,
    #[serde(default)]
    behavior_exit_code: Option<i64>,
    #[serde(default)]
    behavior_stderr: Option<String>,
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
#[path = "verification_tests.rs"]
mod tests;
