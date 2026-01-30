//! Coverage ledger construction from scenario plans.
//!
//! Coverage is derived from explicit scenario intent to avoid Rust-side
//! inference and keep semantics pack-owned.
use super::shared::{is_surface_item_kind, normalize_surface_id};
use crate::enrich;
use crate::scenarios::{load_plan, CoverageItemEntry, CoverageLedger, ScenarioPlan, ScenarioSpec};
use crate::surface;
use crate::util::display_path;
use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Build the coverage ledger for a surface inventory and scenario plan.
pub fn build_coverage_ledger(
    binary_name: &str,
    surface: &surface::SurfaceInventory,
    doc_pack_root: &Path,
    scenarios_path: &Path,
    display_root: Option<&Path>,
) -> Result<CoverageLedger> {
    let plan = load_plan(scenarios_path, doc_pack_root)?;
    if let Some(plan_binary) = plan.binary.as_deref() {
        if plan_binary != binary_name {
            return Err(anyhow!(
                "scenarios plan binary {:?} does not match pack binary {:?}",
                plan_binary,
                binary_name
            ));
        }
    }

    let surface_path = doc_pack_root.join("inventory").join("surface.json");
    let surface_evidence = enrich::evidence_from_path(doc_pack_root, &surface_path)?;
    let plan_evidence = enrich::evidence_from_path(doc_pack_root, scenarios_path)?;
    let mut items = init_coverage_items(surface);
    let mut warnings = Vec::new();
    let mut unknown_items = BTreeSet::new();
    let blocked_map = collect_blocked_items(&plan);

    apply_scenarios_to_coverage(&plan, &mut items, &mut warnings, &mut unknown_items);
    apply_blocked_items(blocked_map, &mut items, &mut warnings, &mut unknown_items);

    let (entries, counts) = build_coverage_entries(items, &surface_evidence, &plan_evidence);

    let generated_at_epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("compute timestamp")?
        .as_millis();

    Ok(CoverageLedger {
        schema_version: 3,
        generated_at_epoch_ms,
        binary_name: binary_name.to_string(),
        scenarios_path: display_path(scenarios_path, display_root),
        validation_source: "plan".to_string(),
        items_total: entries.len(),
        behavior_count: counts.behavior_count,
        rejected_count: counts.rejected_count,
        acceptance_count: counts.acceptance_count,
        blocked_count: counts.blocked_count,
        uncovered_count: counts.uncovered_count,
        items: entries,
        unknown_items: unknown_items.into_iter().collect(),
        warnings,
    })
}

fn init_coverage_items(surface: &surface::SurfaceInventory) -> BTreeMap<String, CoverageState> {
    let mut items = BTreeMap::new();
    for item in surface
        .items
        .iter()
        .filter(|item| is_surface_item_kind(&item.kind))
    {
        let aliases = if item.display != item.id {
            vec![item.display.clone()]
        } else {
            Vec::new()
        };
        items.insert(
            item.id.clone(),
            CoverageState {
                aliases,
                evidence: item.evidence.clone(),
                ..CoverageState::default()
            },
        );
    }
    items
}

fn collect_blocked_items(plan: &ScenarioPlan) -> HashMap<String, BlockedInfo> {
    let mut blocked_map = HashMap::new();
    let Some(coverage) = plan.coverage.as_ref() else {
        return blocked_map;
    };
    for blocked in &coverage.blocked {
        for item_id in &blocked.item_ids {
            let normalized = normalize_surface_id(item_id);
            if normalized.is_empty() {
                continue;
            }
            let entry = blocked_map.entry(normalized).or_insert(BlockedInfo {
                reason: blocked.reason.clone(),
                details: blocked.details.clone(),
                tags: blocked.tags.clone(),
            });
            if entry.reason != blocked.reason {
                entry.reason = format!("{}, {}", entry.reason, blocked.reason);
            }
            if let Some(details) = blocked.details.as_ref() {
                let updated = match entry.details.take() {
                    Some(existing) if existing != *details => format!("{existing}; {details}"),
                    Some(existing) => existing,
                    None => details.clone(),
                };
                entry.details = Some(updated);
            }
            for tag in &blocked.tags {
                if !entry.tags.contains(tag) {
                    entry.tags.push(tag.clone());
                }
            }
            entry.tags.sort();
            entry.tags.dedup();
        }
    }
    blocked_map
}

