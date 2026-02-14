//! Auto-verification scenario generation.
//!
//! Functions for generating and tracking automatic verification scenarios.

use crate::enrich;
use crate::scenarios;
use crate::semantics;
use crate::surface;
use anyhow::Result;
use std::path::Path;

/// Batch of auto-verification scenarios to run.
pub(super) struct AutoVerificationBatch {
    pub scenarios: Vec<scenarios::ScenarioSpec>,
    pub max_new_runs_per_apply: usize,
    pub targets: scenarios::AutoVerificationTargets,
    pub surface: surface::SurfaceInventory,
}

/// Generate auto-verification scenarios for the given plan.
///
/// When `scope_context` is set, only surfaces with matching `context_argv` are
/// included in verification targets.
pub(super) fn auto_verification_scenarios(
    plan: &scenarios::ScenarioPlan,
    doc_pack_root: &Path,
    staging_root: &Path,
    verbose: bool,
    verification_tier: &str,
    scope_context: &[String],
) -> Result<Option<AutoVerificationBatch>> {
    let mut surface = match load_surface_for_auto(doc_pack_root, staging_root, verbose)? {
        Some(surface) => surface,
        None => return Ok(None),
    };

    // Filter surface items by scope_context if set
    if !scope_context.is_empty() {
        surface
            .items
            .retain(|item| item.context_argv.starts_with(scope_context));
        if verbose && surface.items.is_empty() {
            eprintln!(
                "warning: no surface items match scope context {:?}",
                scope_context
            );
        }
    }

    let semantics = match semantics::load_semantics(doc_pack_root) {
        Ok(semantics) => semantics,
        Err(err) => {
            if verbose {
                eprintln!("warning: skipping auto verification (load semantics failed: {err})");
            }
            return Ok(None);
        }
    };
    let Some(targets) = (if verification_tier == "behavior" {
        scenarios::auto_verification_targets_for_behavior(plan, &surface, &semantics)
    } else {
        scenarios::auto_verification_targets(plan, &surface)
    }) else {
        return Ok(None);
    };

    // Load prereqs for seed resolution
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    let prereqs = super::prereq_inference::load_prereqs_for_auto_verify(&paths).ok();

    let scenarios =
        scenarios::auto_verification_scenarios(&targets, &semantics, &surface, prereqs.as_ref());
    Ok(Some(AutoVerificationBatch {
        scenarios,
        max_new_runs_per_apply: targets.max_new_runs_per_apply,
        targets,
        surface,
    }))
}

/// Build progress tracking information for auto-verification.
pub(super) fn auto_verification_progress(
    plan: &enrich::EnrichPlan,
    scenario_plan: &scenarios::ScenarioPlan,
    config: &enrich::EnrichConfig,
    batch: &AutoVerificationBatch,
    paths: &enrich::DocPackPaths,
) -> scenarios::AutoVerificationProgress {
    if let Some(summary) = plan.verification_plan.as_ref() {
        return plan_summary_progress(summary, batch.targets.max_new_runs_per_apply);
    }

    let binary_name = batch
        .surface
        .binary_name
        .clone()
        .unwrap_or_else(|| "<binary>".to_string());
    let template_path = paths
        .root()
        .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);
    let ledger_entries = scenarios::build_verification_ledger(
        &binary_name,
        &batch.surface,
        paths.root(),
        &paths.scenarios_plan_path(),
        &template_path,
        None,
        Some(paths.root()),
    )
    .ok()
    .map(|ledger| scenarios::verification_entries_by_surface_id(ledger.entries));
    let verification_tier = config.verification_tier.as_deref().unwrap_or("behavior");
    if let Ok(semantics) = semantics::load_semantics(paths.root()) {
        if let Some(summary) = crate::status::auto_verification_plan_summary(
            scenario_plan,
            &batch.surface,
            ledger_entries.as_ref(),
            verification_tier,
            &semantics,
        ) {
            return plan_summary_progress(&summary, batch.targets.max_new_runs_per_apply);
        }
    }

    // Group by derived kind based on id shape
    let mut options_count = 0usize;
    let mut other_count = 0usize;
    for id in &batch.targets.target_ids {
        if id.starts_with('-') {
            options_count += 1;
        } else {
            other_count += 1;
        }
    }
    let mut remaining_by_kind = Vec::new();
    if options_count > 0 {
        remaining_by_kind.push(scenarios::AutoVerificationKindProgress {
            kind: "option".to_string(),
            remaining_count: options_count,
        });
    }
    if other_count > 0 {
        remaining_by_kind.push(scenarios::AutoVerificationKindProgress {
            kind: "other".to_string(),
            remaining_count: other_count,
        });
    }
    scenarios::AutoVerificationProgress {
        remaining_total: Some(batch.targets.target_ids.len()),
        remaining_by_kind,
        max_new_runs_per_apply: batch.targets.max_new_runs_per_apply,
    }
}

/// Convert a verification plan summary to auto-verification progress.
fn plan_summary_progress(
    summary: &enrich::VerificationPlanSummary,
    max_new_runs_per_apply: usize,
) -> scenarios::AutoVerificationProgress {
    let remaining_by_kind = summary
        .by_kind
        .iter()
        .map(|group| scenarios::AutoVerificationKindProgress {
            kind: group.kind.clone(),
            remaining_count: group.remaining_count,
        })
        .collect();
    scenarios::AutoVerificationProgress {
        remaining_total: Some(summary.remaining_count),
        remaining_by_kind,
        max_new_runs_per_apply,
    }
}

/// Load surface inventory for auto-verification, checking staged version first.
pub(super) fn load_surface_for_auto(
    doc_pack_root: &Path,
    staging_root: &Path,
    verbose: bool,
) -> Result<Option<surface::SurfaceInventory>> {
    let staged_surface = staging_root.join("inventory").join("surface.json");
    let surface_path = if staged_surface.is_file() {
        staged_surface
    } else {
        doc_pack_root.join("inventory").join("surface.json")
    };
    if !surface_path.is_file() {
        return Ok(None);
    }
    match surface::load_surface_inventory(&surface_path) {
        Ok(surface) => Ok(Some(surface)),
        Err(err) => {
            if verbose {
                eprintln!("warning: skipping auto verification (invalid surface inventory: {err})");
            }
            Ok(None)
        }
    }
}
