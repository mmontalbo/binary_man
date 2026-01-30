//! Plan helper utilities for status/plan generation.
//!
//! These utilities translate requirement outcomes into deterministic actions.
use crate::enrich;
use anyhow::{anyhow, Context, Result};
use std::path::Path;

/// Convert requirement statuses into planned actions.
pub fn planned_actions_from_requirements(
    requirements: &[enrich::RequirementStatus],
) -> Vec<enrich::PlannedAction> {
    let mut actions = std::collections::BTreeSet::new();
    for req in requirements {
        if req.status == enrich::RequirementState::Met {
            continue;
        }
        actions.insert(req.id.planned_action());
    }
    actions.into_iter().collect()
}

/// Compute whether the plan is present and stale relative to the lock.
pub fn plan_status(
    lock: Option<&enrich::EnrichLock>,
    plan: Option<&enrich::EnrichPlan>,
) -> enrich::PlanStatus {
    let lock_inputs_hash = lock.map(|lock| lock.inputs_hash.clone());
    let Some(plan) = plan else {
        return enrich::PlanStatus {
            present: false,
            stale: false,
            inputs_hash: None,
            lock_inputs_hash,
        };
    };
    let stale = match lock {
        Some(lock) => plan.lock.inputs_hash != lock.inputs_hash,
        None => true,
    };
    enrich::PlanStatus {
        present: true,
        stale,
        inputs_hash: Some(plan.lock.inputs_hash.clone()),
        lock_inputs_hash,
    }
}

/// Load `enrich/plan.out.json` from disk.
pub fn load_plan(doc_pack_root: &Path) -> Result<enrich::EnrichPlan> {
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.plan_path();
    if !path.is_file() {
        return Err(anyhow!(
            "missing plan at {} (run `bman plan --doc-pack {}` first)",
            path.display(),
            doc_pack_root.display()
        ));
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let plan: enrich::EnrichPlan = serde_json::from_slice(&bytes).context("parse plan JSON")?;
    if plan.schema_version != enrich::PLAN_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported plan schema_version {}",
            plan.schema_version
        ));
    }
    Ok(plan)
}

/// Write a plan snapshot to `enrich/plan.out.json`.
pub fn write_plan(doc_pack_root: &Path, plan: &enrich::EnrichPlan) -> Result<()> {
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.plan_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create enrich dir")?;
    }
    let text = serde_json::to_string_pretty(plan).context("serialize plan")?;
    std::fs::write(&path, text.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}
