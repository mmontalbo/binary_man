pub const USAGE_FROM_SCENARIOS_SQL: &str = include_str!("../queries/usage_from_scenarios.sql");
pub const SURFACE_FROM_SCENARIOS_SQL: &str =
    include_str!("../queries/surface_from_scenarios.sql");
pub const VERIFICATION_FROM_SCENARIOS_SQL: &str =
    include_str!("../queries/verification_from_scenarios.sql");
pub const VERIFICATION_FROM_SCENARIOS_00_INPUTS_NORMALIZATION_SQL: &str =
    include_str!("../queries/verification_from_scenarios/00_inputs_normalization.sql");
pub const VERIFICATION_FROM_SCENARIOS_10_BEHAVIOR_ASSERTION_EVAL_SQL: &str =
    include_str!("../queries/verification_from_scenarios/10_behavior_assertion_eval.sql");
pub const VERIFICATION_FROM_SCENARIOS_20_COVERAGE_REASONING_SQL: &str =
    include_str!("../queries/verification_from_scenarios/20_coverage_reasoning.sql");
pub const VERIFICATION_FROM_SCENARIOS_30_ROLLUPS_OUTPUT_SQL: &str =
    include_str!("../queries/verification_from_scenarios/30_rollups_output.sql");
pub const ENRICH_AGENT_PROMPT_MD: &str = include_str!("../prompts/enrich_agent_prompt.md");
pub const ENRICH_SEMANTICS_JSON: &str = include_str!("../templates/enrich_semantics.json");
pub const SCENARIOS_PLAN_JSON: &str = include_str!("../templates/scenarios_plan.json");
pub const BINARY_LENS_EXPORT_PLAN_JSON: &str =
    include_str!("../templates/binary_lens_export_plan.json");
