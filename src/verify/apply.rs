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
    Attempt, BaselineRecord, DiffKind, Outcome, ProbeResult, Seed, State, Status, VerifiedSeed,
};
use anyhow::Result;
use std::path::Path;

/// Shared context for running test/probe scenarios.
///
/// Bundles the stable parameters that are the same for every scenario
/// within a single verification cycle.
pub(super) struct ScenarioContext<'a> {
    pub pack_path: &'a Path,
    pub binary: &'a str,
    pub context_argv: &'a [String],
    pub cycle: u32,
    pub with_pty: bool,
    /// Required positional args from invocation hint (e.g., ["pattern", "file"] for grep).
    /// Appended to both control and option argv when non-empty.
    pub invocation_args: &'a [String],
}

/// Join a surface option with its value argument.
///
/// Many CLI tools require values attached to their option token:
/// - Short: `-U1` not `-U 1` (git treats `1` as a path)
/// - Long: `--unified=1` not `--unified 1`
///
/// When the first extra_arg is a value (doesn't start with `-`),
/// join it to the surface_id. Remaining extra_args stay separate.
fn join_option_value(
    surface_id: &str,
    extra_args: Vec<String>,
    value_hint: Option<&str>,
) -> Vec<String> {
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

    // Long options: only join with = when we have positive evidence of a value
    // (value_hint from help text parsing). Without it, the option is likely boolean
    // and the extra_arg is a positional argument, not the option's value.
    if surface_id.starts_with("--") {
        if value_hint.is_some() {
            let joined = format!("{}={}", surface_id, first);
            let mut args = vec![joined];
            args.extend(extra_args.into_iter().skip(1));
            return args;
        }
        // No value_hint → keep separate (boolean flag or unknown)
        let mut args = vec![surface_id.to_string()];
        args.extend(extra_args);
        return args;
    }

    // Short options: only concatenate for 2-char surfaces with numeric values
    // e.g., -U3 (git), -j4 (make). Never for -D help, -maxdepth 1, etc.
    if surface_id.len() == 2 && first.chars().all(|c| c.is_ascii_digit()) {
        let joined = format!("{}{}", surface_id, first);
        let mut args = vec![joined];
        args.extend(extra_args.into_iter().skip(1));
        return args;
    }

    // Default: keep separate (multi-char single-dash like -maxdepth, or
    // 2-char with non-numeric value like -D help)
    let mut args = vec![surface_id.to_string()];
    args.extend(extra_args);
    args
}

/// Result of running a probe scenario (for parallel execution).
///
/// Contains bilateral comparison data — both control and option outputs.
#[derive(Debug)]
pub(super) struct ProbeRunResult {
    pub surface_id: String,
    pub extra_args: Vec<String>,
    pub argv: Vec<String>,
    pub seed: Seed,
    pub stdout_preview: Option<String>,
    pub stderr_preview: Option<String>,
    pub exit_code: Option<i32>,
    pub control_stdout_preview: Option<String>,
    /// Whether control and option outputs differ (any channel).
    pub outputs_differ: bool,
    pub setup_failed: bool,
    pub cycle: u32,
    /// Per-channel comparison results.
    pub stdout_differs: bool,
    pub stderr_differs: bool,
    pub exit_code_differs: bool,
    pub control_stderr_preview: Option<String>,
    pub control_exit_code: Option<i32>,
    /// Which setup command failed, if any.
    pub setup_detail: Option<String>,
}

