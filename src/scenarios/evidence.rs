//! Scenario evidence formats and helpers.
//!
//! Evidence is written as JSON blobs so SQL lenses can consume it without Rust
//! embedding interpretation logic.
use crate::staging::write_staged_json;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use super::ScenarioExpect;

/// Result of running a setup command before the main scenario command.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SetupCommandResult {
    /// The command that was run.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Whether the command succeeded.
    #[serde(default)]
    pub success: bool,
    /// Exit code of the command.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Whether the command timed out.
    #[serde(default)]
    pub timed_out: bool,
    /// Stderr output (if failed).
    #[serde(default)]
    pub stderr: String,
}

/// Example report used to populate man page examples.
#[derive(Debug, Deserialize, Serialize)]
pub struct ExamplesReport {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: String,
    pub pack_root: String,
    pub scenarios_path: String,
    pub scenario_count: usize,
    pub pass_count: usize,
    pub fail_count: usize,
    pub run_ids: Vec<String>,
    pub scenarios: Vec<ScenarioOutcome>,
}

/// Outcome for a single scenario in the examples report.
#[derive(Debug, Deserialize, Serialize)]
pub struct ScenarioOutcome {
    pub scenario_id: String,
    pub publish: bool,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub timeout_seconds: Option<f64>,
    pub net_mode: Option<String>,
    pub no_sandbox: Option<bool>,
    pub no_strace: Option<bool>,
    pub snippet_max_lines: usize,
    pub snippet_max_bytes: usize,
    pub run_argv0: String,
    pub expected: ScenarioExpect,
    pub run_id: Option<String>,
    pub manifest_ref: Option<String>,
    pub stdout_ref: Option<String>,
    pub stderr_ref: Option<String>,
    pub observed_exit_code: Option<i32>,
    pub observed_exit_signal: Option<i32>,
    pub observed_timed_out: bool,
    pub pass: bool,
    pub failures: Vec<String>,
    pub command_line: String,
    pub stdout_snippet: String,
    pub stderr_snippet: String,
}

/// Filter the examples report down to publishable scenarios.
pub fn publishable_examples_report(mut report: ExamplesReport) -> Option<ExamplesReport> {
    let scenarios: Vec<ScenarioOutcome> = report
        .scenarios
        .into_iter()
        .filter(|scenario| scenario.publish)
        .collect();
    if scenarios.is_empty() {
        return None;
    }
    let pass_count = scenarios.iter().filter(|scenario| scenario.pass).count();
    let fail_count = scenarios.len() - pass_count;
    let mut run_id_set = BTreeSet::new();
    for scenario in &scenarios {
        if let Some(run_id) = scenario.run_id.as_ref() {
            run_id_set.insert(run_id.clone());
        }
    }
    report.scenario_count = scenarios.len();
    report.pass_count = pass_count;
    report.fail_count = fail_count;
    report.run_ids = run_id_set.into_iter().collect();
    report.scenarios = scenarios;
    Some(report)
}

/// Result of checking a file path for file-based assertions.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FileCheckResult {
    pub exists: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_dir: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_preview: Option<String>,
}

/// Evidence blob captured for a scenario run.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ScenarioEvidence {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub scenario_id: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub timeout_seconds: Option<f64>,
    pub net_mode: Option<String>,
    pub no_sandbox: Option<bool>,
    pub no_strace: Option<bool>,
    pub snippet_max_lines: usize,
    pub snippet_max_bytes: usize,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
    /// File state captured for file-based assertions.
    /// Keys are relative paths from scenario working directory.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files_checked: BTreeMap<String, FileCheckResult>,
    /// True if a setup command failed before the main command ran.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub setup_failed: bool,
    /// Results of setup commands (populated when setup_failed is true).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup_results: Vec<SetupCommandResult>,
}

/// Index summarizing scenario runs for quick status checks.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ScenarioIndex {
    pub schema_version: u32,
    pub scenarios: Vec<ScenarioIndexEntry>,
}

/// Index entry capturing last run metadata for a scenario.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ScenarioIndexEntry {
    pub scenario_id: String,
    pub scenario_digest: String,
    #[serde(default)]
    pub last_run_epoch_ms: Option<u128>,
    #[serde(default)]
    pub last_pass: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_paths: Vec<String>,
}

pub(crate) fn stage_scenario_evidence(
    staging_root: &Path,
    evidence: &ScenarioEvidence,
) -> Result<String> {
    let rel = scenario_output_rel_path(&evidence.scenario_id, evidence.generated_at_epoch_ms);
    write_staged_json(staging_root, &rel, evidence)?;
    Ok(rel)
}

fn scenario_output_rel_path(scenario_id: &str, generated_at_epoch_ms: u128) -> String {
    format!(
        "inventory/scenarios/{}-{}.json",
        scenario_id, generated_at_epoch_ms
    )
}

pub(crate) fn read_scenario_index(path: &Path) -> Result<Option<ScenarioIndex>> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let index: ScenarioIndex = serde_json::from_slice(&bytes).context("parse scenarios index")?;
    if index.schema_version != super::SCENARIO_INDEX_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported scenarios index schema_version {}",
            index.schema_version
        ));
    }
    Ok(Some(index))
}

pub(crate) struct ScenarioIndexState {
    pub(crate) existing: Option<ScenarioIndex>,
    pub(crate) entries: BTreeMap<String, ScenarioIndexEntry>,
    pub(crate) changed: bool,
}

pub(crate) fn load_scenario_index_state(
    scenarios_index_path: &Path,
    retain_ids: &BTreeSet<String>,
    verbose: bool,
) -> ScenarioIndexState {
    let existing = match read_scenario_index(scenarios_index_path) {
        Ok(index) => index,
        Err(err) => {
            if verbose {
                eprintln!(
                    "warning: failed to read scenario index {}: {err}",
                    scenarios_index_path.display()
                );
            }
            None
        }
    };
    let mut entries = BTreeMap::new();
    if let Some(index) = existing.as_ref() {
        for entry in &index.scenarios {
            entries.insert(entry.scenario_id.clone(), entry.clone());
        }
    }
    let before_retain = entries.len();
    entries.retain(|id, _| retain_ids.contains(id));
    let changed = before_retain != entries.len();
    ScenarioIndexState {
        existing,
        entries,
        changed,
    }
}

pub(crate) fn write_scenario_index_if_needed(
    staging_root: Option<&Path>,
    entries: BTreeMap<String, ScenarioIndexEntry>,
    has_existing_index: bool,
    index_changed: bool,
) -> Result<()> {
    let Some(staging_root) = staging_root else {
        return Ok(());
    };
    if index_changed || !has_existing_index {
        let mut entries: Vec<ScenarioIndexEntry> = entries.into_values().collect();
        entries.sort_by(|a, b| a.scenario_id.cmp(&b.scenario_id));
        let index = ScenarioIndex {
            schema_version: super::SCENARIO_INDEX_SCHEMA_VERSION,
            scenarios: entries,
        };
        write_staged_json(staging_root, "inventory/scenarios/index.json", &index)?;
    }
    Ok(())
}

