//! Execute the grid: states × invocations → observations.

use crate::parse::{Script, StdinSource, Test};
use crate::sandbox;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::process::{Command, Stdio};

/// Observation from a single execution.
#[derive(Debug, Clone)]
pub struct Observation {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

/// The full grid result.
#[derive(Debug)]
pub struct GridResult {
    /// (context_name, test_index) → observation
    pub cells: HashMap<(String, usize), Observation>,
    /// Context names that failed during setup
    pub setup_failures: HashMap<String, String>,
    /// Total contexts
    pub context_count: usize,
    /// Total tests
    pub test_count: usize,
}

/// Run the entire grid.
pub fn run_grid(binary: &str, script: &Script) -> Result<GridResult> {
    let mut cells: HashMap<(String, usize), Observation> = HashMap::new();
    let mut setup_failures: HashMap<String, String> = HashMap::new();

    for ctx in &script.contexts {
        // Create fresh sandbox for this context
        let sandbox_dir = tempfile::Builder::new()
            .prefix("probe_")
            .tempdir()
            .context("create sandbox")?;
        let work_dir = sandbox_dir.path();

        // Apply setup commands
        match sandbox::apply_setup(work_dir, binary, &ctx.commands) {
            Ok(()) => {}
            Err(e) => {
                setup_failures.insert(ctx.name.clone(), format!("{}", e));
                continue;
            }
        }

        // Run each applicable test
        for (ti, test) in script.tests.iter().enumerate() {
            if let Some(ref scoped) = test.in_contexts {
                if !scoped.contains(&ctx.name) {
                    continue;
                }
            }

            let obs = run_invocation(binary, test, work_dir)?;
            cells.insert((ctx.name.clone(), ti), obs);
        }
    }

    Ok(GridResult {
        cells,
        setup_failures,
        context_count: script.contexts.len(),
        test_count: script.tests.len(),
    })
}

fn run_invocation(
    binary: &str,
    test: &Test,
    work_dir: &std::path::Path,
) -> Result<Observation> {
    let mut cmd = Command::new(binary);
    cmd.args(&test.args);
    cmd.current_dir(work_dir);

    // Stdin
    match &test.stdin {
        Some(StdinSource::Lines(_)) => {
            cmd.stdin(Stdio::piped());
        }
        Some(StdinSource::FromFile(path)) => {
            let full = work_dir.join(path);
            let file = std::fs::File::open(&full)
                .with_context(|| format!("open stdin file {}", path))?;
            cmd.stdin(Stdio::from(file));
        }
        None => {
            cmd.stdin(Stdio::null());
        }
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Minimal environment
    cmd.env_clear();
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd.env("HOME", work_dir);
    cmd.env("LANG", "C");
    cmd.env("LC_ALL", "C");

    let mut child = cmd.spawn()
        .with_context(|| format!("spawn {} {:?}", binary, test.args))?;

    // Write stdin if piped
    if let Some(StdinSource::Lines(lines)) = &test.stdin {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            let content = lines.join("\n") + "\n";
            let _ = stdin.write_all(content.as_bytes());
        }
    }

    let output = child.wait_with_output()
        .with_context(|| format!("wait for {} {:?}", binary, test.args))?;

    Ok(Observation {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code(),
    })
}
