use crate::enrich;
use crate::scenarios;
use crate::surface;
use std::collections::BTreeMap;

pub(crate) fn intent_matches_verification_tier(
    intent: scenarios::VerificationIntent,
    tier: &str,
) -> bool {
    match tier {
        "behavior" => intent == scenarios::VerificationIntent::VerifyBehavior,
        _ => matches!(
            intent,
            scenarios::VerificationIntent::VerifyAccepted
                | scenarios::VerificationIntent::VerifyBehavior
        ),
    }
}

pub(crate) fn intent_label(intent: scenarios::VerificationIntent) -> &'static str {
    match intent {
        scenarios::VerificationIntent::VerifyBehavior => "behavior",
        scenarios::VerificationIntent::VerifyAccepted => "existence",
        scenarios::VerificationIntent::Exclude => "exclude",
    }
}

pub(crate) fn verification_entry_state(
    entry: Option<&scenarios::VerificationEntry>,
    intent: scenarios::VerificationIntent,
) -> (&str, &[String], &[String]) {
    const EMPTY: &[String] = &[];
    match (entry, intent) {
        (Some(entry), scenarios::VerificationIntent::VerifyBehavior) => (
            entry.behavior_status.as_str(),
            entry.behavior_scenario_ids.as_slice(),
            entry.behavior_scenario_paths.as_slice(),
        ),
        (Some(entry), scenarios::VerificationIntent::VerifyAccepted) => (
            entry.status.as_str(),
            entry.scenario_ids.as_slice(),
            entry.scenario_paths.as_slice(),
        ),
        _ => ("unknown", EMPTY, EMPTY),
    }
}

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
    let coverage_tier = match entry.intent {
        scenarios::VerificationIntent::VerifyBehavior => Some("behavior".to_string()),
        scenarios::VerificationIntent::VerifyAccepted => Some("acceptance".to_string()),
        scenarios::VerificationIntent::Exclude => return None,
    };
    let argv = if target_id.starts_with('-') {
        vec![target_id.to_string()]
    } else {
        vec![target_id.to_string(), "--help".to_string()]
    };
    let mut updated = plan.clone();
    let stub_id = verification_stub_id(&updated, target_id);
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
        coverage_tier,
        covers: vec![target_id.to_string()],
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect::default(),
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
