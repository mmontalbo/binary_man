use crate::enrich;
use crate::scenarios;
use std::collections::BTreeMap;

use crate::status::verification::{
    intent_label, intent_matches_verification_tier, verification_entry_state,
    verification_stub_from_queue,
};

pub(super) struct VerificationActionArgs<'a> {
    pub(super) plan: &'a scenarios::ScenarioPlan,
    pub(super) ledger_entries: &'a BTreeMap<String, scenarios::VerificationEntry>,
    pub(super) verification_tier: &'a str,
    pub(super) verification_next_action: &'a mut Option<enrich::NextAction>,
    pub(super) paths: &'a enrich::DocPackPaths,
    pub(super) binary_name: Option<&'a str>,
    pub(super) discovered_untriaged_empty: bool,
    pub(super) blockers_empty: bool,
    pub(super) missing_empty: bool,
}

pub(super) fn maybe_set_verification_action_from_ledger(args: VerificationActionArgs<'_>) {
    let VerificationActionArgs {
        plan,
        ledger_entries,
        verification_tier,
        verification_next_action,
        paths,
        binary_name,
        discovered_untriaged_empty,
        blockers_empty,
        missing_empty,
    } = args;
    if verification_next_action.is_some() {
        return;
    }
    if plan.verification.queue.is_empty()
        || !discovered_untriaged_empty
        || !blockers_empty
        || !missing_empty
    {
        return;
    }

    for entry in plan.verification.queue.iter() {
        if entry.intent == scenarios::VerificationIntent::Exclude {
            continue;
        }
        if !intent_matches_verification_tier(entry.intent, verification_tier) {
            continue;
        }
        let surface_id = entry.surface_id.trim();
        if surface_id.is_empty() {
            continue;
        }
        let (status, scenario_ids, scenario_paths) =
            verification_entry_state(ledger_entries.get(surface_id), entry.intent);
        if scenario_ids.is_empty() {
            if let Some(content) = verification_stub_from_queue(plan, entry) {
                *verification_next_action = Some(enrich::NextAction::Edit {
                    path: "scenarios/plan.json".to_string(),
                    content,
                    reason: format!(
                        "add a {} scenario for {surface_id}",
                        intent_label(entry.intent)
                    ),
                });
            }
            break;
        }
        if scenario_paths.is_empty() {
            let root = paths.root().display();
            *verification_next_action = Some(enrich::NextAction::Command {
                command: format!(
                    "bman validate --doc-pack {root} && bman plan --doc-pack {root} && bman apply --doc-pack {root}"
                ),
                reason: format!(
                    "run {} verification for {surface_id}",
                    intent_label(entry.intent)
                ),
            });
            break;
        }
        if status != "verified" {
            let content = serde_json::to_string_pretty(plan)
                .unwrap_or_else(|_| scenarios::plan_stub(binary_name));
            *verification_next_action = Some(enrich::NextAction::Edit {
                path: "scenarios/plan.json".to_string(),
                content,
                reason: format!(
                    "fix {} scenario for {surface_id}",
                    intent_label(entry.intent)
                ),
            });
            break;
        }
    }
}