/// Result of running a test scenario (for parallel execution).
///
/// Contains all data needed to update state after parallel execution.
#[derive(Debug)]
pub(super) struct TestResult {
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
    /// The LM's prediction for this attempt (if any).
    pub prediction: Option<Prediction>,
    /// Whether the LM's prediction matched the actual outcome.
    pub prediction_matched: Option<bool>,
    /// Whether the predicted *channel* (stdout/stderr/exitcode) actually changed,
    /// even if the specific prediction content was wrong. Used for telemetry to
    /// distinguish "right channel, wrong content" from "wrong channel entirely".
    pub prediction_channel_matched: Option<bool>,
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

/// Check whether the predicted *channel* actually changed.
///
/// More lenient than `verify_prediction`: only checks if the predicted
/// output channel (stdout, stderr, exit code) differs between control
/// and option, ignoring specific content.
fn verify_prediction_channel(
    prediction: &Prediction,
    option_evidence: &Evidence,
    control_evidence: &Evidence,
) -> bool {
    match &prediction.diff_type {
        PredictedDiff::StdoutEmpty | PredictedDiff::StdoutContains(_) => {
            option_evidence.stdout != control_evidence.stdout
        }
        PredictedDiff::StderrDifferent => {
            option_evidence.stderr != control_evidence.stderr
        }
        PredictedDiff::ExitCodeDifferent => {
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
pub(super) fn run_test_scenario(
    sc: &ScenarioContext<'_>,
    surface_id: &str,
    extra_args: Vec<String>,
    seed: Seed,
    surface_pty: bool,
    prediction: Option<Prediction>,
    value_hint: Option<&str>,
) -> Result<TestResult> {
    let scenario_id = format!("{}_c{}", sanitize_id(surface_id), sc.cycle);

    // Construct args: join option + value when needed (e.g. -U1, --unified=1)
    let args = join_option_value(surface_id, extra_args, value_hint);

    // Augment both control and option with required positional args
    // (e.g., grep needs "pattern file" to produce any output)
    let mut control_argv: Vec<String> = sc.context_argv.to_vec();
    control_argv.extend_from_slice(sc.invocation_args);
    let mut full_argv = control_argv.clone();
    full_argv.extend(args.iter().cloned());

    // Run both control and option in the SAME sandbox
    // This ensures they see identical filesystem state (same git commit hashes, etc.)
    // The sandbox is read-only after setup to catch commands that mutate state
    let (control_evidence, option_evidence) = run_scenario_pair(
        &scenario_id,
        sc.binary,
        &control_argv,
        &full_argv,
        &seed,
        surface_pty,
    )?;

    // Write evidence files
    let control_path = format!("evidence/{}_control.json", scenario_id);
    write_evidence(sc.pack_path, &control_path, &control_evidence)?;
    let evidence_path = format!("evidence/{}.json", scenario_id);
    write_evidence(sc.pack_path, &evidence_path, &option_evidence)?;

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
    let prediction_channel_matched = prediction
        .as_ref()
        .map(|p| verify_prediction_channel(p, &option_evidence, &control_evidence));

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
        prediction,
        prediction_matched,
        prediction_channel_matched,
    })
}

/// Merge a test result into state.
///
/// This is the fast, sequential part after parallel execution.
pub(super) fn merge_test_result(state: &mut State, result: TestResult) {
    let already_seeded = state.has_seed_for(&result.surface_id);

    if let Some(entry) = state.entries.iter_mut().find(|e| e.id == result.surface_id) {
        // Compute prediction gate before moving fields into the Attempt.
        let prediction_blocked = if matches!(result.outcome, Outcome::Verified { .. })
            && result.prediction_matched == Some(false)
        {
            // Gate on prediction: if the LM made a prediction and it was wrong,
            // don't verify — keep Pending so the LM can self-correct.
            // Exception: if the test command succeeded (diff_kind == Stdout only)
            // but produced empty output while control had output, the option
            // demonstrably filters output. The prediction was wrong because the
            // seed/args didn't match (e.g. -cmin +120 on fresh files), not because
            // the LM misunderstands the option. Accept this as verified.
            let is_stdout_only = matches!(
                &result.outcome,
                Outcome::Verified { diff_kind } if *diff_kind == DiffKind::Stdout
            );
            let test_stdout_empty = result
                .stdout_metrics
                .as_ref()
                .is_some_and(|m| m.is_empty);
            let control_has_output = result
                .control_stdout_preview
                .as_ref()
                .is_some_and(|s| !s.is_empty());
            !(is_stdout_only && test_stdout_empty && control_has_output)
        } else {
            false
        };

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
            prediction: result.prediction,
            prediction_matched: result.prediction_matched,
            prediction_channel_matched: result.prediction_channel_matched,
        });

        if matches!(result.outcome, Outcome::Verified { .. }) && !prediction_blocked {
            entry.status = Status::Verified;

            // Add to seed bank if not already present
            if !already_seeded {
                if let Some(last) = entry.attempts.last() {
                    state.seed_bank.push(VerifiedSeed {
                        surface_id: result.surface_id.clone(),
                        args: last.args.clone(),
                        seed: last.seed.clone(),
                        verified_at: state.cycle,
                        hint: None,
                    });
                }
            }
        }
    }
}

