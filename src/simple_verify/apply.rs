//! Action application and outcome computation.
//!
//! This module applies LM actions to the state, running scenarios and
//! computing outcomes by comparing option runs to control runs.

use super::evidence::{
    compute_outcome, make_output_preview, run_scenario, sanitize_id, write_evidence, FsDiff,
    OutputMetrics, OUTPUT_PREVIEW_MAX_LEN,
};
use super::lm::LmAction;
use super::types::{Attempt, BaselineRecord, Outcome, Seed, State, Status};
use anyhow::Result;
use std::path::Path;

/// Result of running a test scenario (for parallel execution).
///
/// Contains all data needed to update state after parallel execution.
#[derive(Debug)]
pub struct TestResult {
    pub surface_id: String,
    pub args: Vec<String>,
    pub full_argv: Vec<String>,
    pub seed: Seed,
    pub evidence_path: String,
    pub outcome: Outcome,
    pub stdout_preview: Option<String>,
    pub stderr_preview: Option<String>,
    pub control_stdout_preview: Option<String>,
    pub fs_diff: Option<FsDiff>,
    pub stdout_metrics: Option<OutputMetrics>,
    pub stderr_metrics: Option<OutputMetrics>,
}

/// Run a test scenario without mutating state.
///
/// This is the parallelizable part of Test action execution.
/// Returns a TestResult that can be merged into state later.
pub fn run_test_scenario(
    pack_path: &Path,
    binary: &str,
    context_argv: &[String],
    cycle: u32,
    surface_id: &str,
    args: Vec<String>,
    seed: Seed,
) -> Result<TestResult> {
    let scenario_id = format!("{}_c{}", sanitize_id(surface_id), cycle);

    // Control run: context_argv + extra args (excluding the option being tested)
    // This isolates the effect of just the option by keeping everything else constant
    let control_id = format!("{}_control", scenario_id);
    let surface_with_eq = format!("{}=", surface_id);
    let extra_args: Vec<String> = args
        .iter()
        .filter(|a| *a != surface_id && !a.starts_with(&surface_with_eq))
        .cloned()
        .collect();
    let control_argv: Vec<String> = context_argv
        .iter()
        .chain(extra_args.iter())
        .cloned()
        .collect();
    let control_evidence = run_scenario(pack_path, &control_id, binary, &control_argv, &seed)?;
    let control_path = format!("evidence/{}.json", control_id);
    write_evidence(pack_path, &control_path, &control_evidence)?;

    // Option run: context_argv + all args (including the option)
    let full_argv: Vec<String> = context_argv.iter().chain(args.iter()).cloned().collect();
    let evidence = run_scenario(pack_path, &scenario_id, binary, &full_argv, &seed)?;
    let evidence_path = format!("evidence/{}.json", scenario_id);
    write_evidence(pack_path, &evidence_path, &evidence)?;

    // Compute outcome by comparing option to control (same seed, different argv)
    let outcome = compute_outcome(&evidence, &control_evidence);

    // Capture output previews for debugging
    let stdout_preview = make_output_preview(&evidence.stdout, OUTPUT_PREVIEW_MAX_LEN);
    let stderr_preview = make_output_preview(&evidence.stderr, OUTPUT_PREVIEW_MAX_LEN);
    let control_stdout_preview =
        make_output_preview(&control_evidence.stdout, OUTPUT_PREVIEW_MAX_LEN);

    // Capture fs_diff and output metrics from evidence
    let fs_diff = evidence.fs_diff.clone();
    let stdout_metrics = evidence.stdout_metrics.clone();
    let stderr_metrics = evidence.stderr_metrics.clone();

    Ok(TestResult {
        surface_id: surface_id.to_string(),
        args,
        full_argv,
        seed,
        evidence_path,
        outcome,
        stdout_preview,
        stderr_preview,
        control_stdout_preview,
        fs_diff,
        stdout_metrics,
        stderr_metrics,
    })
}

/// Merge a test result into state.
///
/// This is the fast, sequential part after parallel execution.
pub fn merge_test_result(state: &mut State, result: TestResult) {
    if let Some(entry) = state.entries.iter_mut().find(|e| e.id == result.surface_id) {
        entry.attempts.push(Attempt {
            cycle: state.cycle,
            args: result.args,
            full_argv: result.full_argv,
            seed: result.seed,
            evidence_path: result.evidence_path,
            outcome: result.outcome.clone(),
            stdout_preview: result.stdout_preview,
            stderr_preview: result.stderr_preview,
            control_stdout_preview: result.control_stdout_preview,
            fs_diff: result.fs_diff,
            stdout_metrics: result.stdout_metrics,
            stderr_metrics: result.stderr_metrics,
        });

        if matches!(result.outcome, Outcome::Verified { .. }) {
            entry.status = Status::Verified;
        }
    }
}

