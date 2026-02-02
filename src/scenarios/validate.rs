use anyhow::{anyhow, Context, Result};
use regex::Regex;
use std::path::Path;

use super::seed::validate_seed_spec;
use super::{BehaviorAssertion, ScenarioDefaults, ScenarioExpect, ScenarioKind, ScenarioSpec};

pub(crate) fn validate_scenario_defaults(
    defaults: &ScenarioDefaults,
    doc_pack_root: &Path,
) -> Result<()> {
    if let Some(timeout_seconds) = defaults.timeout_seconds {
        if !timeout_seconds.is_finite() || timeout_seconds < 0.0 {
            return Err(anyhow!("defaults.timeout_seconds must be >= 0"));
        }
    }
    if let Some(seed) = defaults.seed.as_ref() {
        validate_seed_spec(seed).context("validate defaults.seed")?;
    } else if let Some(seed_dir) = defaults.seed_dir.as_deref() {
        let trimmed = seed_dir.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("defaults.seed_dir must not be empty"));
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            return Err(anyhow!("defaults.seed_dir must be a relative path"));
        }
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(anyhow!("defaults.seed_dir must not contain '..'"));
        }
        let resolved = doc_pack_root.join(trimmed);
        if !resolved.is_dir() {
            return Err(anyhow!(
                "defaults.seed_dir does not exist at {}",
                resolved.display()
            ));
        }
    }
    if let Some(cwd) = defaults.cwd.as_deref() {
        let trimmed = cwd.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("defaults.cwd must not be empty"));
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            return Err(anyhow!("defaults.cwd must be a relative path"));
        }
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(anyhow!("defaults.cwd must not contain '..'"));
        }
    }
    if let Some(net_mode) = defaults.net_mode.as_deref() {
        if net_mode != "off" && net_mode != "inherit" {
            return Err(anyhow!(
                "defaults.net_mode must be \"off\" or \"inherit\" (got {net_mode:?})"
            ));
        }
    }
    if let Some(max_lines) = defaults.snippet_max_lines {
        if max_lines == 0 {
            return Err(anyhow!("defaults.snippet_max_lines must be > 0"));
        }
    }
    if let Some(max_bytes) = defaults.snippet_max_bytes {
        if max_bytes == 0 {
            return Err(anyhow!("defaults.snippet_max_bytes must be > 0"));
        }
    }
    Ok(())
}

