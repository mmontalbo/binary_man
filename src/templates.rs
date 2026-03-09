//! Embedded templates for SQL queries and JSON scaffolds.
//!
//! These constants are compiled into the binary via `include_str!` so the tool
//! can bootstrap doc packs without requiring external template files.

// Test-only templates (used in integration tests for fixture setup)
#[cfg(test)]
pub const USAGE_FROM_SCENARIOS_SQL: &str = include_str!("../queries/usage_from_scenarios.sql");
#[cfg(test)]
pub const SURFACE_FROM_SCENARIOS_SQL: &str = include_str!("../queries/surface_from_scenarios.sql");
#[cfg(test)]
pub const VERIFICATION_FROM_SCENARIOS_SQL: &str =
    include_str!("../queries/verification_from_scenarios.sql");
#[cfg(test)]
pub const VERIFICATION_FROM_SCENARIOS_00_INPUTS_NORMALIZATION_SQL: &str =
    include_str!("../queries/verification_from_scenarios/00_inputs_normalization.sql");
#[cfg(test)]
pub const VERIFICATION_FROM_SCENARIOS_10_BEHAVIOR_ASSERTION_EVAL_SQL: &str =
    include_str!("../queries/verification_from_scenarios/10_behavior_assertion_eval.sql");
#[cfg(test)]
pub const VERIFICATION_FROM_SCENARIOS_20_COVERAGE_REASONING_SQL: &str =
    include_str!("../queries/verification_from_scenarios/20_coverage_reasoning.sql");
#[cfg(test)]
pub const VERIFICATION_FROM_SCENARIOS_30_ROLLUPS_OUTPUT_SQL: &str =
    include_str!("../queries/verification_from_scenarios/30_rollups_output.sql");

// Production templates (used in non-test code)
pub const ENRICH_SEMANTICS_JSON: &str = include_str!("../templates/enrich_semantics.json");
pub const SCENARIOS_PLAN_JSON: &str = include_str!("../templates/scenarios_plan.json");
