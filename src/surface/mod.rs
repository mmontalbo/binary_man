//! Surface discovery pipeline.
//!
//! Surface items are derived from SQL lenses over scenario evidence to keep
//! semantics pack-owned and deterministic.
mod behavior_exclusion;
mod lens;
mod overlays;
mod types;

use crate::enrich;
use crate::pack;
use crate::scenarios;
use crate::staging::{collect_files_recursive, write_staged_bytes};
use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) use behavior_exclusion::{validate_behavior_exclusions, BehaviorExclusionLedgerEntry};
pub(crate) use overlays::{
    collect_behavior_exclusions, load_surface_overlays_if_exists, SurfaceBehaviorExclusion,
};
pub use types::{SurfaceDiscovery, SurfaceInventory, SurfaceInvocation, SurfaceItem};

const SURFACE_SCHEMA_VERSION: u32 = 2;
const SURFACE_OVERLAYS_SCHEMA_VERSION: u32 = 3;

#[derive(Default)]
pub(super) struct SurfaceState {
    discovery: Vec<SurfaceDiscovery>,
    items: Vec<SurfaceItem>,
    blockers: Vec<enrich::Blocker>,
    seen: BTreeMap<String, usize>,
    subcommand_hint_evidence: Vec<enrich::EvidenceRef>,
}

struct PlanState {
    plan_path: PathBuf,
    plan_evidence: enrich::EvidenceRef,
    plan_present: bool,
    help_scenarios_present: bool,
}

/// Run surface discovery and stage `inventory/surface.json`.
pub fn apply_surface_discovery(
    doc_pack_root: &Path,
    staging_root: &Path,
    inputs_hash: Option<&str>,
    manifest: Option<&pack::PackManifest>,
    lens_flake: &str,
    verbose: bool,
) -> Result<()> {
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    let mut state = SurfaceState::default();

    let plan_state = load_plan_state(&paths, &mut state)?;
    if plan_state.plan_present && !plan_state.help_scenarios_present {
        state.blockers.push(enrich::Blocker {
            code: "surface_help_scenarios_missing".to_string(),
            message: "no help scenarios available for surface discovery".to_string(),
            evidence: vec![plan_state.plan_evidence.clone()],
            next_action: Some("add a help scenario in scenarios/plan.json".to_string()),
        });
    }

    let pack_scenarios = paths.inventory_scenarios_dir();
    let staging_scenarios = staging_root.join("inventory").join("scenarios");
    let pack_has_scenarios = has_scenario_files(&pack_scenarios)?;
    let staging_has_scenarios = has_scenario_files(&staging_scenarios)?;
    let staging_has_scenarios = maybe_auto_run_help_scenarios(AutoRunHelpScenariosArgs {
        doc_pack_root,
        staging_root,
        paths: &paths,
        plan_state: &plan_state,
        manifest,
        lens_flake,
        verbose,
        pack_has_scenarios,
        staging_has_scenarios,
        state: &mut state,
    })?;

    lens::run_surface_lenses(
        doc_pack_root,
        staging_root,
        pack_has_scenarios,
        staging_has_scenarios,
        &paths,
        &mut state,
    )?;
    overlays::apply_surface_overlays(&paths, &mut state)?;
    lens::add_subcommand_missing_blocker(&mut state);

    let surface = SurfaceInventory {
        schema_version: SURFACE_SCHEMA_VERSION,
        generated_at_epoch_ms: enrich::now_epoch_ms()?,
        binary_name: manifest.map(|m| m.binary_name.clone()),
        inputs_hash: inputs_hash.map(|hash| hash.to_string()),
        discovery: state.discovery,
        items: state.items,
        blockers: state.blockers,
    };
    let bytes = serde_json::to_vec_pretty(&surface).context("serialize surface")?;
    write_staged_bytes(staging_root, "inventory/surface.json", &bytes)?;

    if verbose {
        eprintln!("staged surface inventory under {}", staging_root.display());
    }

    Ok(())
}

