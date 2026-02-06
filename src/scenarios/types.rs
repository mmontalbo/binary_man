//! JSON schema types for scenario planning, evidence, and ledgers.
//!
//! These types keep scenario intent and evidence pack-owned while Rust stays
//! a mechanical executor.
use crate::enrich;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

fn default_true() -> bool {
    true
}

fn default_scenario_kind() -> ScenarioKind {
    ScenarioKind::Behavior
}

/// Scenario classification used by validation and rendering.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioKind {
    Help,
    Behavior,
}

/// Scenario run mode used for rerun behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScenarioRunMode {
    Default,
    RerunAll,
    RerunFailed,
}

/// Seed entry type for filesystem fixtures.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SeedEntryKind {
    Dir,
    File,
    Symlink,
}

/// Single seed entry for a scenario fixture.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ScenarioSeedEntry {
    pub path: String,
    pub kind: SeedEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

/// Collection of seed entries to materialize before a scenario run.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ScenarioSeedSpec {
    #[serde(default)]
    pub entries: Vec<ScenarioSeedEntry>,
}

/// Default scenario runtime options applied when fields are omitted.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct ScenarioDefaults {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<ScenarioSeedSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_sandbox: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_strace: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_max_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_max_bytes: Option<usize>,
}

/// Scenario plan file (`scenarios/plan.json`).
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ScenarioPlan {
    pub schema_version: u32,
    #[serde(default)]
    pub binary: Option<String>,
    #[serde(default)]
    pub default_env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<ScenarioDefaults>,
    #[serde(default)]
    pub coverage: Option<CoverageNotes>,
    #[serde(default)]
    pub verification: VerificationPlan,
    #[serde(default)]
    pub scenarios: Vec<ScenarioSpec>,
}

impl ScenarioPlan {
    pub fn collect_queue_exclusions(&self) -> (Vec<VerificationExcludedEntry>, BTreeSet<String>) {
        let mut excluded_by_id: BTreeMap<String, VerificationExcludedEntry> = BTreeMap::new();
        for entry in &self.verification.queue {
            if entry.intent != VerificationIntent::Exclude {
                continue;
            }
            let surface_id = entry.surface_id.trim();
            if surface_id.is_empty() {
                continue;
            }
            let excluded_entry =
                excluded_by_id
                    .entry(surface_id.to_string())
                    .or_insert_with(|| VerificationExcludedEntry {
                        surface_id: surface_id.to_string(),
                        reason_code: None,
                        note: None,
                        prereqs: Vec::new(),
                        reason: None,
                    });
            for prereq in &entry.prereqs {
                if !excluded_entry.prereqs.contains(prereq) {
                    excluded_entry.prereqs.push(*prereq);
                }
            }
            if excluded_entry.reason.is_none() {
                if let Some(reason) = entry.reason.as_deref() {
                    let trimmed = reason.trim();
                    if !trimmed.is_empty() {
                        excluded_entry.reason = Some(trimmed.to_string());
                    }
                }
            }
        }
        let mut excluded: Vec<VerificationExcludedEntry> = excluded_by_id.into_values().collect();
        excluded.sort_by(|a, b| a.surface_id.cmp(&b.surface_id));
        let excluded_ids = excluded
            .iter()
            .map(|entry| entry.surface_id.clone())
            .collect();
        (excluded, excluded_ids)
    }
}

/// Verification plan portion of the scenario plan.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct VerificationPlan {
    #[serde(default)]
    pub queue: Vec<VerificationQueueEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<VerificationPolicy>,
}

/// Auto-verification policy for discovered surface kinds.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct VerificationPolicy {
    pub kinds: Vec<VerificationTargetKind>,
    pub max_new_runs_per_apply: usize,
}

/// Supported auto-verification kinds.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationTargetKind {
    Option,
    Subcommand,
}

impl VerificationTargetKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            VerificationTargetKind::Option => "option",
            VerificationTargetKind::Subcommand => "subcommand",
        }
    }
}

