//! Enrich configuration helpers.
//!
//! This module loads, validates, and normalizes the pack-owned config so the
//! workflow can remain deterministic and schema-driven.
use super::{
    DocPackPaths, EnrichConfig, RequirementId, CONFIG_SCHEMA_VERSION,
    SCENARIO_USAGE_LENS_TEMPLATE_REL, SURFACE_LENS_TEMPLATE_RELS,
    VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS, VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL,
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

const SCENARIOS_PLAN_REL: &str = "scenarios/plan.json";
const BINARY_LENS_EXPORT_PLAN_REL: &str = "binary_lens/export_plan.json";

/// Build the default config used when a pack is first initialized.
///
/// Defaults favor deterministic, scenario-only evidence to avoid hidden inputs.
pub fn default_config() -> EnrichConfig {
    EnrichConfig {
        schema_version: CONFIG_SCHEMA_VERSION,
        usage_lens_template: SCENARIO_USAGE_LENS_TEMPLATE_REL.to_string(),
        requirements: default_requirements(),
        verification_tier: Some("accepted".to_string()),
    }
}

/// Render a pretty JSON config stub for new packs or edit suggestions.
pub fn config_stub() -> String {
    let config = default_config();
    serde_json::to_string_pretty(&config).expect("serialize config stub")
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
    if config.usage_lens_template.trim().is_empty() {
        return Err(anyhow!("usage_lens_template must be non-empty"));
    }
    if let Some(tier) = config.verification_tier.as_deref() {
        if tier != "accepted" && tier != "behavior" {
            return Err(anyhow!(
                "verification_tier must be \"accepted\" or \"behavior\" (got {tier:?})"
            ));
        }
    }
    validate_relative_path(&config.usage_lens_template, "usage_lens_template")?;
    Ok(())
}

/// Resolve and validate required inputs for lock hashing.
///
/// This keeps lock + plan staleness detection tied to actual pack-owned inputs.
pub fn resolve_inputs(config: &EnrichConfig, doc_pack_root: &Path) -> Result<Vec<PathBuf>> {
    let mut required_inputs = Vec::new();
    required_inputs.push(config.usage_lens_template.clone());
    for rel in SURFACE_LENS_TEMPLATE_RELS {
        required_inputs.push(rel.to_string());
    }
    required_inputs.push(VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL.to_string());
    for rel in VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS {
        required_inputs.push(rel.to_string());
    }
    required_inputs.push(SCENARIOS_PLAN_REL.to_string());
    required_inputs.push(BINARY_LENS_EXPORT_PLAN_REL.to_string());
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

fn has_parent_components(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
