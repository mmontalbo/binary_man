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

    eprintln!("Binary: {}", binary);
    eprintln!("Setup: {} commands", script.setup.len());
    eprintln!("Tests: {}", script.tests.len());

    let results = execute::run_script(binary, &script)?;

    // Report
    let mut total_predictions = 0;
    let mut total_passed = 0;

    for result in &results {
        let passed = result.checks.iter().filter(|c| c.passed).count();
        let total = result.checks.len();
        total_predictions += total;
        total_passed += passed;

        let status = if passed == total { "✓" } else { "✗" };
        eprintln!(
            "  {} test args {:?}: {}/{} predictions",
            status,
            result.args,
            passed,
            total
        );
        for check in &result.checks {
            let mark = if check.passed { "✓" } else { "✗" };
            if !check.passed {
                eprintln!("    {} {}", mark, check.detail);
                for ctx_line in &check.context {
                    eprintln!("      {}", ctx_line);
                }
            }
        }
    }

    eprintln!(
        "\nResult: {}/{} predictions passed",
        total_passed, total_predictions
    );

    if total_passed < total_predictions {
        std::process::exit(1);
    }

    Ok(())
}
