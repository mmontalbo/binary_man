//! Execute test scripts and collect observations.

use crate::parse::Script;
use crate::sandbox;
use crate::validate;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::process::{Command, Stdio};

/// Captured output from a single invocation.
#[derive(Debug, Clone)]
pub struct Observation {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

/// Result of validating one test's predictions.
#[derive(Debug)]
#[allow(dead_code)]
pub struct TestResult {
    pub args: Vec<String>,
    pub observation: Observation,
    pub checks: Vec<CheckResult>,
    /// Flags whose output also passes all expectations (low specificity).
    pub confused_with: Vec<String>,
}

#[derive(Debug)]
pub struct CheckResult {
    pub passed: bool,
    pub detail: String,
    /// Additional context lines shown on failure (observed values, diffs, etc.)
    pub context: Vec<String>,
    /// Whether this check discriminates this invocation from at least one
    /// other invocation in the script.
    pub discriminates: Option<bool>,
}

const COMMON_FLAGS: &[&str] = &[
    "-a", "-A", "-b", "-B", "-c", "-d", "-f", "-F", "-g", "-G",
    "-h", "-i", "-l", "-L", "-m", "-n", "-N", "-o", "-p", "-q",
    "-Q", "-r", "-R", "-s", "-S", "-t", "-u", "-U", "-v", "-x",
    "-X", "-1",
];

/// Run an entire test script and validate predictions.
pub fn run_script(binary: &str, script: &Script) -> Result<Vec<TestResult>> {
    // Create sandbox
    let sandbox_dir = tempfile::Builder::new()
        .prefix("probe_")
        .tempdir()
        .context("create sandbox")?;

    let work_dir = sandbox_dir.path();

    // Apply setup
    sandbox::apply_setup(work_dir, &script.setup)?;

    // Collect all observations (keyed by args for cross-referencing)
    let mut observations: HashMap<Vec<String>, Observation> = HashMap::new();
    let mut results = Vec::new();

    // First pass: run all tests to collect observations
    for test in &script.tests {
        let obs = run_invocation(binary, &test.args, work_dir)?;
        observations.insert(test.args.clone(), obs.clone());
    }

    // Run all common flags for cross-flag specificity check
    let mut flag_observations: HashMap<String, Observation> = HashMap::new();
    for flag in COMMON_FLAGS {
        let args = vec![".".to_string(), flag.to_string()];
        if !observations.contains_key(&args) {
            if let Ok(obs) = run_invocation(binary, &args, work_dir) {
                flag_observations.insert(flag.to_string(), obs);
            }
        }
    }

    // Second pass: validate predictions + discrimination + specificity
    for (i, test) in script.tests.iter().enumerate() {
        let obs = observations.get(&test.args).unwrap().clone();
        let mut checks = validate::check_expectations(test, &obs, &observations);

        // Discrimination: check against all OTHER invocations in the script.
        // A check discriminates if it fails for at least one other invocation.
        let mut disc = vec![false; checks.len()];
        for (j, other_test) in script.tests.iter().enumerate() {
            if j == i {
                continue;
            }
            if let Some(other_obs) = observations.get(&other_test.args) {
                let other_checks =
                    validate::check_expectations(test, other_obs, &observations);
                for (ci, oc) in other_checks.iter().enumerate() {
                    if !oc.passed {
                        disc[ci] = true;
                    }
                }
            }
        }
        for (ci, check) in checks.iter_mut().enumerate() {
            check.discriminates = if script.tests.len() > 1 {
                Some(disc[ci])
            } else {
                None // Single test block — discrimination not applicable
            };
        }

        // Cross-flag specificity: which other flags pass all expectations?
        let mut confused_with = Vec::new();
        let tested_flags: Vec<&str> = test
            .args
            .iter()
            .filter(|a| a.starts_with('-'))
            .map(|a| a.as_str())
            .collect();

        for (flag, flag_obs) in &flag_observations {
            if tested_flags.contains(&flag.as_str()) {
                continue;
            }
            let xcheck = validate::check_expectations(test, flag_obs, &observations);
            if xcheck.iter().all(|c| c.passed) {
                confused_with.push(flag.clone());
            }
        }
        confused_with.sort();

        results.push(TestResult {
            args: test.args.clone(),
            observation: obs,
            checks,
            confused_with,
        });
    }

    Ok(results)
}

/// Run a single invocation and capture output.
fn run_invocation(binary: &str, args: &[String], work_dir: &std::path::Path) -> Result<Observation> {
    let mut cmd = Command::new(binary);
    cmd.args(args);
    cmd.current_dir(work_dir);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Set minimal environment
    cmd.env_clear();
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd.env("HOME", work_dir);
    cmd.env("LANG", "C");
    cmd.env("LC_ALL", "C");

    let output = cmd
        .output()
        .with_context(|| format!("run {} {:?}", binary, args))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    Ok(Observation {
        stdout,
        stderr,
        exit_code: output.status.code(),
    })
}
