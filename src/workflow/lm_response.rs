//! LM response handling for decision point protocol.
//!
//! This module defines the contract for LM responses to decision items and
//! provides validation and application logic.
use crate::scenarios::{BehaviorAssertion, ScenarioSpec};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;

/// Schema version for LM response format.
pub const LM_RESPONSE_SCHEMA_VERSION: u32 = 1;

/// Container for LM responses to decision items.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LmResponseBatch {
    /// Schema version for forward compatibility.
    #[serde(default)]
    pub schema_version: u32,

    /// List of responses to individual decision items.
    pub responses: Vec<LmDecisionResponse>,
}

/// Response to a single decision item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LmDecisionResponse {
    /// The surface_id this response addresses (must match a decision item).
    pub surface_id: String,

    /// The action to take for this decision.
    pub action: LmAction,
}

/// Actions an LM can take for a decision item.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LmAction {
    /// Add or update a behavior scenario.
    AddScenario {
        /// Scenario specification to upsert.
        scenario: Box<ScenarioSpec>,
    },

    /// Fix assertions in an existing scenario.
    FixAssertions {
        /// ID of the scenario to update.
        scenario_id: String,
        /// New assertions to replace existing ones.
        assertions: Vec<BehaviorAssertion>,
    },

    /// Add value examples for an option that requires a value.
    AddValueExamples {
        /// Example values to add (e.g., ["always", "never", "auto"]).
        value_examples: Vec<String>,
    },

    /// Add requires_argv for options that need other flags.
    AddRequiresArgv {
        /// Arguments required before this option (e.g., ["-l"]).
        requires_argv: Vec<String>,
    },

    /// Update the baseline_scenario_id for a behavior scenario.
    /// Used when delta comparison should use a different baseline.
    UpdateBaseline {
        /// ID of the scenario to update.
        scenario_id: String,
        /// New baseline_scenario_id to use.
        baseline_scenario_id: String,
    },

    /// Mark the item as excluded (fixture gap, not testable, etc.).
    AddExclusion {
        /// Reason code for exclusion.
        reason_code: ExclusionReasonCode,
        /// Human-readable note explaining why.
        note: String,
    },

    /// Skip this item for now (will remain in decisions list).
    Skip {
        /// Reason for skipping.
        reason: String,
    },
}

/// Valid exclusion reason codes (must match surface/overlays.rs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReasonCode {
    /// Cannot create fixture to demonstrate behavior (e.g., needs special chars).
    FixtureGap,
    /// No assertion can reliably distinguish baseline from variant output.
    AssertionGap,
    /// Output is nondeterministic and cannot be reliably tested.
    Nondeterministic,
    /// Requires an interactive TTY which is not available in sandbox.
    RequiresInteractiveTty,
    /// Has unsafe side effects that cannot be tested in sandbox.
    UnsafeSideEffects,
}

/// Result of validating an LM response batch.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationResult {
    /// Number of valid responses.
    pub valid_count: usize,
    /// Number of skipped responses.
    pub skipped_count: usize,
    /// Validation errors for invalid responses.
    pub errors: Vec<ValidationError>,
}

/// A validation error for a single response.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationError {
    /// The surface_id that had the error.
    pub surface_id: String,
    /// The error message.
    pub message: String,
}

/// Validated and categorized responses ready for application.
#[derive(Debug, Clone, Default)]
pub struct ValidatedResponses {
    /// Scenarios to upsert (keyed by scenario_id).
    pub scenarios_to_upsert: Vec<ScenarioSpec>,
    /// Assertion fixes (scenario_id -> new assertions).
    pub assertion_fixes: BTreeMap<String, Vec<BehaviorAssertion>>,
    /// Value examples to add (surface_id -> examples).
    pub value_examples: BTreeMap<String, Vec<String>>,
    /// Requires argv to add (surface_id -> argv).
    pub requires_argv: BTreeMap<String, Vec<String>>,
    /// Baseline updates (scenario_id -> new baseline_scenario_id).
    pub baseline_updates: BTreeMap<String, String>,
    /// Exclusions to add (surface_id -> (reason_code, note)).
    pub exclusions: BTreeMap<String, (ExclusionReasonCode, String)>,
    /// Skipped items with reasons.
    pub skipped: BTreeMap<String, String>,
}

