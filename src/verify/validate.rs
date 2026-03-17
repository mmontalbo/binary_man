//! Validation for LM actions.
//!
//! Before applying an LM action, we validate it against the current state
//! to catch common mistakes early and provide helpful error messages.
//!
//! Note: The system automatically prepends context_argv to LM-provided args,
//! so we don't need to validate that the LM included the context prefix.

use super::lm::LmAction;
use super::types::{State, Status, MAX_PROBES_PER_SURFACE};

/// Normalize a surface_id that the LM may have formatted incorrectly.
///
/// Handles cases like:
/// - `--option=value` → (`--option`, [`value`, ...extra_args])
/// - `-Uvalue` → (`-U`, [`value`, ...extra_args])
///
/// Returns the (possibly rewritten) surface_id and extra_args.
fn normalize_surface_id(
    surface_id: String,
    extra_args: Vec<String>,
    state: &State,
) -> (String, Vec<String>) {
    // If surface_id exists as-is, no normalization needed
    if state.entries.iter().any(|e| e.id == surface_id) {
        return (surface_id, extra_args);
    }

    // Try to normalize --option=value format
    if let Some((base, value)) = surface_id.split_once('=') {
        if state.entries.iter().any(|e| e.id == base) {
            let mut new_extra_args = vec![value.to_string()];
            new_extra_args.extend(extra_args);
            return (base.to_string(), new_extra_args);
        }
    }

    // Try to normalize short option with attached value: -Uvalue → -U + value
    if surface_id.starts_with('-') && !surface_id.starts_with("--") && surface_id.len() > 2 {
        let base = &surface_id[..2];
        let value = &surface_id[2..];
        if state.entries.iter().any(|e| e.id == base) {
            let mut new_extra_args = vec![value.to_string()];
            new_extra_args.extend(extra_args);
            return (base.to_string(), new_extra_args);
        }
    }

    // No normalization possible, return as-is (will fail validation)
    (surface_id, extra_args)
}

/// Normalize an action to handle common LM formatting issues.
pub(super) fn normalize_action(action: LmAction, state: &State) -> LmAction {
    match action {
        LmAction::Test {
            surface_id,
            extra_args,
            seed,
            prediction,
        } => {
            let (surface_id, extra_args) = normalize_surface_id(surface_id, extra_args, state);
            LmAction::Test {
                surface_id,
                extra_args,
                seed,
                prediction,
            }
        }
        LmAction::Probe {
            surface_id,
            extra_args,
            seed,
        } => {
            let (surface_id, extra_args) = normalize_surface_id(surface_id, extra_args, state);
            LmAction::Probe {
                surface_id,
                extra_args,
                seed,
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
        LmAction::Probe { surface_id, .. } => {
            match state.entries.iter().find(|e| &e.id == surface_id) {
                None => return Err(format!("Unknown surface: {}", surface_id)),
                Some(entry) if !matches!(entry.status, Status::Pending) => {
                    return Err(format!(
                        "Surface {} is not pending (status: {:?})",
                        surface_id, entry.status
                    ));
                }
                Some(entry) if entry.probes.len() >= MAX_PROBES_PER_SURFACE => {
                    return Err(format!(
                        "Surface {} has exhausted probe budget ({}/{})",
                        surface_id,
                        entry.probes.len(),
                        MAX_PROBES_PER_SURFACE
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
                    probes: vec![],
                    attempts: vec![],
                    category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                    critique_demotions: 0,
                    characterization: None,
                },
                SurfaceEntry {
                    id: "--quiet".to_string(),
                    description: "Be quiet".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Verified,
                    probes: vec![],
                    attempts: vec![],
                    category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                    critique_demotions: 0,
                    characterization: None,
                },
            ],
            cycle: 0,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
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

    #[test]
    fn test_validate_probe_ok() {
        let state = test_state();
        let action = LmAction::Probe {
            surface_id: "--verbose".to_string(),
            extra_args: vec![],
            seed: Seed::default(),
        };
        assert!(validate_action(&action, &state).is_ok());
    }

    #[test]
    fn test_validate_probe_unknown_surface() {
        let state = test_state();
        let action = LmAction::Probe {
            surface_id: "--unknown".to_string(),
            extra_args: vec![],
            seed: Seed::default(),
        };
        let result = validate_action(&action, &state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown surface"));
    }

    #[test]
    fn test_validate_probe_not_pending() {
        let state = test_state();
        let action = LmAction::Probe {
            surface_id: "--quiet".to_string(), // Verified
            extra_args: vec![],
            seed: Seed::default(),
        };
        let result = validate_action(&action, &state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not pending"));
    }

    #[test]
    fn test_validate_probe_budget_exhausted() {
        use super::super::types::ProbeResult;

        let mut state = test_state();
        // Fill up probe budget
        if let Some(entry) = state.entries.iter_mut().find(|e| e.id == "--verbose") {
            for i in 0..MAX_PROBES_PER_SURFACE {
                entry.probes.push(ProbeResult {
                    cycle: i as u32,
                    argv: vec!["--verbose".to_string()],
                    seed: Seed::default(),
                    stdout_preview: None,
                    stderr_preview: None,
                    exit_code: Some(0),
                    control_stdout_preview: None,
                    outputs_differ: false,
                    setup_failed: false,
                    stdout_differs: false,
                    stderr_differs: false,
                    exit_code_differs: false,
                    control_stderr_preview: None,
                    control_exit_code: Some(0),
                    setup_detail: None,
                });
            }
        }

        let action = LmAction::Probe {
            surface_id: "--verbose".to_string(),
            extra_args: vec![],
            seed: Seed::default(),
        };
        let result = validate_action(&action, &state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("probe budget"));
    }
}