fn apply_scenarios_to_coverage(
    plan: &ScenarioPlan,
    items: &mut BTreeMap<String, CoverageState>,
    warnings: &mut Vec<String>,
    unknown_items: &mut BTreeSet<String>,
) {
    for scenario in &plan.scenarios {
        if scenario.coverage_ignore {
            continue;
        }
        if scenario.covers.is_empty() {
            warnings.push(format!(
                "scenario {:?} missing covers for coverage",
                scenario.id
            ));
            continue;
        }
        let tier = coverage_tier(scenario);
        let option_ids = scenario_surface_ids(scenario);
        for item_id in option_ids {
            match items.get_mut(&item_id) {
                Some(entry) => match tier {
                    CoverageTier::Behavior => {
                        entry.behavior_scenarios.insert(scenario.id.clone());
                    }
                    CoverageTier::Rejection => {
                        entry.rejection_scenarios.insert(scenario.id.clone());
                    }
                    CoverageTier::Acceptance => {
                        entry.acceptance_scenarios.insert(scenario.id.clone());
                    }
                },
                None => {
                    unknown_items.insert(item_id);
                }
            }
        }
    }
}

fn apply_blocked_items(
    blocked_map: HashMap<String, BlockedInfo>,
    items: &mut BTreeMap<String, CoverageState>,
    warnings: &mut Vec<String>,
    unknown_items: &mut BTreeSet<String>,
) {
    for (option_id, blocked) in blocked_map {
        match items.get_mut(&option_id) {
            Some(entry) => {
                if !entry.behavior_scenarios.is_empty() {
                    warnings.push(format!(
                        "item {:?} marked blocked but has behavior coverage",
                        option_id
                    ));
                }
                entry.blocked = Some(blocked);
            }
            None => {
                warnings.push(format!(
                    "blocked item {:?} not found in surface inventory",
                    option_id
                ));
                unknown_items.insert(option_id);
            }
        }
    }
}

#[derive(Default)]
struct CoverageCounts {
    behavior_count: usize,
    rejected_count: usize,
    acceptance_count: usize,
    blocked_count: usize,
    uncovered_count: usize,
}

fn build_coverage_entries(
    items: BTreeMap<String, CoverageState>,
    surface_evidence: &enrich::EvidenceRef,
    plan_evidence: &enrich::EvidenceRef,
) -> (Vec<CoverageItemEntry>, CoverageCounts) {
    let mut entries = Vec::new();
    let mut counts = CoverageCounts::default();

    for (item_id, entry) in items {
        let behavior_scenarios: Vec<String> = entry.behavior_scenarios.into_iter().collect();
        let rejection_scenarios: Vec<String> = entry.rejection_scenarios.into_iter().collect();
        let acceptance_scenarios: Vec<String> = entry.acceptance_scenarios.into_iter().collect();
        let (blocked_reason, blocked_details, blocked_tags, is_blocked) =
            match entry.blocked.as_ref() {
                Some(blocked) => (
                    Some(blocked.reason.clone()),
                    blocked.details.clone(),
                    blocked.tags.clone(),
                    true,
                ),
                None => (None, None, Vec::new(), false),
            };
        let status = if !behavior_scenarios.is_empty() {
            counts.behavior_count += 1;
            "behavior"
        } else if !rejection_scenarios.is_empty() {
            counts.rejected_count += 1;
            "rejected"
        } else if !acceptance_scenarios.is_empty() {
            counts.acceptance_count += 1;
            "acceptance"
        } else if is_blocked {
            counts.blocked_count += 1;
            "blocked"
        } else {
            counts.uncovered_count += 1;
            "uncovered"
        };

        let mut evidence = entry.evidence.clone();
        evidence.push(surface_evidence.clone());
        evidence.push(plan_evidence.clone());
        enrich::dedupe_evidence_refs(&mut evidence);

        entries.push(CoverageItemEntry {
            item_id,
            aliases: entry.aliases,
            status: status.to_string(),
            behavior_scenarios,
            rejection_scenarios,
            acceptance_scenarios,
            blocked_reason,
            blocked_details,
            blocked_tags,
            evidence,
        });
    }

    (entries, counts)
}

#[derive(Debug, Clone)]
struct BlockedInfo {
    reason: String,
    details: Option<String>,
    tags: Vec<String>,
}

#[derive(Debug, Default)]
struct CoverageState {
    aliases: Vec<String>,
    evidence: Vec<enrich::EvidenceRef>,
    behavior_scenarios: BTreeSet<String>,
    rejection_scenarios: BTreeSet<String>,
    acceptance_scenarios: BTreeSet<String>,
    blocked: Option<BlockedInfo>,
}

#[derive(Debug)]
enum CoverageTier {
    Behavior,
    Rejection,
    Acceptance,
}

fn coverage_tier(scenario: &ScenarioSpec) -> CoverageTier {
    match scenario.coverage_tier.as_deref() {
        Some("behavior") => CoverageTier::Behavior,
        Some("rejection") => CoverageTier::Rejection,
        _ => CoverageTier::Acceptance,
    }
}

fn scenario_surface_ids(scenario: &ScenarioSpec) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for token in &scenario.covers {
        let normalized = normalize_surface_id(token);
        if !normalized.is_empty() {
            ids.insert(normalized);
        }
    }
    ids.into_iter().collect()
}
