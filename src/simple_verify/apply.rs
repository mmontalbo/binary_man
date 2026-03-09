//! Action application and outcome computation.
//!
//! This module applies LM actions to the state, running scenarios and
//! computing outcomes by comparing outputs to the baseline.

use super::evidence::{load_evidence, run_scenario, truncate_str, write_evidence, Evidence};
use super::lm::LmAction;
use super::types::{Attempt, BaselineRecord, DiffKind, Outcome, State, Status};
use anyhow::{Context, Result};
use std::path::Path;

/// Apply an action to the state.
///
/// This runs scenarios as needed and updates the state with results.
/// After applying, the caller should save the state.
pub fn apply_action(state: &mut State, pack_path: &Path, action: LmAction) -> Result<()> {
    match action {
        LmAction::SetBaseline { argv, seed } => {
            let evidence = run_scenario(pack_path, "baseline", &state.binary, &argv, &seed)?;
            let evidence_path = "evidence/baseline.json".to_string();
            write_evidence(pack_path, &evidence_path, &evidence)?;

            state.baseline = Some(BaselineRecord {
                argv,
                seed,
                evidence_path,
            });
        }

        LmAction::Test {
            surface_id,
            argv,
            seed,
        } => {
            let scenario_id = format!("{}_c{}", sanitize_id(&surface_id), state.cycle);
            let evidence = run_scenario(pack_path, &scenario_id, &state.binary, &argv, &seed)?;
            let evidence_path = format!("evidence/{}.json", scenario_id);
            write_evidence(pack_path, &evidence_path, &evidence)?;

            // Compute outcome by comparing to baseline
            let outcome = compute_outcome(&evidence, state.baseline.as_ref(), pack_path)?;

            // Update entry
            if let Some(entry) = state.entries.iter_mut().find(|e| e.id == surface_id) {
                entry.attempts.push(Attempt {
                    cycle: state.cycle,
                    argv,
                    seed,
                    evidence_path,
                    outcome: outcome.clone(),
                });

                if matches!(outcome, Outcome::Verified { .. }) {
                    entry.status = Status::Verified;
                }
            }
        }

        LmAction::Exclude { surface_id, reason } => {
            if let Some(entry) = state.entries.iter_mut().find(|e| e.id == surface_id) {
                entry.status = Status::Excluded { reason };
            }
        }
    }
    Ok(())
}

