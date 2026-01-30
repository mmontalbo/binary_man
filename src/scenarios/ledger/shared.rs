//! Shared helpers for ledger building.
//!
//! These helpers keep normalization consistent between coverage and verification
//! outputs without embedding parsing heuristics.
/// Normalize surface ids by stripping value assignments.
pub fn normalize_surface_id(token: &str) -> String {
    let trimmed = token.trim();
    if let Some((head, _)) = trimmed.split_once('=') {
        head.to_string()
    } else {
        trimmed.to_string()
    }
}

pub(super) fn is_surface_item_kind(kind: &str) -> bool {
    matches!(kind, "option" | "command" | "subcommand")
}
