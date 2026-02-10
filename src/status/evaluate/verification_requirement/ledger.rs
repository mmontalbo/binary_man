use crate::enrich;
use crate::scenarios;
use crate::surface;
use std::collections::BTreeMap;
use std::path::Path;

pub(super) struct VerificationLedgerSnapshot {
    pub(super) entries: BTreeMap<String, scenarios::VerificationEntry>,
    pub(super) verified_count: usize,
    pub(super) unverified_count: usize,
    pub(super) behavior_verified_count: usize,
    pub(super) behavior_unverified_count: usize,
}

/// Build verification ledger entries on-the-fly from scenario evidence.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_verification_ledger_entries(
    binary_name: Option<&str>,
    surface: &surface::SurfaceInventory,
    plan: &scenarios::ScenarioPlan,
    paths: &enrich::DocPackPaths,
    template_path: &Path,
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
        Ok(ledger) => Some(VerificationLedgerSnapshot {
            entries: scenarios::verification_entries_by_surface_id(ledger.entries),
            verified_count: ledger.verified_count,
            unverified_count: ledger.unverified_count,
            behavior_verified_count: ledger.behavior_verified_count,
            behavior_unverified_count: ledger.behavior_unverified_count,
        }),
        Err(err) => {
            let failure_path = scenarios::verification_query_template_failure_path(&err)
                .map(|path| doc_pack_relative_or_display(paths, path));
            let next_action_path = failure_path
                .clone()
                .unwrap_or_else(|| enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL.to_string());
            let mut evidence = vec![template_evidence.clone()];
            if let Some(path) = scenarios::verification_query_template_failure_path(&err) {
                if let Ok(include_evidence) = paths.evidence_from_path(path) {
                    evidence.push(include_evidence);
                }
            }
            enrich::dedupe_evidence_refs(&mut evidence);
            let message = if let Some(path) = failure_path {
                format!("verification query template error at {path}: {err}")
            } else {
                err.to_string()
            };
            let blocker = enrich::Blocker {
                code: "verification_query_error".to_string(),
                message,
                evidence,
                next_action: Some(format!("fix {next_action_path}")),
            };
            local_blockers.push(blocker);
            None
        }
    }
}

fn doc_pack_relative_or_display(paths: &enrich::DocPackPaths, path: &Path) -> String {
    paths
        .rel_path(path)
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
