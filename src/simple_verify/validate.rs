//! Validation for LM actions.
//!
//! Before applying an LM action, we validate it against the current state
//! to catch common mistakes early and provide helpful error messages.
//!
//! Note: The system automatically prepends context_argv to LM-provided args,
//! so we don't need to validate that the LM included the context prefix.

use super::lm::LmAction;
use super::types::{State, Status};

/// Validate an action against the current state.
///
/// Returns `Ok(())` if the action is valid, or `Err` with a description
/// of what's wrong.
pub fn validate_action(action: &LmAction, state: &State) -> Result<(), String> {
    match action {
        LmAction::SetBaseline { .. } => {
            if state.baseline.is_some() {
                return Err("Baseline already exists".to_string());
            }
            // args can be empty for baseline (just runs context_argv)
        }
        LmAction::Test {
            surface_id, args, ..
        } => {
            // Surface must exist
            if !state.entries.iter().any(|e| &e.id == surface_id) {
                return Err(format!("Unknown surface: {}", surface_id));
            }
            // Surface must be pending
            if let Some(entry) = state.entries.iter().find(|e| &e.id == surface_id) {
                if !matches!(entry.status, Status::Pending) {
                    return Err(format!(
                        "Surface {} is not pending (status: {:?})",
                        surface_id, entry.status
                    ));
                }
            }
            // Test must have args (the option being tested)
            if args.is_empty() {
                return Err(format!("Empty args for surface {}", surface_id));
            }
        }
        LmAction::Exclude { surface_id, reason } => {
            // Surface must exist
            if !state.entries.iter().any(|e| &e.id == surface_id) {
                return Err(format!("Unknown surface: {}", surface_id));
            }
            // Surface must be pending
            if let Some(entry) = state.entries.iter().find(|e| &e.id == surface_id) {
                if !matches!(entry.status, Status::Pending) {
                    return Err(format!(
                        "Surface {} is not pending (status: {:?})",
                        surface_id, entry.status
                    ));
                }
            }
            if reason.is_empty() {
                return Err(format!("Empty exclusion reason for {}", surface_id));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simple_verify::types::{BaselineRecord, Seed, SurfaceEntry, STATE_SCHEMA_VERSION};

    fn test_state() -> State {
        State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "test".to_string(),
            context_argv: vec![],
            baseline: None,
            entries: vec![
                SurfaceEntry {
                    id: "--verbose".to_string(),
                    description: "Be verbose".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Pending,
                    attempts: vec![],
                    retried: false,
                },
                SurfaceEntry {
                    id: "--quiet".to_string(),
                    description: "Be quiet".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Verified,
                    attempts: vec![],
                    retried: false,
                },
            ],
            cycle: 0,
        }
    }

    #[test]
    fn test_validate_set_baseline_ok() {
        let state = test_state();
        let action = LmAction::SetBaseline {
            args: vec!["arg".to_string()],
            seed: Seed::default(),
        };
        assert!(validate_action(&action, &state).is_ok());
    }

    #[test]
    fn test_validate_set_baseline_already_exists() {
        let mut state = test_state();
        state.baseline = Some(BaselineRecord {
            argv: vec![],
            seed: Seed::default(),
            evidence_path: "test".to_string(),
        });

        let action = LmAction::SetBaseline {
            args: vec!["arg".to_string()],
            seed: Seed::default(),
        };
        let result = validate_action(&action, &state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));
    }

    #[test]
    fn test_validate_set_baseline_empty_args_ok() {
        // Empty args is valid - baseline just runs context_argv
        let state = test_state();
        let action = LmAction::SetBaseline {
            args: vec![],
            seed: Seed::default(),
        };
        assert!(validate_action(&action, &state).is_ok());
    }

    #[test]
    fn test_validate_test_ok() {
        let state = test_state();
        let action = LmAction::Test {
            surface_id: "--verbose".to_string(),
            args: vec!["--verbose".to_string()],
            seed: Seed::default(),
        };
        assert!(validate_action(&action, &state).is_ok());
    }

    #[test]
    fn test_validate_test_unknown_surface() {
        let state = test_state();
        let action = LmAction::Test {
            surface_id: "--unknown".to_string(),
            args: vec!["--unknown".to_string()],
            seed: Seed::default(),
        };
        let result = validate_action(&action, &state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown surface"));
    }

    #[test]
    fn test_validate_test_not_pending() {
        let state = test_state();
        let action = LmAction::Test {
            surface_id: "--quiet".to_string(), // This one is Verified
            args: vec!["--quiet".to_string()],
            seed: Seed::default(),
        };
        let result = validate_action(&action, &state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not pending"));
    }

    #[test]
    fn test_validate_test_empty_args() {
        let state = test_state();
        let action = LmAction::Test {
            surface_id: "--verbose".to_string(),
            args: vec![], // Empty args for Test should fail
            seed: Seed::default(),
        };
        let result = validate_action(&action, &state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Empty args"));
    }

    #[test]
    fn test_validate_exclude_ok() {
        let state = test_state();
        let action = LmAction::Exclude {
            surface_id: "--verbose".to_string(),
            reason: "Requires special hardware".to_string(),
        };
        assert!(validate_action(&action, &state).is_ok());
    }

    #[test]
    fn test_validate_exclude_empty_reason() {
        let state = test_state();
        let action = LmAction::Exclude {
            surface_id: "--verbose".to_string(),
            reason: "".to_string(),
        };
        let result = validate_action(&action, &state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Empty exclusion reason"));
    }

    #[test]
    fn test_validate_with_context_argv() {
        // With the new interface, context_argv is handled by the system
        // LM just provides args, which get appended
        let mut state = test_state();
        state.context_argv = vec!["diff".to_string()];

        // LM just provides the option, system prepends context
        let action = LmAction::Test {
            surface_id: "--verbose".to_string(),
            args: vec!["--verbose".to_string()],
            seed: Seed::default(),
        };
        assert!(validate_action(&action, &state).is_ok());
    }

    #[test]
    fn test_validate_baseline_with_context() {
        let mut state = test_state();
        state.context_argv = vec!["diff".to_string()];

        // Baseline with empty args is valid - system uses context_argv
        let action = LmAction::SetBaseline {
            args: vec![],
            seed: Seed::default(),
        };
        assert!(validate_action(&action, &state).is_ok());
    }
}
