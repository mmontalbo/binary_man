//! Enrich configuration helpers.
//!
//! This module loads, validates, and normalizes the pack-owned config so the
//! workflow can remain deterministic and schema-driven.
use super::{
    DocPackPaths, EnrichBootstrap, EnrichConfig, RequirementId, BOOTSTRAP_SCHEMA_VERSION,
    CONFIG_SCHEMA_VERSION, SCENARIO_USAGE_LENS_TEMPLATE_REL, SURFACE_LENS_TEMPLATE_RELS,
};
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

fn default_requirements() -> Vec<RequirementId> {
    vec![
        RequirementId::Surface,
        RequirementId::Verification,
        RequirementId::ManPage,
    ]
}

/// Build the default config used when a pack is first initialized.
///
/// Defaults favor deterministic, scenario-only evidence to avoid hidden inputs.
pub fn default_config() -> EnrichConfig {
    EnrichConfig {
        schema_version: CONFIG_SCHEMA_VERSION,
        scenario_catalogs: Vec::new(),
        requirements: default_requirements(),
        verification_tier: Some("accepted".to_string()),
    }
}

/// Render a pretty JSON config stub for new packs or edit suggestions.
pub fn config_stub() -> String {
    let config = default_config();
    serde_json::to_string_pretty(&config).expect("serialize config stub")
}

/// Render a bootstrap stub to capture the binary when config is missing.
pub fn bootstrap_stub() -> String {
    let stub = EnrichBootstrap {
        schema_version: BOOTSTRAP_SCHEMA_VERSION,
        binary: "REPLACE_ME".to_string(),
        lens_flake: None,
    };
    serde_json::to_string_pretty(&stub).expect("serialize bootstrap stub")
}

/// Load the pack-owned config from `enrich/config.json`.
pub fn load_config(doc_pack_root: &Path) -> Result<EnrichConfig> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.config_path();
    let bytes = fs::read(&path).with_context(|| format!("read config {}", path.display()))?;
    let config: EnrichConfig =
        serde_json::from_slice(&bytes).context("parse enrich config JSON")?;
    Ok(config)
}

/// Load `enrich/bootstrap.json` if present, validating schema and content.
pub fn load_bootstrap_optional(doc_pack_root: &Path) -> Result<Option<EnrichBootstrap>> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.bootstrap_path();
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path).with_context(|| format!("read bootstrap {}", path.display()))?;
    let bootstrap: EnrichBootstrap =
        serde_json::from_slice(&bytes).context("parse enrich bootstrap JSON")?;
    validate_bootstrap(&bootstrap)?;
    Ok(Some(bootstrap))
}

fn validate_bootstrap(bootstrap: &EnrichBootstrap) -> Result<()> {
    if bootstrap.schema_version != BOOTSTRAP_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported bootstrap schema_version {}",
            bootstrap.schema_version
        ));
    }
    if bootstrap.binary.trim().is_empty() {
        return Err(anyhow!("bootstrap binary must be non-empty"));
    }
    Ok(())
}

/// Persist a config to disk in a stable JSON format.
pub fn write_config(doc_pack_root: &Path, config: &EnrichConfig) -> Result<()> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create enrich dir")?;
    }
    let text = serde_json::to_string_pretty(config).context("serialize enrich config")?;
    fs::write(&path, text.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Normalize requirements to defaults when the config omits them.
pub fn normalized_requirements(config: &EnrichConfig) -> Vec<RequirementId> {
    if config.requirements.is_empty() {
        return default_requirements();
    }
    config.requirements.clone()
}

/// Validate config schema and user-provided requirements.
pub fn validate_config(config: &EnrichConfig) -> Result<()> {
    if config.schema_version != CONFIG_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported enrich config schema_version {}",
            config.schema_version
        ));
    }
    if config.scenario_catalogs.len() > 1 {
        return Err(anyhow!(
            "only a single scenario catalog is supported (got {})",
            config.scenario_catalogs.len()
        ));
    }
    if let Some(tier) = config.verification_tier.as_deref() {
        if tier != "accepted" && tier != "behavior" {
            return Err(anyhow!(
                "verification_tier must be \"accepted\" or \"behavior\" (got {tier:?})"
            ));
        }
    }
    validate_relative_list(&config.scenario_catalogs, "scenario_catalogs")?;
    Ok(())
}

/// Resolve and validate required inputs for lock hashing.
///
/// This keeps lock + plan staleness detection tied to actual pack-owned inputs.
pub fn resolve_inputs(config: &EnrichConfig, doc_pack_root: &Path) -> Result<Vec<PathBuf>> {
    let scenario_catalogs = config.scenario_catalogs.clone();
    let scenario_plan = "scenarios/plan.json".to_string();
    let mut required_inputs = Vec::new();
    required_inputs.push(SCENARIO_USAGE_LENS_TEMPLATE_REL.to_string());
    for rel in SURFACE_LENS_TEMPLATE_RELS {
        required_inputs.push(rel.to_string());
    }
    required_inputs.push(scenario_plan);
    required_inputs.extend(scenario_catalogs);
    let mut inputs = Vec::new();
    for rel in required_inputs {
        validate_relative_path(&rel, "input")?;
        let path = doc_pack_root.join(&rel);
        if !path.exists() {
            return Err(anyhow!("missing input {}", rel));
        }
        inputs.push(path);
    }
    Ok(inputs)
}

fn validate_relative_path(rel: &str, label: &str) -> Result<()> {
    let path = Path::new(rel);
    if path.is_absolute() || has_parent_components(path) {
        return Err(anyhow!(
            "{label} entries must be relative paths without '..' (got {rel:?})"
        ));
    }
    Ok(())
}

fn validate_relative_list(entries: &[String], label: &str) -> Result<()> {
    for rel in entries {
        validate_relative_path(rel, label)?;
    }
    Ok(())
}

fn has_parent_components(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}
