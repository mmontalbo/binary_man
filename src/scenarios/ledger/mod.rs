//! Scenario ledger generation.
//!
//! Ledgers summarize scenario evidence for coverage and verification without
//! embedding command semantics in Rust.
mod coverage;
mod shared;
mod verification;

pub use coverage::build_coverage_ledger;
pub use shared::normalize_surface_id;
pub use verification::build_verification_ledger;