pub(crate) fn validate_scenario_spec(scenario: &ScenarioSpec) -> Result<()> {
    let id = scenario.id.trim();
    if id.is_empty() {
        return Err(anyhow!("scenario id must not be empty"));
    }
    if id.contains('/') || id.contains('\\') {
        return Err(anyhow!("scenario id must not include path separators"));
    }
    if scenario.kind == ScenarioKind::Help && !id.starts_with("help--") {
        return Err(anyhow!(
            "help scenarios must have ids starting with \"help--\""
        ));
    }
    if id.starts_with("help--") && scenario.kind != ScenarioKind::Help {
        return Err(anyhow!("help-- scenario ids are reserved for kind=help"));
    }
    if scenario.seed_dir.is_some() && scenario.seed.is_some() {
        return Err(anyhow!("use only one of seed_dir or seed"));
    }
    if let Some(seed) = scenario.seed.as_ref() {
        validate_seed_spec(seed)?;
    }
    if let Some(timeout_seconds) = scenario.timeout_seconds {
        if !timeout_seconds.is_finite() || timeout_seconds < 0.0 {
            return Err(anyhow!("timeout_seconds must be >= 0"));
        }
    }
    if let Some(seed_dir) = scenario.seed_dir.as_deref() {
        let trimmed = seed_dir.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("seed_dir must not be empty"));
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            return Err(anyhow!("seed_dir must be a relative path"));
        }
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(anyhow!("seed_dir must not contain '..'"));
        }
    }
    if let Some(cwd) = scenario.cwd.as_deref() {
        let trimmed = cwd.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("cwd must not be empty"));
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            return Err(anyhow!("cwd must be a relative path"));
        }
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(anyhow!("cwd must not contain '..'"));
        }
    }
    if let Some(net_mode) = scenario.net_mode.as_deref() {
        if net_mode != "off" && net_mode != "inherit" {
            return Err(anyhow!(
                "net_mode must be \"off\" or \"inherit\" (got {net_mode:?})"
            ));
        }
    }
    if let Some(max_lines) = scenario.snippet_max_lines {
        if max_lines == 0 {
            return Err(anyhow!("snippet_max_lines must be > 0"));
        }
    }
    if let Some(max_bytes) = scenario.snippet_max_bytes {
        if max_bytes == 0 {
            return Err(anyhow!("snippet_max_bytes must be > 0"));
        }
    }
    if let Some(coverage_tier) = scenario.coverage_tier.as_deref() {
        if coverage_tier != "acceptance"
            && coverage_tier != "behavior"
            && coverage_tier != "rejection"
        {
            return Err(anyhow!(
                "coverage_tier must be \"acceptance\", \"behavior\", or \"rejection\" (got {coverage_tier:?})"
            ));
        }
    }
    if scenario.kind != ScenarioKind::Behavior {
        if scenario.baseline_scenario_id.is_some() {
            return Err(anyhow!(
                "baseline_scenario_id is only valid for kind=behavior scenarios"
            ));
        }
        if !scenario.assertions.is_empty() {
            return Err(anyhow!(
                "assertions are only valid for kind=behavior scenarios"
            ));
        }
    }
    if let Some(baseline_id) = scenario.baseline_scenario_id.as_deref() {
        if baseline_id.trim().is_empty() {
            return Err(anyhow!("baseline_scenario_id must not be empty"));
        }
    }
    if !scenario.assertions.is_empty() {
        if scenario.coverage_tier.as_deref() != Some("behavior") {
            return Err(anyhow!("assertions require coverage_tier \"behavior\""));
        }
        for assertion in &scenario.assertions {
            validate_behavior_assertion(assertion)?;
        }
    }
    for option_id in &scenario.covers {
        if option_id.trim().is_empty() {
            return Err(anyhow!("covers entries must not be empty"));
        }
    }
    if !scenario.coverage_ignore && !scenario.covers.is_empty() {
        let has_argv = scenario.argv.iter().any(|token| !token.trim().is_empty());
        if !has_argv {
            return Err(anyhow!(
                "scenarios that cover items must include argv tokens"
            ));
        }
    }
    validate_scenario_expect(&scenario.expect)?;
    Ok(())
}

fn validate_scenario_expect(expect: &ScenarioExpect) -> Result<()> {
    validate_regex_patterns(&expect.stdout_regex_all, "stdout_regex_all")?;
    validate_regex_patterns(&expect.stdout_regex_any, "stdout_regex_any")?;
    validate_regex_patterns(&expect.stderr_regex_all, "stderr_regex_all")?;
    validate_regex_patterns(&expect.stderr_regex_any, "stderr_regex_any")?;
    Ok(())
}

fn validate_regex_patterns(patterns: &[String], field: &str) -> Result<()> {
    for pattern in patterns {
        Regex::new(pattern)
            .with_context(|| format!("invalid {field} regex pattern {pattern:?}"))?;
    }
    Ok(())
}

fn validate_behavior_assertion(assertion: &BehaviorAssertion) -> Result<()> {
    match assertion {
        BehaviorAssertion::BaselineStdoutNotContainsSeedPath { path }
        | BehaviorAssertion::BaselineStdoutContainsSeedPath { path }
        | BehaviorAssertion::VariantStdoutContainsSeedPath { path }
        | BehaviorAssertion::VariantStdoutNotContainsSeedPath { path } => {
            if path.trim().is_empty() {
                return Err(anyhow!("assertion path must not be empty"));
            }
        }
        BehaviorAssertion::VariantStdoutDiffersFromBaseline {} => {}
    }
    Ok(())
}
