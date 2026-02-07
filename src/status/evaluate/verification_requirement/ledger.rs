use crate::enrich;
use crate::scenarios;
use crate::surface;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub(super) struct VerificationLedgerSnapshot {
    pub(super) entries: BTreeMap<String, scenarios::VerificationEntry>,
    pub(super) verified_count: usize,
    pub(super) unverified_count: usize,
    pub(super) behavior_verified_count: usize,
    pub(super) behavior_unverified_count: usize,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn load_or_build_verification_ledger_entries(
    binary_name: Option<&str>,
    surface: &surface::SurfaceInventory,
    plan: &scenarios::ScenarioPlan,
    paths: &enrich::DocPackPaths,
    template_path: &std::path::Path,
    lock_status: &enrich::LockStatus,
    local_blockers: &mut Vec<enrich::Blocker>,
    template_evidence: &enrich::EvidenceRef,
) -> Option<VerificationLedgerSnapshot> {
    if let Some(snapshot) = load_cached_verification_ledger_snapshot(
        binary_name,
        surface,
        plan,
        paths,
        template_path,
        lock_status,
    ) {
        return Some(snapshot);
    }
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

fn load_cached_verification_ledger_snapshot(
    binary_name: Option<&str>,
    surface: &surface::SurfaceInventory,
    plan: &scenarios::ScenarioPlan,
    paths: &enrich::DocPackPaths,
    template_path: &Path,
    lock_status: &enrich::LockStatus,
) -> Option<VerificationLedgerSnapshot> {
    if !lock_status.present || lock_status.stale {
        return None;
    }
    let ledger_path = paths.root().join("verification_ledger.json");
    let bytes = fs::read(&ledger_path).ok()?;
    let ledger: scenarios::VerificationLedger = serde_json::from_slice(&bytes).ok()?;
    if !verification_ledger_matches_inputs(
        &ledger,
        binary_name,
        surface,
        plan,
        paths,
        &ledger_path,
        template_path,
    ) {
        return None;
    }
    Some(VerificationLedgerSnapshot {
        entries: scenarios::verification_entries_by_surface_id(ledger.entries),
        verified_count: ledger.verified_count,
        unverified_count: ledger.unverified_count,
        behavior_verified_count: ledger.behavior_verified_count,
        behavior_unverified_count: ledger.behavior_unverified_count,
    })
}

fn verification_ledger_matches_inputs(
    ledger: &scenarios::VerificationLedger,
    binary_name: Option<&str>,
    surface: &surface::SurfaceInventory,
    plan: &scenarios::ScenarioPlan,
    paths: &enrich::DocPackPaths,
    ledger_path: &Path,
    template_path: &Path,
) -> bool {
    let expected_binary = binary_name
        .map(|name| name.to_string())
        .or_else(|| surface.binary_name.clone())
        .or_else(|| plan.binary.clone())
        .unwrap_or_else(|| "<binary>".to_string());
    if ledger.binary_name != expected_binary {
        return false;
    }

    let expected_scenarios_path = match paths.rel_path(&paths.scenarios_plan_path()) {
        Ok(path) => path,
        Err(_) => return false,
    };
    if ledger.scenarios_path != expected_scenarios_path {
        return false;
    }

    let expected_surface_path = match paths.rel_path(&paths.surface_path()) {
        Ok(path) => path,
        Err(_) => return false,
    };
    if ledger.surface_path != expected_surface_path {
        return false;
    }

    let required_freshness_deps = [
        paths.scenarios_plan_path(),
        paths.surface_path(),
        paths.semantics_path(),
        template_path.to_path_buf(),
        paths.inventory_scenarios_dir().join("index.json"),
    ];
    let optional_freshness_deps = [paths.surface_overlays_path()];
    ledger_newer_than_all_dependencies(
        ledger_path,
        &required_freshness_deps,
        &optional_freshness_deps,
    )
}

fn ledger_newer_than_all_dependencies(
    ledger_path: &Path,
    required_dependencies: &[PathBuf],
    optional_dependencies: &[PathBuf],
) -> bool {
    let Some(ledger_modified_ms) = modified_epoch_ms(ledger_path) else {
        return false;
    };
    for path in required_dependencies {
        let Some(dep_modified_ms) = modified_epoch_ms(path) else {
            return false;
        };
        if dep_modified_ms > ledger_modified_ms {
            return false;
        }
    }
    for path in optional_dependencies {
        let Some(dep_modified_ms) = modified_epoch_ms(path) else {
            continue;
        };
        if dep_modified_ms > ledger_modified_ms {
            return false;
        }
    }
    true
}

fn modified_epoch_ms(path: &Path) -> Option<u128> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let duration = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(duration.as_millis())
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
