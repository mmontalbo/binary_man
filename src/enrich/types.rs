//! Shared JSON schema types for enrich artifacts.
//!
//! These types mirror pack-owned JSON files so the workflow remains deterministic
//! and schema-driven without embedding heuristics in code.
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;

/// Requirement identifiers used in config, plan, report, and status.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequirementId {
    Surface,
    Coverage,
    CoverageLedger,
    Verification,
    ExamplesReport,
    ManPage,
}

impl RequirementId {
    /// Return the stable string identifier used in JSON artifacts.
    pub fn as_str(&self) -> &'static str {
        match self {
            RequirementId::Surface => "surface",
            RequirementId::Coverage => "coverage",
            RequirementId::CoverageLedger => "coverage_ledger",
            RequirementId::Verification => "verification",
            RequirementId::ExamplesReport => "examples_report",
            RequirementId::ManPage => "man_page",
        }
    }

    /// Map a requirement to the plan action responsible for satisfying it.
    pub fn planned_action(&self) -> PlannedAction {
        match self {
            RequirementId::Surface => PlannedAction::SurfaceDiscovery,
            RequirementId::Coverage => PlannedAction::CoverageLedger,
            RequirementId::CoverageLedger => PlannedAction::CoverageLedger,
            RequirementId::Verification => PlannedAction::ScenarioRuns,
            RequirementId::ExamplesReport => PlannedAction::ScenarioRuns,
            RequirementId::ManPage => PlannedAction::RenderManPage,
        }
    }
}

impl fmt::Display for RequirementId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Planned actions emitted in `enrich/plan.out.json`.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PlannedAction {
    CoverageLedger,
    RenderManPage,
    ScenarioRuns,
    SurfaceDiscovery,
}

impl PlannedAction {
    /// Return the stable string identifier used in JSON artifacts.
    pub fn as_str(&self) -> &'static str {
        match self {
            PlannedAction::CoverageLedger => "coverage_ledger",
            PlannedAction::RenderManPage => "render_man_page",
            PlannedAction::ScenarioRuns => "scenario_runs",
            PlannedAction::SurfaceDiscovery => "surface_discovery",
        }
    }
}

impl fmt::Display for PlannedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Ord for PlannedAction {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialOrd for PlannedAction {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Requirement fulfillment state used in status + report outputs.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequirementState {
    Met,
    Unmet,
    Blocked,
}

impl RequirementState {
    /// Return the stable string identifier used in JSON artifacts.
    pub fn as_str(&self) -> &'static str {
        match self {
            RequirementState::Met => "met",
            RequirementState::Unmet => "unmet",
            RequirementState::Blocked => "blocked",
        }
    }
}

impl fmt::Display for RequirementState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Top-level decision for the pack after evaluating requirements.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Complete,
    Incomplete,
    Blocked,
}

impl Decision {
    /// Return the stable string identifier used in JSON artifacts.
    pub fn as_str(&self) -> &'static str {
        match self {
            Decision::Complete => "complete",
            Decision::Incomplete => "incomplete",
            Decision::Blocked => "blocked",
        }
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Pack-owned configuration for the enrichment workflow.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EnrichConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub scenario_catalogs: Vec<String>,
    #[serde(default)]
    pub requirements: Vec<RequirementId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_tier: Option<String>,
}

/// Bootstrap stub used when a pack exists without a config.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EnrichBootstrap {
    pub schema_version: u32,
    pub binary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens_flake: Option<String>,
}

/// Lock snapshot tying plan/apply to a stable set of inputs.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EnrichLock {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: Option<String>,
    pub config_path: String,
    pub inputs: Vec<String>,
    pub inputs_hash: String,
}

/// Status of the lock file relative to current inputs.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LockStatus {
    pub present: bool,
    pub stale: bool,
    pub inputs_hash: Option<String>,
}

/// Status of the plan file relative to the current lock and inputs.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PlanStatus {
    pub present: bool,
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_inputs_hash: Option<String>,
}

/// Excluded verification target with a rationale.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VerificationExclusion {
    pub surface_id: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prereqs: Vec<String>,
}

/// Summary of unverified targets grouped by reason code.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VerificationReasonSummary {
    pub reason_code: String,
    pub count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview: Vec<String>,
}

