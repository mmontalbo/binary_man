use super::super::load_manifest_optional;
use super::super::EnrichContext;
use crate::pack;
use anyhow::{anyhow, Result};

pub(super) fn refresh_pack_if_needed(
    ctx: &EnrichContext,
    manifest: Option<&pack::PackManifest>,
    lens_flake: &str,
) -> Result<Option<pack::PackManifest>> {
    let binary_path = manifest
        .as_ref()
        .map(|m| m.binary_path.as_str())
        .ok_or_else(|| anyhow!("manifest missing; cannot refresh pack"))?;
    let export_plan_path = ctx.paths.binary_lens_export_plan_path();
    let plan_path = export_plan_path
        .is_file()
        .then_some(export_plan_path.as_path());
    let from_pack = plan_path.map(|_| ctx.paths.pack_root());
    pack::generate_pack_with_plan(
        binary_path,
        ctx.paths.root(),
        lens_flake,
        plan_path,
        from_pack.as_deref(),
    )?;
    load_manifest_optional(&ctx.paths)
}
