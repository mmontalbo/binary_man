//! Shared helpers for ledger building.
//!
//! These helpers keep normalization consistent between coverage and verification
//! outputs without embedding parsing heuristics.
use crate::surface::SurfaceItem;

/// Normalize surface ids by stripping value assignments.
pub fn normalize_surface_id(token: &str) -> String {
    let trimmed = token.trim();
    if let Some((head, _)) = trimmed.split_once('=') {
        head.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Check if a surface item is an entry point (its id is in context_argv).
pub(super) fn is_entry_point(item: &SurfaceItem) -> bool {
    item.context_argv.last().map(|s| s.as_str()) == Some(item.id.as_str())
}