/// Queue entry describing a surface id to verify or exclude.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct VerificationQueueEntry {
    pub surface_id: String,
    pub intent: VerificationIntent,
    #[serde(default)]
    pub prereqs: Vec<VerificationPrereq>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Intent for verification triage entries.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationIntent {
    VerifyBehavior,
    Exclude,
}

/// Preconditions that explain why an entry cannot be verified yet.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
pub enum VerificationPrereq {
    #[serde(rename = "needs_arg_value")]
    ArgValue,
    #[serde(rename = "needs_seed_fs")]
    SeedFs,
    #[serde(rename = "needs_repo")]
    Repo,
    #[serde(rename = "needs_network")]
    Network,
    #[serde(rename = "needs_interactive")]
    Interactive,
    #[serde(rename = "needs_privilege")]
    Privilege,
}

impl VerificationPrereq {
    pub fn as_str(&self) -> &'static str {
        match self {
            VerificationPrereq::ArgValue => "needs_arg_value",
            VerificationPrereq::SeedFs => "needs_seed_fs",
            VerificationPrereq::Repo => "needs_repo",
            VerificationPrereq::Network => "needs_network",
            VerificationPrereq::Interactive => "needs_interactive",
            VerificationPrereq::Privilege => "needs_privilege",
        }
    }
}

/// Coverage notes for items that are blocked or intentionally skipped.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CoverageNotes {
    #[serde(default)]
    pub blocked: Vec<CoverageBlocked>,
}

/// Coverage block entry for a set of surface ids.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CoverageBlocked {
    #[serde(default, alias = "option_ids")]
    pub item_ids: Vec<String>,
    pub reason: String,
    #[serde(default)]
    pub details: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Scenario specification used to execute a single run.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ScenarioSpec {
    pub id: String,
    #[serde(default = "default_scenario_kind")]
    pub kind: ScenarioKind,
    #[serde(default = "default_true")]
    pub publish: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<ScenarioSeedSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_sandbox: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_strace: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_max_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_max_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_scenario_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assertions: Vec<BehaviorAssertion>,
    #[serde(
        default,
        alias = "covers_options",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub covers: Vec<String>,
    #[serde(default)]
    pub coverage_ignore: bool,
    #[serde(default, skip_serializing_if = "ScenarioExpect::is_empty")]
    pub expect: ScenarioExpect,
}

/// Expectations used to classify a scenario run as accepted or rejected.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScenarioExpect {
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    #[serde(default)]
    pub stdout_contains_all: Vec<String>,
    #[serde(default)]
    pub stdout_contains_any: Vec<String>,
    #[serde(default)]
    pub stdout_regex_all: Vec<String>,
    #[serde(default)]
    pub stdout_regex_any: Vec<String>,
    #[serde(default)]
    pub stderr_contains_all: Vec<String>,
    #[serde(default)]
    pub stderr_contains_any: Vec<String>,
    #[serde(default)]
    pub stderr_regex_all: Vec<String>,
    #[serde(default)]
    pub stderr_regex_any: Vec<String>,
}

impl ScenarioExpect {
    fn has_output_predicate(&self) -> bool {
        !self.stdout_contains_all.is_empty()
            || !self.stdout_contains_any.is_empty()
            || !self.stdout_regex_all.is_empty()
            || !self.stdout_regex_any.is_empty()
            || !self.stderr_contains_all.is_empty()
            || !self.stderr_contains_any.is_empty()
            || !self.stderr_regex_all.is_empty()
            || !self.stderr_regex_any.is_empty()
    }

    fn is_empty(&self) -> bool {
        self.exit_code.is_none() && self.exit_signal.is_none() && !self.has_output_predicate()
    }
}