/// Run a probe scenario without mutating state.
///
/// Runs BOTH control and option commands in the same sandbox (bilateral
/// comparison). Returns whether outputs differ so the caller can
/// auto-promote to a Test without an extra LM round-trip.
pub(super) fn run_probe_scenario(
    sc: &ScenarioContext<'_>,
    surface_id: &str,
    extra_args: Vec<String>,
    seed: Seed,
    value_hint: Option<&str>,
) -> Result<ProbeRunResult> {
    let scenario_id = format!("probe_{}_c{}", sanitize_id(surface_id), sc.cycle);

    // Construct args: join option + value when needed
    let args = join_option_value(surface_id, extra_args.clone(), value_hint);

    let mut control_argv: Vec<String> = sc.context_argv.to_vec();
    control_argv.extend_from_slice(sc.invocation_args);
    let mut full_argv = control_argv.clone();
    full_argv.extend(args);

    // Run both control and option in the same sandbox (bilateral)
    let (control_evidence, option_evidence) = run_scenario_pair(
        &scenario_id,
        sc.binary,
        &control_argv,
        &full_argv,
        &seed,
        sc.with_pty,
    )?;

    let stdout_preview = make_output_preview(&option_evidence.stdout, OUTPUT_PREVIEW_MAX_LEN);
    let stderr_preview = make_output_preview(&option_evidence.stderr, OUTPUT_PREVIEW_MAX_LEN);
    let control_stdout_preview =
        make_output_preview(&control_evidence.stdout, OUTPUT_PREVIEW_MAX_LEN);
    let control_stderr_preview =
        make_output_preview(&control_evidence.stderr, OUTPUT_PREVIEW_MAX_LEN);

    // Per-channel bilateral comparison
    let stdout_differs = option_evidence.stdout != control_evidence.stdout;
    let stderr_differs = option_evidence.stderr != control_evidence.stderr;
    let exit_code_differs = option_evidence.exit_code != control_evidence.exit_code;

    // Reject stderr-only diffs when both runs failed — these are just error message
    // variations, not behavioral differences. Matches compute_outcome's filter.
    let both_failed = option_evidence.exit_code.unwrap_or(0) != 0
        && control_evidence.exit_code.unwrap_or(0) != 0;
    let stderr_only_diff = stderr_differs && !stdout_differs && !exit_code_differs;
    let outputs_differ =
        (stdout_differs || stderr_differs || exit_code_differs) && !(stderr_only_diff && both_failed);

    // Extract setup failure detail: which command failed and its exit code
    let setup_detail = if option_evidence.setup_failed {
        option_evidence
            .setup_results
            .iter()
            .find(|sr| sr.exit_code != Some(0))
            .map(|sr| {
                let cmd = sr.argv.join(" ");
                let code = sr.exit_code.map_or("?".to_string(), |c| c.to_string());
                format!("{} (exit {})", cmd, code)
            })
    } else {
        None
    };

    Ok(ProbeRunResult {
        surface_id: surface_id.to_string(),
        extra_args,
        argv: full_argv,
        seed,
        stdout_preview,
        stderr_preview,
        exit_code: option_evidence.exit_code,
        control_stdout_preview,
        outputs_differ,
        setup_failed: option_evidence.setup_failed,
        cycle: sc.cycle,
        stdout_differs,
        stderr_differs,
        exit_code_differs,
        control_stderr_preview,
        control_exit_code: control_evidence.exit_code,
        setup_detail,
    })
}

/// Merge a probe result into state.
pub(super) fn merge_probe_result(state: &mut State, result: ProbeRunResult) {
    if let Some(entry) = state.find_entry_mut(&result.surface_id) {
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
            stdout_differs: result.stdout_differs,
            stderr_differs: result.stderr_differs,
            exit_code_differs: result.exit_code_differs,
            control_stderr_preview: result.control_stderr_preview,
            control_exit_code: result.control_exit_code,
            setup_detail: result.setup_detail,
        });
    }
}

