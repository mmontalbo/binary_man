use crate::enrich;
use crate::scenarios;
use crate::surface;
use std::collections::{BTreeMap, BTreeSet};

use crate::status::verification::{intent_matches_verification_tier, verification_entry_state};

pub(super) struct VerificationLedgerSnapshot {
    pub(super) entries: BTreeMap<String, scenarios::VerificationEntry>,
    pub(super) verified_count: usize,
    pub(super) unverified_count: usize,
    pub(super) behavior_verified_count: usize,
    pub(super) behavior_unverified_count: usize,
}

pub(super) fn build_verification_ledger_entries(
    binary_name: Option<&str>,
    surface: &surface::SurfaceInventory,
    plan: &scenarios::ScenarioPlan,
    paths: &enrich::DocPackPaths,
    template_path: &std::path::Path,
    local_blockers: &mut Vec<enrich::Blocker>,
    template_evidence: &enrich::EvidenceRef,
) -> Option<VerificationLedgerSnapshot> {
    let verification_binary = binary_name
        .map(|name| name.to_string())
        .or_else(|| surface.binary_name.clone())
        .or_else(|| plan.binary.clone())
        .unwrap_or_else(|| "<binary>".to_string());
    match scenarios::build_verification_ledger(
        &verification_binary,
        surface,
        paths.root(),
        &paths.scenarios_plan_path(),
        template_path,
        None,
        Some(paths.root()),
    ) {
        Ok(ledger) => {
            let mut ledger_entries = BTreeMap::new();
            for entry in ledger.entries {
                ledger_entries.insert(entry.surface_id.clone(), entry);
            }
            Some(VerificationLedgerSnapshot {
                entries: ledger_entries,
                verified_count: ledger.verified_count,
                unverified_count: ledger.unverified_count,
                behavior_verified_count: ledger.behavior_verified_count,
                behavior_unverified_count: ledger.behavior_unverified_count,
            })
        }
        Err(err) => {
            let blocker = enrich::Blocker {
                code: "verification_query_error".to_string(),
                message: err.to_string(),
                evidence: vec![template_evidence.clone()],
                next_action: Some(format!(
                    "fix {}",
                    enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL
                )),
            };
            local_blockers.push(blocker);
            None
        }
    }
}

pub(super) fn collect_unverified_from_ledger(
    plan: &scenarios::ScenarioPlan,
    ledger_entries: &BTreeMap<String, scenarios::VerificationEntry>,
    verification_tier: &str,
    evidence: &mut Vec<enrich::EvidenceRef>,
) -> (Vec<String>, Vec<String>) {
    let mut triaged_unverified_ids = Vec::new();
    let mut unverified_ids = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in &plan.verification.queue {
        let surface_id = entry.surface_id.trim();
        if surface_id.is_empty() {
            continue;
        }
        if entry.intent == scenarios::VerificationIntent::Exclude {
            continue;
        }
        if !intent_matches_verification_tier(entry.intent, verification_tier) {
            continue;
        }
        let (status, _, _) = verification_entry_state(ledger_entries.get(surface_id), entry.intent);
        let is_verified = status == "verified";
        if !is_verified && seen.insert(surface_id.to_string()) {
            triaged_unverified_ids.push(surface_id.to_string());
            unverified_ids.push(surface_id.to_string());
            if let Some(entry) = ledger_entries.get(surface_id) {
                evidence.extend(entry.evidence.iter().cloned());
            }
        }
    }
    (triaged_unverified_ids, unverified_ids)
}
