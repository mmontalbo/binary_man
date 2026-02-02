use crate::enrich;
use crate::scenarios;
use crate::surface;
use std::collections::{BTreeMap, BTreeSet};

const DEFAULT_BEHAVIOR_SEED_PATH: &str = "seed.txt";
const DEFAULT_BEHAVIOR_SEED_CONTENTS: &str = "seed\n";

pub(crate) struct AutoVerificationState {
    pub(crate) targets: scenarios::AutoVerificationTargets,
    pub(crate) remaining_ids: Vec<String>,
    pub(crate) remaining_by_kind: Vec<AutoVerificationKindState>,
    pub(crate) excluded: Vec<enrich::VerificationExclusion>,
    pub(crate) excluded_count: usize,
}

pub(crate) struct AutoVerificationKindState {
    pub(crate) kind: scenarios::VerificationTargetKind,
    pub(crate) target_count: usize,
    pub(crate) remaining_ids: Vec<String>,
}

pub(crate) fn auto_verification_plan_summary(
    plan: &scenarios::ScenarioPlan,
    surface: &surface::SurfaceInventory,
    ledger_entries: Option<&BTreeMap<String, scenarios::VerificationEntry>>,
    verification_tier: &str,
) -> Option<enrich::VerificationPlanSummary> {
    if verification_tier == "behavior" {
        return behavior_queue_plan_summary(plan, ledger_entries);
    }
    let state = auto_verification_state(plan, surface, ledger_entries, verification_tier)?;
    let remaining_preview = preview_ids(&state.remaining_ids, 10);
    let by_kind = state
        .remaining_by_kind
        .iter()
        .map(|group| enrich::VerificationKindSummary {
            kind: group.kind.as_str().to_string(),
            target_count: group.target_count,
            remaining_count: group.remaining_ids.len(),
            remaining_preview: preview_ids(&group.remaining_ids, 10),
            remaining_ids: None,
        })
        .collect();
    Some(enrich::VerificationPlanSummary {
        target_count: state.targets.target_ids.len(),
        excluded_count: state.excluded_count,
        remaining_count: state.remaining_ids.len(),
        remaining_preview,
        by_kind,
    })
}

fn behavior_queue_plan_summary(
    plan: &scenarios::ScenarioPlan,
    ledger_entries: Option<&BTreeMap<String, scenarios::VerificationEntry>>,
) -> Option<enrich::VerificationPlanSummary> {
    let mut target_ids = Vec::new();
    let mut target_seen = BTreeSet::new();
    let (_excluded_entries, excluded_ids) = plan.collect_queue_exclusions();
    for entry in &plan.verification.queue {
        let id = entry.surface_id.trim();
        if id.is_empty() {
            continue;
        }
        if entry.intent != scenarios::VerificationIntent::VerifyBehavior {
            continue;
        }
        if excluded_ids.contains(id) {
            continue;
        }
        if target_seen.insert(id.to_string()) {
            target_ids.push(id.to_string());
        }
    }
    if target_ids.is_empty() {
        return None;
    }
    let mut remaining_ids = Vec::new();
    if let Some(entries) = ledger_entries {
        for surface_id in &target_ids {
            let status = entries
                .get(surface_id)
                .map(|entry| entry.behavior_status.as_str())
                .unwrap_or("unknown");
            if status != "verified" {
                remaining_ids.push(surface_id.clone());
            }
        }
    } else {
        remaining_ids.extend(target_ids.iter().cloned());
    }
    remaining_ids.sort();
    remaining_ids.dedup();
    let remaining_preview = preview_ids(&remaining_ids, 10);
    Some(enrich::VerificationPlanSummary {
        target_count: target_ids.len(),
        excluded_count: excluded_ids.len(),
        remaining_count: remaining_ids.len(),
        remaining_preview,
        by_kind: Vec::new(),
    })
}

pub(crate) fn auto_verification_state(
    plan: &scenarios::ScenarioPlan,
    surface: &surface::SurfaceInventory,
    ledger_entries: Option<&BTreeMap<String, scenarios::VerificationEntry>>,
    verification_tier: &str,
) -> Option<AutoVerificationState> {
    let targets = scenarios::auto_verification_targets(plan, surface)?;
    let mut remaining_ids = Vec::new();
    let mut remaining_by_kind = Vec::new();
    for (kind, group_ids) in &targets.targets {
        let mut group_remaining = Vec::new();
        for surface_id in group_ids {
            let status = ledger_entries
                .and_then(|entries| entries.get(surface_id))
                .map(|entry| {
                    if verification_tier == "behavior" {
                        entry.behavior_status.as_str()
                    } else {
                        entry.status.as_str()
                    }
                })
                .unwrap_or("unknown");
            if status != "verified" {
                group_remaining.push(surface_id.clone());
                remaining_ids.push(surface_id.clone());
            }
        }
        remaining_by_kind.push(AutoVerificationKindState {
            kind: *kind,
            target_count: group_ids.len(),
            remaining_ids: group_remaining,
        });
    }

    let excluded: Vec<enrich::VerificationExclusion> = targets
        .excluded
        .iter()
        .map(|entry| enrich::VerificationExclusion {
            surface_id: entry.surface_id.clone(),
            reason: entry.reason.clone().unwrap_or_default(),
            prereqs: entry
                .prereqs
                .iter()
                .map(|prereq| prereq.as_str().to_string())
                .collect(),
        })
        .collect();

    Some(AutoVerificationState {
        excluded_count: targets.excluded_ids.len(),
        targets,
        remaining_ids,
        remaining_by_kind,
        excluded,
    })
}