/// Assertion vocabulary for behavior scenarios.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum BehaviorAssertion {
    BaselineStdoutNotContainsSeedPath {
        seed_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_token: Option<String>,
    },
    BaselineStdoutContainsSeedPath {
        seed_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_token: Option<String>,
    },
    VariantStdoutContainsSeedPath {
        seed_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_token: Option<String>,
    },
    VariantStdoutNotContainsSeedPath {
        seed_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_token: Option<String>,
    },
    BaselineStdoutHasLine {
        seed_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_token: Option<String>,
    },
    BaselineStdoutNotHasLine {
        seed_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_token: Option<String>,
    },
    VariantStdoutHasLine {
        seed_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_token: Option<String>,
    },
    VariantStdoutNotHasLine {
        seed_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_token: Option<String>,
    },
    VariantStdoutDiffersFromBaseline {},
}

/// Coverage ledger emitted after scenario runs.
#[derive(Debug, Deserialize, Serialize)]
pub struct CoverageLedger {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: String,
    pub scenarios_path: String,
    pub validation_source: String,
    pub items_total: usize,
    pub behavior_count: usize,
    pub rejected_count: usize,
    pub acceptance_count: usize,
    pub blocked_count: usize,
    pub uncovered_count: usize,
    pub items: Vec<CoverageItemEntry>,
    pub unknown_items: Vec<String>,
    pub warnings: Vec<String>,
}

/// Coverage entry for a single surface item.
#[derive(Debug, Deserialize, Serialize)]
pub struct CoverageItemEntry {
    pub item_id: String,
    pub aliases: Vec<String>,
    pub status: String,
    pub behavior_scenarios: Vec<String>,
    pub rejection_scenarios: Vec<String>,
    pub acceptance_scenarios: Vec<String>,
    pub blocked_reason: Option<String>,
    pub blocked_details: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<enrich::EvidenceRef>,
}

/// Verification ledger emitted after scenario runs.
#[derive(Debug, Deserialize, Serialize)]
pub struct VerificationLedger {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: String,
    pub scenarios_path: String,
    pub surface_path: String,
    pub total_count: usize,
    pub verified_count: usize,
    pub unverified_count: usize,
    pub unverified_ids: Vec<String>,
    pub behavior_verified_count: usize,
    pub behavior_unverified_count: usize,
    pub behavior_unverified_ids: Vec<String>,
    pub excluded_count: usize,
    pub excluded: Vec<VerificationExcludedEntry>,
    pub entries: Vec<VerificationEntry>,
    pub warnings: Vec<String>,
}

/// Verification entry for a single surface id.
#[derive(Debug, Deserialize, Serialize)]
pub struct VerificationEntry {
    pub surface_id: String,
    pub status: String,
    pub behavior_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_exclusion_reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_unverified_reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_unverified_scenario_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_unverified_assertion_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_unverified_assertion_seed_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_unverified_assertion_token: Option<String>,
    #[serde(default)]
    pub scenario_ids: Vec<String>,
    #[serde(default)]
    pub scenario_paths: Vec<String>,
    #[serde(default)]
    pub behavior_scenario_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub behavior_assertion_scenario_ids: Vec<String>,
    #[serde(default)]
    pub behavior_scenario_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delta_evidence_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<enrich::EvidenceRef>,
}

#[cfg(test)]
mod tests {
    use super::ScenarioExpect;

    #[test]
    fn empty_expect_has_no_output_predicate() {
        let expect = ScenarioExpect::default();
        assert!(!expect.has_output_predicate());
    }

    #[test]
    fn exit_only_expect_has_no_output_predicate() {
        let expect = ScenarioExpect {
            exit_code: Some(0),
            ..Default::default()
        };
        assert!(!expect.has_output_predicate());
    }

    #[test]
    fn stdout_predicate_counts_as_output_predicate() {
        let expect = ScenarioExpect {
            stdout_contains_any: vec!["total".to_string()],
            ..Default::default()
        };
        assert!(expect.has_output_predicate());
    }
}

/// Excluded verification entry recorded in the ledger.
#[derive(Debug, Deserialize, Serialize)]
pub struct VerificationExcludedEntry {
    pub surface_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub prereqs: Vec<VerificationPrereq>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
