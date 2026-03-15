//! Action application and outcome computation.
//!
//! This module applies LM actions to the state, running scenarios and
//! computing outcomes by comparing option runs to control runs.

use super::evidence::{
    compute_outcome, make_output_preview, run_scenario, run_scenario_pair, sanitize_id,
    write_evidence, Evidence, FsDiff, OutputMetrics, OUTPUT_PREVIEW_MAX_LEN,
};
use super::lm::{LmAction, PredictedDiff, Prediction};
use super::types::{
    Attempt, BaselineRecord, Outcome, ProbeResult, Seed, State, Status, VerifiedSeed,
};
use anyhow::Result;
use std::path::Path;

/// Join a surface option with its value argument.
///
/// Many CLI tools require values attached to their option token:
/// - Short: `-U1` not `-U 1` (git treats `1` as a path)
/// - Long: `--unified=1` not `--unified 1`
///
/// When the first extra_arg is a value (doesn't start with `-`),
/// join it to the surface_id. Remaining extra_args stay separate.
fn join_option_value(surface_id: &str, extra_args: Vec<String>) -> Vec<String> {
    if extra_args.is_empty() {
        return vec![surface_id.to_string()];
    }

    let first = &extra_args[0];
    // If first extra_arg starts with '-', it's a flag, not a value
    if first.starts_with('-') {
        let mut args = vec![surface_id.to_string()];
        args.extend(extra_args);
        return args;
    }

    // Join value to the surface_id
    let joined = if surface_id.starts_with("--") {
        format!("{}={}", surface_id, first)
    } else {
        format!("{}{}", surface_id, first)
    };

    let mut args = vec![joined];
    args.extend(extra_args.into_iter().skip(1));
    args
}

/// Result of running a probe scenario (for parallel execution).
///
/// Contains bilateral comparison data — both control and option outputs.
#[derive(Debug)]
pub struct ProbeRunResult {
    pub surface_id: String,
    pub extra_args: Vec<String>,
    pub argv: Vec<String>,
    pub seed: Seed,
    pub stdout_preview: Option<String>,
    pub stderr_preview: Option<String>,
    pub exit_code: Option<u32>,
    pub control_stdout_preview: Option<String>,
    /// Whether control and option outputs differ.
    pub outputs_differ: bool,
    pub setup_failed: bool,
    pub cycle: u32,
}

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
    /// Whether the LM's prediction matched the actual outcome.
    pub prediction_matched: Option<bool>,
}

/// Verify whether a prediction matches the actual outcome.
///
/// Returns true if the predicted diff matches what actually happened.
fn verify_prediction(
    prediction: &Prediction,
    option_evidence: &Evidence,
    control_evidence: &Evidence,
) -> bool {
    match &prediction.diff_type {
        PredictedDiff::StdoutEmpty => {
            // Option output should be empty or significantly shorter than control
            option_evidence.stdout.is_empty()
                || option_evidence.stdout.len() < control_evidence.stdout.len() / 2
        }
        PredictedDiff::StdoutContains(s) => {
            // Option output should contain the specified text
            option_evidence.stdout.contains(s)
        }
        PredictedDiff::StdoutDifferent => {
            // Stdout should differ between control and option
            option_evidence.stdout != control_evidence.stdout
        }
        PredictedDiff::StderrDifferent => {
            // Stderr should differ between control and option
            option_evidence.stderr != control_evidence.stderr
        }
        PredictedDiff::ExitCodeDifferent => {
            // Exit codes should differ
            option_evidence.exit_code != control_evidence.exit_code
        }
    }
}

