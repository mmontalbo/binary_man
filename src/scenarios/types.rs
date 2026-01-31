//! JSON schema types for scenario planning, evidence, and ledgers.
//!
//! These types keep scenario intent and evidence pack-owned while Rust stays
//! a mechanical executor.
use crate::enrich;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    #[serde(default)]
    pub contents: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
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
    #[serde(default)]
    pub excludes: Vec<VerificationPolicyExclude>,
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

/// Exclusion entry for auto-verification policy.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct VerificationPolicyExclude {
    pub surface_id: String,
    #[serde(default)]
    pub prereqs: Vec<VerificationPrereq>,
    pub reason: String,
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
    VerifyAccepted,
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
    fn is_empty(&self) -> bool {
        self.exit_code.is_none()
            && self.exit_signal.is_none()
            && self.stdout_contains_all.is_empty()
            && self.stdout_contains_any.is_empty()
            && self.stdout_regex_all.is_empty()
            && self.stdout_regex_any.is_empty()
            && self.stderr_contains_all.is_empty()
            && self.stderr_contains_any.is_empty()
            && self.stderr_regex_all.is_empty()
            && self.stderr_regex_any.is_empty()
    }
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
    #[serde(default)]
    pub scenario_ids: Vec<String>,
    #[serde(default)]
    pub scenario_paths: Vec<String>,
    #[serde(default)]
    pub behavior_scenario_ids: Vec<String>,
    #[serde(default)]
    pub behavior_scenario_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<enrich::EvidenceRef>,
}

/// Excluded verification entry recorded in the ledger.
#[derive(Debug, Deserialize, Serialize)]
pub struct VerificationExcludedEntry {
    pub surface_id: String,
    #[serde(default)]
    pub prereqs: Vec<VerificationPrereq>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
