//! Scenario planning, execution, and evidence handling.
//!
//! Scenarios are the only execution primitive: plans describe intent, runs
//! generate evidence, and ledgers summarize coverage/verification without
//! embedding command semantics in Rust.
const DEFAULT_SNIPPET_MAX_BYTES: usize = 4096;
const DEFAULT_SNIPPET_MAX_LINES: usize = 60;
const MAX_SCENARIO_EVIDENCE_BYTES: usize = 64 * 1024;
const SCENARIO_PLAN_SCHEMA_VERSION: u32 = 4;
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
    auto_verification_scenarios, auto_verification_targets, AutoVerificationTargets,
};
pub use ledger::{build_coverage_ledger, build_verification_ledger, normalize_surface_id};
pub(crate) use plan::load_plan_if_exists;
pub use plan::{load_plan, plan_stub, validate_plan};

pub use evidence::{
    publishable_examples_report, ExamplesReport, ScenarioIndex, ScenarioIndexEntry,
};
pub(crate) use evidence::{read_runs_index_bytes, read_scenario_index};
pub use run::{run_scenarios, RunScenariosArgs};
pub use types::*;
