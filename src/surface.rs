use crate::enrich;
use crate::pack;
use crate::staging::{collect_files_recursive, write_staged_bytes};
use crate::util::{sha256_hex, truncate_bytes};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const PROBE_TIMEOUT_SECS: u64 = 2;
const MAX_PROBE_OUTPUT_BYTES: usize = 64 * 1024;
const SURFACE_SCHEMA_VERSION: u32 = 1;
const SURFACE_SEED_SCHEMA_VERSION: u32 = 1;
const PROBE_SCHEMA_VERSION: u32 = 1;
const PROBE_PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone)]
pub struct SurfaceInventory {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: Option<String>,
    #[serde(default)]
    pub inputs_hash: Option<String>,
    pub discovery: Vec<SurfaceDiscovery>,
    pub items: Vec<SurfaceItem>,
    pub blockers: Vec<enrich::Blocker>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SurfaceDiscovery {
    pub code: String,
    pub status: String,
    pub evidence: Vec<enrich::EvidenceRef>,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SurfaceItem {
    pub kind: String,
    pub id: String,
    pub display: String,
    #[serde(default)]
    pub description: Option<String>,
    pub evidence: Vec<enrich::EvidenceRef>,
}

#[derive(Serialize, Deserialize, Clone)]
struct SurfaceSeed {
    schema_version: u32,
    #[serde(default)]
    items: Vec<SurfaceSeedItem>,
}

#[derive(Serialize, Deserialize, Clone)]
struct SurfaceSeedItem {
    kind: String,
    id: String,
    #[serde(default)]
    display: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ProbePlan {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    #[serde(default)]
    pub probes: Vec<ProbePlanEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ProbePlanEntry {
    pub id: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default = "default_probe_enabled")]
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct ProbeResult {
    schema_version: u32,
    generated_at_epoch_ms: u128,
    #[serde(default)]
    probe_id: String,
    argv: Vec<String>,
    exit_code: Option<i32>,
    timed_out: bool,
    duration_ms: u128,
    stdout: String,
    stderr: String,
}

#[derive(Clone)]
struct ProbeSpec {
    id: String,
    argv: Vec<String>,
}

#[derive(Clone)]
struct ProbeEvidence {
    id: String,
    result: Option<ProbeResult>,
    evidence: enrich::EvidenceRef,
}

#[derive(Deserialize)]
struct RunsIndex {
    #[serde(default)]
    run_count: Option<usize>,
    #[serde(default)]
    runs: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct SubcommandRow {
    #[serde(default)]
    subcommand: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    probe_path: Option<String>,
    #[serde(default)]
    multi_command_hint: bool,
}

struct SubcommandHit {
    row: SubcommandRow,
    source_root: PathBuf,
}

pub fn apply_surface_discovery(
    doc_pack_root: &Path,
    staging_root: &Path,
    inputs_hash: Option<&str>,
    manifest: Option<&pack::PackManifest>,
    verbose: bool,
) -> Result<()> {
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    let mut discovery = Vec::new();
    let mut items = Vec::new();
    let mut blockers = Vec::new();
    let mut seen = BTreeMap::new();

    let seed_path = paths.surface_seed_path();
    if seed_path.is_file() {
        let evidence = paths.evidence_from_path(&seed_path)?;
        match load_surface_seed(&seed_path) {
            Ok(seed) => {
                discovery.push(SurfaceDiscovery {
                    code: "seed:surface".to_string(),
                    status: "used".to_string(),
                    evidence: vec![evidence.clone()],
                    message: None,
                });
                let mut invalid = Vec::new();
                for item in seed.items {
                    if !is_supported_surface_kind(&item.kind) || item.id.trim().is_empty() {
                        invalid.push(item.id.clone());
                        continue;
                    }
                    let surface_item = SurfaceItem {
                        kind: item.kind,
                        id: item.id.trim().to_string(),
                        display: item.display.unwrap_or_else(|| item.id.trim().to_string()),
                        description: item.description,
                        evidence: vec![evidence.clone()],
                    };
                    merge_surface_item(&mut items, &mut seen, surface_item);
                }
                if !invalid.is_empty() {
                    blockers.push(enrich::Blocker {
                        code: "surface_seed_items_invalid".to_string(),
                        message: "surface seed contains unsupported items".to_string(),
                        evidence: vec![evidence],
                        next_action: Some("fix inventory/surface.seed.json".to_string()),
                    });
                }
            }
            Err(err) => {
                blockers.push(enrich::Blocker {
                    code: "surface_seed_parse_error".to_string(),
                    message: err.to_string(),
                    evidence: vec![evidence],
                    next_action: Some("fix inventory/surface.seed.json".to_string()),
                });
            }
        }
    }

    let plan_path = paths.probes_plan_path();
    let plan_evidence = paths.evidence_from_path(&plan_path)?;
    let mut probe_plan = None;
    if plan_path.is_file() {
        match load_probe_plan(&plan_path) {
            Ok(plan) => {
                discovery.push(SurfaceDiscovery {
                    code: "probe_plan".to_string(),
                    status: "used".to_string(),
                    evidence: vec![plan_evidence.clone()],
                    message: None,
                });
                probe_plan = Some(plan);
            }
            Err(err) => {
                discovery.push(SurfaceDiscovery {
                    code: "probe_plan".to_string(),
                    status: "error".to_string(),
                    evidence: vec![plan_evidence.clone()],
                    message: Some(err.to_string()),
                });
                blockers.push(enrich::Blocker {
                    code: "probe_plan_parse_error".to_string(),
                    message: err.to_string(),
                    evidence: vec![plan_evidence.clone()],
                    next_action: Some("fix inventory/probes/plan.json".to_string()),
                });
            }
        }
    } else {
        discovery.push(SurfaceDiscovery {
            code: "probe_plan".to_string(),
            status: "missing".to_string(),
            evidence: vec![plan_evidence.clone()],
            message: Some("probe plan missing".to_string()),
        });
    }

    let probe_specs = build_probe_specs(probe_plan.as_ref());
    let mut probe_run_blocker = None;
    if let Some(manifest) = manifest {
        let binary_path = PathBuf::from(&manifest.binary_path);
        if binary_path.is_file() {
            for spec in probe_specs {
                match run_probe(&binary_path, &spec.id, &spec.argv, doc_pack_root) {
                    Ok(result) => {
                        let bytes =
                            serde_json::to_vec_pretty(&result).context("serialize probe")?;
                        let rel_path =
                            probe_output_rel_path(&result.probe_id, result.generated_at_epoch_ms);
                        write_staged_bytes(staging_root, &rel_path, &bytes)?;
                    }
                    Err(err) => {
                        discovery.push(SurfaceDiscovery {
                            code: format!("probe:{}", spec.id),
                            status: "error".to_string(),
                            evidence: Vec::new(),
                            message: Some(err.to_string()),
                        });
                    }
                }
            }
        } else {
            probe_run_blocker = Some(enrich::Blocker {
                code: "probe_missing_binary".to_string(),
                message: format!("binary_path {} not found", binary_path.display()),
                evidence: vec![paths.evidence_from_path(&paths.pack_manifest_path())?],
                next_action: Some("regenerate binary.lens pack to refresh manifest".to_string()),
            });
        }
    } else {
        probe_run_blocker = Some(enrich::Blocker {
            code: "probe_missing_manifest".to_string(),
            message: "manifest missing; cannot run probes".to_string(),
            evidence: vec![paths.evidence_from_path(&paths.pack_manifest_path())?],
            next_action: Some("generate binary.lens pack under the doc pack".to_string()),
        });
    }

    let probe_outputs = collect_probe_evidence(staging_root, &paths)?;
    if probe_outputs.is_empty() {
        if let Some(blocker) = probe_run_blocker.clone() {
            blockers.push(blocker);
        }
    }

    for probe in &probe_outputs {
        let status = match probe.result.as_ref() {
            Some(result) if result.timed_out => "timeout",
            Some(result) if result.exit_code == Some(0) => "ok",
            Some(_) => "nonzero",
            None => "invalid",
        };
        discovery.push(SurfaceDiscovery {
            code: format!("probe:{}", probe.id),
            status: status.to_string(),
            evidence: vec![probe.evidence.clone()],
            message: None,
        });
        if let Some(result) = probe.result.as_ref() {
            if let Some(item) = surface_item_from_probe(result, &probe.evidence) {
                merge_surface_item(&mut items, &mut seen, item);
            }
        }
    }

    let mut subcommand_hint_evidence = Vec::new();
    let subcommands_template_path =
        doc_pack_root.join(enrich::SUBCOMMANDS_FROM_PROBES_TEMPLATE_REL);
    let template_evidence = paths.evidence_from_path(&subcommands_template_path)?;
    if subcommands_template_path.is_file() {
        match fs::read_to_string(&subcommands_template_path) {
            Ok(template_sql) => {
                let mut hits = Vec::new();
                let mut ran = false;
                let mut query_errors = Vec::new();
                let mut found_subcommands = false;
                if has_probe_files(&paths.probes_dir())? {
                    ran = true;
                    match run_subcommands_query(doc_pack_root, &template_sql) {
                        Ok(mut rows) => hits.append(&mut rows),
                        Err(err) => query_errors.push(err.to_string()),
                    }
                }
                let staging_probes = staging_root.join("inventory").join("probes");
                if has_probe_files(&staging_probes)? {
                    ran = true;
                    match run_subcommands_query(staging_root, &template_sql) {
                        Ok(mut rows) => hits.append(&mut rows),
                        Err(err) => query_errors.push(err.to_string()),
                    }
                }
                for hit in hits {
                    let evidence = match evidence_from_probe_hit(&hit) {
                        Ok(Some(evidence)) => evidence,
                        Ok(None) => continue,
                        Err(err) => {
                            query_errors.push(err.to_string());
                            continue;
                        }
                    };
                    if hit.row.multi_command_hint {
                        subcommand_hint_evidence.push(evidence.clone());
                    }
                    if let Some(id) = hit.row.subcommand.as_ref().map(|s| s.trim()) {
                        if id.is_empty() {
                            continue;
                        }
                        let description = hit
                            .row
                            .description
                            .as_ref()
                            .map(|desc| desc.trim().to_string())
                            .filter(|desc| !desc.is_empty());
                        let item = SurfaceItem {
                            kind: "subcommand".to_string(),
                            id: id.to_string(),
                            display: id.to_string(),
                            description,
                            evidence: vec![evidence],
                        };
                        merge_surface_item(&mut items, &mut seen, item);
                        found_subcommands = true;
                    }
                }
                let status = if !query_errors.is_empty() {
                    "error"
                } else if ran && found_subcommands {
                    "used"
                } else if ran {
                    "empty"
                } else {
                    "skipped"
                };
                discovery.push(SurfaceDiscovery {
                    code: "subcommands_from_probes".to_string(),
                    status: status.to_string(),
                    evidence: vec![template_evidence.clone()],
                    message: if query_errors.is_empty() {
                        None
                    } else {
                        Some(query_errors.join("; "))
                    },
                });
                if !query_errors.is_empty() {
                    blockers.push(enrich::Blocker {
                        code: "subcommands_query_error".to_string(),
                        message: "subcommands query failed".to_string(),
                        evidence: vec![template_evidence.clone()],
                        next_action: Some(format!(
                            "fix {}",
                            enrich::SUBCOMMANDS_FROM_PROBES_TEMPLATE_REL
                        )),
                    });
                }
            }
            Err(err) => {
                discovery.push(SurfaceDiscovery {
                    code: "subcommands_from_probes".to_string(),
                    status: "error".to_string(),
                    evidence: vec![template_evidence.clone()],
                    message: Some(err.to_string()),
                });
                blockers.push(enrich::Blocker {
                    code: "subcommands_template_read_error".to_string(),
                    message: err.to_string(),
                    evidence: vec![template_evidence.clone()],
                    next_action: Some(format!(
                        "fix {}",
                        enrich::SUBCOMMANDS_FROM_PROBES_TEMPLATE_REL
                    )),
                });
            }
        }
    } else {
        discovery.push(SurfaceDiscovery {
            code: "subcommands_from_probes".to_string(),
            status: "missing".to_string(),
            evidence: vec![template_evidence.clone()],
            message: Some("subcommands template missing".to_string()),
        });
    }

    let runs_index_path = paths.pack_root().join("runs").join("index.json");
    let runs_evidence = paths.evidence_from_path(&runs_index_path)?;
    let mut runs_present = false;
    if runs_index_path.is_file() {
        let bytes = fs::read(&runs_index_path)
            .with_context(|| format!("read {}", runs_index_path.display()))?;
        let index: RunsIndex = serde_json::from_slice(&bytes).context("parse runs index JSON")?;
        let run_count = index.run_count.unwrap_or_else(|| index.runs.len());
        runs_present = run_count > 0;
        discovery.push(SurfaceDiscovery {
            code: "runs_index".to_string(),
            status: if runs_present {
                "used".to_string()
            } else {
                "empty".to_string()
            },
            evidence: vec![runs_evidence.clone()],
            message: None,
        });
    } else {
        discovery.push(SurfaceDiscovery {
            code: "runs_index".to_string(),
            status: "missing".to_string(),
            evidence: vec![runs_evidence.clone()],
            message: Some("runs index missing".to_string()),
        });
    }

    if probe_outputs.is_empty() && !runs_present {
        blockers.push(enrich::Blocker {
            code: "surface_evidence_missing".to_string(),
            message: "no probe outputs or scenario runs available".to_string(),
            evidence: vec![runs_evidence],
            next_action: Some(
                "capture help/usage evidence via probes or scenario runs".to_string(),
            ),
        });
    }

    if !subcommand_hint_evidence.is_empty() {
        let has_subcommands = items.iter().any(|item| item.kind == "subcommand");
        if !has_subcommands {
            dedupe_evidence(&mut subcommand_hint_evidence);
            blockers.push(enrich::Blocker {
                code: "surface_subcommands_missing".to_string(),
                message: "multi-command usage detected but no subcommands extracted".to_string(),
                evidence: subcommand_hint_evidence,
                next_action: Some(
                    "add probes in inventory/probes/plan.json or adjust queries/subcommands_from_probes.sql"
                        .to_string(),
                ),
            });
        }
    }

    let surface = SurfaceInventory {
        schema_version: SURFACE_SCHEMA_VERSION,
        generated_at_epoch_ms: enrich::now_epoch_ms()?,
        binary_name: manifest.map(|m| m.binary_name.clone()),
        inputs_hash: inputs_hash.map(|hash| hash.to_string()),
        discovery,
        items,
        blockers,
    };
    let bytes = serde_json::to_vec_pretty(&surface).context("serialize surface")?;
    write_staged_bytes(staging_root, "inventory/surface.json", &bytes)?;

    if verbose {
        eprintln!("staged surface inventory under {}", staging_root.display());
    }

    Ok(())
}

pub fn load_surface_inventory(path: &Path) -> Result<SurfaceInventory> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let surface: SurfaceInventory =
        serde_json::from_slice(&bytes).context("parse surface inventory")?;
    Ok(surface)
}

fn load_surface_seed(path: &Path) -> Result<SurfaceSeed> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let seed: SurfaceSeed = serde_json::from_slice(&bytes).context("parse surface seed")?;
    if seed.schema_version != SURFACE_SEED_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported surface seed schema_version {}",
            seed.schema_version
        ));
    }
    Ok(seed)
}

