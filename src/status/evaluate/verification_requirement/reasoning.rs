use super::super::preview_ids;
use super::{BehaviorExclusionState, LedgerEntries};
use crate::enrich;
use crate::status::verification_policy::BehaviorReasonKind;

const BEHAVIOR_WARNING_PREVIEW_LIMIT: usize = 10;

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
    BehaviorReasonKind::from_code(Some(reason_code))
        .as_code()
        .to_string()
}

pub(super) fn behavior_recommended_fix(reason_code: &str) -> &'static str {
    match BehaviorReasonKind::from_code(Some(reason_code)) {
        BehaviorReasonKind::NoScenario => {
            "add behavior scenario with baseline_scenario_id and assertions"
        }
        BehaviorReasonKind::ScenarioError => {
            "fix scenario configuration (argv, seed, or assertions)"
        }
        BehaviorReasonKind::AssertionFailed => "fix assertions or update seed fixtures",
        BehaviorReasonKind::OutputsEqual => {
            "add workaround hints, rerun, then exclude with evidence if still equal"
        }
        BehaviorReasonKind::AutoVerifyTimeout => {
            "deferred (auto_verify timed out, likely interactive command)"
        }
    }
}

fn behavior_diagnostic_fix_hint(reason_code: &str) -> &'static str {
    behavior_recommended_fix(reason_code)
}

pub(super) fn build_behavior_unverified_preview(
    remaining_ids: &[String],
    missing_value_examples: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
) -> Vec<enrich::BehaviorUnverifiedPreview> {
    preview_ids(remaining_ids)
        .into_iter()
        .map(|surface_id| {
            let entry = ledger_entries.get(&surface_id);
            enrich::BehaviorUnverifiedPreview {
                reason_code: behavior_reason_code_for_id(
                    &surface_id,
                    missing_value_examples,
                    ledger_entries,
                ),
                auto_verify_exit_code: entry.and_then(|e| e.auto_verify_exit_code),
                auto_verify_stderr: entry.and_then(|e| e.auto_verify_stderr.clone()),
                surface_id,
            }
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

pub(super) fn build_behavior_warnings(
    required_ids: &[String],
    ledger_entries: &LedgerEntries,
    include_full: bool,
) -> Vec<enrich::BehaviorVerificationWarning> {
    let mut warnings = Vec::new();
    for surface_id in required_ids {
        let Some(entry) = ledger_entries.get(surface_id) else {
            continue;
        };
        if entry.behavior_confounded_extra_surface_ids.is_empty() {
            continue;
        }
        if !include_full && warnings.len() >= BEHAVIOR_WARNING_PREVIEW_LIMIT {
            break;
        }
        let scenario_id = entry
            .behavior_confounded_scenario_ids
            .first()
            .cloned()
            .or_else(|| entry.behavior_unverified_scenario_id.clone())
            .or_else(|| entry.behavior_scenario_ids.first().cloned());
        let surface_list = entry.behavior_confounded_extra_surface_ids.join(", ");
        let message = match scenario_id.as_deref() {
            Some(id) => {
                format!("scenario {id} covers {surface_id} but also exercises {surface_list}")
            }
            None => format!("{surface_id} coverage also exercises {surface_list}"),
        };
        warnings.push(enrich::BehaviorVerificationWarning {
            surface_id: surface_id.clone(),
            scenario_id,
            warning_code: "confounded_behavior_coverage".to_string(),
            message,
            extra_surface_ids: entry.behavior_confounded_extra_surface_ids.clone(),
        });
    }
    warnings
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
    _ledger_entries: &LedgerEntries,
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
    let excluded_by_id = crate::surface::validate_behavior_exclusions(&exclusions, &required_set)?;

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
    _assertion_kind: Option<&str>,
    _assertion_seed_path: Option<&str>,
) -> String {
    let reason_kind = BehaviorReasonKind::from_code(reason_code);
    let reason_code = reason_kind.as_code();
    let fix = behavior_recommended_fix(reason_code);
    match reason_kind {
        BehaviorReasonKind::NoScenario => {
            format!("{reason_code}: {fix} for {surface_id}")
        }
        BehaviorReasonKind::ScenarioError => {
            format!("{reason_code}: {fix} for scenario {scenario_id}")
        }
        BehaviorReasonKind::AssertionFailed => {
            format!("{reason_code}: {fix} in scenario {scenario_id}")
        }
        BehaviorReasonKind::OutputsEqual => {
            format!("{reason_code}: {fix} for scenario {scenario_id}")
        }
        BehaviorReasonKind::AutoVerifyTimeout => {
            format!("{reason_code}: deferred (auto_verify timed out) for {surface_id}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        behavior_diagnostic_fix_hint, build_behavior_unverified_diagnostics,
        build_behavior_warnings,
    };
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
            behavior_confounded_scenario_ids: Vec::new(),
            behavior_confounded_extra_surface_ids: Vec::new(),
            auto_verify_exit_code: None,
            auto_verify_stderr: None,
            behavior_exit_code: None,
            behavior_stderr: None,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn behavior_unverified_diagnostics_include_reason_code_fix_hints() {
        // Test all reason codes that can be produced by the system
        let reason_codes = [
            "no_scenario",
            "outputs_equal",
            "assertion_failed",
            "scenario_error",
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

    #[test]
    fn behavior_warnings_include_confounded_coverage_details() {
        let mut ledger = BTreeMap::new();
        let mut entry = entry_with_reason("--color", "assertion_failed");
        entry.behavior_confounded_scenario_ids = vec!["verify_color".to_string()];
        entry.behavior_confounded_extra_surface_ids = vec!["--group-directories-first".to_string()];
        ledger.insert("--color".to_string(), entry);

        let warnings = build_behavior_warnings(&["--color".to_string()], &ledger, true);
        assert_eq!(warnings.len(), 1);
        let warning = &warnings[0];
        assert_eq!(warning.surface_id, "--color");
        assert_eq!(warning.warning_code, "confounded_behavior_coverage");
        assert_eq!(warning.scenario_id.as_deref(), Some("verify_color"));
        assert_eq!(
            warning.extra_surface_ids,
            vec!["--group-directories-first".to_string()]
        );
    }
}