/// Run a test scenario without mutating state.
///
/// This is the parallelizable part of Test action execution.
/// Returns a TestResult that can be merged into state later.
///
/// IMPORTANT: Control and option commands run in the SAME sandbox to ensure
/// they see identical filesystem state (including git commit hashes). This
/// prevents false positives from timestamp-dependent content like git commits.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_test_scenario(
    pack_path: &Path,
    binary: &str,
    context_argv: &[String],
    cycle: u32,
    surface_id: &str,
    extra_args: Vec<String>,
    seed: Seed,
    with_pty: bool,
    prediction: Option<Prediction>,
) -> Result<TestResult> {
    let scenario_id = format!("{}_c{}", sanitize_id(surface_id), cycle);

    // Construct args: join option + value when needed (e.g. -U1, --unified=1)
    let args = join_option_value(surface_id, extra_args);

    // Control argv: context_argv only (no extra args for control)
    let control_argv: Vec<String> = context_argv.to_vec();

    // Option argv: context_argv + all args
    let full_argv: Vec<String> = context_argv.iter().chain(args.iter()).cloned().collect();

    // Run both control and option in the SAME sandbox
    // This ensures they see identical filesystem state (same git commit hashes, etc.)
    // The sandbox is read-only after setup to catch commands that mutate state
    let (control_evidence, option_evidence) = run_scenario_pair(
        &scenario_id,
        binary,
        &control_argv,
        &full_argv,
        &seed,
        with_pty,
    )?;

    // Write evidence files
    let control_path = format!("evidence/{}_control.json", scenario_id);
    write_evidence(pack_path, &control_path, &control_evidence)?;
    let evidence_path = format!("evidence/{}.json", scenario_id);
    write_evidence(pack_path, &evidence_path, &option_evidence)?;

    // Compute outcome by comparing option to control (same seed, different argv)
    let outcome = compute_outcome(&option_evidence, &control_evidence);

    // Capture output previews for debugging
    let stdout_preview = make_output_preview(&option_evidence.stdout, OUTPUT_PREVIEW_MAX_LEN);
    let stderr_preview = make_output_preview(&option_evidence.stderr, OUTPUT_PREVIEW_MAX_LEN);
    let control_stdout_preview =
        make_output_preview(&control_evidence.stdout, OUTPUT_PREVIEW_MAX_LEN);

    // Capture fs_diff and output metrics from evidence
    let fs_diff = option_evidence.fs_diff.clone();
    let stdout_metrics = option_evidence.stdout_metrics.clone();
    let stderr_metrics = option_evidence.stderr_metrics.clone();

    // Verify prediction if provided
    let prediction_matched = prediction
        .as_ref()
        .map(|p| verify_prediction(p, &option_evidence, &control_evidence));

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
        prediction_matched,
    })
}

/// Merge a test result into state.
///
/// This is the fast, sequential part after parallel execution.
pub(super) fn merge_test_result(state: &mut State, result: TestResult) {
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
            prediction_matched: result.prediction_matched,
        });

        if matches!(result.outcome, Outcome::Verified { .. }) {
            entry.status = Status::Verified;

            // Add to seed bank if not already present
            let seed = entry.attempts.last().map(|a| a.seed.clone()).unwrap();
            if !state
                .seed_bank
                .iter()
                .any(|s| s.surface_id == result.surface_id)
            {
                let args = entry.attempts.last().map(|a| a.args.clone()).unwrap();
                state.seed_bank.push(VerifiedSeed {
                    surface_id: result.surface_id.clone(),
                    args,
                    seed,
                    verified_at: state.cycle,
                    hint: None,
                });
            }
        }
    }
}

/// Run a probe scenario without mutating state.
///
/// Runs BOTH control and option commands in the same sandbox (bilateral
/// comparison). Returns whether outputs differ so the caller can
/// auto-promote to a Test without an extra LM round-trip.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_probe_scenario(
    binary: &str,
    context_argv: &[String],
    cycle: u32,
    surface_id: &str,
    extra_args: Vec<String>,
    seed: Seed,
    with_pty: bool,
) -> Result<ProbeRunResult> {
    let scenario_id = format!("probe_{}_c{}", sanitize_id(surface_id), cycle);

    // Construct args: join option + value when needed
    let args = join_option_value(surface_id, extra_args.clone());

    // Control argv: context_argv only
    let control_argv: Vec<String> = context_argv.to_vec();

    // Full argv: context_argv + all args
    let full_argv: Vec<String> = context_argv.iter().chain(args.iter()).cloned().collect();

    // Run both control and option in the same sandbox (bilateral)
    let (control_evidence, option_evidence) = run_scenario_pair(
        &scenario_id,
        binary,
        &control_argv,
        &full_argv,
        &seed,
        with_pty,
    )?;

    let stdout_preview = make_output_preview(&option_evidence.stdout, OUTPUT_PREVIEW_MAX_LEN);
    let stderr_preview = make_output_preview(&option_evidence.stderr, OUTPUT_PREVIEW_MAX_LEN);
    let control_stdout_preview =
        make_output_preview(&control_evidence.stdout, OUTPUT_PREVIEW_MAX_LEN);

    // Bilateral comparison: do the outputs differ?
    let outputs_differ = option_evidence.stdout != control_evidence.stdout
        || option_evidence.stderr != control_evidence.stderr
        || option_evidence.exit_code != control_evidence.exit_code;

    Ok(ProbeRunResult {
        surface_id: surface_id.to_string(),
        extra_args,
        argv: full_argv,
        seed,
        stdout_preview,
        stderr_preview,
        exit_code: option_evidence.exit_code.map(|c| c as u32),
        control_stdout_preview,
        outputs_differ,
        setup_failed: option_evidence.setup_failed,
        cycle,
    })
}

