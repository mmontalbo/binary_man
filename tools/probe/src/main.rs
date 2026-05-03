use anyhow::{Context, Result};
use std::path::PathBuf;

mod execute;
mod parse;
mod sandbox;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        eprintln!("Usage: bman-probe <binary> <test-file>");
        std::process::exit(1);
    }

    let binary = &args[1];
    let test_path = PathBuf::from(&args[2]);

    cmd_run(binary, &test_path)
}

fn cmd_run(binary: &str, test_path: &PathBuf) -> Result<()> {
    let source = std::fs::read_to_string(test_path)
        .with_context(|| format!("read {}", test_path.display()))?;

    // Strip old results
    let base_source = strip_results(&source);

    let mut script = parse::parse_script(&base_source)
        .with_context(|| format!("parse {}", test_path.display()))?;

    // Load shared setup.test if present
    if let Some(parent) = test_path.parent() {
        let setup_path = parent.join("setup.test");
        if setup_path.exists() && setup_path != *test_path {
            let setup_source = std::fs::read_to_string(&setup_path)
                .with_context(|| format!("read {}", setup_path.display()))?;
            let setup_script = parse::parse_script(&setup_source)
                .with_context(|| format!("parse {}", setup_path.display()))?;

            // Merge setup contexts (prepend, so surface file contexts can extend them)
            let has_own = script.contexts.iter().any(|c| c.name != "(default)")
                || (script.contexts.len() == 1 && !script.contexts[0].commands.is_empty());
            if !has_own {
                script.contexts = setup_script.contexts;
            } else {
                // Prepend setup contexts so they're available for extends
                let mut merged = setup_script.contexts;
                merged.extend(script.contexts);
                script.contexts = merged;
                // Re-resolve extends with the merged set
                // (already resolved individually, but cross-file extends need re-resolution)
            }

            // Merge setup tests (baseline invocations)
            for setup_test in setup_script.tests {
                if !script.tests.iter().any(|t| t.args == setup_test.args) {
                    script.tests.insert(0, setup_test);
                }
            }
        }
    }

    // Report grid size
    let ctx_names: Vec<&str> = script.contexts.iter().map(|c| c.name.as_str()).collect();
    let total_runs = script.contexts.len() * script.tests.len();
    eprintln!(
        "#> {} states x {} invocations = {} runs",
        script.contexts.len(),
        script.tests.len(),
        total_runs
    );
    eprintln!("States: {}", ctx_names.join(", "));

    // Execute the grid
    let grid = execute::run_grid(binary, &script)?;

    // Format observations
    let mut results_lines: Vec<String> = Vec::new();
    results_lines.push(String::new());
    results_lines.push(format!(
        "#> {} states x {} invocations = {} runs",
        grid.context_count, grid.test_count, grid.context_count * grid.test_count
    ));

    // Report setup failures
    for (ctx, err) in &grid.setup_failures {
        results_lines.push(format!("#> {}: setup failed — {}", ctx, err));
    }

    for (ti, test) in script.tests.iter().enumerate() {
        let args_str = if test.args.is_empty() {
            "(no args)".to_string()
        } else {
            test.args.iter().map(|a| format!("\"{}\"", a)).collect::<Vec<_>>().join(" ")
        };
        results_lines.push(String::new());
        results_lines.push(format!("#> test args {}:", args_str));

        // Collect observations for this test across contexts
        let mut obs_by_ctx: Vec<(String, &execute::Observation)> = Vec::new();
        for ctx in &script.contexts {
            if let Some(obs) = grid.cells.get(&(ctx.name.clone(), ti)) {
                obs_by_ctx.push((ctx.name.clone(), obs));
            }
        }

        if obs_by_ctx.is_empty() {
            results_lines.push("#>   (no observations)".to_string());
            continue;
        }

        // Collapse identical observations
        let groups = collapse_observations(&obs_by_ctx);

        for (ctx_names, obs) in &groups {
            let label = ctx_names.join(", ");
            results_lines.push(format!("#>   {}:", label));

            // Stdout
            let stdout_lines: Vec<&str> = obs.stdout.lines().collect();
            if stdout_lines.is_empty() {
                results_lines.push("#>     stdout: (empty)".to_string());
            } else {
                results_lines.push(format!("#>     stdout ({} lines):", stdout_lines.len()));
                for line in stdout_lines.iter().take(20) {
                    results_lines.push(format!("#>       {}", line));
                }
                if stdout_lines.len() > 20 {
                    results_lines.push(format!(
                        "#>       ... ({} more lines)",
                        stdout_lines.len() - 20
                    ));
                }
            }

            // Stderr (only when non-empty)
            if !obs.stderr.trim().is_empty() {
                results_lines.push(format!("#>     stderr: {}", obs.stderr.trim()));
            }

            // Exit code
            results_lines.push(format!(
                "#>     exit: {}",
                obs.exit_code.unwrap_or(-1)
            ));
        }

        // Also report to stderr for live feedback
        let first_obs = &obs_by_ctx[0].1;
        let lines = first_obs.stdout.lines().count();
        let exit = first_obs.exit_code.unwrap_or(-1);
        eprintln!(
            "  test args {}: {} contexts, {} stdout lines, exit {}",
            args_str, obs_by_ctx.len(), lines, exit
        );
    }

    // Write results back to file
    let mut output = base_source.trim_end().to_string();
    output.push('\n');
    for line in &results_lines {
        output.push_str(line);
        output.push('\n');
    }

    let tmp_path = test_path.with_extension("test.tmp");
    std::fs::write(&tmp_path, &output)
        .with_context(|| format!("write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, test_path)
        .with_context(|| format!("rename to {}", test_path.display()))?;

    Ok(())
}

/// Group contexts that produced identical observations.
fn collapse_observations<'a>(
    obs_by_ctx: &[(String, &'a execute::Observation)],
) -> Vec<(Vec<String>, &'a execute::Observation)> {
    let mut groups: Vec<(Vec<String>, &'a execute::Observation)> = Vec::new();

    for (ctx_name, obs) in obs_by_ctx {
        let found = groups.iter_mut().find(|(_, existing)| {
            existing.stdout == obs.stdout
                && existing.stderr == obs.stderr
                && existing.exit_code == obs.exit_code
        });
        if let Some((names, _)) = found {
            names.push(ctx_name.clone());
        } else {
            groups.push((vec![ctx_name.clone()], obs));
        }
    }

    groups
}

fn strip_results(source: &str) -> String {
    let mut lines: Vec<&str> = source.lines().collect();
    // Find first #> line (results start)
    if let Some(pos) = lines.iter().position(|l| l.trim().starts_with("#>")) {
        let mut start = pos;
        while start > 0 && lines[start - 1].trim().is_empty() {
            start -= 1;
        }
        lines.truncate(start);
    }
    let mut result = lines.join("\n");
    result.push('\n');
    result
}
