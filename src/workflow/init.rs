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
    install_binary_lens_export_plan(&paths, args.force)?;
    let manifest_path = paths.pack_manifest_path();
    if !manifest_path.is_file() {
        let mut bootstrap = None;
        let mut binary = args.binary.clone();
        if binary.is_none() {
            match enrich::load_bootstrap_optional(paths.root()) {
                Ok(Some(loaded)) => {
                    binary = Some(loaded.binary.clone());
                    bootstrap = Some(loaded);
                }
                Ok(None) => {
                    return Err(anyhow!(
                        "pack missing; provide --binary or create enrich/bootstrap.json"
                    ));
                }
                Err(err) => {
                    return Err(anyhow!(
                        "pack missing; provide --binary or create enrich/bootstrap.json ({})",
                        err
                    ));
                }
            }
        }
        let binary = binary
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow!("pack missing; provide --binary or create enrich/bootstrap.json")
            })?;
        let lens_flake_input = bootstrap
            .as_ref()
            .and_then(|loaded| loaded.lens_flake.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(args.lens_flake.as_str());
        let lens_flake = resolve_flake_ref(lens_flake_input)?;
        let export_plan_path = paths.binary_lens_export_plan_path();
        let plan_path = export_plan_path
            .is_file()
            .then_some(export_plan_path.as_path());
        pack::generate_pack_with_plan(binary, paths.root(), &lens_flake, plan_path, None)?;
    }

    install_query_templates(&paths, args.force)?;
    let manifest = load_manifest_optional(&paths)?;
    install_scenario_plan(
        &paths,
        args.force,
        manifest.as_ref().map(|m| m.binary_name.as_str()),
    )?;
    install_agent_prompt(&paths, args.force)?;
    install_semantics(&paths, args.force)?;
    ensure_empty_fixture(paths.root())?;

    let config = enrich::default_config();
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

fn install_query_templates(paths: &enrich::DocPackPaths, force: bool) -> Result<()> {
    write_query_template(
        paths.root(),
        enrich::SCENARIO_USAGE_LENS_TEMPLATE_REL,
        templates::USAGE_FROM_SCENARIOS_SQL,
        force,
    )?;
    write_query_template(
        paths.root(),
        enrich::SUBCOMMANDS_FROM_SCENARIOS_TEMPLATE_REL,
        templates::SUBCOMMANDS_FROM_SCENARIOS_SQL,
        force,
    )?;
    write_query_template(
        paths.root(),
        enrich::OPTIONS_FROM_SCENARIOS_TEMPLATE_REL,
        templates::OPTIONS_FROM_SCENARIOS_SQL,
        force,
    )?;
    write_query_template(
        paths.root(),
        enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL,
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

fn install_agent_prompt(paths: &enrich::DocPackPaths, force: bool) -> Result<()> {
    let path = paths.agent_prompt_path();
    if path.is_file() && !force {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&path, templates::ENRICH_AGENT_PROMPT_MD.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn install_semantics(paths: &enrich::DocPackPaths, force: bool) -> Result<()> {
    let path = paths.semantics_path();
    if path.is_file() && !force {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&path, templates::ENRICH_SEMANTICS_JSON.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn install_binary_lens_export_plan(paths: &enrich::DocPackPaths, force: bool) -> Result<()> {
    let path = paths.binary_lens_export_plan_path();
    if path.is_file() && !force {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&path, templates::BINARY_LENS_EXPORT_PLAN_JSON.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn write_query_template(
    doc_pack_root: &Path,
    rel_path: &str,
    contents: &str,
    force: bool,
) -> Result<()> {
    let path = doc_pack_root.join(rel_path);
    if path.is_file() && !force {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&path, contents.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}
