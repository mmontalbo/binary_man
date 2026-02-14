//! Prereq inference orchestration.
//!
//! After surface discovery completes, this module analyzes surface item
//! descriptions to infer prerequisites. This enables smarter auto-verification
//! by skipping interactive options and providing appropriate fixtures.

use crate::enrich::{load_prereqs, write_prereqs, DocPackPaths, PrereqsFile};
use crate::surface::SurfaceInventory;
use crate::workflow::lm_client::{invoke_lm_for_prereqs, LmClientConfig, SurfaceItemInfo};
use anyhow::Result;

/// Run prereq inference for surface items that don't have mappings yet.
///
/// This function:
/// 1. Loads existing prereqs.json (if any)
/// 2. Finds surface items without prereq mappings (filtered by scope_context)
/// 3. If any unmapped items exist and LM is configured, invokes LM
/// 4. Merges LM response into prereqs.json and saves
///
/// Returns the loaded/updated PrereqsFile.
pub fn infer_prereqs_for_surface(
    paths: &DocPackPaths,
    surface: &SurfaceInventory,
    lm_config: Option<&LmClientConfig>,
    scope_context: &[String],
    verbose: bool,
) -> Result<Option<PrereqsFile>> {
    let prereqs_path = paths.prereqs_path();
    let mut prereqs = load_prereqs(&prereqs_path)?.unwrap_or_else(PrereqsFile::new);

    // Find surface items without prereq mappings, filtered by scope_context
    let surface_ids: Vec<&str> = surface
        .items
        .iter()
        .filter(|item| !item.id.trim().is_empty())
        .filter(|item| {
            // Filter by scope_context if set
            scope_context.is_empty() || item.context_argv.starts_with(scope_context)
        })
        .map(|item| item.id.as_str())
        .collect();

    let unmapped = prereqs.unmapped_surface_ids(surface_ids.iter().copied());

    if unmapped.is_empty() {
        if verbose {
            eprintln!("prereq_inference: all surface items have prereq mappings");
        }
        return Ok(Some(prereqs));
    }

    // If no LM configured, return existing prereqs without inference
    let Some(lm_config) = lm_config else {
        if verbose {
            eprintln!(
                "prereq_inference: {} items need prereq inference but no LM configured",
                unmapped.len()
            );
        }
        return Ok(Some(prereqs));
    };

    if verbose {
        eprintln!(
            "prereq_inference: inferring prereqs for {} surface items",
            unmapped.len()
        );
    }

    // Build surface item info for LM
    let items: Vec<SurfaceItemInfo> = unmapped
        .iter()
        .filter_map(|id| {
            surface
                .items
                .iter()
                .find(|item| item.id == *id)
                .map(|item| SurfaceItemInfo {
                    id: item.id.clone(),
                    description: item.description.clone(),
                    forms: item.forms.clone(),
                })
        })
        .collect();

    let binary_name = surface.binary_name.as_deref().unwrap_or("<binary>");

    // Invoke LM for prereq inference
    let inferred = invoke_lm_for_prereqs(lm_config, binary_name, &prereqs.definitions, &items)?;

    if verbose {
        eprintln!(
            "prereq_inference: LM returned {} definitions, {} mappings",
            inferred.definitions.len(),
            inferred.surface_map.len()
        );
    }

    // Merge inferred prereqs
    prereqs.merge(&inferred);

    // Clean up unreferenced definitions
    prereqs.gc_definitions();

    // Save updated prereqs
    write_prereqs(&prereqs_path, &prereqs)?;

    if verbose {
        eprintln!(
            "prereq_inference: saved prereqs to {}",
            prereqs_path.display()
        );
    }

    Ok(Some(prereqs))
}

/// Load prereqs file if it exists, otherwise return empty.
pub fn load_prereqs_for_auto_verify(paths: &DocPackPaths) -> Result<PrereqsFile> {
    let prereqs_path = paths.prereqs_path();
    Ok(load_prereqs(&prereqs_path)?.unwrap_or_else(PrereqsFile::new))
}
