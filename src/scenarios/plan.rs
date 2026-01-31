//! Scenario plan loading and validation.
//!
//! Plans are strictly validated to keep scenario execution deterministic and
//! pack-owned.
use crate::templates;
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::Path;

use super::validate::{validate_scenario_defaults, validate_scenario_spec};
use super::{ScenarioPlan, VerificationIntent, SCENARIO_PLAN_SCHEMA_VERSION};

/// Load and validate a scenario plan from disk.
pub fn load_plan(path: &Path, doc_pack_root: &Path) -> Result<ScenarioPlan> {
    let bytes =
        fs::read(path).with_context(|| format!("read scenarios plan {}", path.display()))?;
    let plan: ScenarioPlan = serde_json::from_slice(&bytes).context("parse scenarios plan JSON")?;
    validate_plan(&plan, doc_pack_root)?;
    Ok(plan)
}

pub(crate) fn load_plan_if_exists(
    path: &Path,
    doc_pack_root: &Path,
) -> Result<Option<ScenarioPlan>> {
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(load_plan(path, doc_pack_root)?))
}

/// Validate a scenario plan against schema and filesystem constraints.
pub fn validate_plan(plan: &ScenarioPlan, doc_pack_root: &Path) -> Result<()> {
    if plan.schema_version != SCENARIO_PLAN_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported scenarios plan schema_version {}",
            plan.schema_version
        ));
    }
    if plan.scenarios.is_empty() {
        return Err(anyhow!("scenarios plan contains no scenarios"));
    }
    if let Some(coverage) = plan.coverage.as_ref() {
        for blocked in &coverage.blocked {
            if blocked.item_ids.is_empty() {
                return Err(anyhow!("coverage.blocked entries must include item_ids"));
            }
            if blocked.reason.trim().is_empty() {
                return Err(anyhow!("coverage.blocked reason must not be empty"));
            }
        }
    }
    if let Some(defaults) = plan.defaults.as_ref() {
        validate_scenario_defaults(defaults, doc_pack_root)
            .context("validate scenario defaults")?;
    }
    for (idx, entry) in plan.verification.queue.iter().enumerate() {
        if entry.surface_id.trim().is_empty() {
            return Err(anyhow!(
                "verification.queue[{idx}] surface_id must not be empty"
            ));
        }
        if entry.intent == VerificationIntent::Exclude {
            let reason = entry.reason.as_deref().unwrap_or("");
            if reason.trim().is_empty() {
                return Err(anyhow!(
                    "verification.queue[{idx}] exclude intent requires reason"
                ));
            }
        }
    }
    if let Some(policy) = plan.verification.policy.as_ref() {
        if policy.kinds.is_empty() {
            return Err(anyhow!(
                "verification.policy.kinds must include at least one kind"
            ));
        }
        let mut seen_kinds = std::collections::BTreeSet::new();
        for kind in &policy.kinds {
            let kind_str = kind.as_str();
            if !seen_kinds.insert(kind_str) {
                return Err(anyhow!(
                    "verification.policy.kinds contains duplicate kind {kind_str}"
                ));
            }
        }
        if policy.max_new_runs_per_apply == 0 {
            return Err(anyhow!(
                "verification.policy.max_new_runs_per_apply must be > 0"
            ));
        }
        for (idx, entry) in policy.excludes.iter().enumerate() {
            if entry.surface_id.trim().is_empty() {
                return Err(anyhow!(
                    "verification.policy.excludes[{idx}] surface_id must not be empty"
                ));
            }
            if entry.reason.trim().is_empty() {
                return Err(anyhow!(
                    "verification.policy.excludes[{idx}] reason must not be empty"
                ));
            }
        }
    }
    for scenario in &plan.scenarios {
        validate_scenario_spec(scenario)
            .with_context(|| format!("validate scenario {}", scenario.id))?;
    }
    Ok(())
}

/// Render a minimal scenario plan stub for edit suggestions.
pub fn plan_stub(binary_name: Option<&str>) -> String {
    let mut plan: ScenarioPlan = serde_json::from_str(templates::SCENARIOS_PLAN_JSON)
        .expect("parse scenarios plan template");
    if let Some(binary) = binary_name {
        plan.binary = Some(binary.to_string());
    }
    serde_json::to_string_pretty(&plan).expect("serialize scenarios plan stub")
}