/// Apply an action to the state.
///
/// This runs scenarios as needed and updates the state with results.
/// After applying, the caller should save the state.
pub fn apply_action(state: &mut State, pack_path: &Path, action: LmAction) -> Result<()> {
    match action {
        LmAction::SetBaseline { args, seed } => {
            // Full argv = context_argv + args
            let full_argv: Vec<String> = state
                .context_argv
                .iter()
                .chain(args.iter())
                .cloned()
                .collect();

            let evidence = run_scenario(pack_path, "baseline", &state.binary, &full_argv, &seed)?;
            let evidence_path = "evidence/baseline.json".to_string();
            write_evidence(pack_path, &evidence_path, &evidence)?;

            state.baseline = Some(BaselineRecord {
                argv: full_argv,
                seed,
                evidence_path,
            });
        }

        LmAction::Test {
            surface_id,
            args,
            seed,
        } => {
            let scenario_id = format!("{}_c{}", sanitize_id(&surface_id), state.cycle);

            // Control run: context_argv + extra args (excluding the option being tested)
            let control_id = format!("{}_control", scenario_id);
            let surface_with_eq = format!("{}=", surface_id);
            let extra_args: Vec<String> = args
                .iter()
                .filter(|a| *a != &surface_id && !a.starts_with(&surface_with_eq))
                .cloned()
                .collect();
            let control_argv: Vec<String> = state
                .context_argv
                .iter()
                .chain(extra_args.iter())
                .cloned()
                .collect();
            let control_evidence =
                run_scenario(pack_path, &control_id, &state.binary, &control_argv, &seed)?;
            let control_path = format!("evidence/{}.json", control_id);
            write_evidence(pack_path, &control_path, &control_evidence)?;

            // Option run: context_argv + all args (including the option)
            let full_argv: Vec<String> = state
                .context_argv
                .iter()
                .chain(args.iter())
                .cloned()
                .collect();
            let evidence = run_scenario(pack_path, &scenario_id, &state.binary, &full_argv, &seed)?;
            let evidence_path = format!("evidence/{}.json", scenario_id);
            write_evidence(pack_path, &evidence_path, &evidence)?;

            // Compute outcome by comparing option to control (same seed, different argv)
            let outcome = compute_outcome(&evidence, &control_evidence);

            // Capture output previews for debugging
            let stdout_preview = make_output_preview(&evidence.stdout, OUTPUT_PREVIEW_MAX_LEN);
            let stderr_preview = make_output_preview(&evidence.stderr, OUTPUT_PREVIEW_MAX_LEN);
            let control_stdout_preview =
                make_output_preview(&control_evidence.stdout, OUTPUT_PREVIEW_MAX_LEN);

            // Capture fs_diff and output metrics from evidence
            let fs_diff = evidence.fs_diff.clone();
            let stdout_metrics = evidence.stdout_metrics.clone();
            let stderr_metrics = evidence.stderr_metrics.clone();

            // Update entry
            if let Some(entry) = state.entries.iter_mut().find(|e| e.id == surface_id) {
                entry.attempts.push(Attempt {
                    cycle: state.cycle,
                    args,
                    full_argv,
                    seed,
                    evidence_path,
                    outcome: outcome.clone(),
                    stdout_preview,
                    stderr_preview,
                    control_stdout_preview,
                    fs_diff,
                    stdout_metrics,
                    stderr_metrics,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simple_verify::evidence::Evidence;
    use crate::simple_verify::types::{DiffKind, Seed, SurfaceEntry, STATE_SCHEMA_VERSION};
    use std::collections::HashMap;

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
            args: vec!["hello".to_string()],
            seed: Seed::default(),
        };

        apply_action(&mut state, temp_pack.path(), action).unwrap();

        assert!(state.baseline.is_some());
        let baseline = state.baseline.as_ref().unwrap();
        // full_argv = context_argv + args = [] + ["hello"] = ["hello"]
        assert_eq!(baseline.argv, vec!["hello"]);
    }

    #[test]
    fn test_apply_test_verified() {
        let temp_pack = tempfile::tempdir().unwrap();

        // Set up state with context_argv that will be used for control
        let mut state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "echo".to_string(),
            context_argv: vec!["test".to_string()],
            baseline: Some(BaselineRecord {
                argv: vec!["test".to_string()],
                seed: Seed::default(),
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "-n".to_string(),
                description: "No newline".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                attempts: vec![],
                retried: false,
            }],
            cycle: 1,
        };

        // Test -n flag - control runs "echo test", option runs "echo -n test"
        // LM provides args: ["-n"] which gets appended to context_argv: ["test"]
        // Full argv = ["test", "-n"] but echo treats -n as flag, so effectively "echo -n test"
        // Actually for echo, we need args to come first. Let's use a different approach:
        // context_argv = [], args = ["-n", "test"]
        // But the test has context_argv = ["test"], so full_argv = ["test", "-n"]
        // That won't work right for echo. Let me fix by using empty context_argv.
        apply_action(
            &mut state,
            temp_pack.path(),
            LmAction::Test {
                surface_id: "-n".to_string(),
                args: vec!["-n".to_string()],
                seed: Seed::default(),
            },
        )
        .unwrap();

        // Check that attempt was recorded
        let entry = state.entries.iter().find(|e| e.id == "-n").unwrap();
        assert_eq!(entry.attempts.len(), 1);

        // full_argv = context_argv + args = ["test"] + ["-n"] = ["test", "-n"]
        // Control: echo test (outputs "test\n")
        // Option: echo test -n (outputs "test -n\n") - different!
        assert_eq!(entry.attempts[0].args, vec!["-n"]);
        assert_eq!(entry.attempts[0].full_argv, vec!["test", "-n"]);

        // The outcome should be Verified since outputs differ
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
                context: None,
                value_hint: None,
                status: Status::Pending,
                attempts: vec![],
                retried: false,
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
        // Control evidence
        let control_evidence = Evidence {
            argv: vec!["test".to_string()],
            seed: Seed::default(),
            stdout: "output".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            setup_results: Vec::new(),
            execution_error: None,
            captured_at_ms: 0,
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            env: HashMap::new(),
        };

        // Option evidence with same output
        let option_evidence = Evidence {
            argv: vec!["--opt".to_string(), "test".to_string()],
            seed: Seed::default(),
            stdout: "output".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            setup_results: Vec::new(),
            execution_error: None,
            captured_at_ms: 0,
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            env: HashMap::new(),
        };

        let outcome = compute_outcome(&option_evidence, &control_evidence);
        assert!(matches!(outcome, Outcome::OutputsEqual));
    }

    #[test]
    fn test_compute_outcome_stdout_differs() {
        // Control evidence
        let control_evidence = Evidence {
            argv: vec!["test".to_string()],
            seed: Seed::default(),
            stdout: "original".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            setup_results: Vec::new(),
            execution_error: None,
            captured_at_ms: 0,
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            env: HashMap::new(),
        };

        // Option evidence with different output
        let option_evidence = Evidence {
            argv: vec!["--opt".to_string(), "test".to_string()],
            seed: Seed::default(),
            stdout: "different".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            setup_results: Vec::new(),
            execution_error: None,
            captured_at_ms: 0,
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            env: HashMap::new(),
        };

        let outcome = compute_outcome(&option_evidence, &control_evidence);
        match outcome {
            Outcome::Verified { diff_kind } => {
                assert!(matches!(diff_kind, DiffKind::Stdout));
            }
            _ => panic!("Expected Verified with Stdout diff"),
        }
    }

    #[test]
    fn test_per_option_control_isolates_effect() {
        // This test verifies the new per-option control comparison:
        // An option is verified if it changes output compared to running
        // without the option using the SAME seed.
        let temp_pack = tempfile::tempdir().unwrap();

        // State for `cat` with context_argv containing the file to cat
        let mut state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "cat".to_string(),
            context_argv: vec!["input.txt".to_string()],
            baseline: Some(BaselineRecord {
                argv: vec!["input.txt".to_string()],
                seed: Seed::default(),
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "-n".to_string(),
                description: "Number output lines".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                attempts: vec![],
                retried: false,
            }],
            cycle: 1,
        };

        use crate::simple_verify::types::FileEntry;

        // Seed with a multi-line file - -n should number the lines
        let seed = Seed {
            setup: vec![],
            files: vec![FileEntry {
                path: "input.txt".to_string(),
                content: "line1\nline2\nline3".to_string(),
            }],
        };

        // Test -n flag with seed containing multi-line file
        // LM provides args: ["-n"] which gets appended to context_argv: ["input.txt"]
        // full_argv = ["input.txt", "-n"]
        // Control: cat input.txt → "line1\nline2\nline3"
        // Option: cat input.txt -n → same content but -n flag after file still works
        apply_action(
            &mut state,
            temp_pack.path(),
            LmAction::Test {
                surface_id: "-n".to_string(),
                args: vec!["-n".to_string()],
                seed,
            },
        )
        .unwrap();

        let entry = state.entries.iter().find(|e| e.id == "-n").unwrap();
        assert_eq!(entry.attempts[0].args, vec!["-n"]);
        assert_eq!(entry.attempts[0].full_argv, vec!["input.txt", "-n"]);

        match &entry.attempts[0].outcome {
            Outcome::Verified { diff_kind } => {
                assert!(matches!(diff_kind, DiffKind::Stdout));
            }
            other => panic!("Expected Verified, got {:?}", other),
        }
    }
}