fn load_plan_state(paths: &enrich::DocPackPaths, state: &mut SurfaceState) -> Result<PlanState> {
    let plan_path = paths.scenarios_plan_path();
    let plan_evidence = paths.evidence_from_path(&plan_path)?;
    let mut plan_present = false;
    let mut help_scenarios_present = false;

    match scenarios::load_plan_if_exists(&plan_path, paths.root()) {
        Ok(Some(loaded)) => {
            state.discovery.push(SurfaceDiscovery {
                code: "scenarios_plan".to_string(),
                status: "used".to_string(),
                evidence: vec![plan_evidence.clone()],
                message: None,
            });
            help_scenarios_present = loaded
                .scenarios
                .iter()
                .any(|scenario| scenario.kind == scenarios::ScenarioKind::Help);
            plan_present = true;
        }
        Ok(None) => {
            state.discovery.push(SurfaceDiscovery {
                code: "scenarios_plan".to_string(),
                status: "missing".to_string(),
                evidence: vec![plan_evidence.clone()],
                message: Some("scenarios plan missing".to_string()),
            });
            state.blockers.push(enrich::Blocker {
                code: "scenario_plan_missing".to_string(),
                message: "scenarios plan missing".to_string(),
                evidence: vec![plan_evidence.clone()],
                next_action: Some("create scenarios/plan.json".to_string()),
            });
        }
        Err(err) => {
            state.discovery.push(SurfaceDiscovery {
                code: "scenarios_plan".to_string(),
                status: "error".to_string(),
                evidence: vec![plan_evidence.clone()],
                message: Some(err.to_string()),
            });
            state.blockers.push(enrich::Blocker {
                code: "scenario_plan_invalid".to_string(),
                message: err.to_string(),
                evidence: vec![plan_evidence.clone()],
                next_action: Some("fix scenarios/plan.json".to_string()),
            });
        }
    }

    Ok(PlanState {
        plan_path,
        plan_evidence,
        plan_present,
        help_scenarios_present,
    })
}

struct AutoRunHelpScenariosArgs<'a> {
    doc_pack_root: &'a Path,
    staging_root: &'a Path,
    paths: &'a enrich::DocPackPaths,
    plan_state: &'a PlanState,
    manifest: Option<&'a pack::PackManifest>,
    lens_flake: &'a str,
    verbose: bool,
    pack_has_scenarios: bool,
    staging_has_scenarios: bool,
    state: &'a mut SurfaceState,
}

fn maybe_auto_run_help_scenarios(args: AutoRunHelpScenariosArgs<'_>) -> Result<bool> {
    let AutoRunHelpScenariosArgs {
        doc_pack_root,
        staging_root,
        paths,
        plan_state,
        manifest,
        lens_flake,
        verbose,
        pack_has_scenarios,
        mut staging_has_scenarios,
        state,
    } = args;
    if !pack_has_scenarios && !staging_has_scenarios && plan_state.help_scenarios_present {
        if let Some(manifest) = manifest {
            let binary_path = PathBuf::from(&manifest.binary_path);
            if !binary_path.is_file() {
                state.blockers.push(enrich::Blocker {
                    code: "scenario_missing_binary".to_string(),
                    message: format!("binary_path {} not found", binary_path.display()),
                    evidence: vec![paths.evidence_from_path(&paths.pack_manifest_path())?],
                    next_action: Some(
                        "regenerate binary.lens pack to refresh manifest".to_string(),
                    ),
                });
            } else if !paths.pack_root().is_dir() {
                state.blockers.push(enrich::Blocker {
                    code: "scenario_missing_pack".to_string(),
                    message: "pack root missing; cannot run scenarios".to_string(),
                    evidence: vec![paths.evidence_from_path(&paths.pack_manifest_path())?],
                    next_action: Some("generate binary.lens pack under the doc pack".to_string()),
                });
            } else {
                let _report = scenarios::run_scenarios(&scenarios::RunScenariosArgs {
                    pack_root: &paths.pack_root(),
                    run_root: doc_pack_root,
                    binary_name: &manifest.binary_name,
                    scenarios_path: &plan_state.plan_path,
                    lens_flake,
                    display_root: Some(doc_pack_root),
                    staging_root: Some(staging_root),
                    kind_filter: Some(scenarios::ScenarioKind::Help),
                    run_mode: scenarios::ScenarioRunMode::Default,
                    forced_rerun_scenario_ids: Vec::new(),
                    extra_scenarios: Vec::new(),
                    auto_run_limit: None,
                    auto_progress: None,
                    verbose,
                })?;
                state.discovery.push(SurfaceDiscovery {
                    code: "help_scenarios_auto_run".to_string(),
                    status: "used".to_string(),
                    evidence: vec![plan_state.plan_evidence.clone()],
                    message: Some("auto-ran help scenarios for surface discovery".to_string()),
                });
                staging_has_scenarios =
                    has_scenario_files(&staging_root.join("inventory").join("scenarios"))?;
            }
        } else {
            state.blockers.push(enrich::Blocker {
                code: "scenario_missing_manifest".to_string(),
                message: "manifest missing; cannot run scenarios".to_string(),
                evidence: vec![paths.evidence_from_path(&paths.pack_manifest_path())?],
                next_action: Some("generate binary.lens pack under the doc pack".to_string()),
            });
        }
    }
    Ok(staging_has_scenarios)
}

