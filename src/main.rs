use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use binary_grid::{execute, output, parse, sandbox};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let dry_run = args.iter().any(|a| a == "--dry-run");
    let compact = args.iter().any(|a| a == "--compact");
    let trace = args.iter().any(|a| a == "--trace");
    let positional: Vec<&String> = args.iter().skip(1).filter(|a| !a.starts_with("--")).collect();

    if positional.is_empty() {
        eprintln!("Usage: bgrid [--dry-run] [--compact] [--trace] <binary> [<probe-file>]");
        eprintln!("       bgrid <binary>                        discover flags from --help");
        eprintln!("       bgrid <binary> <file.probe>            run observation grid");
        eprintln!("       bgrid --compact <binary> <file.probe>  summary-only output");
        eprintln!("       bgrid --trace <binary> <file.probe>    include file access traces");
        std::process::exit(1);
    }

    // If last arg ends in .probe, it's run mode. Otherwise, discovery.
    let last = positional.last().unwrap();
    if last.ends_with(".probe") {
        let binary = positional[0];
        let test_path = PathBuf::from(last.as_str());
        if dry_run {
            cmd_dry_run(&test_path)
        } else {
            let sandbox = sandbox::Sandbox::new(trace)?;
            cmd_run(binary, &test_path, &sandbox, compact)
        }
    } else {
        let sandbox = sandbox::Sandbox::new(false)?;
        cmd_discover(&positional, &sandbox)
    }
}