/// Apply an action to the state.
///
/// This runs scenarios as needed and updates the state with results.
/// After applying, the caller should save the state.
pub(super) fn apply_action(state: &mut State, pack_path: &Path, action: LmAction) -> Result<()> {
    let invocation_args: Vec<String> = state
        .invocation_hint
        .as_ref()
        .map(|h| h.required_args.clone())
        .unwrap_or_default();
    match action {
        LmAction::SetBaseline { seed } => {
            // Full argv = context_argv (no extra args for baseline)
            let full_argv: Vec<String> = state.context_argv.clone();

            let evidence = run_scenario("baseline", &state.binary, &full_argv, &seed, false)?;
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
            ..
        } => {
            let sc = ScenarioContext {
                pack_path,
                binary: &state.binary,
                context_argv: &state.context_argv,
                cycle: state.cycle,
                with_pty: false,
                invocation_args: &invocation_args,
            };
            let vh = state
                .find_entry(&surface_id)
                .and_then(|e| e.value_hint.as_deref());
            let result = run_test_scenario(&sc, &surface_id, extra_args, seed, false, prediction, vh)?;
            merge_test_result(state, result);
        }

        LmAction::Probe {
            surface_id,
            extra_args,
            seed,
            ..
        } => {
            let sc = ScenarioContext {
                pack_path,
                binary: &state.binary,
                context_argv: &state.context_argv,
                cycle: state.cycle,
                with_pty: false,
                invocation_args: &invocation_args,
            };
            let vh = state
                .find_entry(&surface_id)
                .and_then(|e| e.value_hint.as_deref());
            let result = run_probe_scenario(&sc, &surface_id, extra_args, seed, vh)?;
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
    fn test_join_option_value_long_option_with_hint() {
        // --unified=3 (value_hint present → join with =)
        assert_eq!(
            join_option_value("--unified", vec!["3".to_string()], Some("<n>")),
            vec!["--unified=3"]
        );
    }

    #[test]
    fn test_join_option_value_long_option_no_hint() {
        // --ignore-case hello (no value_hint → keep separate, boolean flag)
        assert_eq!(
            join_option_value("--ignore-case", vec!["hello".to_string()], None),
            vec!["--ignore-case", "hello"]
        );
    }

    #[test]
    fn test_join_option_value_short_numeric() {
        // -U3 (git diff context lines)
        assert_eq!(
            join_option_value("-U", vec!["3".to_string()], Some("<n>")),
            vec!["-U3"]
        );
    }

    #[test]
    fn test_join_option_value_short_non_numeric() {
        // -D help (find debug option) — must NOT concatenate
        assert_eq!(
            join_option_value("-D", vec!["help".to_string()], None),
            vec!["-D", "help"]
        );
    }

    #[test]
    fn test_join_option_value_multi_char_single_dash() {
        // -maxdepth 1 (find) — must NOT concatenate
        assert_eq!(
            join_option_value("-maxdepth", vec!["1".to_string()], None),
            vec!["-maxdepth", "1"]
        );
        // -regextype posix-extended
        assert_eq!(
            join_option_value("-regextype", vec!["posix-extended".to_string()], None),
            vec!["-regextype", "posix-extended"]
        );
    }

    #[test]
    fn test_join_option_value_no_extra_args() {
        assert_eq!(
            join_option_value("--verbose", vec![], None),
            vec!["--verbose"]
        );
        assert_eq!(join_option_value("-n", vec![], None), vec!["-n"]);
    }

    #[test]
    fn test_join_option_value_flag_as_first_arg() {
        // First arg starts with '-', treated as a separate flag
        assert_eq!(
            join_option_value("--color", vec!["--always".to_string()], Some("<when>")),
            vec!["--color", "--always"]
        );
    }

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
            examples_section: String::new(),
            experiment_params: None,
            invocation_hint: None,
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
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 1,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
            invocation_hint: None,
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
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 1,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
            invocation_hint: None,
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
