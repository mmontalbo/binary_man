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
}

#[derive(Debug)]
pub struct CheckResult {
    pub passed: bool,
    pub detail: String,
    /// Additional context lines shown on failure (observed values, diffs, etc.)
    pub context: Vec<String>,
}

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

    // Second pass: validate predictions
    for test in &script.tests {
        let obs = observations.get(&test.args).unwrap().clone();
        let checks = validate::check_expectations(test, &obs, &observations);
        results.push(TestResult {
            args: test.args.clone(),
            observation: obs,
            checks,
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
