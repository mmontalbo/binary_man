//! Workflow orchestration for the deterministic enrich loop.
//!
//! Each step is intentionally small so the CLI can remain thin and the
//! artifact-driven flow stays predictable.
mod apply;
mod context;
mod decisions;
mod init;
pub(crate) mod lm_client;
pub(crate) mod lm_response;
mod plan;
mod run;
mod status;
mod validate;

pub(crate) use apply::run_apply;
pub(crate) use context::{load_manifest_optional, EnrichContext};
pub(crate) use init::run_init;
pub(crate) use plan::run_plan;
pub use run::run_run;
pub use status::{run_status, status_summary_for_doc_pack};
pub(crate) use validate::run_validate;