/// Sanitize a surface ID for use in filenames.
fn sanitize_id(id: &str) -> String {
    // Leading dashes are common in option names but problematic in filenames
    let trimmed = id.trim_start_matches('-');
    trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Compute the outcome by comparing evidence to baseline.
fn compute_outcome(
    evidence: &Evidence,
    baseline: Option<&BaselineRecord>,
    pack_path: &Path,
) -> Result<Outcome> {
    // Handle execution errors
    if let Some(error) = &evidence.execution_error {
        return Ok(Outcome::ExecutionError {
            error: error.clone(),
        });
    }

    // Handle setup failures
    if evidence.setup_failed {
        return Ok(Outcome::SetupFailed {
            hint: truncate_str(&evidence.stderr, 200),
        });
    }

    // Handle crashes (non-zero exit with no stdout)
    if let Some(exit_code) = evidence.exit_code {
        if exit_code != 0 && evidence.stdout.is_empty() {
            return Ok(Outcome::Crashed {
                hint: format!(
                    "exit={}, stderr: {}",
                    exit_code,
                    truncate_str(&evidence.stderr, 150)
                ),
            });
        }
    }

    // Compare to baseline
    let Some(baseline) = baseline else {
        // No baseline to compare - treat as equal (shouldn't happen in practice)
        return Ok(Outcome::OutputsEqual);
    };

    let baseline_evidence = load_evidence(pack_path, &baseline.evidence_path)
        .context("load baseline evidence for comparison")?;

    let stdout_differs = evidence.stdout != baseline_evidence.stdout;
    let stderr_differs = evidence.stderr != baseline_evidence.stderr;
    let exit_differs = evidence.exit_code != baseline_evidence.exit_code;

    if stdout_differs || stderr_differs || exit_differs {
        let diff_kind = match (stdout_differs, stderr_differs, exit_differs) {
            (true, false, false) => DiffKind::Stdout,
            (false, true, false) => DiffKind::Stderr,
            (false, false, true) => DiffKind::ExitCode,
            _ => DiffKind::Multiple,
        };
        Ok(Outcome::Verified { diff_kind })
    } else {
        Ok(Outcome::OutputsEqual)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simple_verify::types::{Seed, SurfaceEntry, STATE_SCHEMA_VERSION};

    #[test]
    fn test_sanitize_id() {
        assert_eq!(sanitize_id("--verbose"), "verbose");
        assert_eq!(sanitize_id("-v"), "v");
        assert_eq!(sanitize_id("--color=always"), "color_always");
        assert_eq!(sanitize_id("normal-id"), "normal-id");
    }

    #[test]
    fn test_apply_set_baseline() {
        let temp_pack = tempfile::tempdir().unwrap();

        let mut state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "echo".to_string(),
            context_argv: vec![],
            baseline: None,
            entries: vec![],
            cycle: 1,
        };

        let action = LmAction::SetBaseline {
            argv: vec!["hello".to_string()],
            seed: Seed::default(),
        };

        apply_action(&mut state, temp_pack.path(), action).unwrap();

        assert!(state.baseline.is_some());
        let baseline = state.baseline.as_ref().unwrap();
        assert_eq!(baseline.argv, vec!["hello"]);
    }

    #[test]
    fn test_apply_test_verified() {
        let temp_pack = tempfile::tempdir().unwrap();

        // First set up baseline
        let mut state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "echo".to_string(),
            context_argv: vec![],
            baseline: None,
            entries: vec![SurfaceEntry {
                id: "-n".to_string(),
                description: "No newline".to_string(),
                value_hint: None,
                status: Status::Pending,
                attempts: vec![],
            }],
            cycle: 1,
        };

        // Set baseline
        apply_action(
            &mut state,
            temp_pack.path(),
            LmAction::SetBaseline {
                argv: vec!["test".to_string()],
                seed: Seed::default(),
            },
        )
        .unwrap();

        // Test -n flag (should produce different output - no newline)
        apply_action(
            &mut state,
            temp_pack.path(),
            LmAction::Test {
                surface_id: "-n".to_string(),
                argv: vec!["-n".to_string(), "test".to_string()],
                seed: Seed::default(),
            },
        )
        .unwrap();

        // Check that attempt was recorded
        let entry = state.entries.iter().find(|e| e.id == "-n").unwrap();
        assert_eq!(entry.attempts.len(), 1);

        // The outcome should be Verified since echo -n produces different output
        // (no trailing newline)
        match &entry.attempts[0].outcome {
            Outcome::Verified { .. } => {}
            other => panic!("Expected Verified, got {:?}", other),
        }
        assert!(matches!(entry.status, Status::Verified));
    }

    #[test]
    fn test_apply_exclude() {
        let temp_pack = tempfile::tempdir().unwrap();

        let mut state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "test".to_string(),
            context_argv: vec![],
            baseline: None,
            entries: vec![SurfaceEntry {
                id: "--special".to_string(),
                description: "Special option".to_string(),
                value_hint: None,
                status: Status::Pending,
                attempts: vec![],
            }],
            cycle: 1,
        };

        apply_action(
            &mut state,
            temp_pack.path(),
            LmAction::Exclude {
                surface_id: "--special".to_string(),
                reason: "Requires root".to_string(),
            },
        )
        .unwrap();

        let entry = state.entries.iter().find(|e| e.id == "--special").unwrap();
        match &entry.status {
            Status::Excluded { reason } => {
                assert_eq!(reason, "Requires root");
            }
            _ => panic!("Expected Excluded status"),
        }
    }

    #[test]
    fn test_compute_outcome_outputs_equal() {
        let temp_pack = tempfile::tempdir().unwrap();

        // Write baseline evidence
        let baseline_evidence = Evidence {
            argv: vec!["test".to_string()],
            seed: Seed::default(),
            stdout: "output".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            execution_error: None,
            captured_at_ms: 0,
        };
        write_evidence(
            temp_pack.path(),
            "evidence/baseline.json",
            &baseline_evidence,
        )
        .unwrap();

        // Test evidence with same output
        let test_evidence = Evidence {
            argv: vec!["test".to_string()],
            seed: Seed::default(),
            stdout: "output".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            execution_error: None,
            captured_at_ms: 0,
        };

        let baseline_record = BaselineRecord {
            argv: vec!["test".to_string()],
            seed: Seed::default(),
            evidence_path: "evidence/baseline.json".to_string(),
        };

        let outcome =
            compute_outcome(&test_evidence, Some(&baseline_record), temp_pack.path()).unwrap();
        assert!(matches!(outcome, Outcome::OutputsEqual));
    }

    #[test]
    fn test_compute_outcome_stdout_differs() {
        let temp_pack = tempfile::tempdir().unwrap();

        // Write baseline evidence
        let baseline_evidence = Evidence {
            argv: vec!["test".to_string()],
            seed: Seed::default(),
            stdout: "original".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            execution_error: None,
            captured_at_ms: 0,
        };
        write_evidence(
            temp_pack.path(),
            "evidence/baseline.json",
            &baseline_evidence,
        )
        .unwrap();

        // Test evidence with different output
        let test_evidence = Evidence {
            argv: vec!["test".to_string()],
            seed: Seed::default(),
            stdout: "different".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            execution_error: None,
            captured_at_ms: 0,
        };

        let baseline_record = BaselineRecord {
            argv: vec!["test".to_string()],
            seed: Seed::default(),
            evidence_path: "evidence/baseline.json".to_string(),
        };

        let outcome =
            compute_outcome(&test_evidence, Some(&baseline_record), temp_pack.path()).unwrap();
        match outcome {
            Outcome::Verified { diff_kind } => {
                assert!(matches!(diff_kind, DiffKind::Stdout));
            }
            _ => panic!("Expected Verified with Stdout diff"),
        }
    }
}
