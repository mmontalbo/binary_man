//! Scenario planning, execution, and evidence handling.
//!
//! Scenarios are the only execution primitive: plans describe intent, runs
//! generate evidence, and ledgers summarize coverage/verification without
//! embedding command semantics in Rust.
const DEFAULT_SNIPPET_MAX_BYTES: usize = 4096;
const DEFAULT_SNIPPET_MAX_LINES: usize = 60;
const MAX_SCENARIO_EVIDENCE_BYTES: usize = 64 * 1024;
const SCENARIO_PLAN_SCHEMA_VERSION: u32 = 11;
pub(crate) const SCENARIO_EVIDENCE_SCHEMA_VERSION: u32 = 3;
const SCENARIO_INDEX_SCHEMA_VERSION: u32 = 1;
pub(crate) const AUTO_VERIFY_SCENARIO_PREFIX: &str = "auto_verify::";
const MAX_SEED_ENTRIES: usize = 128;
const MAX_SEED_TOTAL_BYTES: usize = 64 * 1024;

mod auto_verification;
mod config;
pub(crate) mod evidence;
mod ledger;
mod plan;
mod run;
mod seed;
mod types;
mod validate;

pub use auto_verification::{
    auto_verification_scenarios, auto_verification_targets, auto_verification_targets_for_behavior,
    AutoVerificationTargets,
};
pub(crate) use ledger::verification_query_template_failure_path;
pub use ledger::{build_coverage_ledger, build_verification_ledger, normalize_surface_id};
pub(crate) use plan::load_plan_if_exists;
pub use plan::{load_plan, plan_stub, validate_plan};
pub(crate) use seed::{default_behavior_seed, DEFAULT_BEHAVIOR_SEED_DIR};

pub use evidence::{
    publishable_examples_report, ExamplesReport, ScenarioIndex, ScenarioIndexEntry,
};
pub(crate) use evidence::{read_runs_index_bytes, read_scenario_index};
pub use run::{
    run_scenarios, AutoVerificationKindProgress, AutoVerificationProgress, RunScenariosArgs,
};
pub use types::*;

pub(crate) fn verification_entries_by_surface_id(
    entries: Vec<VerificationEntry>,
) -> std::collections::BTreeMap<String, VerificationEntry> {
    let mut by_surface_id = std::collections::BTreeMap::new();
    for entry in entries {
        by_surface_id.insert(entry.surface_id.clone(), entry);
    }
    by_surface_id
}
