use super::super::{LedgerEntries, QueueVerificationContext};
use super::constants::{
    STUB_BLOCKERS_PREVIEW_LIMIT, STUB_DELTA_EVIDENCE_PATHS_LIMIT, STUB_EVIDENCE_PREVIEW_LIMIT,
    STUB_FORMS_PREVIEW_LIMIT, STUB_REQUIRES_ARGV_PREVIEW_LIMIT, STUB_VALUE_EXAMPLES_PREVIEW_LIMIT,
};
use crate::enrich;
use crate::scenarios;

pub(crate) fn build_stub_blockers_preview(
    ctx: &QueueVerificationContext<'_>,
    surface_ids: &[String],
    ledger_entries: &LedgerEntries,
    reason_code: &str,
    include_overlays_evidence: bool,
) -> Vec<enrich::VerificationStubBlockerPreview> {
    let overlays_evidence = if include_overlays_evidence {
        ctx.paths
            .evidence_from_path(&ctx.paths.surface_overlays_path())
            .ok()
    } else {
        None
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut previews = Vec::new();
    for raw_surface_id in surface_ids {
        if previews.len() >= STUB_BLOCKERS_PREVIEW_LIMIT {
            break;
        }
        let surface_id = raw_surface_id.trim();
        if surface_id.is_empty() {
            continue;
        }
        if !seen.insert(surface_id.to_string()) {
            continue;
        }
        let surface_item = crate::surface::primary_surface_item_by_id(ctx.surface, surface_id);
        let entry = ledger_entries.get(surface_id);
        previews.push(enrich::VerificationStubBlockerPreview {
            surface_id: surface_id.to_string(),
            reason_code: reason_code.to_string(),
            surface: stub_surface_preview(ctx.surface, surface_id, "option"),
            delta: stub_delta_preview(entry),
            evidence: stub_blocker_evidence_preview(
                surface_item,
                entry,
                overlays_evidence.as_ref(),
                include_overlays_evidence,
            ),
        });
    }
    previews
}

/// Derive kind from surface item using heuristics.
fn derive_kind(item: &crate::surface::SurfaceItem) -> String {
    // Entry points (id in context_argv) are commands/subcommands
    if item.context_argv.last().map(|s| s.as_str()) == Some(item.id.as_str()) {
        return "subcommand".to_string();
    }
    // Items starting with - are options
    if item.id.starts_with('-') {
        return "option".to_string();
    }
    // Default to option for non-entry-point items
    "option".to_string()
}

fn stub_surface_preview(
    surface: &crate::surface::SurfaceInventory,
    surface_id: &str,
    fallback_kind: &str,
) -> enrich::VerificationStubSurfacePreview {
    if let Some(item) = crate::surface::primary_surface_item_by_id(surface, surface_id) {
        return enrich::VerificationStubSurfacePreview {
            kind: derive_kind(item),
            forms: preview_non_empty_strings(&item.forms, STUB_FORMS_PREVIEW_LIMIT),
            value_arity: item.invocation.value_arity.clone(),
            value_separator: item.invocation.value_separator.clone(),
            value_placeholder: trimmed_option(item.invocation.value_placeholder.as_deref()),
            requires_argv: preview_non_empty_strings(
                &item.invocation.requires_argv,
                STUB_REQUIRES_ARGV_PREVIEW_LIMIT,
            ),
            value_examples_preview: preview_non_empty_strings(
                &item.invocation.value_examples,
                STUB_VALUE_EXAMPLES_PREVIEW_LIMIT,
            ),
        };
    }

    enrich::VerificationStubSurfacePreview {
        kind: fallback_kind.to_string(),
        forms: Vec::new(),
        value_arity: "unknown".to_string(),
        value_separator: "unknown".to_string(),
        value_placeholder: None,
        requires_argv: Vec::new(),
        value_examples_preview: Vec::new(),
    }
}

fn stub_delta_preview(
    entry: Option<&scenarios::VerificationEntry>,
) -> enrich::VerificationStubDeltaPreview {
    let Some(entry) = entry else {
        return enrich::VerificationStubDeltaPreview {
            delta_outcome: None,
            delta_evidence_paths: Vec::new(),
        };
    };
    enrich::VerificationStubDeltaPreview {
        delta_outcome: entry.delta_outcome.clone(),
        delta_evidence_paths: preview_non_empty_strings(
            &entry.delta_evidence_paths,
            STUB_DELTA_EVIDENCE_PATHS_LIMIT,
        ),
    }
}

fn stub_blocker_evidence_preview(
    surface_item: Option<&crate::surface::SurfaceItem>,
    entry: Option<&scenarios::VerificationEntry>,
    overlays_evidence: Option<&enrich::EvidenceRef>,
    include_overlays_evidence: bool,
) -> Vec<enrich::EvidenceRef> {
    let mut evidence = Vec::new();
    if let Some(item) = surface_item {
        evidence.extend(item.evidence.iter().take(2).cloned());
    }
    if let Some(entry) = entry {
        for path in
            preview_non_empty_strings(&entry.delta_evidence_paths, STUB_DELTA_EVIDENCE_PATHS_LIMIT)
        {
            if let Some(found) = entry
                .evidence
                .iter()
                .find(|candidate| candidate.path == path)
            {
                evidence.push(found.clone());
            } else {
                evidence.push(enrich::EvidenceRef { path, sha256: None });
            }
        }
    }
    if include_overlays_evidence {
        if let Some(evidence_ref) = overlays_evidence {
            evidence.push(evidence_ref.clone());
        }
    }
    if evidence.is_empty() {
        if let Some(entry) = entry {
            evidence.extend(entry.evidence.iter().take(2).cloned());
        }
    }
    enrich::dedupe_evidence_refs(&mut evidence);
    if evidence.len() > STUB_EVIDENCE_PREVIEW_LIMIT {
        evidence.truncate(STUB_EVIDENCE_PREVIEW_LIMIT);
    }
    evidence
}

fn preview_non_empty_strings(values: &[String], limit: usize) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .take(limit)
        .map(|value| value.to_string())
        .collect()
}