fn preview_ids(ids: &[String], limit: usize) -> Vec<String> {
    ids.iter().take(limit).cloned().collect()
}

pub(crate) fn verification_stub_from_queue(
    plan: &scenarios::ScenarioPlan,
    entry: &scenarios::VerificationQueueEntry,
) -> Option<String> {
    let target_id = entry.surface_id.trim();
    if target_id.is_empty() {
        return None;
    }
    if entry.intent != scenarios::VerificationIntent::VerifyBehavior {
        return None;
    }
    let baseline_id = find_behavior_baseline_id(plan)?;
    behavior_stub(plan, target_id, &baseline_id)
}

pub(crate) fn find_behavior_baseline_id(plan: &scenarios::ScenarioPlan) -> Option<String> {
    if plan
        .scenarios
        .iter()
        .any(|scenario| scenario.id == "baseline")
    {
        return Some("baseline".to_string());
    }
    for scenario in &plan.scenarios {
        let Some(baseline_id) = scenario.baseline_scenario_id.as_deref() else {
            continue;
        };
        if plan
            .scenarios
            .iter()
            .any(|candidate| candidate.id == baseline_id)
        {
            return Some(baseline_id.to_string());
        }
    }
    None
}

pub(crate) fn behavior_baseline_stub(plan: &scenarios::ScenarioPlan) -> Option<String> {
    if find_behavior_baseline_id(plan).is_some() {
        return None;
    }
    let mut updated = plan.clone();
    let defaults = updated.defaults.get_or_insert_with(Default::default);
    ensure_default_behavior_seed(defaults);
    updated.scenarios.push(scenarios::ScenarioSpec {
        id: "baseline".to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: Vec::new(),
        env: BTreeMap::new(),
        seed_dir: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("behavior".to_string()),
        baseline_scenario_id: None,
        assertions: Vec::new(),
        covers: Vec::new(),
        coverage_ignore: true,
        expect: scenarios::ScenarioExpect {
            exit_code: Some(0),
            ..Default::default()
        },
    });
    serde_json::to_string_pretty(&updated).ok()
}

fn behavior_stub(
    plan: &scenarios::ScenarioPlan,
    surface_id: &str,
    baseline_id: &str,
) -> Option<String> {
    let mut updated = plan.clone();
    let defaults = updated.defaults.get_or_insert_with(Default::default);
    ensure_default_behavior_seed(defaults);
    let stub_id = verification_stub_id(&updated, surface_id);
    let argv = if surface_id.starts_with('-') {
        vec![surface_id.to_string()]
    } else {
        vec![surface_id.to_string(), "--help".to_string()]
    };
    updated.scenarios.push(scenarios::ScenarioSpec {
        id: stub_id,
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv,
        env: BTreeMap::new(),
        seed_dir: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("behavior".to_string()),
        baseline_scenario_id: Some(baseline_id.to_string()),
        assertions: vec![
            scenarios::BehaviorAssertion::BaselineStdoutNotContainsSeedPath {
                path: "seed.txt".to_string(),
            },
            scenarios::BehaviorAssertion::VariantStdoutContainsSeedPath {
                path: "seed.txt".to_string(),
            },
        ],
        covers: vec![surface_id.to_string()],
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect {
            exit_code: Some(0),
            ..Default::default()
        },
    });
    serde_json::to_string_pretty(&updated).ok()
}

fn verification_stub_id(plan: &scenarios::ScenarioPlan, surface_id: &str) -> String {
    let sanitized = sanitize_scenario_id(surface_id);
    let base = format!("verify_{sanitized}");
    unique_scenario_id(plan, &base)
}

fn unique_scenario_id(plan: &scenarios::ScenarioPlan, base: &str) -> String {
    if plan.scenarios.iter().all(|scenario| scenario.id != base) {
        return base.to_string();
    }
    let mut idx = 1;
    loop {
        let candidate = format!("{base}-{idx}");
        if plan
            .scenarios
            .iter()
            .all(|scenario| scenario.id != candidate)
        {
            return candidate;
        }
        idx += 1;
    }
}

fn sanitize_scenario_id(surface_id: &str) -> String {
    let trimmed = surface_id.trim();
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let cleaned = out.trim_matches('_');
    if cleaned.is_empty() {
        "id".to_string()
    } else {
        cleaned.to_string()
    }
}

fn ensure_default_behavior_seed(defaults: &mut scenarios::ScenarioDefaults) {
    if defaults.seed.is_none() {
        defaults.seed = Some(default_behavior_seed());
    }
}

fn default_behavior_seed() -> scenarios::ScenarioSeedSpec {
    scenarios::ScenarioSeedSpec {
        entries: vec![scenarios::ScenarioSeedEntry {
            path: DEFAULT_BEHAVIOR_SEED_PATH.to_string(),
            kind: scenarios::SeedEntryKind::File,
            contents: Some(DEFAULT_BEHAVIOR_SEED_CONTENTS.to_string()),
            target: None,
            mode: None,
        }],
    }
}
