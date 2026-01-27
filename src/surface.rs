use crate::enrich;
use crate::pack;
use crate::scenarios;
use crate::staging::{collect_files_recursive, write_staged_bytes};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const SURFACE_SCHEMA_VERSION: u32 = 1;
const SURFACE_SEED_SCHEMA_VERSION: u32 = 1;

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

#[derive(Deserialize)]
struct SurfaceLensRow {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    display: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    scenario_path: Option<String>,
    #[serde(default)]
    multi_command_hint: bool,
}

type ScenarioHit<T> = (T, PathBuf);

struct ScenarioQueryRun<T> {
    hits: Vec<T>,
    ran: bool,
    errors: Vec<String>,
}

fn run_scenario_query<T, F>(
    pack_root: &Path,
    staging_root: &Path,
    template_sql: &str,
    pack_has_scenarios: bool,
    staging_has_scenarios: bool,
    mut run_query: F,
) -> ScenarioQueryRun<T>
where
    F: FnMut(&Path, &str) -> Result<Vec<T>>,
{
    let mut hits = Vec::new();
    let mut ran = false;
    let mut errors = Vec::new();

    if pack_has_scenarios {
        ran = true;
        match run_query(pack_root, template_sql) {
            Ok(mut rows) => hits.append(&mut rows),
            Err(err) => errors.push(err.to_string()),
        }
    }
    if staging_has_scenarios {
        ran = true;
        match run_query(staging_root, template_sql) {
            Ok(mut rows) => hits.append(&mut rows),
            Err(err) => errors.push(err.to_string()),
        }
    }

    ScenarioQueryRun { hits, ran, errors }
}

fn query_status(ran: bool, found: bool, has_errors: bool) -> &'static str {
    if has_errors {
        "error"
    } else if ran && found {
        "used"
    } else if ran {
        "empty"
    } else {
        "skipped"
    }
}

