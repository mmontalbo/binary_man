use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::PathBuf;

use binary_grid::{analyze, discover, execute, output, parse, report, sandbox};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let dry_run = args.iter().any(|a| a == "--dry-run");
    let positional: Vec<&String> = args.iter().skip(1).filter(|a| !a.starts_with("--")).collect();

    if positional.is_empty() {
        eprintln!("Usage: bgrid [options] <binary> [<probe-file>]");
        eprintln!("       bgrid <binary>                            explore: discover + run");
        eprintln!("       bgrid <binary> <file.probe>               run observation grid");
        eprintln!("       bgrid --dry-run <binary> <file.probe>     show grid without executing");
        std::process::exit(1);
    }

    let last = positional.last().unwrap();
    if last.ends_with(".probe") {
        let binary = positional[0];
        let test_path = PathBuf::from(last.as_str());
        if dry_run {
            cmd_dry_run(&test_path)
        } else {
            let sandbox = sandbox::Sandbox::new()?;
            cmd_run(binary, &test_path, &sandbox)
        }
    } else {
        let sandbox = sandbox::Sandbox::new()?;
        cmd_discover(&positional, &sandbox)
    }
}

fn cmd_discover(command: &[&String], sandbox: &sandbox::Sandbox) -> Result<()> {
    let binary = command[0].as_str();
    let sub_args: Vec<&str> = command[1..].iter().map(|s| s.as_str()).collect();

    // --- Explore mode ---
    let cmd_label = if sub_args.is_empty() {
        binary.to_string()
    } else {
        format!("{} {}", binary, sub_args.join(" "))
    };

    // Single-phase exploration: fixed DoE design (no iterative refinement).
    // All single-flag and pairwise-combo runs are generated up front.
    let (script, flag_info) = discover::generate_initial_script(binary, &sub_args, sandbox)?;
    eprintln!("=== Exploring {} ===", cmd_label);
    eprintln!("{} contexts, {} runs, {} cells",
        script.contexts.len(), script.runs.len(), execute::count_cells(&script));

    let grid = execute::run_grid(binary, &script, std::path::Path::new("."), sandbox)?;

    let t_analysis = std::time::Instant::now();
    let metrics = analyze::analyze(&script, &grid, Some(&flag_info), None);
    let analysis_elapsed = t_analysis.elapsed();

    // Compute isolation
    let mut ever_isolated: HashSet<String> = HashSet::new();
    for group in &metrics.groups {
        let effectively_isolated = group.isolated() || (group.run_labels.len() == 2 && {
            let stems: HashSet<String> = group.run_labels.iter()
                .filter_map(|l| report::flag_stem(l))
                .map(|s| report::canonical_flag(&s, Some(&flag_info.aliases)))
                .collect();
            stems.len() == 1
        });
        if effectively_isolated {
            for label in &group.run_labels {
                ever_isolated.insert(label.clone());
            }
        }
    }

    eprintln!("{} groups, {} isolated, {} identical",
        metrics.groups.len(), ever_isolated.len(), metrics.identical_count());

    let rounds = vec![report::RoundSummary {
        round: 0,
        total_groups: metrics.groups.len(),
        isolated: ever_isolated.len(),
        identical: metrics.identical_count(),
        strategies: vec!["single-phase".into()],
    }];

    let all_runs: Vec<&analyze::RunAnalysis> = metrics.runs.iter().collect();
    let t_report = std::time::Instant::now();
    let report = report::format_exploration_report(
        &rounds,
        &metrics,
        Some(&flag_info),
        &ever_isolated,
        &cmd_label,
        &all_runs,
        &script.contexts,
    );
    let report_elapsed = t_report.elapsed();
    eprintln!("  timing: analysis={}ms report={}ms",
        analysis_elapsed.as_millis(), report_elapsed.as_millis());
    print!("{}", report);

    Ok(())
}

