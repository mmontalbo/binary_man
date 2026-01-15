//! Scenario JSON model and validation rules (v0).

use serde::{Deserialize, Serialize};

use crate::paths::validate_relative_path;

/// Maximum wall-clock time accepted by the runner.
pub(crate) const MAX_WALL_TIME_MS: u64 = 60_000;
/// Maximum CPU time accepted by the runner.
pub(crate) const MAX_CPU_TIME_MS: u64 = 30_000;
/// Maximum address space size accepted by the runner.
pub(crate) const MAX_MEMORY_KB: u64 = 262_144;
/// Maximum file size accepted by the runner.
pub(crate) const MAX_FILE_SIZE_KB: u64 = 10_240;
/// Maximum number of args accepted by the runner.
pub(crate) const MAX_ARGS: usize = 256;
/// Maximum length of a single arg accepted by the runner.
pub(crate) const MAX_ARG_LEN: usize = 4096;
/// Maximum length of the rationale field.
pub(crate) const MAX_RATIONALE_LEN: usize = 1024;

/// Top-level scenario spec parsed from JSON.
#[derive(Deserialize, Serialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scenario {
    pub(crate) scenario_id: String,
    pub(crate) rationale: String,
    pub(crate) binary: ScenarioBinary,
    pub(crate) args: Vec<String>,
    pub(crate) fixture: ScenarioFixture,
    pub(crate) limits: ScenarioLimits,
    pub(crate) artifacts: ScenarioArtifacts,
}

/// Binary reference for the scenario.
#[derive(Deserialize, Serialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScenarioBinary {
    pub(crate) path: String,
}

/// Fixture ID for the scenario.
#[derive(Deserialize, Serialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScenarioFixture {
    pub(crate) id: String,
}

/// Resource limits applied to the scenario.
#[derive(Deserialize, Serialize, Debug, Copy, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScenarioLimits {
    pub(crate) wall_time_ms: u64,
    pub(crate) cpu_time_ms: u64,
    pub(crate) memory_kb: u64,
    pub(crate) file_size_kb: u64,
}

/// Artifact capture settings.
#[derive(Deserialize, Serialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScenarioArtifacts {
    pub(crate) capture_stdout: bool,
    pub(crate) capture_stderr: bool,
    pub(crate) capture_exit_code: bool,
}

/// Validate scenario limits and fields, returning errors if any.
pub(crate) fn validate_scenario(scenario: &Scenario) -> Option<Vec<String>> {
    let mut errors = Vec::new();
    if scenario.scenario_id.trim().is_empty() {
        errors.push("scenario_id is required".to_string());
    }
    if scenario.rationale.trim().is_empty() {
        errors.push("rationale is required".to_string());
    }
    if scenario.rationale.len() > MAX_RATIONALE_LEN {
        errors.push(format!(
            "rationale exceeds max length ({MAX_RATIONALE_LEN})"
        ));
    }
    if scenario.rationale.contains('\0') {
        errors.push("rationale contains NUL".to_string());
    }
    if scenario.binary.path.trim().is_empty() {
        errors.push("binary.path is required".to_string());
    }
    if scenario.args.len() > MAX_ARGS {
        errors.push(format!("args exceeds max count ({MAX_ARGS})"));
    }
    for (idx, arg) in scenario.args.iter().enumerate() {
        if arg.len() > MAX_ARG_LEN {
            errors.push(format!("args[{idx}] exceeds max length ({MAX_ARG_LEN})"));
        }
        if arg.contains('\0') {
            errors.push(format!("args[{idx}] contains NUL"));
        }
    }
    if let Err(err) = validate_relative_path(&scenario.fixture.id) {
        errors.push(format!("fixture.id invalid: {err}"));
    }
    validate_limit(
        "wall_time_ms",
        scenario.limits.wall_time_ms,
        MAX_WALL_TIME_MS,
        &mut errors,
    );
    validate_limit(
        "cpu_time_ms",
        scenario.limits.cpu_time_ms,
        MAX_CPU_TIME_MS,
        &mut errors,
    );
    validate_limit(
        "memory_kb",
        scenario.limits.memory_kb,
        MAX_MEMORY_KB,
        &mut errors,
    );
    validate_limit(
        "file_size_kb",
        scenario.limits.file_size_kb,
        MAX_FILE_SIZE_KB,
        &mut errors,
    );
    if !scenario.artifacts.capture_exit_code {
        errors.push("artifacts.capture_exit_code must be true".to_string());
    }

    if errors.is_empty() {
        None
    } else {
        Some(errors)
    }
}

fn validate_limit(name: &str, value: u64, max: u64, errors: &mut Vec<String>) {
    if value == 0 {
        errors.push(format!("{name} must be > 0"));
    } else if value > max {
        errors.push(format!("{name} exceeds max ({max})"));
    }
}