/// Compact triage summary used in status output.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VerificationTriageSummary {
    #[serde(default)]
    pub discovered_untriaged_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovered_untriaged_preview: Vec<String>,
    #[serde(default)]
    pub triaged_unverified_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triaged_unverified_preview: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remaining_by_kind: Vec<VerificationKindSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<VerificationExclusion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub behavior_unverified_reasons: Vec<VerificationReasonSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered_untriaged_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triaged_unverified_ids: Option<Vec<String>>,
}

/// Verification plan snapshot summary emitted by `bman plan`.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VerificationPlanSummary {
    pub target_count: usize,
    pub excluded_count: usize,
    pub remaining_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remaining_preview: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_kind: Vec<VerificationKindSummary>,
}

/// Summary for verification targets grouped by kind.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VerificationKindSummary {
    pub kind: String,
    pub target_count: usize,
    pub remaining_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remaining_preview: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_ids: Option<Vec<String>>,
}

/// Requirement evaluation outcome, evidence, and blockers.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RequirementStatus {
    pub id: RequirementId,
    pub status: RequirementState,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unverified_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_verified_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_unverified_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationTriageSummary>,
    pub evidence: Vec<EvidenceRef>,
    pub blockers: Vec<Blocker>,
}

/// Reference to an evidence file for traceability.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EvidenceRef {
    pub path: String,
    pub sha256: Option<String>,
}

/// Scenario failures surfaced in status for quick remediation.
#[derive(Debug, Serialize, Clone)]
pub struct ScenarioFailure {
    pub scenario_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
}

/// Summary of a single lens execution (usage/surface/verification).
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LensSummary {
    pub kind: String,
    pub template_path: String,
    pub status: String,
    pub evidence: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Blocking condition that prevents a requirement from being met.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Blocker {
    pub code: String,
    pub message: String,
    pub evidence: Vec<EvidenceRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

/// Deterministic next action used by both humans and agents.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NextAction {
    Command {
        command: String,
        reason: String,
    },
    Edit {
        path: String,
        content: String,
        reason: String,
    },
}

/// Canonical status summary emitted by `bman status --json`.
#[derive(Debug, Serialize, Clone)]
pub struct StatusSummary {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: Option<String>,
    pub lock: LockStatus,
    pub plan: PlanStatus,
    pub requirements: Vec<RequirementStatus>,
    pub missing_artifacts: Vec<String>,
    pub blockers: Vec<Blocker>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenario_failures: Vec<ScenarioFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lens_summary: Vec<LensSummary>,
    pub decision: Decision,
    pub decision_reason: Option<String>,
    pub next_action: NextAction,
    pub warnings: Vec<String>,
    pub man_warnings: Vec<String>,
    pub force_used: bool,
}

/// Summary of a single workflow step for history/reporting.
#[derive(Debug, Serialize, Clone)]
pub struct EnrichRunSummary {
    pub step: String,
    pub started_at_epoch_ms: u128,
    pub finished_at_epoch_ms: u128,
    pub success: bool,
    pub inputs_hash: Option<String>,
    pub outputs_hash: Option<String>,
    pub message: Option<String>,
}

/// History entry recorded after each workflow step.
#[derive(Debug, Serialize)]
pub struct EnrichHistoryEntry {
    pub schema_version: u32,
    pub started_at_epoch_ms: u128,
    pub finished_at_epoch_ms: u128,
    pub step: String,
    pub inputs_hash: Option<String>,
    pub outputs_hash: Option<String>,
    pub success: bool,
    pub message: Option<String>,
    pub force_used: bool,
}

/// Plan snapshot emitted by `bman plan`.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EnrichPlan {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: Option<String>,
    pub lock: EnrichLock,
    pub requirements: Vec<RequirementStatus>,
    pub planned_actions: Vec<PlannedAction>,
    pub next_action: NextAction,
    pub decision: Decision,
    pub decision_reason: Option<String>,
    pub force_used: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_plan: Option<VerificationPlanSummary>,
}

/// Report emitted after `bman apply` completes.
#[derive(Debug, Serialize, Clone)]
pub struct EnrichReport {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: Option<String>,
    pub lock: Option<EnrichLock>,
    pub requirements: Vec<RequirementStatus>,
    pub blockers: Vec<Blocker>,
    pub missing_artifacts: Vec<String>,
    pub decision: Decision,
    pub decision_reason: Option<String>,
    pub next_action: NextAction,
    pub last_run: Option<EnrichRunSummary>,
    pub force_used: bool,
}
