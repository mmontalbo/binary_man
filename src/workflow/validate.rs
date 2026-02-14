//! Workflow validate step.
//!
//! Validation snapshots inputs into a lock so later steps can detect staleness.
use super::EnrichContext;
use crate::cli::ValidateArgs;
use crate::docpack::ensure_doc_pack_root;
use crate::enrich;
use crate::scenarios;
use crate::semantics;
use crate::surface;
use anyhow::{anyhow, Context, Result};
use std::collections::BTreeSet;

/// Run the validate step and write `enrich/lock.json`.
pub fn run_validate(args: &ValidateArgs) -> Result<()> {
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, false)?;
    let ctx = EnrichContext::load(doc_pack_root)?;
    ctx.require_config()?;
    enrich::validate_config(&ctx.config)?;
    let _semantics = semantics::load_semantics(ctx.paths.root())?;
    let _plan = scenarios::load_plan(&ctx.paths.scenarios_plan_path(), ctx.paths.root())?;
    validate_behavior_exclusions(&ctx.paths)?;
    let lock = enrich::build_lock(ctx.paths.root(), &ctx.config, ctx.binary_name())?;
    enrich::write_lock(ctx.paths.root(), &lock)?;
    if args.verbose {
        eprintln!("wrote {}", ctx.paths.lock_path().display());
    }
    Ok(())
}

fn validate_behavior_exclusions(paths: &enrich::DocPackPaths) -> Result<()> {
    let overlays_path = paths.surface_overlays_path();
    let overlays = surface::load_surface_overlays_if_exists(&overlays_path)?;
    let Some(overlays) = overlays else {
        return Ok(());
    };
    let exclusions = surface::collect_behavior_exclusions(&overlays);
    if exclusions.is_empty() {
        return Ok(());
    }

    let surface_path = paths.surface_path();
    if !surface_path.is_file() {
        return Err(anyhow!(
            "behavior exclusions require inventory/surface.json (missing {})",
            surface_path.display()
        ));
    }
    let surface_inventory = surface::load_surface_inventory(&surface_path)
        .with_context(|| format!("read {}", surface_path.display()))?;
    surface::validate_surface_inventory(&surface_inventory)
        .with_context(|| format!("validate {}", surface_path.display()))?;
    // Behavior exclusions apply to non-entry-point items (options, flags, etc.)
    let surface_ids: BTreeSet<String> = surface_inventory
        .items
        .iter()
        .filter(|item| {
            // Exclude entry points (items whose id is in context_argv)
            item.context_argv.last().map(|s| s.as_str()) != Some(item.id.as_str())
        })
        .map(|item| item.id.trim())
        .filter(|id| !id.is_empty())
        .map(|id| id.to_string())
        .collect();

    let _validated = surface::validate_behavior_exclusions(&exclusions, &surface_ids)?;

    Ok(())
}

// Tests for validate_behavior_exclusions duplicate detection are in
// surface/behavior_exclusion.rs since the underlying validation is done there.