pub fn validate_surface_inventory(surface: &SurfaceInventory) -> Result<()> {
    if surface.schema_version != SURFACE_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported surface schema_version {}",
            surface.schema_version
        ));
    }
    for item in &surface.items {
        if !is_supported_surface_kind(item.kind.as_str()) {
            return Err(anyhow!("unsupported surface item kind {:?}", item.kind));
        }
        if item.id.trim().is_empty() {
            return Err(anyhow!("surface item id must not be empty"));
        }
    }
    Ok(())
}

pub fn meaningful_surface_items(surface: &SurfaceInventory) -> usize {
    surface
        .items
        .iter()
        .filter(|item| is_supported_surface_kind(item.kind.as_str()))
        .filter(|item| !item.id.trim().is_empty())
        .count()
}

pub fn default_probe_plan() -> ProbePlan {
    ProbePlan {
        schema_version: PROBE_PLAN_SCHEMA_VERSION,
        generated_at_epoch_ms: enrich::now_epoch_ms().unwrap_or(0),
        probes: vec![
            ProbePlanEntry {
                id: "help".to_string(),
                argv: vec!["--help".to_string()],
                enabled: true,
            },
            ProbePlanEntry {
                id: "dash-h".to_string(),
                argv: vec!["-h".to_string()],
                enabled: true,
            },
            ProbePlanEntry {
                id: "help-subcommand".to_string(),
                argv: vec!["help".to_string()],
                enabled: true,
            },
            ProbePlanEntry {
                id: "no-args".to_string(),
                argv: Vec::new(),
                enabled: true,
            },
        ],
    }
}