/// Load a surface inventory from disk.
pub fn load_surface_inventory(path: &Path) -> Result<SurfaceInventory> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let surface: SurfaceInventory =
        serde_json::from_slice(&bytes).context("parse surface inventory")?;
    Ok(surface)
}

/// Validate a surface inventory against the expected schema version.
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

/// Count meaningful surface items (options/commands/subcommands).
pub fn meaningful_surface_items(surface: &SurfaceInventory) -> usize {
    surface
        .items
        .iter()
        .filter(|item| is_supported_surface_kind(item.kind.as_str()))
        .filter(|item| !item.id.trim().is_empty())
        .count()
}

/// Return the preferred surface item for a given id.
///
/// If multiple items share an id, option entries win because verification stubs
/// and overlays are option-centric.
pub(crate) fn primary_surface_item_by_id<'a>(
    surface: &'a SurfaceInventory,
    surface_id: &str,
) -> Option<&'a SurfaceItem> {
    let mut fallback = None;
    for item in &surface.items {
        if item.id.trim() != surface_id {
            continue;
        }
        if item.kind == "option" {
            return Some(item);
        }
        if fallback.is_none() {
            fallback = Some(item);
        }
    }
    fallback
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
        merge_string_list(&mut existing.forms, &item.forms);
        merge_invocation(&mut existing.invocation, &item.invocation);
        return;
    }
    merge_string_list(&mut item.forms, &[]);
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

fn merge_string_list(target: &mut Vec<String>, incoming: &[String]) {
    if incoming.is_empty() && target.is_empty() {
        return;
    }
    let mut merged = BTreeSet::new();
    for value in target.iter().chain(incoming.iter()) {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        merged.insert(trimmed.to_string());
    }
    *target = merged.into_iter().collect();
}

fn merge_invocation(target: &mut types::SurfaceInvocation, incoming: &types::SurfaceInvocation) {
    if target.value_arity == "unknown" && incoming.value_arity != "unknown" {
        target.value_arity = incoming.value_arity.clone();
    } else if target.value_arity != incoming.value_arity && incoming.value_arity != "unknown" {
        target.value_arity = "unknown".to_string();
    }

    target.value_separator =
        merge_value_separator(&target.value_separator, &incoming.value_separator);

    if target
        .value_placeholder
        .as_ref()
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        let incoming_value = incoming
            .value_placeholder
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        if let Some(value) = incoming_value {
            target.value_placeholder = Some(value.to_string());
        }
    }

    merge_string_list(&mut target.value_examples, &incoming.value_examples);
    merge_string_list(&mut target.requires_argv, &incoming.requires_argv);
}

fn merge_value_separator(current: &str, incoming: &str) -> String {
    if current == incoming {
        return current.to_string();
    }
    if current == "unknown" {
        return incoming.to_string();
    }
    if incoming == "unknown" {
        return current.to_string();
    }
    if current == "either" || incoming == "either" {
        return "either".to_string();
    }
    if (current == "equals" && incoming == "space") || (current == "space" && incoming == "equals")
    {
        return "either".to_string();
    }
    "unknown".to_string()
}

fn surface_item_key(item: &SurfaceItem) -> String {
    format!("{}:{}", item.kind, item.id)
}

fn is_supported_surface_kind(kind: &str) -> bool {
    matches!(kind, "option" | "command" | "subcommand")
}
