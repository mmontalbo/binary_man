use crate::enrich;
use crate::scenarios;
use std::collections::{BTreeMap, BTreeSet};

use crate::status::verification::intent_matches_verification_tier;

pub(super) struct VerificationQueueState {
    pub(super) queue_ids: BTreeSet<String>,
    pub(super) triaged_ids: BTreeSet<String>,
    pub(super) excluded: Vec<enrich::VerificationExclusion>,
}

pub(super) fn collect_verification_queue_state(
    plan: &scenarios::ScenarioPlan,
    verification_tier: &str,
) -> VerificationQueueState {
    let mut queue_ids = BTreeSet::new();
    let mut triaged_ids = BTreeSet::new();
    let mut excluded = Vec::new();

    for entry in &plan.verification.queue {
        let id = entry.surface_id.trim();
        if id.is_empty() {
            continue;
        }
        queue_ids.insert(id.to_string());
        if entry.intent == scenarios::VerificationIntent::Exclude {
            let reason = entry.reason.as_deref().unwrap_or("").trim();
            excluded.push(enrich::VerificationExclusion {
                surface_id: id.to_string(),
                reason: reason.to_string(),
            });
            triaged_ids.insert(id.to_string());
            continue;
        }
        if intent_matches_verification_tier(entry.intent, verification_tier) {
            triaged_ids.insert(id.to_string());
        }
    }

    VerificationQueueState {
        queue_ids,
        triaged_ids,
        excluded,
    }
}

pub(super) fn collect_discovered_untriaged_ids(
    surface_ids: &BTreeSet<String>,
    triaged_ids: &BTreeSet<String>,
    surface_evidence_map: &BTreeMap<String, Vec<enrich::EvidenceRef>>,
    evidence: &mut Vec<enrich::EvidenceRef>,
) -> Vec<String> {
    let mut discovered_untriaged_ids = Vec::new();
    for id in surface_ids.iter() {
        if !triaged_ids.contains(id) {
            discovered_untriaged_ids.push(id.clone());
            if let Some(item_evidence) = surface_evidence_map.get(id) {
                evidence.extend(item_evidence.iter().cloned());
            }
        }
    }
    discovered_untriaged_ids.sort();
    discovered_untriaged_ids
}

pub(super) fn append_missing_queue_ids_blocker(
    surface_ids: &BTreeSet<String>,
    queue_ids: &BTreeSet<String>,
    local_blockers: &mut Vec<enrich::Blocker>,
    surface_evidence: &enrich::EvidenceRef,
    scenarios_evidence: &enrich::EvidenceRef,
) {
    let mut missing_surface_ids = Vec::new();
    for id in queue_ids.iter() {
        if !surface_ids.contains(id) {
            missing_surface_ids.push(id.clone());
        }
    }
    if !missing_surface_ids.is_empty() {
        local_blockers.push(enrich::Blocker {
            code: "verification_surface_missing".to_string(),
            message: format!(
                "verification queue surface_id missing from inventory: {}",
                missing_surface_ids.join(", ")
            ),
            evidence: vec![surface_evidence.clone(), scenarios_evidence.clone()],
            next_action: Some("fix scenarios/plan.json".to_string()),
        });
    }
}

pub(super) fn maybe_set_verification_triage_next_action(
    plan: &scenarios::ScenarioPlan,
    discovered_untriaged_ids: &[String],
    verification_next_action: &mut Option<enrich::NextAction>,
    binary_name: Option<&str>,
) {
    if !plan.verification.queue.is_empty() && discovered_untriaged_ids.is_empty() {
        return;
    }
    if verification_next_action.is_some() {
        return;
    }
    let content =
        serde_json::to_string_pretty(plan).unwrap_or_else(|_| scenarios::plan_stub(binary_name));
    let reason = if plan.verification.queue.is_empty() {
        "add verification triage in scenarios/plan.json".to_string()
    } else {
        format!(
            "add verification triage for {}",
            discovered_untriaged_ids[0]
        )
    };
    *verification_next_action = Some(enrich::NextAction::Edit {
        path: "scenarios/plan.json".to_string(),
        content,
        reason,
    });
}