fn default_probe_specs() -> Vec<ProbeSpec> {
    default_probe_plan()
        .probes
        .into_iter()
        .filter(|probe| probe.enabled)
        .map(|probe| ProbeSpec {
            id: probe.id,
            argv: probe.argv,
        })
        .collect()
}

fn build_probe_specs(plan: Option<&ProbePlan>) -> Vec<ProbeSpec> {
    let mut specs = default_probe_specs();
    let mut index = std::collections::HashMap::new();
    for (idx, spec) in specs.iter().enumerate() {
        index.insert(spec.id.clone(), idx);
    }
    if let Some(plan) = plan {
        for probe in &plan.probes {
            if !probe.enabled {
                continue;
            }
            let id = probe.id.trim();
            if id.is_empty() {
                continue;
            }
            let argv = probe
                .argv
                .iter()
                .map(|arg| arg.trim().to_string())
                .collect::<Vec<_>>();
            let spec = ProbeSpec {
                id: id.to_string(),
                argv,
            };
            if let Some(idx) = index.get(&spec.id).copied() {
                specs[idx] = spec;
            } else {
                index.insert(spec.id.clone(), specs.len());
                specs.push(spec);
            }
        }
    }
    specs
}

fn load_probe_plan(path: &Path) -> Result<ProbePlan> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let plan: ProbePlan = serde_json::from_slice(&bytes).context("parse probe plan")?;
    validate_probe_plan(&plan)?;
    Ok(plan)
}

