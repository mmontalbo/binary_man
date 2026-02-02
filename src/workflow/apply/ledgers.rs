use crate::enrich;
use crate::scenarios;
use crate::staging::write_staged_json;
use crate::surface;
use anyhow::{anyhow, Result};
use std::path::Path;

pub(super) struct LedgerArgs<'a> {
    pub(super) paths: &'a enrich::DocPackPaths,
    pub(super) staging_root: &'a Path,
    pub(super) binary_name: Option<&'a str>,
    pub(super) scenarios_path: &'a Path,
    pub(super) emit_coverage: bool,
    pub(super) emit_verification: bool,
}

pub(super) fn write_ledgers(args: &LedgerArgs<'_>) -> Result<()> {
    let LedgerArgs {
        paths,
        staging_root,
        binary_name,
        scenarios_path,
        emit_coverage,
        emit_verification,
    } = *args;

    if (emit_coverage || emit_verification) && !scenarios_path.is_file() {
        return Err(anyhow!(
            "scenarios plan missing at {}",
            scenarios_path.display()
        ));
    }
    if !scenarios_path.is_file() || (!emit_coverage && !emit_verification) {
        return Ok(());
    }

    let staged_surface = staging_root.join("inventory").join("surface.json");
    let surface_path = if staged_surface.is_file() {
        staged_surface
    } else {
        paths.surface_path()
    };
    if !surface_path.is_file() {
        return Ok(());
    }

    let surface = surface::load_surface_inventory(&surface_path)?;
    if emit_coverage {
        let coverage_binary = binary_name
            .map(|name| name.to_string())
            .or_else(|| surface.binary_name.clone())
            .ok_or_else(|| anyhow!("binary name unavailable for coverage ledger"))?;
        let ledger = scenarios::build_coverage_ledger(
            &coverage_binary,
            &surface,
            paths.root(),
            scenarios_path,
            Some(paths.root()),
        )?;
        write_staged_json(staging_root, "coverage_ledger.json", &ledger)?;
    }

    if emit_verification {
        let verification_template = paths
            .root()
            .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);
        if verification_template.is_file() {
            let verification_binary = binary_name
                .map(|name| name.to_string())
                .or_else(|| surface.binary_name.clone())
                .ok_or_else(|| anyhow!("binary name unavailable for verification ledger"))?;
            let ledger = scenarios::build_verification_ledger(
                &verification_binary,
                &surface,
                paths.root(),
                scenarios_path,
                &verification_template,
                Some(staging_root),
                Some(paths.root()),
            )?;
            write_staged_json(staging_root, "verification_ledger.json", &ledger)?;
        }
    }

    Ok(())
}
