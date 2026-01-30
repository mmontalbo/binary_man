//! Workflow validate step.
//!
//! Validation snapshots inputs into a lock so later steps can detect staleness.
use super::EnrichContext;
use crate::cli::ValidateArgs;
use crate::docpack::ensure_doc_pack_root;
use crate::enrich;
use crate::semantics;
use anyhow::Result;

/// Run the validate step and write `enrich/lock.json`.
pub fn run_validate(args: &ValidateArgs) -> Result<()> {
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, false)?;
    let ctx = EnrichContext::load(doc_pack_root)?;
    ctx.require_config()?;
    enrich::validate_config(&ctx.config)?;
    let _semantics = semantics::load_semantics(ctx.paths.root())?;
    let lock = enrich::build_lock(ctx.paths.root(), &ctx.config, ctx.binary_name())?;
    enrich::write_lock(ctx.paths.root(), &lock)?;
    if args.verbose {
        eprintln!("wrote {}", ctx.paths.lock_path().display());
    }
    Ok(())
}
