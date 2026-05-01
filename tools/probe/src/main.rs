use anyhow::{Context, Result};
use std::path::PathBuf;

mod delta;
mod execute;
mod parse;
mod sandbox;
mod validate;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: bman-probe <binary> <test-file>");
        eprintln!("       bman-probe ls surfaces/ls/-a.test");
        std::process::exit(1);
    }

    let binary = &args[1];
    let test_path = PathBuf::from(&args[2]);

    let source = std::fs::read_to_string(&test_path)
        .with_context(|| format!("read {}", test_path.display()))?;

    let script = parse::parse_script(&source)
        .with_context(|| format!("parse {}", test_path.display()))?;

    let ctx_names: Vec<&str> = script.contexts.iter().map(|c| c.name.as_str()).collect();
    eprintln!("Binary: {}", binary);
    eprintln!("Contexts: {} ({})", script.contexts.len(), ctx_names.join(", "));
    eprintln!("Tests: {}", script.tests.len());

    let results = execute::run_script(binary, &script)?;

    // Report
    let mut total_checks = 0;
    let mut total_passed = 0;

    for result in &results {
        let num_contexts = result.context_results.len();

        // Per-check cross-context summary
        if num_contexts == 0 {
            continue;
        }

        let num_checks = result.context_results[0].checks.len();
        let mut check_passed_in: Vec<Vec<&str>> = vec![Vec::new(); num_checks];
        let mut check_failed_in: Vec<Vec<&str>> = vec![Vec::new(); num_checks];

        for cr in &result.context_results {
            for (ci, check) in cr.checks.iter().enumerate() {
                if check.passed {
                    check_passed_in[ci].push(&cr.context_name);
                } else {
                    check_failed_in[ci].push(&cr.context_name);
                }
            }
        }

        let all_pass = check_failed_in.iter().all(|f| f.is_empty());
        let status = if all_pass { "✓" } else { "✗" };

        // Count checks that passed in ALL contexts
        let properties = check_passed_in
            .iter()
            .filter(|p| p.len() == num_contexts)
            .count();
        let ctx_dependent = check_passed_in
            .iter()
            .filter(|p| !p.is_empty() && p.len() < num_contexts)
            .count();
        let failed = check_failed_in
            .iter()
            .filter(|f| f.len() == num_contexts)
            .count();

        total_checks += num_checks;
        total_passed += properties + ctx_dependent;

        if num_contexts == 1 {
            // Single context — simple output
            let cr = &result.context_results[0];
            let passed = cr.checks.iter().filter(|c| c.passed).count();
            eprintln!(
                "  {} test args {:?}: {}/{} passed",
                status, result.args, passed, num_checks
            );
            for check in &cr.checks {
                if !check.passed {
                    eprintln!("    ✗ {}", check.detail);
                    for ctx_line in &check.context {
                        eprintln!("      {}", ctx_line);
                    }
                }
            }
        } else {
            // Multi-context — show per-check cross-context summary
            eprintln!(
                "  {} test args {:?}: {} checks across {} contexts ({} properties, {} context-dependent, {} failed)",
                status, result.args, num_checks, num_contexts, properties, ctx_dependent, failed
            );

            // Show first context's observations
            let first = &result.context_results[0];
            let stdout_lines = first.observation.stdout.lines().count();
            if stdout_lines > 0 {
                eprintln!("    stdout ({}, {} lines):", first.context_name, stdout_lines);
                for line in first.observation.stdout.lines().take(5) {
                    eprintln!("      {}", line);
                }
                if stdout_lines > 5 {
                    eprintln!("      ... ({} more)", stdout_lines - 5);
                }
            }

            // Per-check summary
            for ci in 0..num_checks {
                let detail = &result.context_results[0].checks[ci].detail;
                let passed = &check_passed_in[ci];
                let failed = &check_failed_in[ci];

                if failed.is_empty() {
                    // Passed everywhere
                    eprintln!("    ✓ {} — all {} contexts", detail, num_contexts);
                } else if passed.is_empty() {
                    // Failed everywhere
                    eprintln!("    ✗ {} — failed in all contexts", detail);
                    // Show context from first failure
                    let first_fail = &result.context_results[0].checks[ci];
                    for ctx_line in &first_fail.context {
                        eprintln!("      {}", ctx_line);
                    }
                } else {
                    // Mixed
                    eprintln!(
                        "    ~ {} — passed in: {} (failed in: {})",
                        detail,
                        passed.join(", "),
                        failed.join(", ")
                    );
                }
            }
        }
    }

    eprintln!("\nResult: {}/{} checks passed", total_passed, total_checks);

    if total_passed < total_checks {
        std::process::exit(1);
    }

    Ok(())
}