pub fn apply_surface_discovery(
    doc_pack_root: &Path,
    staging_root: &Path,
    inputs_hash: Option<&str>,
    manifest: Option<&pack::PackManifest>,
    surface_lens_templates: &[String],
    lens_flake: &str,
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

    let plan_path = paths.scenarios_plan_path();
    let plan_evidence = paths.evidence_from_path(&plan_path)?;
    let mut plan = None;
    let mut help_scenarios_present = false;
    match scenarios::load_plan_if_exists(&plan_path) {
        Ok(Some(loaded)) => {
            discovery.push(SurfaceDiscovery {
                code: "scenarios_plan".to_string(),
                status: "used".to_string(),
                evidence: vec![plan_evidence.clone()],
                message: None,
            });
            help_scenarios_present = loaded
                .scenarios
                .iter()
                .any(|scenario| scenario.kind == scenarios::ScenarioKind::Help);
            plan = Some(loaded);
        }
        Ok(None) => {
            discovery.push(SurfaceDiscovery {
                code: "scenarios_plan".to_string(),
                status: "missing".to_string(),
                evidence: vec![plan_evidence.clone()],
                message: Some("scenarios plan missing".to_string()),
            });
            blockers.push(enrich::Blocker {
                code: "scenario_plan_missing".to_string(),
                message: "scenarios plan missing".to_string(),
                evidence: vec![plan_evidence.clone()],
                next_action: Some("create scenarios/plan.json".to_string()),
            });
        }
        Err(err) => {
            discovery.push(SurfaceDiscovery {
                code: "scenarios_plan".to_string(),
                status: "error".to_string(),
                evidence: vec![plan_evidence.clone()],
                message: Some(err.to_string()),
            });
            blockers.push(enrich::Blocker {
                code: "scenario_plan_invalid".to_string(),
                message: err.to_string(),
                evidence: vec![plan_evidence.clone()],
                next_action: Some("fix scenarios/plan.json".to_string()),
            });
        }
    }

    if plan.is_some() && !help_scenarios_present {
        blockers.push(enrich::Blocker {
            code: "surface_help_scenarios_missing".to_string(),
            message: "no help scenarios available for surface discovery".to_string(),
            evidence: vec![plan_evidence.clone()],
            next_action: Some("add a help scenario in scenarios/plan.json".to_string()),
        });
    }

    let pack_scenarios = paths.inventory_scenarios_dir();
    let staging_scenarios = staging_root.join("inventory").join("scenarios");
    let pack_has_scenarios = has_scenario_files(&pack_scenarios)?;
    let mut staging_has_scenarios = has_scenario_files(&staging_scenarios)?;

    if !pack_has_scenarios && !staging_has_scenarios && help_scenarios_present {
        if let Some(manifest) = manifest {
            let binary_path = PathBuf::from(&manifest.binary_path);
            if !binary_path.is_file() {
                blockers.push(enrich::Blocker {
                    code: "scenario_missing_binary".to_string(),
                    message: format!("binary_path {} not found", binary_path.display()),
                    evidence: vec![paths.evidence_from_path(&paths.pack_manifest_path())?],
                    next_action: Some(
                        "regenerate binary.lens pack to refresh manifest".to_string(),
                    ),
                });
            } else if !paths.pack_root().is_dir() {
                blockers.push(enrich::Blocker {
                    code: "scenario_missing_pack".to_string(),
                    message: "pack root missing; cannot run scenarios".to_string(),
                    evidence: vec![paths.evidence_from_path(&paths.pack_manifest_path())?],
                    next_action: Some("generate binary.lens pack under the doc pack".to_string()),
                });
            } else {
                let _report = scenarios::run_scenarios(
                    &paths.pack_root(),
                    doc_pack_root,
                    &manifest.binary_name,
                    &plan_path,
                    lens_flake,
                    Some(doc_pack_root),
                    Some(staging_root),
                    Some(scenarios::ScenarioKind::Help),
                    scenarios::ScenarioRunMode::Default,
                    verbose,
                )?;
                discovery.push(SurfaceDiscovery {
                    code: "help_scenarios_auto_run".to_string(),
                    status: "used".to_string(),
                    evidence: vec![plan_evidence.clone()],
                    message: Some("auto-ran help scenarios for surface discovery".to_string()),
                });
                staging_has_scenarios = has_scenario_files(&staging_scenarios)?;
            }
        } else {
            blockers.push(enrich::Blocker {
                code: "scenario_missing_manifest".to_string(),
                message: "manifest missing; cannot run scenarios".to_string(),
                evidence: vec![paths.evidence_from_path(&paths.pack_manifest_path())?],
                next_action: Some("generate binary.lens pack under the doc pack".to_string()),
            });
        }
    }

    let mut subcommand_hint_evidence = Vec::new();

    for template_rel in surface_lens_templates {
        let template_path = doc_pack_root.join(template_rel);
        let template_evidence = paths.evidence_from_path(&template_path)?;
        if template_path.is_file() {
            match fs::read_to_string(&template_path) {
                Ok(template_sql) => {
                    let run = run_scenario_query(
                        doc_pack_root,
                        staging_root,
                        &template_sql,
                        pack_has_scenarios,
                        staging_has_scenarios,
                        run_surface_lens_query,
                    );
                    let mut query_errors = run.errors;
                    let mut found_any = false;
                    for (row, source_root) in run.hits {
                        let evidence = match evidence_from_scenario_path(
                            &source_root,
                            row.scenario_path.as_ref(),
                        ) {
                            Ok(Some(evidence)) => evidence,
                            Ok(None) => {
                                if row.multi_command_hint {
                                    query_errors.push(format!(
                                        "lens row missing scenario_path (template {template_rel})"
                                    ));
                                }
                                continue;
                            }
                            Err(err) => {
                                query_errors.push(err.to_string());
                                continue;
                            }
                        };
                        if row.multi_command_hint {
                            subcommand_hint_evidence.push(evidence.clone());
                            found_any = true;
                        }
                        let kind = row.kind.as_ref().map(|value| value.trim()).unwrap_or("");
                        let id = row.id.as_ref().map(|value| value.trim()).unwrap_or("");
                        if kind.is_empty() || id.is_empty() {
                            continue;
                        }
                        if !is_supported_surface_kind(kind) {
                            query_errors.push(format!(
                                "unsupported surface kind {kind:?} (template {template_rel})"
                            ));
                            continue;
                        }
                        let display = row
                            .display
                            .as_ref()
                            .map(|value| value.trim())
                            .filter(|value| !value.is_empty())
                            .unwrap_or(id)
                            .to_string();
                        let description = row
                            .description
                            .as_ref()
                            .map(|desc| desc.trim().to_string())
                            .filter(|desc| !desc.is_empty());
                        let item = SurfaceItem {
                            kind: kind.to_string(),
                            id: id.to_string(),
                            display,
                            description,
                            evidence: vec![evidence],
                        };
                        merge_surface_item(&mut items, &mut seen, item);
                        found_any = true;
                    }
                    let status = query_status(run.ran, found_any, !query_errors.is_empty());
                    discovery.push(SurfaceDiscovery {
                        code: template_rel.to_string(),
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
                            code: "surface_lens_query_error".to_string(),
                            message: format!("surface lens query failed ({template_rel})"),
                            evidence: vec![template_evidence.clone()],
                            next_action: Some(format!("fix {template_rel}")),
                        });
                    }
                }
                Err(err) => {
                    discovery.push(SurfaceDiscovery {
                        code: template_rel.to_string(),
                        status: "error".to_string(),
                        evidence: vec![template_evidence.clone()],
                        message: Some(err.to_string()),
                    });
                    blockers.push(enrich::Blocker {
                        code: "surface_lens_template_read_error".to_string(),
                        message: err.to_string(),
                        evidence: vec![template_evidence.clone()],
                        next_action: Some(format!("fix {template_rel}")),
                    });
                }
            }
        } else {
            discovery.push(SurfaceDiscovery {
                code: template_rel.to_string(),
                status: "missing".to_string(),
                evidence: vec![template_evidence.clone()],
                message: Some("surface lens template missing".to_string()),
            });
        }
    }

    if !subcommand_hint_evidence.is_empty() {
        let has_subcommands = items.iter().any(|item| item.kind == "subcommand");
        if !has_subcommands {
            enrich::dedupe_evidence_refs(&mut subcommand_hint_evidence);
            blockers.push(enrich::Blocker {
                code: "surface_subcommands_missing".to_string(),
                message: "multi-command usage detected but no subcommands extracted".to_string(),
                evidence: subcommand_hint_evidence,
                next_action: Some(
                    "add help scenarios in scenarios/plan.json or adjust queries/subcommands_from_scenarios.sql"
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

fn has_scenario_files(root: &Path) -> Result<bool> {
    if !root.is_dir() {
        return Ok(false);
    }
    for file in collect_files_recursive(root)? {
        if file.extension().and_then(|ext| ext.to_str()) == Some("json") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn run_surface_lens_query(
    root: &Path,
    template_sql: &str,
) -> Result<Vec<ScenarioHit<SurfaceLensRow>>> {
    let output = pack::run_duckdb_query(template_sql, root)?;
    let rows: Vec<SurfaceLensRow> =
        if output.is_empty() || output.iter().all(|byte| byte.is_ascii_whitespace()) {
            Vec::new()
        } else {
            serde_json::from_slice(&output).context("parse surface lens query output")?
        };
    Ok(rows
        .into_iter()
        .map(|row| (row, root.to_path_buf()))
        .collect())
}

fn evidence_from_scenario_path(
    source_root: &Path,
    raw_path: Option<&String>,
) -> Result<Option<enrich::EvidenceRef>> {
    let raw_path = match raw_path {
        Some(path) => path,
        None => return Ok(None),
    };
    let rel = match normalize_scenario_rel_path(raw_path) {
        Some(rel) => rel,
        None => return Ok(None),
    };
    Ok(Some(enrich::evidence_from_rel(source_root, &rel)?))
}

fn normalize_scenario_rel_path(raw: &str) -> Option<String> {
    let normalized = raw.replace('\\', "/");
    if let Some(idx) = normalized.rfind("inventory/scenarios/") {
        return Some(normalized[idx..].to_string());
    }
    if Path::new(&normalized).is_absolute() {
        return None;
    }
    let trimmed = normalized.strip_prefix("./").unwrap_or(normalized.as_str());
    Some(trimmed.to_string())
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

fn surface_item_key(item: &SurfaceItem) -> String {
    format!("{}:{}", item.kind, item.id)
}

fn is_supported_surface_kind(kind: &str) -> bool {
    matches!(kind, "option" | "command" | "subcommand")
}
