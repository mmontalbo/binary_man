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

    // Report grid size (accounting for scoping)
    let mut actual_runs = 0;
    for test in &script.tests {
        for ctx in &script.contexts {
            if let Some(ref scoped) = test.in_contexts {
                if !scoped.contains(&ctx.name) {
                    continue;
                }
            }
            actual_runs += 1;
        }
    }
    eprintln!(
        "#> {} states, {} invocations, {} runs",
        script.contexts.len(),
        script.tests.len(),
        actual_runs
    );

    // Execute the grid
    let grid = execute::run_grid(binary, &script)?;

    // Format observations
    let mut results_lines: Vec<String> = Vec::new();
    results_lines.push(String::new());
    results_lines.push(format!(
        "#> {} states, {} invocations, {} actual runs",
        grid.context_count, grid.test_count, grid.cells.len()
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

        // Find the largest group (most common observation)
        let largest_idx = groups
            .iter()
            .enumerate()
            .max_by_key(|(_, (names, _))| names.len())
            .map(|(i, _)| i)
            .unwrap_or(0);

        // Compact mode: show the majority group, then only the differences
        let (majority_names, majority_obs) = &groups[largest_idx];

        // Show majority group
        let majority_label = if majority_names.len() == obs_by_ctx.len() {
            "all contexts".to_string()
        } else {
            format!("{} contexts ({})", majority_names.len(), majority_names.join(", "))
        };
        results_lines.push(format!("#>   {}:", majority_label));
        format_observation(&mut results_lines, majority_obs);

        // Show differing groups
        for (i, (ctx_names, obs)) in groups.iter().enumerate() {
            if i == largest_idx {
                continue;
            }
            let label = ctx_names.join(", ");
            results_lines.push(format!("#>   differs in {}:", label));
            format_observation(&mut results_lines, obs);
        }

        // Sensitivity summary: which vary-generated contexts differ from majority?
        let sensitive: Vec<&str> = groups
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != largest_idx)
            .flat_map(|(_, (names, _))| names.iter().map(|n| n.as_str()))
            .filter(|n| n.contains(" / "))  // only vary-generated contexts
            .collect();

        if !sensitive.is_empty() {
            results_lines.push(format!(
                "#>   sensitive to: {}",
                sensitive
                    .iter()
                    .map(|s| s.split(" / ").last().unwrap_or(s))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        // Stderr for live feedback
        let exit = obs_by_ctx[0].1.exit_code.unwrap_or(-1);
        let sens_str = if sensitive.is_empty() {
            String::new()
        } else {
            format!(
                " [sensitive: {}]",
                sensitive
                    .iter()
                    .map(|s| s.split(" / ").last().unwrap_or(s))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        eprintln!(
            "  test args {}: {} groups{}, exit {}",
            args_str, groups.len(), sens_str, exit
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

fn format_observation(lines: &mut Vec<String>, obs: &execute::Observation) {
    let stdout_lines: Vec<&str> = obs.stdout.lines().collect();
    if stdout_lines.is_empty() {
        lines.push("#>     stdout: (empty)".to_string());
    } else {
        lines.push(format!("#>     stdout ({} lines):", stdout_lines.len()));
        for line in stdout_lines.iter().take(20) {
            lines.push(format!("#>       {}", line));
        }
        if stdout_lines.len() > 20 {
            lines.push(format!(
                "#>       ... ({} more lines)",
                stdout_lines.len() - 20
            ));
        }
    }
    if !obs.stderr.trim().is_empty() {
        lines.push(format!("#>     stderr: {}", obs.stderr.trim()));
    }
    lines.push(format!("#>     exit: {}", obs.exit_code.unwrap_or(-1)));
    if !obs.fs_changes.is_empty() {
        lines.push("#>     fs:".to_string());
        for change in &obs.fs_changes {
            match change {
                execute::FsChange::Created { path, size } => {
                    lines.push(format!("#>       created: {} ({} bytes)", path, size));
                }
                execute::FsChange::Deleted { path } => {
                    lines.push(format!("#>       deleted: {}", path));
                }
                execute::FsChange::Modified { path, detail } => {
                    lines.push(format!("#>       modified: {} ({})", path, detail));
                }
            }
        }
    }
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
                && existing.fs_changes == obs.fs_changes
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
