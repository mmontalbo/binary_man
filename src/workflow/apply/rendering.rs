use crate::enrich;
use crate::pack;
use crate::scenarios;
use crate::surface;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

pub(super) fn load_examples_report_optional(
    paths: &enrich::DocPackPaths,
) -> Result<Option<scenarios::ExamplesReport>> {
    let path = paths.examples_report_path();
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let report: scenarios::ExamplesReport =
        serde_json::from_slice(&bytes).context("parse examples report")?;
    Ok(Some(report))
}

pub(super) fn resolve_pack_context_with_cwd(
    pack_root: &Path,
    doc_pack_root: &Path,
    duckdb_cwd: &Path,
    scenarios_glob: &str,
) -> Result<pack::PackContext> {
    let template = doc_pack_root.join(enrich::SCENARIO_USAGE_LENS_TEMPLATE_REL);
    pack::load_pack_context_with_template_at(pack_root, &template, duckdb_cwd, Some(scenarios_glob))
}

pub(super) fn staged_help_scenario_evidence_available(staging_root: &Path) -> bool {
    let scenarios_dir = staging_root.join("inventory").join("scenarios");
    let Ok(entries) = fs::read_dir(&scenarios_dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let path = entry.path();
        if !path.is_file() {
            return false;
        }
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| !ext.eq_ignore_ascii_case("json"))
            .unwrap_or(true)
        {
            return false;
        }
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with("help--"))
            .unwrap_or(false)
    })
}

pub(super) fn scenarios_glob(root: &Path) -> String {
    let mut path = root.join("inventory").join("scenarios");
    path.push("*.json");
    path.to_string_lossy().to_string()
}

pub(super) fn load_surface_for_render(
    staging_root: &Path,
    paths: &enrich::DocPackPaths,
) -> Result<Option<surface::SurfaceInventory>> {
    let staged_surface = staging_root.join("inventory").join("surface.json");
    let surface_path = if staged_surface.is_file() {
        staged_surface
    } else {
        paths.surface_path()
    };
    if !surface_path.is_file() {
        return Ok(None);
    }
    Ok(Some(surface::load_surface_inventory(&surface_path)?))
}