fn cmd_discover(command: &[&String], sandbox: &sandbox::Sandbox) -> Result<()> {
    use regex::Regex;

    let binary = command[0].as_str();
    let sub_args: Vec<&str> = command[1..].iter().map(|s| s.as_str()).collect();

    // Try --help, then -h, then no args
    let help_text = try_help(binary, &sub_args, sandbox)?;

    // Extract flags
    let short_re = Regex::new(r"(?:^|\s)-([a-zA-Z0-9])\b").unwrap();
    let long_re = Regex::new(r"--([a-zA-Z][a-zA-Z0-9-]*)(?:[=\s]([A-Z][A-Z_]*))?").unwrap();

    let mut short_flags: Vec<String> = Vec::new();
    let mut long_flags: Vec<(String, Option<String>)> = Vec::new(); // (flag, value_hint)
    let mut seen = HashSet::new();

    for line in help_text.lines() {
        for cap in short_re.captures_iter(line) {
            let flag = format!("-{}", &cap[1]);
            if seen.insert(flag.clone()) {
                short_flags.push(flag);
            }
        }
        for cap in long_re.captures_iter(line) {
            let name = format!("--{}", &cap[1]);
            if name == "--help" || name == "--version" { continue; }
            let hint = cap.get(2).map(|m| m.as_str().to_string());
            if seen.insert(name.clone()) {
                long_flags.push((name, hint));
            }
        }
    }

    // Infer positional arguments from usage line
    let (pattern_arg, file_arg) = infer_base_args(&help_text);
    let takes_stdin = help_text.contains("[FILE]...") || help_text.contains("[file ...]");

    // Build the command label for comments
    let cmd_label = if sub_args.is_empty() {
        binary.to_string()
    } else {
        format!("{} {}", binary, sub_args.join(" "))
    };

    // Output skeleton
    println!("# Discovered from: {} --help", cmd_label);
    println!("# {} short flags, {} long flags found", short_flags.len(), long_flags.len());
    println!();

    // Rich binary-agnostic base context
    // Content designed to exercise: sorting, filtering, field ops, regex, whitespace
    println!("context \"base\"");
    println!("  file \"input.txt\" \"alpha\" \"alpha\" \"10\" \"2\" \"BETA\" \"  spaced  \" \"\" \"Jan\" \"100K\" \"hello world\" \"hello\\tworld\"");
    println!("  file \".hidden\" \"secret content\"");
    println!("  file \"empty.txt\" empty");
    println!("  dir \"subdir\"");
    println!("  file \"subdir/nested.txt\" \"nested content\"");
    println!("  file \"link.txt\" -> \"input.txt\"");
    println!("  file \"exec.sh\" \"#!/bin/sh\\necho hello\"");
    println!("  props \"exec.sh\" executable");
    println!();

    // Content perturbations — vary what's inside the files
    println!("vary from \"base\"");
    println!("  file \"input.txt\" \"single line\"");
    println!("  file \"input.txt\" empty");
    println!("  file \"input.txt\" size 10000");
    println!("  file \"input.txt\" \"a:1:x\" \"b:2:y\" \"c:3:z\"  # field-delimited");
    println!("  file \"input.txt\" \"alpha\" \"beta\" \"gamma\"  # no duplicates");
    println!();

    // Structural perturbations — vary what exists
    println!("vary from \"base\"");
    println!("  remove \".hidden\"");
    println!("  remove \"subdir\"");
    println!("  remove \"link.txt\"");
    println!("  remove \"exec.sh\"");
    println!();

    // Type/name edge cases — structural equivalence classes
    println!("vary from \"base\"");
    println!("  file \"link.txt\" -> \"nonexistent\"  # broken symlink");
    println!("  file \"-rf\" \"flag-like filename\"");
    println!("  props \"subdir\" readonly  # unreadable directory");
    println!();

    // Property perturbations — vary file attributes
    println!("vary from \"base\"");
    println!("  props \"input.txt\" readonly");
    println!("  props \"input.txt\" mtime old");
    println!("  file \"input.txt\" size 1");
    println!();

    // Helper: build a run arg list with optional pattern and file
    let run_prefix: Vec<&str> = sub_args.to_vec();
    let build_run = |flag: Option<&str>, flag_value: Option<&str>, pattern: Option<&str>, file: Option<&str>| -> String {
        let mut parts: Vec<String> = run_prefix.iter().map(|a| format!("\"{}\"", a)).collect();
        if let Some(f) = flag {
            if let Some(v) = flag_value {
                parts.push(format!("\"{}={}\"", f, v));
            } else {
                parts.push(format!("\"{}\"", f));
            }
        }
        if let Some(p) = pattern {
            parts.push(format!("\"{}\"", p));
        }
        if let Some(f) = file {
            parts.push(format!("\"{}\"", f));
        }
        parts.join(" ")
    };

    let pat = pattern_arg.as_deref();
    let fil = file_arg.as_deref();

    // Base invocation + from block
    if pat.is_some() || fil.is_some() {
        let base_run_args = build_run(None, None, pat, fil);

        println!("run {}", base_run_args);
        println!();
        println!("from {}", base_run_args);

        for flag in &short_flags {
            println!("  run {}", build_run(Some(flag), None, pat, fil));
        }
        for (flag, hint) in &long_flags {
            let val = hint.as_ref().map(|h| default_value(h));
            println!("  run {}", build_run(Some(flag), val.as_deref(), pat, fil));
        }
    } else {
        // No base invocation — flat run list
        for flag in &short_flags {
            println!("run {}", build_run(Some(flag), None, None, None));
        }
        for (flag, hint) in &long_flags {
            let val = hint.as_ref().map(|h| default_value(h));
            println!("run {}", build_run(Some(flag), val.as_deref(), None, None));
        }
    }

    // Zero-boundary runs for numeric flags
    let numeric_hints = ["NUM", "NUMBER", "N", "SIZE", "COLS", "WIDTH", "COUNT", "LINES", "BYTES"];
    let zero_flags: Vec<&(String, Option<String>)> = long_flags.iter()
        .filter(|(_, hint)| hint.as_ref().is_some_and(|h| numeric_hints.contains(&h.to_uppercase().as_str())))
        .collect();
    if !zero_flags.is_empty() {
        println!();
        println!("# Boundary: zero value for numeric flags");
        for (flag, _) in &zero_flags {
            println!("run {}", build_run(Some(&format!("{}=0", flag)), None, pat, fil));
        }

        // Negative and overflow boundaries (up to 3 flags)
        let boundary_flags: Vec<&&(String, Option<String>)> = zero_flags.iter().take(3).collect();
        if !boundary_flags.is_empty() {
            println!();
            println!("# Boundary: negative and overflow values");
            for (flag, _) in &boundary_flags {
                println!("run {}", build_run(Some(&format!("{}=-1", flag)), None, pat, fil));
                println!("run {}", build_run(Some(&format!("{}=2147483647", flag)), None, pat, fil));
            }
        }
    }

    // Multi-file run
    if fil.is_some() {
        println!();
        println!("# Multi-file: tests header/total behavior");
        let mut parts: Vec<String> = run_prefix.iter().map(|a| format!("\"{}\"", a)).collect();
        if let Some(p) = pat { parts.push(format!("\"{}\"", p)); }
        parts.push("\"input.txt\"".into());
        parts.push("\"empty.txt\"".into());
        println!("run {}", parts.join(" "));
    }

    // Error provocation: nonexistent file and flag-like filename
    {
        println!();
        println!("# Error: nonexistent file");
        println!("run {}", build_run(None, None, pat, Some("nonexistent-file.txt")));
        println!();
        println!("# Edge case: flag-like filename via -- separator");
        println!("run {}", build_run(Some("--"), None, pat, Some("-rf")));
    }

    // Stdin hint
    if takes_stdin {
        println!();
        println!("# This binary may read stdin:");
        if pat.is_some() {
            println!("# run {}", build_run(None, None, pat, None));
            println!("#   stdin \"line one\" \"line two\" \"line three\"");
        } else {
            let prefix = if sub_args.is_empty() { String::new() } else {
                sub_args.iter().map(|a| format!("\"{}\" ", a)).collect()
            };
            println!("# run {}\"-n\"", prefix);
            println!("#   stdin \"line one\" \"line two\" \"line three\"");
        }
    }

    // Commented-out pairwise combination hint
    if short_flags.len() >= 3 {
        println!();
        println!("# Uncomment for pairwise flag combination testing:");
        let combo_flags: Vec<&String> = short_flags.iter().take(8).collect();
        let combo_base = if let Some(f) = fil { format!("\"{}\"", f) } else { String::new() };
        let combo_prefix = if pat.is_some() {
            format!("\"{}\" {}", pat.unwrap(), combo_base)
        } else {
            combo_base
        };
        println!("# combine {}", combo_prefix);
        for flag in &combo_flags {
            println!("#   \"{}\"", flag);
        }
    }

    eprintln!();
    eprintln!("# Pipe to a file, then run:");
    eprintln!("#   bgrid {} > probes.probe", cmd_label);
    eprintln!("#   bgrid {} probes.probe", cmd_label);

    Ok(())
}