fn trimmed_option(value: Option<&str>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::super::constants::STUB_REASON_MISSING_VALUE_EXAMPLES;
    use super::*;
    use std::collections::BTreeMap;

    fn make_surface_item(surface_id: &str) -> crate::surface::SurfaceItem {
        crate::surface::SurfaceItem {
            id: surface_id.to_string(),
            display: surface_id.to_string(),
            description: None,
            parent_id: None,
            context_argv: Vec::new(),
            forms: vec![
                surface_id.to_string(),
                format!("{surface_id}=<VALUE>"),
                format!("{surface_id} <VALUE>"),
                format!("{surface_id} --long"),
                format!("{surface_id} --short"),
                format!("{surface_id} --extra"),
            ],
            invocation: crate::surface::SurfaceInvocation {
                value_arity: "required".to_string(),
                value_separator: "either".to_string(),
                value_placeholder: Some(" VALUE ".to_string()),
                value_examples: vec![
                    "one".to_string(),
                    "two".to_string(),
                    "three".to_string(),
                    "four".to_string(),
                ],
                requires_argv: vec![
                    "--color=auto".to_string(),
                    "--group-directories-first".to_string(),
                    "--si".to_string(),
                    "--human-readable".to_string(),
                ],
            },
            evidence: vec![enrich::EvidenceRef {
                path: format!("inventory/scenarios/help::{surface_id}.json"),
                sha256: None,
            }],
        }
    }

    fn make_ledger_entry(surface_id: &str) -> scenarios::VerificationEntry {
        scenarios::VerificationEntry {
            surface_id: surface_id.to_string(),
            status: "verified".to_string(),
            behavior_status: "unverified".to_string(),
            behavior_exclusion_reason_code: None,
            behavior_unverified_reason_code: None,
            behavior_unverified_scenario_id: None,
            behavior_unverified_assertion_kind: None,
            behavior_unverified_assertion_seed_path: None,
            behavior_unverified_assertion_token: None,
            scenario_ids: Vec::new(),
            scenario_paths: Vec::new(),
            behavior_scenario_ids: Vec::new(),
            behavior_assertion_scenario_ids: Vec::new(),
            behavior_scenario_paths: Vec::new(),
            delta_outcome: Some("delta_seen".to_string()),
            delta_evidence_paths: vec![
                format!("inventory/scenarios/{surface_id}-delta-baseline.json"),
                format!("inventory/scenarios/{surface_id}-delta-variant.json"),
                format!("inventory/scenarios/{surface_id}-delta-extra.json"),
            ],
            behavior_confounded_scenario_ids: Vec::new(),
            behavior_confounded_extra_surface_ids: Vec::new(),
            auto_verify_exit_code: None,
            auto_verify_stderr: None,
            evidence: vec![
                enrich::EvidenceRef {
                    path: format!("inventory/scenarios/{surface_id}-delta-baseline.json"),
                    sha256: None,
                },
                enrich::EvidenceRef {
                    path: format!("inventory/scenarios/{surface_id}-delta-variant.json"),
                    sha256: None,
                },
            ],
        }
    }

    fn make_surface_inventory(
        items: Vec<crate::surface::SurfaceItem>,
    ) -> crate::surface::SurfaceInventory {
        crate::surface::SurfaceInventory {
            schema_version: 1,
            generated_at_epoch_ms: 0,
            binary_name: Some("bin".to_string()),
            inputs_hash: None,
            discovery: Vec::new(),
            items,
            blockers: Vec::new(),
        }
    }

    fn make_plan() -> scenarios::ScenarioPlan {
        scenarios::ScenarioPlan {
            schema_version: 11,
            binary: Some("bin".to_string()),
            default_env: BTreeMap::new(),
            defaults: None,
            coverage: None,
            verification: scenarios::VerificationPlan::default(),
            scenarios: Vec::new(),
        }
    }

    #[test]
    fn stub_surface_preview_limits_invocation_shape() {
        let surface_id = "--color";
        let surface = make_surface_inventory(vec![make_surface_item(surface_id)]);
        let preview = stub_surface_preview(&surface, surface_id, "option");

        assert_eq!(preview.kind, "option");
        assert_eq!(preview.forms.len(), STUB_FORMS_PREVIEW_LIMIT);
        assert_eq!(
            preview.value_examples_preview.len(),
            STUB_VALUE_EXAMPLES_PREVIEW_LIMIT
        );
        assert_eq!(
            preview.requires_argv.len(),
            STUB_REQUIRES_ARGV_PREVIEW_LIMIT
        );
        assert_eq!(preview.value_placeholder.as_deref(), Some("VALUE"));
    }

    #[test]
    fn stub_delta_preview_limits_paths() {
        let entry = make_ledger_entry("--sort");
        let preview = stub_delta_preview(Some(&entry));

        assert_eq!(
            preview.delta_evidence_paths.len(),
            STUB_DELTA_EVIDENCE_PATHS_LIMIT
        );
    }

    #[test]
    fn build_stub_blockers_preview_caps_item_count() {
        let ids = (0..12)
            .map(|idx| format!("--opt-{idx}"))
            .collect::<Vec<_>>();
        let items = ids
            .iter()
            .map(|surface_id| make_surface_item(surface_id))
            .collect::<Vec<_>>();
        let surface = make_surface_inventory(items);
        let plan = make_plan();
        let paths =
            enrich::DocPackPaths::new(std::env::temp_dir().join("bman-stub-blockers-preview"));
        let mut ledger_entries = BTreeMap::new();
        for surface_id in &ids {
            ledger_entries.insert(surface_id.clone(), make_ledger_entry(surface_id));
        }
        let surface_evidence = enrich::EvidenceRef {
            path: "inventory/surface.json".to_string(),
            sha256: None,
        };
        let scenarios_evidence = enrich::EvidenceRef {
            path: "scenarios/plan.json".to_string(),
            sha256: None,
        };
        let mut evidence = Vec::new();
        let mut local_blockers = Vec::new();
        let mut verification_next_action = None;
        let missing = Vec::new();
        let ctx = super::QueueVerificationContext {
            plan: &plan,
            surface: &surface,
            semantics: None,
            include_full: false,
            ledger_entries: None,
            evidence: &mut evidence,
            local_blockers: &mut local_blockers,
            verification_next_action: &mut verification_next_action,
            missing: &missing,
            paths: &paths,
            surface_evidence: &surface_evidence,
            scenarios_evidence: &scenarios_evidence,
        };

        let previews = build_stub_blockers_preview(
            &ctx,
            &ids,
            &ledger_entries,
            STUB_REASON_MISSING_VALUE_EXAMPLES,
            false,
        );

        assert_eq!(previews.len(), STUB_BLOCKERS_PREVIEW_LIMIT);
        assert!(previews
            .iter()
            .all(|entry| entry.reason_code == STUB_REASON_MISSING_VALUE_EXAMPLES));
    }
}
