//! Workflow init step.
//!
//! Init bootstraps a pack with deterministic defaults so later steps can
//! rely on pack-owned inputs.
use super::load_manifest_optional;
use crate::cli::InitArgs;
use crate::docpack::ensure_doc_pack_root;
use crate::enrich;
use crate::pack;
use crate::scenarios;
use crate::templates;
use crate::util::resolve_flake_ref;
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::Path;

/// Run the init step, creating pack defaults and required templates.
pub fn run_init(args: &InitArgs) -> Result<()> {
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, true)?;
    let paths = enrich::DocPackPaths::new(doc_pack_root);
    let config_path = paths.config_path();
    if config_path.is_file() && !args.force {
        return Err(anyhow!(
            "config already exists at {} (use --force to overwrite)",
            config_path.display()
        ));
    }
    write_doc_pack_file(
        &paths.binary_lens_export_plan_path(),
        templates::BINARY_LENS_EXPORT_PLAN_JSON,
        args.force,
    )?;
    let manifest_path = paths.pack_manifest_path();
    if !manifest_path.is_file() {
        let binary = args
            .binary
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("pack missing; provide --binary"))?;
        let lens_flake = resolve_flake_ref(args.lens_flake.as_str())?;
        let export_plan_path = paths.binary_lens_export_plan_path();
        pack::generate_pack_with_plan(
            binary,
            paths.root(),
            &lens_flake,
            Some(export_plan_path.as_path()),
            None,
        )?;
    }

    let config = enrich::default_config();
    install_query_templates(&paths, config.usage_lens_template.as_str(), args.force)?;
    let manifest = load_manifest_optional(&paths)?;
    install_scenario_plan(
        &paths,
        args.force,
        manifest.as_ref().map(|m| m.binary_name.as_str()),
    )?;
    write_doc_pack_file(
        &paths.agent_prompt_path(),
        templates::ENRICH_AGENT_PROMPT_MD,
        args.force,
    )?;
    write_doc_pack_file(
        &paths.semantics_path(),
        templates::ENRICH_SEMANTICS_JSON,
        args.force,
    )?;
    ensure_empty_fixture(paths.root())?;

    enrich::write_config(paths.root(), &config)?;
    println!("wrote {}", config_path.display());
    Ok(())
}

fn ensure_empty_fixture(doc_pack_root: &Path) -> Result<()> {
    let empty_dir = doc_pack_root.join("fixtures").join("empty");
    if !empty_dir.is_dir() {
        fs::create_dir_all(&empty_dir)
            .with_context(|| format!("create empty fixture {}", empty_dir.display()))?;
    }
    Ok(())
}

fn install_query_templates(
    paths: &enrich::DocPackPaths,
    usage_lens_template: &str,
    force: bool,
) -> Result<()> {
    write_doc_pack_file(
        &paths.root().join(usage_lens_template),
        templates::USAGE_FROM_SCENARIOS_SQL,
        force,
    )?;
    write_doc_pack_file(
        &paths
            .root()
            .join(enrich::SUBCOMMANDS_FROM_SCENARIOS_TEMPLATE_REL),
        templates::SUBCOMMANDS_FROM_SCENARIOS_SQL,
        force,
    )?;
    write_doc_pack_file(
        &paths
            .root()
            .join(enrich::OPTIONS_FROM_SCENARIOS_TEMPLATE_REL),
        templates::OPTIONS_FROM_SCENARIOS_SQL,
        force,
    )?;
    write_doc_pack_file(
        &paths
            .root()
            .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL),
        templates::VERIFICATION_FROM_SCENARIOS_SQL,
        force,
    )?;
    Ok(())
}

fn install_scenario_plan(
    paths: &enrich::DocPackPaths,
    force: bool,
    binary_name: Option<&str>,
) -> Result<()> {
    let path = paths.scenarios_plan_path();
    if path.is_file() && !force {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let text = scenarios::plan_stub(binary_name);
    fs::write(&path, text.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn write_doc_pack_file(path: &Path, contents: &str, force: bool) -> Result<()> {
    if path.is_file() && !force {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(path, contents.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}