fn try_help(binary: &str, sub_args: &[&str], sandbox: &sandbox::Sandbox) -> Result<String> {
    // Create a minimal tempdir as workspace for the sandboxed --help call
    let tmp = tempfile::Builder::new().prefix("bgrid_help_").tempdir()
        .context("create help sandbox")?;

    for help_flag in &["--help", "-h"] {
        let mut args: Vec<&str> = sub_args.to_vec();
        args.push(help_flag);
        let env = std::collections::HashMap::new();
        let mut cmd = sandbox.command(binary, &args, tmp.path(), &env, None);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let output = cmd.output();

        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            // Skip bwrap's own errors (binary not found, etc.)
            if stderr.starts_with("bwrap:") && stdout.is_empty() {
                continue;
            }
            let text = if stdout.len() > stderr.len() { stdout } else { stderr };
            if text.contains('-') && text.len() > 20 {
                return Ok(text.to_string());
            }
        }
    }
    anyhow::bail!("could not get help text from {} (tried --help and -h)", binary)
}

/// Infer positional arguments from usage line.
/// Returns (pattern_arg, file_arg) — pattern_arg is Some for tools like grep/awk/sed.
fn infer_base_args(help_text: &str) -> (Option<String>, Option<String>) {
    let pattern_words = ["PATTERN", "PATTERNS", "EXPRESSION", "REGEX", "REGEXP",
                         "BRE", "ERE", "SCRIPT", "PROGRAM"];

    for line in help_text.lines().take(10) {
        let upper = line.to_uppercase();

        // Check for pattern-before-file: "PATTERN [FILE]", "PATTERNS [FILE]..."
        let has_pattern = pattern_words.iter().any(|p| {
            // Must appear as a standalone word (not inside a flag like --pattern=X)
            upper.contains(&format!(" {} ", p))
            || upper.contains(&format!(" {}...", p))
            || upper.contains(&format!("] {} ", p))
            || upper.ends_with(&format!(" {}", p))
        });

        // Also detect quoted program patterns: 'program' file, {script} [file]
        let has_quoted_program = line.contains("'program'")
            || line.contains("{script")
            || line.contains("'script");

        let has_file = {
            let lower = line.to_lowercase();
            lower.contains("[file]") || lower.contains("<file>")
                || lower.contains("[file ...]") || lower.contains("[file]...")
                || lower.contains("file ...") || lower.contains("[input-file]")
        };
        let has_dir = {
            let lower = line.to_lowercase();
            lower.contains("[dir]") || lower.contains("[directory]") || lower.contains("<dir>")
        };

        if has_pattern || has_quoted_program {
            let file_arg = if has_file {
                Some("input.txt".into())
            } else if has_dir {
                Some(".".into())
            } else {
                Some("input.txt".into()) // assume file if pattern tool
            };
            return (Some("alpha".into()), file_arg);
        }

        if has_file {
            return (None, Some("input.txt".into()));
        }
        if has_dir {
            return (None, Some(".".into()));
        }
    }
    (None, None)
}