fn validate_probe_plan(plan: &ProbePlan) -> Result<()> {
    if plan.schema_version != PROBE_PLAN_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported probe plan schema_version {}",
            plan.schema_version
        ));
    }
    let mut seen = BTreeSet::new();
    for probe in &plan.probes {
        let id = probe.id.trim();
        if id.is_empty() {
            return Err(anyhow!("probe plan entry id must not be empty"));
        }
        if !is_valid_probe_id(id) {
            return Err(anyhow!(
                "probe plan entry id {:?} is not a safe identifier",
                id
            ));
        }
        if !seen.insert(id.to_string()) {
            return Err(anyhow!("probe plan entry id {:?} is duplicated", id));
        }
        for arg in &probe.argv {
            if arg.trim().is_empty() {
                return Err(anyhow!("probe plan entry {:?} has empty argv entries", id));
            }
        }
    }
    Ok(())
}

fn default_probe_enabled() -> bool {
    true
}

fn is_valid_probe_id(id: &str) -> bool {
    !id.contains('/') && !id.contains('\\')
}

fn probe_output_rel_path(probe_id: &str, generated_at_epoch_ms: u128) -> String {
    format!(
        "inventory/probes/{}-{}.json",
        probe_id, generated_at_epoch_ms
    )
}

fn run_probe(
    binary_path: &Path,
    probe_id: &str,
    argv: &[String],
    cwd: &Path,
) -> Result<ProbeResult> {
    let mut cmd = Command::new(binary_path);
    cmd.args(argv)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .current_dir(cwd);

    let start = std::time::Instant::now();
    let mut child = cmd.spawn().context("spawn probe")?;
    let timeout = std::time::Duration::from_secs(PROBE_TIMEOUT_SECS);
    let mut timed_out = false;

    loop {
        if let Some(_status) = child.try_wait().context("check probe status")? {
            break;
        }
        if start.elapsed() > timeout {
            timed_out = true;
            let _ = child.kill();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    let output = child.wait_with_output().context("collect probe output")?;

    let duration_ms = start.elapsed().as_millis();
    let stdout = truncate_bytes(&output.stdout, MAX_PROBE_OUTPUT_BYTES);
    let stderr = truncate_bytes(&output.stderr, MAX_PROBE_OUTPUT_BYTES);

    let mut argv_full = Vec::new();
    argv_full.push(binary_path.display().to_string());
    argv_full.extend(argv.iter().cloned());

    Ok(ProbeResult {
        schema_version: PROBE_SCHEMA_VERSION,
        generated_at_epoch_ms: enrich::now_epoch_ms()?,
        probe_id: probe_id.to_string(),
        argv: argv_full,
        exit_code: output.status.code(),
        timed_out,
        duration_ms,
        stdout,
        stderr,
    })
}

fn collect_probe_evidence(
    staging_root: &Path,
    paths: &enrich::DocPackPaths,
) -> Result<Vec<ProbeEvidence>> {
    let mut candidates: BTreeMap<String, PathBuf> = BTreeMap::new();
    let pack_probe_root = paths.inventory_dir().join("probes");
    let staging_probe_root = staging_root.join("inventory").join("probes");
    for (root, prefer) in [(pack_probe_root, false), (staging_probe_root, true)] {
        if !root.is_dir() {
            continue;
        }
        for file in collect_files_recursive(&root)? {
            if file.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let rel = file.strip_prefix(&root).context("strip probe root")?;
            if !is_probe_evidence_rel_path(rel) {
                continue;
            }
            let rel_path = Path::new("inventory").join("probes").join(rel);
            let rel_string = rel_path.to_string_lossy().to_string();
            if prefer || !candidates.contains_key(&rel_string) {
                candidates.insert(rel_string, file);
            }
        }
    }

    let mut outputs = Vec::new();
    for (rel, path) in candidates {
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let evidence = enrich::EvidenceRef {
            path: rel.clone(),
            sha256: Some(sha256_hex(&bytes)),
        };
        let result: Option<ProbeResult> = serde_json::from_slice(&bytes).ok();
        let id = result
            .as_ref()
            .and_then(|probe| {
                let trimmed = probe.probe_id.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
            .unwrap_or_else(|| {
                Path::new(&rel)
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("probe")
                    .to_string()
            });
        outputs.push(ProbeEvidence {
            id,
            result,
            evidence,
        });
    }

    Ok(outputs)
}

fn has_probe_files(root: &Path) -> Result<bool> {
    if !root.is_dir() {
        return Ok(false);
    }
    for file in collect_files_recursive(root)? {
        if file.extension().and_then(|ext| ext.to_str()) == Some("json") {
            if let Ok(rel) = file.strip_prefix(root) {
                if !is_probe_evidence_rel_path(rel) {
                    continue;
                }
            }
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_probe_evidence_rel_path(rel: &Path) -> bool {
    let file_name = rel.file_name().and_then(|name| name.to_str());
    if matches!(file_name, Some("plan.json") | Some("config.json")) {
        return false;
    }
    if rel
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        == Some("config")
    {
        return false;
    }
    true
}

fn run_subcommands_query(root: &Path, template_sql: &str) -> Result<Vec<SubcommandHit>> {
    let output = pack::run_duckdb_query(template_sql, root)?;
    let rows: Vec<SubcommandRow> =
        if output.is_empty() || output.iter().all(|byte| byte.is_ascii_whitespace()) {
            Vec::new()
        } else {
            serde_json::from_slice(&output).context("parse subcommands query output")?
        };
    Ok(rows
        .into_iter()
        .map(|row| SubcommandHit {
            row,
            source_root: root.to_path_buf(),
        })
        .collect())
}

fn evidence_from_probe_hit(hit: &SubcommandHit) -> Result<Option<enrich::EvidenceRef>> {
    let raw_path = match hit.row.probe_path.as_ref() {
        Some(path) => path,
        None => return Ok(None),
    };
    let rel = match normalize_probe_rel_path(raw_path) {
        Some(rel) => rel,
        None => return Ok(None),
    };
    Ok(Some(evidence_from_rel_path(&hit.source_root, &rel)?))
}

fn normalize_probe_rel_path(raw: &str) -> Option<String> {
    let normalized = raw.replace('\\', "/");
    if let Some(idx) = normalized.rfind("inventory/probes/") {
        return Some(normalized[idx..].to_string());
    }
    if Path::new(&normalized).is_absolute() {
        return None;
    }
    let trimmed = normalized.strip_prefix("./").unwrap_or(normalized.as_str());
    Some(trimmed.to_string())
}

fn evidence_from_rel_path(root: &Path, rel: &str) -> Result<enrich::EvidenceRef> {
    let path = root.join(rel);
    let sha256 = if path.exists() {
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        Some(sha256_hex(&bytes))
    } else {
        None
    };
    Ok(enrich::EvidenceRef {
        path: rel.to_string(),
        sha256,
    })
}

fn merge_surface_item(
    items: &mut Vec<SurfaceItem>,
    seen: &mut BTreeMap<String, usize>,
    mut item: SurfaceItem,
) {
    let key = surface_item_key(&item);
    if let Some(&idx) = seen.get(&key) {
        let existing = &mut items[idx];
        merge_evidence(&mut existing.evidence, &item.evidence);
        if existing.display.trim().is_empty() && !item.display.trim().is_empty() {
            existing.display = std::mem::take(&mut item.display);
        }
        let new_desc = item.description.take().unwrap_or_default();
        if existing
            .description
            .as_ref()
            .map(|desc| desc.trim().is_empty())
            .unwrap_or(true)
            && !new_desc.trim().is_empty()
        {
            existing.description = Some(new_desc);
        }
        return;
    }
    seen.insert(key, items.len());
    items.push(item);
}

fn merge_evidence(target: &mut Vec<enrich::EvidenceRef>, incoming: &[enrich::EvidenceRef]) {
    let mut seen = BTreeSet::new();
    for existing in target.iter() {
        seen.insert(existing.path.clone());
    }
    for entry in incoming {
        if seen.insert(entry.path.clone()) {
            target.push(entry.clone());
        }
    }
}

fn dedupe_evidence(entries: &mut Vec<enrich::EvidenceRef>) {
    let mut seen = BTreeSet::new();
    entries.retain(|entry| seen.insert(entry.path.clone()));
}

fn surface_item_key(item: &SurfaceItem) -> String {
    format!("{}:{}", item.kind, item.id)
}

fn is_supported_surface_kind(kind: &str) -> bool {
    matches!(kind, "option" | "command" | "subcommand")
}

fn surface_item_from_probe(
    probe: &ProbeResult,
    evidence: &enrich::EvidenceRef,
) -> Option<SurfaceItem> {
    let arg = probe.argv.iter().skip(1).last()?;
    let trimmed = arg.trim();
    if trimmed.is_empty() || trimmed == "--" {
        return None;
    }
    let kind = if trimmed.starts_with('-') {
        "option"
    } else {
        "command"
    };
    Some(SurfaceItem {
        kind: kind.to_string(),
        id: trimmed.to_string(),
        display: trimmed.to_string(),
        description: None,
        evidence: vec![evidence.clone()],
    })
}