/// Load and parse an LM response file.
pub fn load_lm_response(path: &std::path::Path) -> Result<LmResponseBatch> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read LM response from {}", path.display()))?;
    let batch: LmResponseBatch =
        serde_json::from_str(&content).context("parse LM response JSON")?;

    if batch.schema_version != 0 && batch.schema_version != LM_RESPONSE_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported LM response schema_version {} (expected {})",
            batch.schema_version,
            LM_RESPONSE_SCHEMA_VERSION
        ));
    }

    Ok(batch)
}

/// Validate an LM response batch against the current decisions.
pub fn validate_responses(
    batch: &LmResponseBatch,
    valid_surface_ids: &std::collections::BTreeSet<String>,
) -> (ValidatedResponses, ValidationResult) {
    let mut validated = ValidatedResponses::default();
    let mut result = ValidationResult {
        valid_count: 0,
        skipped_count: 0,
        errors: Vec::new(),
    };

    for response in &batch.responses {
        let surface_id = response.surface_id.trim();

        // Validate surface_id exists in decisions
        if !valid_surface_ids.contains(surface_id) {
            result.errors.push(ValidationError {
                surface_id: surface_id.to_string(),
                message: format!(
                    "surface_id '{}' not found in current decisions list",
                    surface_id
                ),
            });
            continue;
        }

        match &response.action {
            LmAction::AddScenario { scenario } => {
                if let Err(e) = validate_scenario(scenario, surface_id) {
                    result.errors.push(ValidationError {
                        surface_id: surface_id.to_string(),
                        message: e.to_string(),
                    });
                    continue;
                }
                validated.scenarios_to_upsert.push((**scenario).clone());
                result.valid_count += 1;
            }

            LmAction::FixAssertions {
                scenario_id,
                assertions,
            } => {
                if scenario_id.trim().is_empty() {
                    result.errors.push(ValidationError {
                        surface_id: surface_id.to_string(),
                        message: "fix_assertions requires non-empty scenario_id".to_string(),
                    });
                    continue;
                }
                if assertions.is_empty() {
                    result.errors.push(ValidationError {
                        surface_id: surface_id.to_string(),
                        message: "fix_assertions requires at least one assertion".to_string(),
                    });
                    continue;
                }
                validated
                    .assertion_fixes
                    .insert(scenario_id.clone(), assertions.clone());
                result.valid_count += 1;
            }

            LmAction::AddValueExamples { value_examples } => {
                if value_examples.is_empty() {
                    result.errors.push(ValidationError {
                        surface_id: surface_id.to_string(),
                        message: "add_value_examples requires at least one example".to_string(),
                    });
                    continue;
                }
                let filtered: Vec<String> = value_examples
                    .iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if filtered.is_empty() {
                    result.errors.push(ValidationError {
                        surface_id: surface_id.to_string(),
                        message: "add_value_examples examples must not all be empty".to_string(),
                    });
                    continue;
                }
                validated
                    .value_examples
                    .insert(surface_id.to_string(), filtered);
                result.valid_count += 1;
            }

            LmAction::AddRequiresArgv { requires_argv } => {
                if requires_argv.is_empty() {
                    result.errors.push(ValidationError {
                        surface_id: surface_id.to_string(),
                        message: "add_requires_argv requires at least one argument".to_string(),
                    });
                    continue;
                }
                validated
                    .requires_argv
                    .insert(surface_id.to_string(), requires_argv.clone());
                result.valid_count += 1;
            }

            LmAction::UpdateBaseline {
                scenario_id,
                baseline_scenario_id,
            } => {
                let scenario_id = scenario_id.trim();
                let baseline_scenario_id = baseline_scenario_id.trim();
                if scenario_id.is_empty() {
                    result.errors.push(ValidationError {
                        surface_id: surface_id.to_string(),
                        message: "update_baseline requires non-empty scenario_id".to_string(),
                    });
                    continue;
                }
                if baseline_scenario_id.is_empty() {
                    result.errors.push(ValidationError {
                        surface_id: surface_id.to_string(),
                        message: "update_baseline requires non-empty baseline_scenario_id"
                            .to_string(),
                    });
                    continue;
                }
                validated
                    .baseline_updates
                    .insert(scenario_id.to_string(), baseline_scenario_id.to_string());
                result.valid_count += 1;
            }

            LmAction::AddExclusion { reason_code, note } => {
                let note = note.trim();
                if note.is_empty() {
                    result.errors.push(ValidationError {
                        surface_id: surface_id.to_string(),
                        message: "add_exclusion requires non-empty note".to_string(),
                    });
                    continue;
                }
                if note.chars().count() > 200 {
                    result.errors.push(ValidationError {
                        surface_id: surface_id.to_string(),
                        message: "add_exclusion note must be <= 200 characters".to_string(),
                    });
                    continue;
                }
                validated.exclusions.insert(
                    surface_id.to_string(),
                    (reason_code.clone(), note.to_string()),
                );
                result.valid_count += 1;
            }

            LmAction::Skip { reason } => {
                validated
                    .skipped
                    .insert(surface_id.to_string(), reason.clone());
                result.skipped_count += 1;
            }
        }
    }

    (validated, result)
}