/// Merge a probe result into state.
pub(super) fn merge_probe_result(state: &mut State, result: ProbeRunResult) {
    if let Some(entry) = state.entries.iter_mut().find(|e| e.id == result.surface_id) {
        entry.probes.push(ProbeResult {
            cycle: result.cycle,
            argv: result.argv,
            seed: result.seed,
            stdout_preview: result.stdout_preview,
            stderr_preview: result.stderr_preview,
            exit_code: result.exit_code,
            control_stdout_preview: result.control_stdout_preview,
            outputs_differ: result.outputs_differ,
            setup_failed: result.setup_failed,
        });
    }
}

/// Apply an action to the state.
///
/// This runs scenarios as needed and updates the state with results.
/// After applying, the caller should save the state.
pub(super) fn apply_action(state: &mut State, pack_path: &Path, action: LmAction) -> Result<()> {
    match action {
        LmAction::SetBaseline { seed } => {
            // Full argv = context_argv (no extra args for baseline)
            let full_argv: Vec<String> = state.context_argv.clone();

            let evidence = run_scenario(
                "baseline",
                &state.binary,
                &full_argv,
                &seed,
                false,
            )?;
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
            extra_args,
            seed,
            prediction,
        } => {
            let result = run_test_scenario(
                pack_path,
                &state.binary,
                &state.context_argv,
                state.cycle,
                &surface_id,
                extra_args,
                seed,
                false,
                prediction,
            )?;
            merge_test_result(state, result);
        }

        LmAction::Probe {
            surface_id,
            extra_args,
            seed,
        } => {
            let result = run_probe_scenario(
                &state.binary,
                &state.context_argv,
                state.cycle,
                &surface_id,
                extra_args,
                seed,
                false,
            )?;
            merge_probe_result(state, result);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::evidence::Evidence;
    use crate::verify::types::{
        DiffKind, Seed, SurfaceCategory, SurfaceEntry, STATE_SCHEMA_VERSION,
    };
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
            context_argv: vec!["hello".to_string()],
            baseline: None,
            entries: vec![],
            cycle: 1,
            seed_bank: vec![],
            help_preamble: String::new(),
        };

        let action = LmAction::SetBaseline {
            seed: Seed::default(),
        };

        apply_action(&mut state, temp_pack.path(), action).unwrap();

        assert!(state.baseline.is_some());
        let baseline = state.baseline.as_ref().unwrap();
        // full_argv = context_argv = ["hello"]
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
                probes: vec![],
                attempts: vec![],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                characterization: None,
            }],
            cycle: 1,
            seed_bank: vec![],
            help_preamble: String::new(),
        };

        // Test -n flag
        // extra_args is empty - surface_id "-n" is auto-included
        // Control: echo test (outputs "test\n")
        // Option: echo test -n (outputs "test -n\n") - different!
        apply_action(
            &mut state,
            temp_pack.path(),
            LmAction::Test {
                surface_id: "-n".to_string(),
                extra_args: vec![],
                seed: Seed::default(),
                prediction: None,
            },
        )
        .unwrap();

        // Check that attempt was recorded
        let entry = state.entries.iter().find(|e| e.id == "-n").unwrap();
        assert_eq!(entry.attempts.len(), 1);

        // args = surface_id + extra_args = ["-n"]
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
            with_pty: false,
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
            with_pty: false,
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
            with_pty: false,
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
            with_pty: false,
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
                probes: vec![],
                attempts: vec![],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                characterization: None,
            }],
            cycle: 1,
            seed_bank: vec![],
            help_preamble: String::new(),
        };

        use crate::verify::types::FileEntry;

        // Seed with a multi-line file - -n should number the lines
        let seed = Seed {
            setup: vec![],
            files: vec![FileEntry {
                path: "input.txt".to_string(),
                content: "line1\nline2\nline3".to_string(),
            }],
        };

        // Test -n flag with seed containing multi-line file
        // surface_id "-n" is auto-included, extra_args is empty
        // full_argv = ["input.txt", "-n"]
        // Control: cat input.txt → "line1\nline2\nline3"
        // Option: cat input.txt -n → same content but -n flag after file still works
        apply_action(
            &mut state,
            temp_pack.path(),
            LmAction::Test {
                surface_id: "-n".to_string(),
                extra_args: vec![],
                seed,
                prediction: None,
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
