//! Prereq loading utilities for auto-verification.
//!
//! Prereqs allow surface items to be excluded from auto-verification or
//! to receive specific seed fixtures.

use crate::enrich::{load_prereqs, DocPackPaths, PrereqsFile};
use anyhow::Result;

/// Load prereqs file if it exists, otherwise return empty prereqs.
pub fn load_prereqs_for_auto_verify(paths: &DocPackPaths) -> Result<PrereqsFile> {
    let prereqs_path = paths.prereqs_path();
    Ok(load_prereqs(&prereqs_path)?.unwrap_or_else(PrereqsFile::new))
}
