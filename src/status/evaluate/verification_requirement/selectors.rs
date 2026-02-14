use super::{reasoning::behavior_reason_code_for_id, LedgerEntries};
use crate::scenarios;
use crate::status::verification_policy::{
    BehaviorReasonKind, DeltaOutcomeKind, VerificationStatus, VerificationTier,
};
use std::collections::BTreeSet;

/// Context for behavior lookup operations.
/// Bundles commonly-used lookup sets to reduce parameter count.
pub(super) struct BehaviorLookupContext<'a> {
    pub remaining_ids: &'a BTreeSet<String>,
    pub missing_value_examples: &'a BTreeSet<String>,
    pub needs_apply_ids: &'a BTreeSet<String>,
    pub ledger_entries: &'a LedgerEntries,
}

pub(super) fn behavior_scenario_surface_ids(plan: &scenarios::ScenarioPlan) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for scenario in &plan.scenarios {
        if scenario.coverage_tier.as_deref() != Some("behavior") {
            continue;
        }
        for cover in &scenario.covers {
            let trimmed = cover.trim();
            if trimmed.is_empty() {
                continue;
            }
            ids.insert(trimmed.to_string());
        }
    }
    ids
}

pub(super) fn needs_apply_ids(
    plan_behavior_ids: &BTreeSet<String>,
    remaining_ids: &BTreeSet<String>,
    ledger_entries: &LedgerEntries,
) -> BTreeSet<String> {
    let mut needs_apply = BTreeSet::new();
    for surface_id in remaining_ids {
        if !plan_behavior_ids.contains(surface_id) {
            continue;
        }
        let Some(entry) = ledger_entries.get(surface_id) else {
            continue;
        };
        if entry.behavior_scenario_paths.is_empty() {
            needs_apply.insert(surface_id.clone());
        }
    }
    needs_apply
}

pub(super) fn first_reason_id(
    required_ids: &[String],
    ctx: &BehaviorLookupContext<'_>,
) -> Option<String> {
    for surface_id in required_ids {
        if !ctx.remaining_ids.contains(surface_id) {
            continue;
        }
        if ctx.missing_value_examples.contains(surface_id)
            || ctx.needs_apply_ids.contains(surface_id)
        {
            continue;
        }
        return Some(surface_id.clone());
    }
    None
}

pub(super) fn first_reason_id_by_priority(
    required_ids: &[String],
    ctx: &BehaviorLookupContext<'_>,
    reason_kinds: &[BehaviorReasonKind],
) -> Option<String> {
    for reason_kind in reason_kinds {
        for surface_id in required_ids {
            if !ctx.remaining_ids.contains(surface_id) {
                continue;
            }
            if ctx.missing_value_examples.contains(surface_id)
                || ctx.needs_apply_ids.contains(surface_id)
            {
                continue;
            }
            let code = behavior_reason_code_for_id(
                surface_id,
                ctx.missing_value_examples,
                ctx.ledger_entries,
            );
            let candidate = BehaviorReasonKind::from_code(Some(&code));
            if candidate == *reason_kind {
                return Some(surface_id.clone());
            }
        }
    }
    None
}

pub(super) fn select_delta_outcome_ids_for_remaining(
    required_ids: &[String],
    ctx: &BehaviorLookupContext<'_>,
    outcome: DeltaOutcomeKind,
    limit: usize,
) -> Vec<String> {
    let mut selected = Vec::new();
    for surface_id in required_ids {
        if selected.len() >= limit {
            break;
        }
        if !ctx.remaining_ids.contains(surface_id) {
            continue;
        }
        if ctx.missing_value_examples.contains(surface_id) {
            continue;
        }
        let Some(entry) = ctx.ledger_entries.get(surface_id) else {
            continue;
        };
        if DeltaOutcomeKind::from_code(entry.delta_outcome.as_deref()) != outcome {
            continue;
        }
        selected.push(surface_id.clone());
    }
    selected
}