/// Validate a scenario specification.
fn validate_scenario(scenario: &ScenarioSpec, surface_id: &str) -> Result<()> {
    if scenario.id.trim().is_empty() {
        return Err(anyhow!("scenario id must not be empty"));
    }

    if scenario.argv.is_empty() {
        return Err(anyhow!("scenario argv must not be empty"));
    }

    // Check that the scenario covers the surface_id
    if !scenario.covers.iter().any(|c| c == surface_id) {
        return Err(anyhow!(
            "scenario covers list must include '{}' (got {:?})",
            surface_id,
            scenario.covers
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn make_scenario(id: &str, surface_id: &str) -> ScenarioSpec {
        ScenarioSpec {
            id: id.to_string(),
            kind: crate::scenarios::ScenarioKind::Behavior,
            publish: false,
            argv: vec![surface_id.to_string(), "work".to_string()],
            env: BTreeMap::new(),
            seed_dir: None,
            seed: None,
            cwd: None,
            timeout_seconds: None,
            net_mode: None,
            no_sandbox: None,
            no_strace: None,
            snippet_max_lines: None,
            snippet_max_bytes: None,
            coverage_tier: Some("behavior".to_string()),
            baseline_scenario_id: Some("baseline".to_string()),
            assertions: Vec::new(),
            covers: vec![surface_id.to_string()],
            coverage_ignore: false,
            expect: crate::scenarios::ScenarioExpect::default(),
        }
    }

    #[test]
    fn test_validate_add_scenario() {
        let surface_ids: std::collections::BTreeSet<String> =
            ["--verbose".to_string()].into_iter().collect();

        let batch = LmResponseBatch {
            schema_version: 1,
            responses: vec![LmDecisionResponse {
                surface_id: "--verbose".to_string(),
                action: LmAction::AddScenario {
                    scenario: Box::new(make_scenario("verify_--verbose", "--verbose")),
                },
            }],
        };

        let (validated, result) = validate_responses(&batch, &surface_ids);

        assert_eq!(result.valid_count, 1);
        assert_eq!(result.errors.len(), 0);
        assert_eq!(validated.scenarios_to_upsert.len(), 1);
    }

    #[test]
    fn test_validate_unknown_surface_id() {
        let surface_ids: std::collections::BTreeSet<String> =
            ["--verbose".to_string()].into_iter().collect();

        let batch = LmResponseBatch {
            schema_version: 1,
            responses: vec![LmDecisionResponse {
                surface_id: "--unknown".to_string(),
                action: LmAction::Skip {
                    reason: "test".to_string(),
                },
            }],
        };

        let (_, result) = validate_responses(&batch, &surface_ids);

        assert_eq!(result.valid_count, 0);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].message.contains("not found"));
    }

    #[test]
    fn test_validate_add_value_examples() {
        let surface_ids: std::collections::BTreeSet<String> =
            ["--color".to_string()].into_iter().collect();

        let batch = LmResponseBatch {
            schema_version: 1,
            responses: vec![LmDecisionResponse {
                surface_id: "--color".to_string(),
                action: LmAction::AddValueExamples {
                    value_examples: vec![
                        "always".to_string(),
                        "never".to_string(),
                        "auto".to_string(),
                    ],
                },
            }],
        };

        let (validated, result) = validate_responses(&batch, &surface_ids);

        assert_eq!(result.valid_count, 1);
        assert_eq!(validated.value_examples.get("--color").unwrap().len(), 3);
    }

    #[test]
    fn test_validate_add_exclusion() {
        let surface_ids: std::collections::BTreeSet<String> =
            ["--escape".to_string()].into_iter().collect();

        let batch = LmResponseBatch {
            schema_version: 1,
            responses: vec![LmDecisionResponse {
                surface_id: "--escape".to_string(),
                action: LmAction::AddExclusion {
                    reason_code: ExclusionReasonCode::FixtureGap,
                    note: "requires control characters which JSON seed cannot represent"
                        .to_string(),
                },
            }],
        };

        let (validated, result) = validate_responses(&batch, &surface_ids);

        assert_eq!(result.valid_count, 1);
        assert!(validated.exclusions.contains_key("--escape"));
    }

    #[test]
    fn test_scenario_must_cover_surface_id() {
        let surface_ids: std::collections::BTreeSet<String> =
            ["--verbose".to_string()].into_iter().collect();

        let mut scenario = make_scenario("verify_--verbose", "--verbose");
        scenario.covers = vec!["--other".to_string()]; // Wrong coverage

        let batch = LmResponseBatch {
            schema_version: 1,
            responses: vec![LmDecisionResponse {
                surface_id: "--verbose".to_string(),
                action: LmAction::AddScenario { scenario: Box::new(scenario) },
            }],
        };

        let (_, result) = validate_responses(&batch, &surface_ids);

        assert_eq!(result.valid_count, 0);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0]
            .message
            .contains("covers list must include"));
    }

    #[test]
    fn test_parse_lm_response_json() {
        let json = r#"{
            "schema_version": 1,
            "responses": [
                {
                    "surface_id": "--verbose",
                    "action": {
                        "kind": "add_value_examples",
                        "value_examples": ["1", "2", "3"]
                    }
                },
                {
                    "surface_id": "--quiet",
                    "action": {
                        "kind": "skip",
                        "reason": "need more context"
                    }
                }
            ]
        }"#;

        let batch: LmResponseBatch = serde_json::from_str(json).unwrap();
        assert_eq!(batch.responses.len(), 2);
    }

    #[test]
    fn test_validate_update_baseline() {
        let surface_ids: std::collections::BTreeSet<String> =
            ["--color".to_string()].into_iter().collect();

        let batch = LmResponseBatch {
            schema_version: 1,
            responses: vec![LmDecisionResponse {
                surface_id: "--color".to_string(),
                action: LmAction::UpdateBaseline {
                    scenario_id: "verify_--color".to_string(),
                    baseline_scenario_id: "help_color_context".to_string(),
                },
            }],
        };

        let (validated, result) = validate_responses(&batch, &surface_ids);

        assert_eq!(result.valid_count, 1);
        assert_eq!(result.errors.len(), 0);
        assert_eq!(
            validated.baseline_updates.get("verify_--color"),
            Some(&"help_color_context".to_string())
        );
    }

    #[test]
    fn test_validate_update_baseline_rejects_empty_ids() {
        let surface_ids: std::collections::BTreeSet<String> =
            ["--color".to_string()].into_iter().collect();

        let batch = LmResponseBatch {
            schema_version: 1,
            responses: vec![LmDecisionResponse {
                surface_id: "--color".to_string(),
                action: LmAction::UpdateBaseline {
                    scenario_id: "  ".to_string(),
                    baseline_scenario_id: "help".to_string(),
                },
            }],
        };

        let (_, result) = validate_responses(&batch, &surface_ids);

        assert_eq!(result.valid_count, 0);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].message.contains("non-empty scenario_id"));
    }
}
