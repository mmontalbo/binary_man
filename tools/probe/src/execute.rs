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

/// Result of validating one test's predictions across contexts.
#[derive(Debug)]
pub struct TestResult {
    pub args: Vec<String>,
    pub context_results: Vec<ContextTestResult>,
}

/// Result of one test in one context.
#[derive(Debug)]
pub struct ContextTestResult {
    pub context_name: String,
    pub observation: Observation,
    pub checks: Vec<CheckResult>,
}

#[derive(Debug)]
pub struct CheckResult {
    pub passed: bool,
    pub detail: String,
    pub context: Vec<String>,
    /// Whether this check discriminates this invocation from at least one
    /// other invocation in this context.
    pub discriminates: Option<bool>,
}

/// Run an entire test script across all contexts.
pub fn run_script(binary: &str, script: &Script) -> Result<Vec<TestResult>> {
    // For each context, run all applicable tests
    // Key: (context_name, args) -> observation
    let mut all_observations: HashMap<(String, Vec<String>), Observation> = HashMap::new();

    for ctx in &script.contexts {
        let sandbox_dir = tempfile::Builder::new()
            .prefix("probe_")
            .tempdir()
            .context("create sandbox")?;
        let work_dir = sandbox_dir.path();

        sandbox::apply_setup(work_dir, &ctx.commands)?;

        for test in &script.tests {
            // Check if this test applies to this context
            if let Some(ref scoped) = test.in_contexts {
                if !scoped.contains(&ctx.name) {
                    continue;
                }
            }

            let obs = run_invocation(binary, &test.args, work_dir)?;
            all_observations.insert((ctx.name.clone(), test.args.clone()), obs);
        }
    }

    // Build results: for each test, collect per-context results
    let mut results = Vec::new();

    for (ti, test) in script.tests.iter().enumerate() {
        let mut context_results = Vec::new();

        for ctx in &script.contexts {
            // Skip contexts this test doesn't apply to
            if let Some(ref scoped) = test.in_contexts {
                if !scoped.contains(&ctx.name) {
                    continue;
                }
            }

            let key = (ctx.name.clone(), test.args.clone());
            let obs = match all_observations.get(&key) {
                Some(o) => o.clone(),
                None => continue,
            };

            // Build observations map for this context (for vs references)
            let ctx_observations: HashMap<Vec<String>, Observation> = script
                .tests
                .iter()
                .filter_map(|t| {
                    let k = (ctx.name.clone(), t.args.clone());
                    all_observations.get(&k).map(|o| (t.args.clone(), o.clone()))
                })
                .collect();

            let mut checks = validate::check_expectations(test, &obs, &ctx_observations);

            // Discrimination: check against all other invocations in this context
            let mut disc = vec![false; checks.len()];
            for (tj, other_test) in script.tests.iter().enumerate() {
                if tj == ti {
                    continue;
                }
                let other_key = (ctx.name.clone(), other_test.args.clone());
                if let Some(other_obs) = all_observations.get(&other_key) {
                    let other_checks =
                        validate::check_expectations(test, other_obs, &ctx_observations);
                    for (ci, oc) in other_checks.iter().enumerate() {
                        if !oc.passed {
                            disc[ci] = true;
                        }
                    }
                }
            }
            let has_peers = script.tests.len() > 1;
            for (ci, check) in checks.iter_mut().enumerate() {
                check.discriminates = if has_peers { Some(disc[ci]) } else { None };
            }

            context_results.push(ContextTestResult {
                context_name: ctx.name.clone(),
                observation: obs,
                checks,
            });
        }

        results.push(TestResult {
            args: test.args.clone(),
            context_results,
        });
    }

    Ok(results)
}

/// Run a single invocation and capture output.
fn run_invocation(
    binary: &str,
    args: &[String],
    work_dir: &std::path::Path,
) -> Result<Observation> {
    let mut cmd = Command::new(binary);
    cmd.args(args);
    cmd.current_dir(work_dir);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

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