fn default_value(hint: &str) -> String {
    match hint.to_uppercase().as_str() {
        "NUM" | "NUMBER" | "N" | "SIZE" | "COLS" | "WIDTH" => "10".into(),
        "FILE" | "PATH" | "FILENAME" => "input.txt".into(),
        "DIR" | "DIRECTORY" => ".".into(),
        "PATTERN" | "PAT" | "REGEX" => ".*".into(),
        _ => hint.to_lowercase(),
    }
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
            println!("    {}. {}", i + 1, format_setup_cmd(cmd));
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

fn format_setup_cmd(cmd: &parse::SetupCommand) -> String {
    match cmd {
        parse::SetupCommand::CreateFile { path, content } => {
            match content {
                parse::FileContent::Lines(l) if l.len() <= 1 => format!("file {:?} {:?}", path, l.first().map(|s| s.as_str()).unwrap_or("")),
                parse::FileContent::Lines(l) => format!("file {:?} ({} lines)", path, l.len()),
                parse::FileContent::Size(n) => format!("file {:?} size {}", path, n),
                parse::FileContent::Empty => format!("file {:?} empty", path),
                parse::FileContent::From(src) => format!("file {:?} from {:?}", path, src),
            }
        }
        parse::SetupCommand::CreateDir { path } => format!("dir {:?}", path),
        parse::SetupCommand::CreateLink { path, target } => format!("file {:?} -> {:?}", path, target),
        parse::SetupCommand::SetProps { path, .. } => format!("props {:?} ...", path),
        parse::SetupCommand::SetEnv { var, value } => format!("env {} {:?}", var, value),
        parse::SetupCommand::Remove { path } => format!("remove {:?}", path),
        parse::SetupCommand::RemoveEnv { var } => format!("remove env {}", var),
        parse::SetupCommand::Invoke { args } => format!("invoke {}", args.iter().map(|a| format!("{:?}", a)).collect::<Vec<_>>().join(" ")),
    }
}

fn cmd_run(binary: &str, test_path: &PathBuf, sandbox: &sandbox::Sandbox, compact: bool) -> Result<()> {
    let script = load_script(test_path)?;

    execute::validate_from_references(&script);

    let actual_runs = execute::count_cells(&script);
    eprintln!(
        "{} contexts, {} runs, {} cells",
        script.contexts.len(), script.runs.len(), actual_runs
    );

    // Execute
    let probe_dir = test_path.parent().unwrap_or(std::path::Path::new("."));
    let grid = execute::run_grid(binary, &script, probe_dir, sandbox)?;

    // Build results
    let mut out = String::new();
    out.push_str(&format!(
        "# Results for {}\n# {} contexts, {} runs, {} cells\n",
        test_path.file_name().unwrap_or_default().to_string_lossy(),
        grid.context_count, script.runs.len(), grid.cells.len()
    ));

    for (ctx, err) in &grid.setup_failures {
        out.push_str(&format!("\n# SETUP FAILED {}: {}\n", ctx, err));
    }

    // Collect all observations indexed by args for diff lookups
    let obs_by_args: HashMap<(&[String], &str), &execute::Observation> = grid.cells.iter()
        .map(|((ctx, ri), obs)| {
            let args = &script.runs[*ri].args;
            ((args.as_slice(), ctx.as_str()), obs)
        })
        .collect();

    // Per-run analysis: collect majority observation and metadata for each run
    struct RunInfo<'a> {
        args_str: String,
        majority_obs: &'a execute::Observation,
        majority_names: Vec<&'a str>,
        groups: Vec<(Vec<&'a str>, &'a execute::Observation)>,
        sensitive_parts: Vec<String>,
        universals: Vec<String>,
        obs_list: Vec<(&'a str, &'a execute::Observation)>,
        from_ref: Option<&'a Vec<String>>,
    }

    let mut run_infos: Vec<RunInfo> = Vec::new();

    for (ri, run) in script.runs.iter().enumerate() {
        let args_str = output::format_args(&run.args);

        // Collect observations across contexts
        let mut obs_list: Vec<(&str, &execute::Observation)> = Vec::new();
        for ctx in &script.contexts {
            if let Some(obs) = grid.cells.get(&(ctx.name.clone(), ri)) {
                obs_list.push((&ctx.name, obs));
            }
        }

        if obs_list.is_empty() {
            eprintln!("  run {}: (no observations)", args_str);
            continue;
        }

        // Collapse identical observations
        let groups = execute::collapse(&obs_list);
        let largest_idx = groups.iter().enumerate()
            .max_by_key(|(_, (names, _))| names.len())
            .map(|(i, _)| i).unwrap_or(0);
        let (majority_names, majority_obs) = &groups[largest_idx];

        // Compute quantified sensitivity — effect size per perturbation
        let majority_lines: usize = majority_obs.stdout.lines().count();
        let mut sensitive_parts: Vec<String> = Vec::new();
        for (i, (names, obs)) in groups.iter().enumerate() {
            if i == largest_idx { continue; }
            for name in names {
                if !name.contains(" / ") { continue; }
                let label = name.split(" / ").last().unwrap_or(name);
                let obs_lines = obs.stdout.lines().count();
                let mut effects = Vec::new();
                let line_diff = obs_lines as i64 - majority_lines as i64;
                if line_diff != 0 {
                    effects.push(format!("{:+} lines", line_diff));
                } else if obs.stdout != majority_obs.stdout {
                    effects.push("reordered".into());
                }
                if obs.exit_code != majority_obs.exit_code {
                    effects.push(format!("exit {}→{}",
                        majority_obs.exit_code.unwrap_or(-1),
                        obs.exit_code.unwrap_or(-1)));
                }
                if effects.is_empty() {
                    sensitive_parts.push(label.to_string());
                } else {
                    sensitive_parts.push(format!("{} ({})", label, effects.join(", ")));
                }
            }
        }

        // Compute universals
        let exit_codes: Vec<i32> = obs_list.iter()
            .map(|(_, o)| o.exit_code.unwrap_or(-1))
            .collect::<HashSet<_>>().into_iter().collect();
        let all_stdout_nonempty = obs_list.iter().all(|(_, o)| !o.stdout.trim().is_empty());
        let all_stdout_empty = obs_list.iter().all(|(_, o)| o.stdout.trim().is_empty());
        let all_has_fs = obs_list.iter().all(|(_, o)| !o.fs_changes.is_empty());
        let mut universals = Vec::new();
        let has_signal = exit_codes.iter().any(|c| *c > 128);
        if exit_codes.len() == 1 {
            universals.push(format!("exit {}", output::format_exit(exit_codes[0])));
        } else {
            let mut sorted = exit_codes.clone();
            sorted.sort();
            universals.push(format!("exit {{{}}}", sorted.iter().map(|c| output::format_exit(*c)).collect::<Vec<_>>().join(",")));
        }
        if has_signal {
            universals.push("SIGNAL".into());
        }
        if all_stdout_nonempty { universals.push("stdout not empty".into()); }
        if all_stdout_empty { universals.push("stdout empty".into()); }
        if all_has_fs { universals.push("modifies filesystem".into()); }

        if !sensitive_parts.is_empty() {
            sensitive_parts.sort_by(|a, b| {
                let a_has = a.contains('(');
                let b_has = b.contains('(');
                b_has.cmp(&a_has)
            });
        }

        // Stderr feedback
        let exit = obs_list[0].1.exit_code.unwrap_or(-1);
        let sens_label = if sensitive_parts.is_empty() { String::new() } else {
            format!(" [{}]", sensitive_parts.join(", "))
        };
        eprintln!("  run {}: {}/{} distinct, exit {}{}", args_str, groups.len(), obs_list.len(), output::format_exit(exit), sens_label);

        run_infos.push(RunInfo {
            args_str,
            majority_obs,
            majority_names: majority_names.clone(),
            groups,
            sensitive_parts,
            universals,
            obs_list,
            from_ref: run.diff_from.as_ref(),
        });
    }

    // --- Output ---
    if compact {
        // Compact mode: group runs by identical majority observation
        // Only group runs with the same from-reference and in-scope
        struct RunGroup<'a> {
            run_labels: Vec<String>,
            majority_obs: &'a execute::Observation,
            majority_names: Vec<&'a str>,
            universals: Vec<String>,
            sensitive_parts: Vec<String>,
            from_ref: Option<&'a Vec<String>>,
            // Per-run vs-diffs for runs in from blocks
            vs_diffs: Vec<(String, String)>, // (args_str, diff_summary)
        }

        let mut run_groups: Vec<RunGroup> = Vec::new();

        for info in &run_infos {
            // Find existing group with identical majority obs + same scope
            let found = run_groups.iter_mut().find(|g| {
                g.majority_obs.stdout == info.majority_obs.stdout
                && g.majority_obs.stderr == info.majority_obs.stderr
                && g.majority_obs.exit_code == info.majority_obs.exit_code
                && g.majority_obs.fs_changes == info.majority_obs.fs_changes
                && g.from_ref == info.from_ref
            });

            // Compute vs-diff for from-block runs
            let vs_diff = if let Some(ref_args) = info.from_ref {
                let majority_ctx = info.majority_names[0];
                if let Some(ref_obs) = obs_by_args.get(&(ref_args.as_slice(), majority_ctx)) {
                    let diff = execute::compute_diff(ref_obs, info.majority_obs);
                    if !diff.is_empty() {
                        Some(diff.join("; "))
                    } else {
                        Some("identical".into())
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(group) = found {
                group.run_labels.push(info.args_str.clone());
                if let Some(diff) = vs_diff {
                    group.vs_diffs.push((info.args_str.clone(), diff));
                }
                // Merge sensitivity — take union
                for sp in &info.sensitive_parts {
                    if !group.sensitive_parts.contains(sp) {
                        group.sensitive_parts.push(sp.clone());
                    }
                }
            } else {
                let mut vs_diffs = Vec::new();
                if let Some(diff) = vs_diff {
                    vs_diffs.push((info.args_str.clone(), diff));
                }
                run_groups.push(RunGroup {
                    run_labels: vec![info.args_str.clone()],
                    majority_obs: info.majority_obs,
                    majority_names: info.majority_names.clone(),
                    universals: info.universals.clone(),
                    sensitive_parts: info.sensitive_parts.clone(),
                    from_ref: info.from_ref,
                    vs_diffs,
                });
            }
        }

        // Output grouped results
        let total_runs: usize = run_groups.iter().map(|g| g.run_labels.len()).sum();
        out.push_str(&format!("\n# {} runs in {} behavioral groups\n", total_runs, run_groups.len()));

        for (gi, group) in run_groups.iter().enumerate() {
            out.push_str(&format!("\n## group {} ({} runs): {}\n",
                gi + 1,
                group.run_labels.len(),
                group.run_labels.join(", ")));

            // Summary
            let mut summary = group.universals.clone();
            if !group.sensitive_parts.is_empty() {
                summary.push(format!("sensitive to: {}", group.sensitive_parts.join(", ")));
            }
            if !summary.is_empty() {
                out.push_str(&format!("  {}\n", summary.join(" | ")));
            }

            // Majority output
            out.push_str(&format!("  {}:\n", output::format_context_group(&group.majority_names, grid.context_count)));
            output::format_obs(&mut out, group.majority_obs, "    ");

            // vs-diffs (how each run differs from its from-reference)
            if !group.vs_diffs.is_empty() {
                let ref_str = group.from_ref.map(|r| output::format_args(r)).unwrap_or_default();
                // If all vs-diffs are the same, show once
                let all_same = group.vs_diffs.iter().all(|(_, d)| *d == group.vs_diffs[0].1);
                if all_same {
                    out.push_str(&format!("  vs {}: {}\n", ref_str, group.vs_diffs[0].1));
                } else {
                    for (args, diff) in &group.vs_diffs {
                        out.push_str(&format!("  {} vs {}: {}\n", args, ref_str, diff));
                    }
                }
            }
        }
    } else {
        // Full mode: per-run output (original behavior)
        for info in &run_infos {
            out.push_str(&format!("\nrun {}:\n", info.args_str));

            let mut summary_parts = Vec::new();
            summary_parts.push(format!("{}/{} distinct", info.groups.len(), info.obs_list.len()));
            if !info.universals.is_empty() {
                summary_parts.push(info.universals.join(", "));
            }
            if !info.sensitive_parts.is_empty() {
                summary_parts.push(format!("sensitive to: {}", info.sensitive_parts.join(", ")));
            }
            out.push_str(&format!("  {} | {}\n",
                output::format_context_group(&info.obs_list.iter().map(|(n, _)| *n).collect::<Vec<_>>(), info.obs_list.len()),
                summary_parts.join(" | ")
            ));

            // Majority group
            let largest_idx = info.groups.iter().enumerate()
                .max_by_key(|(_, (names, _))| names.len())
                .map(|(i, _)| i).unwrap_or(0);
            out.push_str(&format!("  {}:\n", output::format_context_group(&info.majority_names, info.obs_list.len())));
            output::format_obs(&mut out, info.majority_obs, "    ");

            // Differing groups
            for (i, (names, obs)) in info.groups.iter().enumerate() {
                if i == largest_idx { continue; }
                out.push_str(&format!("  differs in {}:\n", names.join(", ")));
                output::format_obs(&mut out, obs, "    ");
                let delta = execute::compute_diff(info.majority_obs, obs);
                if !delta.is_empty() {
                    out.push_str(&format!("    delta: {}\n", delta.join("; ")));
                }
            }

            // From-block diffs
            if let Some(ref_args) = info.from_ref {
                out.push_str(&format!("  from {}:\n", output::format_args(ref_args)));
                for (ctx_name, obs) in &info.obs_list {
                    let ref_obs = obs_by_args.get(&(ref_args.as_slice(), *ctx_name));
                    if let Some(ref_obs) = ref_obs {
                        let diff = execute::compute_diff(ref_obs, obs);
                        if diff.is_empty() { continue; }
                        out.push_str(&format!("    {}:\n", ctx_name));
                        for line in &diff {
                            out.push_str(&format!("      {}\n", line));
                        }
                    }
                }
                let majority_ctx = info.majority_names[0];
                if let Some(ref_obs) = obs_by_args.get(&(ref_args.as_slice(), majority_ctx)) {
                    let diff = execute::compute_diff(ref_obs, info.majority_obs);
                    if !diff.is_empty() && info.majority_names.len() > 1 {
                        out.push_str(&format!("    ({} contexts):\n", info.majority_names.len()));
                        for line in &diff {
                            out.push_str(&format!("      {}\n", line));
                        }
                    }
                }
            }
        }
    }

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
