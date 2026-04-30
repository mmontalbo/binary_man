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
    let mut total_discriminating = 0;
    let mut total_non_disc = 0;

    // For diff display: use the simplest invocation (fewest args) as baseline
    let baseline_obs = results
        .iter()
        .min_by_key(|r| r.args.len())
        .map(|r| &r.observation);

    for result in &results {
        let passed = result.checks.iter().filter(|c| c.passed).count();
        let total = result.checks.len();
        let disc = result.checks.iter().filter(|c| c.discriminates == Some(true)).count();
        let non_disc = result.checks.iter().filter(|c| c.discriminates == Some(false)).count();
        total_predictions += total;
        total_passed += passed;
        total_discriminating += disc;
        total_non_disc += non_disc;

        let status = if passed == total { "✓" } else { "✗" };
        let disc_note = if non_disc > 0 {
            format!(" ({} non-discriminating)", non_disc)
        } else {
            String::new()
        };
        eprintln!(
            "  {} test args {:?}: {}/{} predictions{}",
            status, result.args, passed, total, disc_note
        );
        for check in &result.checks {
            if !check.passed {
                eprintln!("    ✗ {}", check.detail);
                for ctx_line in &check.context {
                    eprintln!("      {}", ctx_line);
                }
            } else if check.discriminates == Some(false) {
                eprintln!("    ~ {}", check.detail);
            }
        }

        // Show actual diff when there are non-discriminating checks
        if non_disc > 0 {
            if let Some(baseline) = baseline_obs {
                if !std::ptr::eq(baseline, &result.observation) {
                    show_diff(baseline, &result.observation);
                }
            }
        }

        // Show cross-flag confusion
        if !result.confused_with.is_empty() {
            eprintln!(
                "    [confused with: {}]",
                result.confused_with.join(", ")
            );
        }
    }

    let disc_total = total_discriminating + total_non_disc;
    eprintln!(
        "\nResult: {}/{} predictions passed, {}/{} discriminating",
        total_passed, total_predictions, total_discriminating, disc_total,
    );

    if total_passed < total_predictions {
        std::process::exit(1);
    }

    Ok(())
}

/// Show a compact diff between control and option observations.
fn show_diff(control: &execute::Observation, option: &execute::Observation) {
    let ctrl_lines: Vec<&str> = control.stdout.lines().collect();
    let opt_lines: Vec<&str> = option.stdout.lines().collect();

    let ctrl_set: std::collections::HashSet<&str> = ctrl_lines.iter().copied().collect();
    let opt_set: std::collections::HashSet<&str> = opt_lines.iter().copied().collect();

    let only_ctrl: Vec<&&str> = ctrl_lines.iter().filter(|l| !opt_set.contains(**l)).collect();
    let only_opt: Vec<&&str> = opt_lines.iter().filter(|l| !ctrl_set.contains(**l)).collect();

    if only_ctrl.is_empty() && only_opt.is_empty() {
        if ctrl_lines == opt_lines {
            eprintln!("    [stdout identical — flag has no visible effect]");
        } else {
            eprintln!("    [stdout same lines, different order]");
        }
    } else {
        if !only_ctrl.is_empty() {
            eprintln!("    [only in control]");
            for l in only_ctrl.iter().take(5) {
                eprintln!("      - {}", l);
            }
            if only_ctrl.len() > 5 {
                eprintln!("      ... ({} more)", only_ctrl.len() - 5);
            }
        }
        if !only_opt.is_empty() {
            eprintln!("    [only in option]");
            for l in only_opt.iter().take(5) {
                eprintln!("      + {}", l);
            }
            if only_opt.len() > 5 {
                eprintln!("      ... ({} more)", only_opt.len() - 5);
            }
        }
    }

    if control.stderr != option.stderr {
        eprintln!("    [stderr differs]");
    }

    if control.exit_code != option.exit_code {
        eprintln!(
            "    [exit: {} → {}]",
            control.exit_code.map_or("?".into(), |c| c.to_string()),
            option.exit_code.map_or("?".into(), |c| c.to_string()),
        );
    }
}
