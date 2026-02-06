use super::super::preview_ids;
use super::{BehaviorExclusionState, LedgerEntries};
use crate::enrich;

pub(super) fn behavior_reason_code_for_id(
    surface_id: &str,
    missing_value_examples: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
) -> String {
    if missing_value_examples.contains(surface_id) {
        return "missing_value_examples".to_string();
    }
    let reason_code = ledger_entries
        .get(surface_id)
        .and_then(|entry| entry.behavior_unverified_reason_code.as_ref())
        .map(String::as_str)
        .unwrap_or("unknown");
    normalize_behavior_reason_code(reason_code).to_string()
}

fn normalize_behavior_reason_code(reason_code: &str) -> &str {
    match reason_code {
        "surface_missing" => "unknown",
        _ => reason_code,
    }
}

pub(super) fn behavior_recommended_fix(reason_code: &str) -> &'static str {
    match reason_code {
        "missing_value_examples" => {
            "add value_examples overlay in inventory/surface.overlays.json"
        }
        "missing_behavior_scenario" => "add behavior scenario",
        "scenario_failed" => "fix behavior scenario run",
        "missing_assertions" => "add non-empty assertions[] semantic predicates",
        "assertion_seed_path_not_seeded" => "fix seed_path (seed.entries path) + stdout_token",
        "seed_signature_mismatch" => "align baseline and variant seed entries",
        "seed_mismatch" => "add seed-grounded assertions",
        "missing_delta_assertion" => "add delta assertion pair",
        "missing_semantic_predicate" => "add stdout/stderr expect predicate",
        "outputs_equal" => "add requires_argv workaround overlay, rerun delta verification, then exclude with evidence if still equal",
        "assertion_failed" => "fix assertion failure",
        _ => "inspect verification_ledger.json",
    }
}

fn behavior_diagnostic_fix_hint(reason_code: &str) -> &'static str {
    match reason_code {
        "missing_behavior_scenario" => {
            "merge scaffold into scenarios/plan.json, then fill coverage_tier/covers/baseline_scenario_id/assertions"
        }
        "missing_assertions" => {
            "add non-empty assertions[] and at least one stable expect.stdout_* or expect.stderr_* predicate"
        }
        "missing_delta_assertion" => {
            "add a delta pair (baseline_* + variant_*) or variant_stdout_differs_from_baseline"
        }
        "missing_semantic_predicate" => {
            "add a stable stdout/stderr semantic predicate in expect.*"
        }
        "outputs_equal" => {
            "change variant argv/fixture so output differs from baseline, rerun apply, then re-check"
        }
        "scenario_failed" => "fix argv/seed/expect so baseline and variant runs both pass",
        _ => behavior_recommended_fix(reason_code),
    }
}

pub(super) fn build_behavior_unverified_preview(
    remaining_ids: &[String],
    missing_value_examples: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
) -> Vec<enrich::BehaviorUnverifiedPreview> {
    preview_ids(remaining_ids)
        .into_iter()
        .map(|surface_id| enrich::BehaviorUnverifiedPreview {
            reason_code: behavior_reason_code_for_id(
                &surface_id,
                missing_value_examples,
                ledger_entries,
            ),
            surface_id,
        })
        .collect()
}

pub(super) fn build_behavior_unverified_diagnostics(
    remaining_ids: &[String],
    missing_value_examples: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
    include_full: bool,
) -> Vec<enrich::BehaviorUnverifiedDiagnostic> {
    let ids = if include_full {
        remaining_ids.to_vec()
    } else {
        preview_ids(remaining_ids)
    };
    ids.into_iter()
        .map(|surface_id| {
            let reason_code =
                behavior_reason_code_for_id(&surface_id, missing_value_examples, ledger_entries);
            let entry = ledger_entries.get(&surface_id);
            enrich::BehaviorUnverifiedDiagnostic {
                scenario_id: entry
                    .and_then(|entry| {
                        entry
                            .behavior_unverified_scenario_id
                            .as_deref()
                            .or_else(|| entry.behavior_scenario_ids.first().map(String::as_str))
                    })
                    .map(str::to_string),
                assertion_kind: entry
                    .and_then(|entry| entry.behavior_unverified_assertion_kind.clone()),
                assertion_seed_path: entry
                    .and_then(|entry| entry.behavior_unverified_assertion_seed_path.clone()),
                assertion_token: entry
                    .and_then(|entry| entry.behavior_unverified_assertion_token.clone()),
                surface_id,
                fix_hint: behavior_diagnostic_fix_hint(&reason_code).to_string(),
                reason_code,
            }
        })
        .collect()
}

