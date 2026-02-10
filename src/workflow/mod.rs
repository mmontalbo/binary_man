//! Workflow orchestration for the deterministic enrich loop.
//!
//! Each step is intentionally small so the CLI can remain thin and the
//! artifact-driven flow stays predictable.
mod apply;
mod context;
mod decisions;
mod init;
mod lm_response;
mod merge_behavior_edit;
mod plan;
mod status;
mod validate;

pub(crate) use apply::run_apply;
pub(crate) use context::{load_manifest_optional, EnrichContext};
pub use init::run_init;
pub use merge_behavior_edit::run_merge_behavior_edit;
pub use plan::run_plan;
pub use status::{run_status, status_summary_for_doc_pack};
pub use validate::run_validate;