pub(super) fn collect_missing_value_examples(
    surface: &crate::surface::SurfaceInventory,
    remaining_ids: &[String],
    ledger_entries: &LedgerEntries,
) -> BTreeSet<String> {
    let mut missing = BTreeSet::new();
    for surface_id in remaining_ids {
        let Some(item) = crate::surface::primary_surface_item_by_id(surface, surface_id) else {
            continue;
        };
        if !invocation_needs_value_examples(&item.invocation) {
            continue;
        }
        if !item.invocation.value_examples.is_empty() {
            continue;
        }
        if let Some(entry) = ledger_entries.get(surface_id) {
            if entry.behavior_scenario_ids.is_empty() {
                missing.insert(surface_id.clone());
            }
        }
    }
    missing
}

fn invocation_needs_value_examples(invocation: &crate::surface::SurfaceInvocation) -> bool {
    invocation.value_arity.trim() == "required"
}

/// Derive kind from surface item using heuristics.
fn derive_kind_from_item(item: &crate::surface::SurfaceItem) -> String {
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

pub(super) fn surface_kind_for_id(
    surface: &crate::surface::SurfaceInventory,
    surface_id: &str,
    fallback_kind: &str,
) -> String {
    if let Some(item) = crate::surface::primary_surface_item_by_id(surface, surface_id) {
        return derive_kind_from_item(item);
    }
    fallback_kind.to_string()
}

pub(super) fn surface_has_requires_argv_hint(
    surface: &crate::surface::SurfaceInventory,
    surface_id: &str,
) -> bool {
    crate::surface::primary_surface_item_by_id(surface, surface_id)
        .is_some_and(|item| !item.invocation.requires_argv.is_empty())
}

pub(super) fn behavior_counts_for_ids(
    required_ids: &[String],
    ledger_entries: &LedgerEntries,
) -> (usize, usize) {
    let mut verified = 0;
    let mut unverified = 0;
    for surface_id in required_ids {
        let status = VerificationStatus::from_entry(
            ledger_entries.get(surface_id),
            VerificationTier::Behavior,
        );
        if status == VerificationStatus::Verified {
            verified += 1;
        } else if status.counts_as_unverified() {
            unverified += 1;
        }
    }
    (verified, unverified)
}

#[cfg(test)]
mod tests {
    use super::surface_kind_for_id;

    fn empty_surface() -> crate::surface::SurfaceInventory {
        crate::surface::SurfaceInventory {
            schema_version: 2,
            generated_at_epoch_ms: 0,
            binary_name: Some("bin".to_string()),
            inputs_hash: None,
            discovery: Vec::new(),
            items: Vec::new(),
            blockers: Vec::new(),
        }
    }

    #[test]
    fn surface_kind_for_id_uses_explicit_fallback_without_id_shape_heuristics() {
        let surface = empty_surface();
        let kind = surface_kind_for_id(&surface, "not_a_flag_or_command_name", "option");
        assert_eq!(kind, "option");
    }

    #[test]
    fn surface_kind_for_id_derives_subcommand_from_context_argv() {
        let mut surface = empty_surface();
        // An entry point: context_argv contains the item's id
        surface.items.push(crate::surface::SurfaceItem {
            id: "show".to_string(),
            display: "show".to_string(),
            description: None,
            parent_id: None,
            context_argv: vec!["show".to_string()],
            forms: vec!["show".to_string()],
            invocation: crate::surface::SurfaceInvocation::default(),
            evidence: Vec::new(),
        });
        let kind = surface_kind_for_id(&surface, "show", "option");
        assert_eq!(kind, "subcommand");
    }

    #[test]
    fn surface_kind_for_id_derives_option_from_dash_prefix() {
        let mut surface = empty_surface();
        surface.items.push(crate::surface::SurfaceItem {
            id: "--verbose".to_string(),
            display: "--verbose".to_string(),
            description: None,
            parent_id: None,
            context_argv: Vec::new(),
            forms: vec!["--verbose".to_string()],
            invocation: crate::surface::SurfaceInvocation::default(),
            evidence: Vec::new(),
        });
        let kind = surface_kind_for_id(&surface, "--verbose", "subcommand");
        assert_eq!(kind, "option");
    }
}
