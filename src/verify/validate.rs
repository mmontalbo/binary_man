//! Validation for LM actions.
//!
//! Before applying an LM action, we validate it against the current state
//! to catch common mistakes early and provide helpful error messages.
//!
//! Note: The system automatically prepends context_argv to LM-provided args,
//! so we don't need to validate that the LM included the context prefix.

use super::lm::LmAction;
use super::types::{State, Status};

/// Normalize an action to handle common LM formatting issues.
///
/// Handles cases like:
/// - `--option=value` → surface_id: `--option`, extra_args: [`value`]
/// - `-Uvalue` → surface_id: `-U`, extra_args: [`value`] (short option with attached value)
pub(super) fn normalize_action(action: LmAction, state: &State) -> LmAction {
    match action {
        LmAction::Test {
            surface_id,
            extra_args,
            seed,
            prediction,
        } => {
            // If surface_id exists as-is, no normalization needed
            if state.entries.iter().any(|e| e.id == surface_id) {
                return LmAction::Test {
                    surface_id,
                    extra_args,
                    seed,
                    prediction,
                };
            }

            // Try to normalize --option=value format
            if let Some((base, value)) = surface_id.split_once('=') {
                if state.entries.iter().any(|e| e.id == base) {
                    let mut new_extra_args = vec![value.to_string()];
                    new_extra_args.extend(extra_args);
                    return LmAction::Test {
                        surface_id: base.to_string(),
                        extra_args: new_extra_args,
                        seed,
                        prediction,
                    };
                }
            }

            // Try to normalize short option with attached value: -Uvalue → -U + value
            // Handles -U10, -Spattern, etc.
            if surface_id.starts_with('-')
                && !surface_id.starts_with("--")
                && surface_id.len() > 2
            {
                let base = &surface_id[..2]; // -U, -S, etc.
                let value = &surface_id[2..]; // 10, pattern, etc.
                if state.entries.iter().any(|e| e.id == base) {
                    let mut new_extra_args = vec![value.to_string()];
                    new_extra_args.extend(extra_args);
                    return LmAction::Test {
                        surface_id: base.to_string(),
                        extra_args: new_extra_args,
                        seed,
                        prediction,
                    };
                }
            }

            // No normalization possible, return as-is (will fail validation)
            LmAction::Test {
                surface_id,
                extra_args,
                seed,
                prediction,
            }
        }
        other => other,
    }
}

/// Validate an action against the current state.
///
/// Returns `Ok(())` if the action is valid, or `Err` with a description
/// of what's wrong.
pub(super) fn validate_action(action: &LmAction, state: &State) -> Result<(), String> {
    match action {
        LmAction::SetBaseline { .. } => {
            if state.baseline.is_some() {
                return Err("Baseline already exists".to_string());
            }
        }
        LmAction::Test { surface_id, .. } => {
            match state.entries.iter().find(|e| &e.id == surface_id) {
                None => return Err(format!("Unknown surface: {}", surface_id)),
                Some(entry) if !matches!(entry.status, Status::Pending) => {
                    return Err(format!(
                        "Surface {} is not pending (status: {:?})",
                        surface_id, entry.status
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::types::{
        BaselineRecord, Seed, SurfaceCategory, SurfaceEntry, STATE_SCHEMA_VERSION,
    };

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
                category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                },
                SurfaceEntry {
                    id: "--quiet".to_string(),
                    description: "Be quiet".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Verified,
                    attempts: vec![],
                category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                },
            ],
            cycle: 0,
            seed_bank: vec![],
            help_preamble: String::new(),
        }
    }

    #[test]
    fn test_validate_set_baseline_ok() {
        let state = test_state();
        let action = LmAction::SetBaseline {
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
            seed: Seed::default(),
        };
        let result = validate_action(&action, &state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));
    }

    #[test]
    fn test_validate_set_baseline_empty_seed_ok() {
        // Empty seed is valid - baseline just runs context_argv
        let state = test_state();
        let action = LmAction::SetBaseline {
            seed: Seed::default(),
        };
        assert!(validate_action(&action, &state).is_ok());
    }

    #[test]
    fn test_validate_test_ok() {
        let state = test_state();
        let action = LmAction::Test {
            surface_id: "--verbose".to_string(),
            extra_args: vec![],
            seed: Seed::default(),
            prediction: None,
        };
        assert!(validate_action(&action, &state).is_ok());
    }

    #[test]
    fn test_validate_test_unknown_surface() {
        let state = test_state();
        let action = LmAction::Test {
            surface_id: "--unknown".to_string(),
            extra_args: vec![],
            seed: Seed::default(),
            prediction: None,
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
            extra_args: vec![],
            seed: Seed::default(),
            prediction: None,
        };
        let result = validate_action(&action, &state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not pending"));
    }

    #[test]
    fn test_validate_test_empty_extra_args_ok() {
        // Empty extra_args is valid - surface_id is auto-included
        let state = test_state();
        let action = LmAction::Test {
            surface_id: "--verbose".to_string(),
            extra_args: vec![],
            seed: Seed::default(),
            prediction: None,
        };
        let result = validate_action(&action, &state);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_with_context_argv() {
        // With the new interface, context_argv is handled by the system
        // LM just provides surface_id, system auto-includes it
        let mut state = test_state();
        state.context_argv = vec!["diff".to_string()];

        // LM just provides the option, system auto-includes it
        let action = LmAction::Test {
            surface_id: "--verbose".to_string(),
            extra_args: vec![],
            seed: Seed::default(),
            prediction: None,
        };
        assert!(validate_action(&action, &state).is_ok());
    }

    #[test]
    fn test_validate_baseline_with_context() {
        let mut state = test_state();
        state.context_argv = vec!["diff".to_string()];

        // Baseline is valid - system uses context_argv
        let action = LmAction::SetBaseline {
            seed: Seed::default(),
        };
        assert!(validate_action(&action, &state).is_ok());
    }
}
