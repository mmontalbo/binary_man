//! Pack-owned configuration and status schema types.
//!
//! The enrich module centralizes schema versions, path handling, and typed JSON
//! structures so the workflow stays deterministic and pack-owned.
/// Current schema version for `enrich/config.json`.
pub const CONFIG_SCHEMA_VERSION: u32 = 3;
/// Current schema version for `enrich/lock.json`.
pub const LOCK_SCHEMA_VERSION: u32 = 2;
/// Current schema version for `enrich/plan.out.json`.
pub const PLAN_SCHEMA_VERSION: u32 = 2;
/// Current schema version for `enrich/report.json`.
pub const REPORT_SCHEMA_VERSION: u32 = 1;
/// Current schema version for `enrich/history.jsonl`.
pub const HISTORY_SCHEMA_VERSION: u32 = 1;

/// Default usage lens for scenario-only evidence.
pub const SCENARIO_USAGE_LENS_TEMPLATE_REL: &str = "queries/usage_from_scenarios.sql";
/// Default surface lens for scenario-only evidence (extracts options and entry points).
pub const SURFACE_FROM_SCENARIOS_TEMPLATE_REL: &str = "queries/surface_from_scenarios.sql";
/// Default verification lens for scenario-only evidence.
pub const VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL: &str =
    "queries/verification_from_scenarios.sql";
/// Included verification section templates used by the top-level verification lens.
pub const VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS: [&str; 4] = [
    "queries/verification_from_scenarios/00_inputs_normalization.sql",
    "queries/verification_from_scenarios/10_behavior_assertion_eval.sql",
    "queries/verification_from_scenarios/20_coverage_reasoning.sql",
    "queries/verification_from_scenarios/30_rollups_output.sql",
];
/// Default surface lenses for scenario-only evidence.
pub const SURFACE_LENS_TEMPLATE_RELS: [&str; 1] = [SURFACE_FROM_SCENARIOS_TEMPLATE_REL];
/// Pack-owned prompt guidance installed during init.
pub const ENRICH_AGENT_PROMPT_REL: &str = "enrich/agent_prompt.md";

mod config;
mod evidence;
mod history;
mod lock;
mod paths;
mod prereqs;
mod types;

pub use config::{
    config_stub, default_config, load_config, normalized_requirements, resolve_inputs,
    resolve_lm_command, validate_config, write_config,
};
pub use evidence::{dedupe_evidence_refs, evidence_from_path, evidence_from_rel};
pub use history::{append_history, write_report};
pub use lock::{build_lock, hash_paths, load_lock, lock_status, now_epoch_ms, write_lock};
pub use paths::DocPackPaths;
pub use prereqs::{
    load_prereqs, write_prereqs, FlatSeed, PrereqInferenceDefinition, PrereqsFile,
    PREREQS_SCHEMA_VERSION,
};
pub use types::*;