fn load_script(test_path: &PathBuf) -> Result<parse::Script> {
    let source = std::fs::read_to_string(test_path)
        .with_context(|| format!("read {}", test_path.display()))?;

    let mut script = parse::parse_script(&source)
        .with_context(|| format!("parse {}", test_path.display()))?;

    // Load shared contexts from setup.probe or contexts.probe
    if let Some(parent) = test_path.parent() {
        for setup_name in &["setup.probe", "contexts.probe"] {
            let setup_path = parent.join(setup_name);
            if setup_path.exists() && setup_path != *test_path {
                let setup_source = std::fs::read_to_string(&setup_path)
                    .with_context(|| format!("read {}", setup_path.display()))?;
                let setup_script = parse::parse_script(&setup_source)
                    .with_context(|| format!("parse {}", setup_path.display()))?;

                let has_own = script.contexts.iter().any(|c| c.name != "(default)")
                    || (script.contexts.len() == 1 && !script.contexts[0].commands.is_empty());
                if !has_own {
                    script.contexts = setup_script.contexts;
                } else {
                    let mut merged = setup_script.contexts;
                    merged.extend(script.contexts);
                    script.contexts = merged;
                }
                break;
            }
        }
    }

    Ok(script)
}

fn cmd_dry_run(test_path: &PathBuf) -> Result<()> {
    let script = load_script(test_path)?;

    println!("contexts:");
    for ctx in &script.contexts {
        println!("  {:?} ({} commands)", ctx.name, ctx.commands.len());
        for (i, cmd) in ctx.commands.iter().enumerate() {
            println!("    {}. {}", i + 1, output::format_setup_cmd(cmd));
        }
    }

    println!("\nruns:");
    for (i, run) in script.runs.iter().enumerate() {
        let args = output::format_args(&run.args);
        let scope = match &run.in_contexts {
            Some(ctxs) => format!(" in {}", ctxs.iter().map(|c| format!("{:?}", c)).collect::<Vec<_>>().join(", ")),
            None => String::new(),
        };
        let from = match &run.diff_from {
            Some(ref_args) => format!(" from {}", output::format_args(ref_args)),
            None => String::new(),
        };
        println!("  [{}] {}{}{}", i, args, from, scope);
    }

    let cells = execute::count_cells(&script);
    println!("\ngrid: {} contexts x {} runs = {} cells", script.contexts.len(), script.runs.len(), cells);

    execute::validate_from_references(&script);

    Ok(())
}

fn cmd_run(binary: &str, test_path: &PathBuf, sandbox: &sandbox::Sandbox) -> Result<()> {
    let script = load_script(test_path)?;

    execute::validate_from_references(&script);

    let actual_cells = execute::count_cells(&script);
    eprintln!(
        "{} contexts, {} runs, {} cells",
        script.contexts.len(), script.runs.len(), actual_cells
    );

    let probe_dir = test_path.parent().unwrap_or(std::path::Path::new("."));
    let grid = execute::run_grid(binary, &script, probe_dir, sandbox)?;

    let flag_info = discover::try_help(binary, &[], sandbox)
        .map(|text| discover::extract_flag_info(&text))
        .ok();

    let metrics = analyze::analyze(&script, &grid, flag_info.as_ref(), None);

    let probe_name = test_path.file_name().unwrap_or_default().to_string_lossy();
    let out = report::format_run_report(
        &metrics,
        flag_info.as_ref(),
        &probe_name,
        grid.cells.len(),
        &grid.setup_failures,
    );

    // Write results file
    let results_path = test_path.with_extension("results");
    std::fs::write(&results_path, &out)
        .with_context(|| format!("write {}", results_path.display()))?;
    eprintln!("wrote {}", results_path.display());

    if !grid.setup_failures.is_empty() {
        eprintln!("{} context(s) failed setup", grid.setup_failures.len());
        if grid.cells.is_empty() {
            std::process::exit(1);
        }
    }

    Ok(())
}