pub(super) fn build_behavior_reason_summary(
    remaining_ids: &[String],
    missing_value_examples: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
) -> Vec<enrich::VerificationReasonSummary> {
    if remaining_ids.is_empty() {
        return Vec::new();
    }
    let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for surface_id in remaining_ids {
        let reason_code =
            behavior_reason_code_for_id(surface_id, missing_value_examples, ledger_entries);
        grouped
            .entry(reason_code)
            .or_default()
            .push(surface_id.clone());
    }
    grouped
        .into_iter()
        .map(|(reason_code, mut ids)| {
            ids.sort();
            enrich::VerificationReasonSummary {
                recommended_fix: behavior_recommended_fix(&reason_code).to_string(),
                reason_code,
                count: ids.len(),
                preview: preview_ids(&ids),
            }
        })
        .collect()
}

pub(super) fn load_behavior_exclusion_state(
    paths: &enrich::DocPackPaths,
    required_ids: &[String],
    ledger_entries: &LedgerEntries,
    include_full: bool,
) -> anyhow::Result<BehaviorExclusionState> {
    let overlays_path = paths.surface_overlays_path();
    let overlays = crate::surface::load_surface_overlays_if_exists(&overlays_path)?;
    let Some(overlays) = overlays else {
        return Ok(BehaviorExclusionState::default());
    };
    let exclusions = crate::surface::collect_behavior_exclusions(&overlays);
    if exclusions.is_empty() {
        return Ok(BehaviorExclusionState::default());
    }

    let required_set: std::collections::BTreeSet<String> = required_ids.iter().cloned().collect();
    let mut ledger_by_surface_id = std::collections::BTreeMap::new();
    for (surface_id, entry) in ledger_entries {
        ledger_by_surface_id.insert(
            surface_id.clone(),
            crate::surface::BehaviorExclusionLedgerEntry {
                delta_outcome: entry.delta_outcome.clone(),
                delta_evidence_paths: entry.delta_evidence_paths.clone(),
            },
        );
    }
    let excluded_by_id = crate::surface::validate_behavior_exclusions(
        &exclusions,
        &required_set,
        &ledger_by_surface_id,
        "missing from verification_ledger entries",
        "requires delta_outcome evidence",
    )?;

    let mut excluded_ids: Vec<String> = excluded_by_id.keys().cloned().collect();
    excluded_ids.sort();
    let excluded_preview = preview_ids(&excluded_ids);
    let excluded_for_summary = if include_full {
        excluded_ids.clone()
    } else {
        excluded_preview.clone()
    };
    let excluded = excluded_for_summary
        .iter()
        .filter_map(|surface_id| excluded_by_id.get(surface_id))
        .map(|entry| {
            let mut reason = entry.exclusion.reason_code.as_str().to_string();
            if let Some(note) = entry.exclusion.note.as_deref() {
                reason = format!("{reason}: {}", note.trim());
            }
            enrich::VerificationExclusion {
                surface_id: entry.surface_id.clone(),
                reason,
                prereqs: Vec::new(),
            }
        })
        .collect();
    let excluded_reason_summary = build_behavior_excluded_reason_summary(&excluded_by_id);

    Ok(BehaviorExclusionState {
        excluded_by_id,
        excluded_ids,
        excluded_preview,
        excluded,
        excluded_reason_summary,
    })
}

fn build_behavior_excluded_reason_summary(
    excluded_by_id: &std::collections::BTreeMap<String, crate::surface::SurfaceBehaviorExclusion>,
) -> Vec<enrich::VerificationReasonSummary> {
    let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (surface_id, exclusion) in excluded_by_id {
        grouped
            .entry(exclusion.exclusion.reason_code.as_str().to_string())
            .or_default()
            .push(surface_id.clone());
    }
    grouped
        .into_iter()
        .map(|(reason_code, mut ids)| {
            ids.sort();
            enrich::VerificationReasonSummary {
                reason_code: reason_code.clone(),
                count: ids.len(),
                preview: preview_ids(&ids),
                recommended_fix: behavior_exclusion_recommended_fix(&reason_code).to_string(),
            }
        })
        .collect()
}

fn behavior_exclusion_recommended_fix(reason_code: &str) -> &'static str {
    match reason_code {
        "unsafe_side_effects" => "keep exclusion evidence synced with delta reruns",
        "fixture_gap" => "keep exclusion evidence synced with fixture/workaround attempts",
        "assertion_gap" => "keep exclusion evidence synced with assertion/workaround attempts",
        "nondeterministic" => "keep exclusion evidence synced with repeated delta runs",
        "requires_interactive_tty" => "keep exclusion evidence synced with delta attempts",
        _ => "keep exclusion evidence synced with delta attempts",
    }
}

pub(super) fn behavior_unverified_reason(
    reason_code: Option<&str>,
    scenario_id: &str,
    surface_id: &str,
    assertion_kind: Option<&str>,
    assertion_seed_path: Option<&str>,
) -> String {
    let reason_code = reason_code.unwrap_or("unknown");
    let recommended_fix = behavior_recommended_fix(reason_code);
    let assertion_context = format_assertion_context(assertion_kind, assertion_seed_path);
    match reason_code {
        "missing_assertions" => format!(
            "reason_code={reason_code}; {recommended_fix} for scenario {scenario_id}{assertion_context}; expect.* alone does not verify behavior. Prefer exact-line stdout assertions for short tokens."
        ),
        "assertion_seed_path_not_seeded" | "seed_mismatch" => format!(
            "reason_code={reason_code}; {recommended_fix} for scenario {scenario_id}{assertion_context}. Example:\nseed.entries: [{{\"path\":\"work/file.txt\",\"kind\":\"file\",\"contents\":\"...\"}}]\nassertion: {{\"seed_path\":\"work/file.txt\",\"stdout_token\":\"file.txt\"}}"
        ),
        "seed_signature_mismatch" => format!(
            "reason_code={reason_code}; {recommended_fix} for scenario {scenario_id}{assertion_context}"
        ),
        "missing_delta_assertion" => format!(
            "reason_code={reason_code}; {recommended_fix} for scenario {scenario_id}{assertion_context}"
        ),
        "missing_semantic_predicate" => format!(
            "reason_code={reason_code}; {recommended_fix} for scenario {scenario_id}{assertion_context}"
        ),
        "outputs_equal" => format!(
            "reason_code={reason_code}; {recommended_fix} for scenario {scenario_id}{assertion_context}. Add requires_argv workaround hints, rerun delta checks, and only exclude after recording attempted_workarounds evidence."
        ),
        "assertion_failed" => format!(
            "reason_code={reason_code}; {recommended_fix} in scenario {scenario_id}{assertion_context}"
        ),
        "scenario_failed" => format!(
            "reason_code={reason_code}; {recommended_fix} in scenario {scenario_id}"
        ),
        "missing_behavior_scenario" => {
            format!("reason_code={reason_code}; {recommended_fix} for {surface_id}")
        }
        "missing_value_examples" => {
            format!("reason_code={reason_code}; {recommended_fix} for {surface_id}")
        }
        _ => format!("reason_code={reason_code}; {recommended_fix} for {surface_id}"),
    }
}

fn format_assertion_context(
    assertion_kind: Option<&str>,
    assertion_seed_path: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(kind) = assertion_kind {
        let kind = kind.trim();
        if !kind.is_empty() {
            parts.push(format!("assertion={kind}"));
        }
    }
    if let Some(seed_path) = assertion_seed_path {
        let seed_path = seed_path.trim();
        if !seed_path.is_empty() {
            parts.push(format!("seed_path={seed_path}"));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::{behavior_diagnostic_fix_hint, build_behavior_unverified_diagnostics};
    use crate::scenarios;
    use std::collections::{BTreeMap, BTreeSet};

    fn entry_with_reason(surface_id: &str, reason_code: &str) -> scenarios::VerificationEntry {
        scenarios::VerificationEntry {
            surface_id: surface_id.to_string(),
            status: "unverified".to_string(),
            behavior_status: "unverified".to_string(),
            behavior_exclusion_reason_code: None,
            behavior_unverified_reason_code: Some(reason_code.to_string()),
            behavior_unverified_scenario_id: Some(format!("verify_{surface_id}")),
            behavior_unverified_assertion_kind: None,
            behavior_unverified_assertion_seed_path: None,
            behavior_unverified_assertion_token: None,
            scenario_ids: Vec::new(),
            scenario_paths: Vec::new(),
            behavior_scenario_ids: vec![format!("verify_{surface_id}")],
            behavior_assertion_scenario_ids: Vec::new(),
            behavior_scenario_paths: Vec::new(),
            delta_outcome: None,
            delta_evidence_paths: Vec::new(),
            evidence: Vec::new(),
        }
    }

    #[test]
    fn behavior_unverified_diagnostics_include_reason_code_fix_hints() {
        let reason_codes = [
            "missing_behavior_scenario",
            "missing_assertions",
            "missing_delta_assertion",
            "missing_semantic_predicate",
            "outputs_equal",
            "scenario_failed",
        ];
        let remaining_ids = reason_codes
            .iter()
            .enumerate()
            .map(|(idx, _)| format!("--opt-{idx}"))
            .collect::<Vec<_>>();
        let mut ledger = BTreeMap::new();
        for (surface_id, reason_code) in remaining_ids.iter().zip(reason_codes) {
            ledger.insert(
                surface_id.clone(),
                entry_with_reason(surface_id, reason_code),
            );
        }

        let diagnostics =
            build_behavior_unverified_diagnostics(&remaining_ids, &BTreeSet::new(), &ledger, true);
        assert_eq!(diagnostics.len(), reason_codes.len());

        for diagnostic in diagnostics {
            let expected_hint = behavior_diagnostic_fix_hint(&diagnostic.reason_code);
            assert_eq!(diagnostic.fix_hint, expected_hint);
        }
    }
}
