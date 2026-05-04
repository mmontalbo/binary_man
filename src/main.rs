use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

mod execute;
mod parse;
mod sandbox;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let dry_run = args.iter().any(|a| a == "--dry-run");
    let compact = args.iter().any(|a| a == "--compact");
    let positional: Vec<&String> = args.iter().skip(1).filter(|a| !a.starts_with("--")).collect();

    if positional.is_empty() {
        eprintln!("Usage: bman [--dry-run] [--compact] <binary> [<probe-file>]");
        eprintln!("       bman <binary>                        discover flags from --help");
        eprintln!("       bman <binary> <file.probe>            run observation grid");
        eprintln!("       bman --compact <binary> <file.probe>  summary-only output");
        std::process::exit(1);
    }

    // If last arg ends in .probe, it's run mode. Otherwise, discovery.
    let last = positional.last().unwrap();
    if last.ends_with(".probe") {
        let binary = positional[0];
        let test_path = PathBuf::from(last.as_str());
        if dry_run {
            cmd_dry_run(binary, &test_path)
        } else {
            let sandbox = sandbox::Sandbox::new()?;
            cmd_run(binary, &test_path, &sandbox, compact)
        }
    } else {
        let sandbox = sandbox::Sandbox::new()?;
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

    // Infer base invocation from usage line
    let base_arg = infer_base_arg(&help_text);
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
    println!("context \"base\"");
    println!("  file \"input.txt\" \"alpha\" \"alpha\" \"10\" \"2\" \"BETA\" \"  spaced  \" \"\"");
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
    println!("  file \"input.txt\" \"a:1:x\" \"b:2:y\" \"c:3:z\"  # structured");
    println!();

    // Structural perturbations — vary what exists
    println!("vary from \"base\"");
    println!("  remove \".hidden\"");
    println!("  remove \"subdir\"");
    println!("  remove \"link.txt\"");
    println!("  remove \"exec.sh\"");
    println!();

    // Property perturbations — vary file attributes
    println!("vary from \"base\"");
    println!("  props \"input.txt\" readonly");
    println!("  props \"input.txt\" mtime old");
    println!("  file \"input.txt\" size 1");
    println!();

    // Base invocation + from block
    if let Some(ref base) = base_arg {
        // Prefix with subcommand args if any
        let run_prefix: Vec<&str> = sub_args.to_vec();
        let base_run_args = if run_prefix.is_empty() {
            format!("\"{}\"", base)
        } else {
            let mut parts: Vec<String> = run_prefix.iter().map(|a| format!("\"{}\"", a)).collect();
            parts.push(format!("\"{}\"", base));
            parts.join(" ")
        };

        println!("run {}", base_run_args);
        println!();
        println!("from {}", base_run_args);

        for flag in &short_flags {
            let mut parts: Vec<String> = run_prefix.iter().map(|a| format!("\"{}\"", a)).collect();
            parts.push(format!("\"{}\"", flag));
            if !base.is_empty() {
                parts.push(format!("\"{}\"", base));
            }
            println!("  run {}", parts.join(" "));
        }
        for (flag, hint) in &long_flags {
            let mut parts: Vec<String> = run_prefix.iter().map(|a| format!("\"{}\"", a)).collect();
            if let Some(h) = hint {
                parts.push(format!("\"{}={}\"", flag, default_value(h)));
            } else {
                parts.push(format!("\"{}\"", flag));
            }
            if !base.is_empty() {
                parts.push(format!("\"{}\"", base));
            }
            println!("  run {}", parts.join(" "));
        }
    } else {
        // No base invocation — flat run list
        let run_prefix: Vec<&str> = sub_args.to_vec();

        for flag in &short_flags {
            let mut parts: Vec<String> = run_prefix.iter().map(|a| format!("\"{}\"", a)).collect();
            parts.push(format!("\"{}\"", flag));
            println!("run {}", parts.join(" "));
        }
        for (flag, hint) in &long_flags {
            let mut parts: Vec<String> = run_prefix.iter().map(|a| format!("\"{}\"", a)).collect();
            if let Some(h) = hint {
                parts.push(format!("\"{}={}\"", flag, default_value(h)));
            } else {
                parts.push(format!("\"{}\"", flag));
            }
            println!("run {}", parts.join(" "));
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
        let run_prefix: Vec<&str> = sub_args.to_vec();
        for (flag, _) in &zero_flags {
            let mut parts: Vec<String> = run_prefix.iter().map(|a| format!("\"{}\"", a)).collect();
            parts.push(format!("\"{}=0\"", flag));
            if let Some(ref base) = base_arg {
                parts.push(format!("\"{}\"", base));
            }
            println!("run {}", parts.join(" "));
        }
    }

    // Multi-file run
    if base_arg.is_some() {
        println!();
        println!("# Multi-file: tests header/total behavior");
        let run_prefix: Vec<&str> = sub_args.to_vec();
        let mut parts: Vec<String> = run_prefix.iter().map(|a| format!("\"{}\"", a)).collect();
        parts.push("\"input.txt\"".into());
        parts.push("\"empty.txt\"".into());
        println!("run {}", parts.join(" "));
    }

    // Error provocation: nonexistent file
    {
        println!();
        println!("# Error: nonexistent file");
        let run_prefix: Vec<&str> = sub_args.to_vec();
        let mut parts: Vec<String> = run_prefix.iter().map(|a| format!("\"{}\"", a)).collect();
        parts.push("\"nonexistent-file.txt\"".into());
        println!("run {}", parts.join(" "));
    }

    // Stdin hint
    if takes_stdin {
        println!();
        println!("# This binary may read stdin:");
        let prefix = if sub_args.is_empty() { String::new() } else {
            sub_args.iter().map(|a| format!("\"{}\" ", a)).collect()
        };
        println!("# run {}\"-n\"", prefix);
        println!("#   stdin \"line one\" \"line two\" \"line three\"");
    }

    eprintln!();
    eprintln!("# Pipe to a file, then run:");
    eprintln!("#   bman {} > probes.probe", cmd_label);
    eprintln!("#   bman {} probes.probe", cmd_label);

    Ok(())
}

fn try_help(binary: &str, sub_args: &[&str], sandbox: &sandbox::Sandbox) -> Result<String> {
    // Create a minimal tempdir as workspace for the sandboxed --help call
    let tmp = tempfile::Builder::new().prefix("bman_help_").tempdir()
        .context("create help sandbox")?;

    for help_flag in &["--help", "-h"] {
        let mut args: Vec<&str> = sub_args.to_vec();
        args.push(help_flag);
        let env = std::collections::HashMap::new();
        let mut cmd = sandbox.command(binary, &args, tmp.path(), &env);
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

fn infer_base_arg(help_text: &str) -> Option<String> {
    // Look for usage patterns like [FILE]..., [DIR]..., <file>
    for line in help_text.lines().take(10) {
        let lower = line.to_lowercase();
        if lower.contains("[file]") || lower.contains("<file>") || lower.contains("[file ...]") {
            return Some("input.txt".into());
        }
        if lower.contains("[dir]") || lower.contains("[directory]") || lower.contains("<dir>") {
            return Some(".".into());
        }
    }
    None
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

fn cmd_dry_run(_binary: &str, test_path: &PathBuf) -> Result<()> {
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
        let args = format_args(&run.args);
        let scope = match &run.in_contexts {
            Some(ctxs) => format!(" in {}", ctxs.iter().map(|c| format!("{:?}", c)).collect::<Vec<_>>().join(", ")),
            None => String::new(),
        };
        let from = match &run.diff_from {
            Some(ref_args) => format!(" from {}", format_args(ref_args)),
            None => String::new(),
        };
        println!("  [{}] {}{}{}", i, args, from, scope);
    }

    // Count cells
    let mut cells = 0;
    for run in &script.runs {
        for ctx in &script.contexts {
            if let Some(ref scoped) = run.in_contexts {
                let matches = scoped.iter().any(|s| {
                    *s == ctx.name
                    || ctx.name.starts_with(&format!("{} / ", s))
                    || ctx.extends.as_deref() == Some(s.as_str())
                });
                if !matches { continue; }
            }
            cells += 1;
        }
    }
    println!("\ngrid: {} contexts x {} runs = {} cells", script.contexts.len(), script.runs.len(), cells);

    // Validate from-references
    for run in &script.runs {
        if let Some(ref ref_args) = run.diff_from {
            let has_match = script.runs.iter().any(|r| r.args == *ref_args && r.diff_from.is_none());
            if !has_match {
                let args_str = format_args(ref_args);
                eprintln!("warning: from {} has no matching standalone run (add `run {}` outside any from block)", args_str, args_str);
            }
        }
    }

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

    // Validate from-references
    for run in &script.runs {
        if let Some(ref ref_args) = run.diff_from {
            let has_match = script.runs.iter().any(|r| r.args == *ref_args && r.diff_from.is_none());
            if !has_match {
                let args_str = format_args(ref_args);
                eprintln!("warning: from {} has no matching standalone run (add `run {}` outside any from block)", args_str, args_str);
            }
        }
    }

    // Count actual runs
    let mut actual_runs = 0;
    for run in &script.runs {
        for ctx in &script.contexts {
            if let Some(ref scoped) = run.in_contexts {
                let matches = scoped.iter().any(|s| {
                    *s == ctx.name
                    || ctx.name.starts_with(&format!("{} / ", s))
                    || ctx.extends.as_deref() == Some(s.as_str())
                });
                if !matches { continue; }
            }
            actual_runs += 1;
        }
    }
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

    for (ri, run) in script.runs.iter().enumerate() {
        let args_str = format_args(&run.args);
        out.push_str(&format!("\nrun {}:\n", args_str));

        // Collect observations across contexts
        let mut obs_list: Vec<(&str, &execute::Observation)> = Vec::new();
        for ctx in &script.contexts {
            if let Some(obs) = grid.cells.get(&(ctx.name.clone(), ri)) {
                obs_list.push((&ctx.name, obs));
            }
        }

        if obs_list.is_empty() {
            out.push_str("  (no observations)\n");
            continue;
        }

        // Collapse identical observations
        let groups = collapse(&obs_list);
        let largest_idx = groups.iter().enumerate()
            .max_by_key(|(_, (names, _))| names.len())
            .map(|(i, _)| i).unwrap_or(0);
        let (majority_names, majority_obs) = &groups[largest_idx];

        // Compute sensitivity
        let sensitive: Vec<&str> = groups.iter().enumerate()
            .filter(|(i, _)| *i != largest_idx)
            .flat_map(|(_, (names, _))| names.iter().copied())
            .filter(|n| n.contains(" / "))
            .collect();

        // Compute universals
        let all_exit_same = obs_list.iter().all(|(_, o)| o.exit_code == obs_list[0].1.exit_code);
        let all_stdout_nonempty = obs_list.iter().all(|(_, o)| !o.stdout.trim().is_empty());
        let all_stdout_empty = obs_list.iter().all(|(_, o)| o.stdout.trim().is_empty());
        let all_has_fs = obs_list.iter().all(|(_, o)| !o.fs_changes.is_empty());
        let mut universals = Vec::new();
        if all_exit_same {
            universals.push(format!("exit {}", obs_list[0].1.exit_code.unwrap_or(-1)));
        }
        if all_stdout_nonempty { universals.push("stdout not empty".into()); }
        if all_stdout_empty { universals.push("stdout empty".into()); }
        if all_has_fs { universals.push("modifies filesystem".into()); }

        // Summary line
        let mut summary_parts = Vec::new();
        summary_parts.push(format!("{} groups", groups.len()));
        if !universals.is_empty() {
            summary_parts.push(universals.join(", "));
        }
        if !sensitive.is_empty() {
            summary_parts.push(format!("sensitive to: {}",
                sensitive.iter()
                    .map(|s| s.split(" / ").last().unwrap_or(s))
                    .collect::<Vec<_>>().join(", ")
            ));
        }
        out.push_str(&format!("  {} | {}\n",
            format_context_group(&obs_list.iter().map(|(n, _)| *n).collect::<Vec<_>>(), obs_list.len()),
            summary_parts.join(" | ")
        ));

        if !compact {
            // Show majority group
            out.push_str(&format!("  {}:\n", format_context_group(majority_names, obs_list.len())));
            format_obs(&mut out, majority_obs, "    ");

            // Show differing groups with delta vs majority
            for (i, (names, obs)) in groups.iter().enumerate() {
                if i == largest_idx { continue; }
                out.push_str(&format!("  differs in {}:\n", names.join(", ")));
                format_obs(&mut out, obs, "    ");
                let delta = compute_diff(majority_obs, obs);
                if !delta.is_empty() {
                    out.push_str(&format!("    delta: {}\n", delta.join("; ")));
                }
            }

            // Diff from reference (if in a from block)
            if let Some(ref ref_args) = run.diff_from {
                out.push_str(&format!("  from {}:\n", format_args(ref_args)));

                for (ctx_name, obs) in &obs_list {
                    let ref_obs = obs_by_args.get(&(ref_args.as_slice(), *ctx_name));
                    if let Some(ref_obs) = ref_obs {
                        let diff = compute_diff(ref_obs, obs);
                        if diff.is_empty() {
                            continue; // same as reference in this context, skip
                        }
                        out.push_str(&format!("    {}:\n", ctx_name));
                        for line in &diff {
                            out.push_str(&format!("      {}\n", line));
                        }
                    }
                }

                // Summarize: show the diff that applies to the majority
                let majority_ctx = majority_names[0];
                if let Some(ref_obs) = obs_by_args.get(&(ref_args.as_slice(), majority_ctx)) {
                    let diff = compute_diff(ref_obs, majority_obs);
                    if !diff.is_empty() && majority_names.len() > 1 {
                        out.push_str(&format!("    ({} contexts):\n", majority_names.len()));
                        for line in &diff {
                            out.push_str(&format!("      {}\n", line));
                        }
                    }
                }
            }
        }

        // Stderr feedback
        let exit = obs_list[0].1.exit_code.unwrap_or(-1);
        let sens_label = if sensitive.is_empty() { String::new() } else {
            format!(" [{}]", sensitive.iter()
                .map(|s| s.split(" / ").last().unwrap_or(s))
                .collect::<Vec<_>>().join(", "))
        };
        eprintln!("  run {}: {} groups, exit {}{}", args_str, groups.len(), exit, sens_label);
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

fn format_args(args: &[String]) -> String {
    if args.is_empty() {
        "(no args)".into()
    } else {
        args.iter().map(|a| format!("\"{}\"", a)).collect::<Vec<_>>().join(" ")
    }
}

fn format_context_group(names: &[&str], total: usize) -> String {
    if names.len() == 1 {
        names[0].to_string()
    } else if names.len() == total {
        "all contexts".into()
    } else {
        format!("{} contexts ({})", names.len(), names.join(", "))
    }
}

fn format_obs(out: &mut String, obs: &execute::Observation, indent: &str) {
    let stdout_lines: Vec<&str> = obs.stdout.lines().collect();
    if stdout_lines.is_empty() {
        out.push_str(&format!("{}stdout: (empty)\n", indent));
    } else {
        out.push_str(&format!("{}stdout ({} lines):\n", indent, stdout_lines.len()));
        for line in stdout_lines.iter().take(20) {
            out.push_str(&format!("{}  {}\n", indent, line));
        }
        if stdout_lines.len() > 20 {
            out.push_str(&format!("{}  ... ({} more)\n", indent, stdout_lines.len() - 20));
        }
    }
    if !obs.stderr.trim().is_empty() {
        out.push_str(&format!("{}stderr: {}\n", indent, obs.stderr.trim()));
    }
    out.push_str(&format!("{}exit: {}\n", indent, obs.exit_code.unwrap_or(-1)));
    if !obs.fs_changes.is_empty() {
        out.push_str(&format!("{}fs:\n", indent));
        for change in &obs.fs_changes {
            match change {
                execute::FsChange::Created { path, size } => {
                    out.push_str(&format!("{}  created: {} ({} bytes)\n", indent, path, size));
                }
                execute::FsChange::Deleted { path } => {
                    out.push_str(&format!("{}  deleted: {}\n", indent, path));
                }
                execute::FsChange::Modified { path, detail } => {
                    out.push_str(&format!("{}  modified: {} ({})\n", indent, path, detail));
                }
            }
        }
    }
}

fn compute_diff(reference: &execute::Observation, option: &execute::Observation) -> Vec<String> {
    let mut lines = Vec::new();

    // Stdout diff
    let ref_lines: HashSet<&str> = reference.stdout.lines().collect();
    let opt_lines: HashSet<&str> = option.stdout.lines().collect();
    let ref_vec: Vec<&str> = reference.stdout.lines().collect();
    let opt_vec: Vec<&str> = option.stdout.lines().collect();

    let only_in_ref: Vec<&&str> = ref_vec.iter().filter(|l| !opt_lines.contains(**l)).collect();
    let only_in_opt: Vec<&&str> = opt_vec.iter().filter(|l| !ref_lines.contains(**l)).collect();
    let shared: Vec<&&str> = ref_vec.iter().filter(|l| opt_lines.contains(**l)).collect();

    if ref_vec == opt_vec {
        // stdout identical — check other dimensions
    } else if only_in_opt.is_empty() && only_in_ref.is_empty() && ref_vec != opt_vec {
        lines.push("stdout: same lines, different order".into());
    } else {
        if !only_in_opt.is_empty() {
            let preview: Vec<&str> = only_in_opt.iter().take(5).map(|l| **l).collect();
            lines.push(format!("{} only in this: {}", only_in_opt.len(), preview.join(", ")));
        }
        if !only_in_ref.is_empty() {
            let preview: Vec<&str> = only_in_ref.iter().take(5).map(|l| **l).collect();
            lines.push(format!("{} only in ref: {}", only_in_ref.len(), preview.join(", ")));
        }
        if !shared.is_empty() {
            lines.push(format!("{} shared", shared.len()));
        }
    }

    // Exit diff
    if reference.exit_code != option.exit_code {
        lines.push(format!("exit: {} → {}",
            reference.exit_code.unwrap_or(-1),
            option.exit_code.unwrap_or(-1)));
    }

    // Stderr diff
    if reference.stderr != option.stderr {
        if reference.stderr.is_empty() && !option.stderr.is_empty() {
            lines.push(format!("stderr added: {}", option.stderr.trim()));
        } else if !reference.stderr.is_empty() && option.stderr.is_empty() {
            lines.push("stderr removed".into());
        } else {
            lines.push("stderr changed".into());
        }
    }

    // Fs diff
    let ref_fs: HashSet<&execute::FsChange> = reference.fs_changes.iter().collect();
    let opt_fs: HashSet<&execute::FsChange> = option.fs_changes.iter().collect();
    let new_fs: Vec<_> = option.fs_changes.iter().filter(|c| !ref_fs.contains(c)).collect();
    let gone_fs: Vec<_> = reference.fs_changes.iter().filter(|c| !opt_fs.contains(c)).collect();
    for c in &new_fs {
        lines.push(format!("fs additional: {:?}", c));
    }
    for c in &gone_fs {
        lines.push(format!("fs missing: {:?}", c));
    }

    lines
}

fn collapse<'a>(
    obs_list: &[(&'a str, &'a execute::Observation)],
) -> Vec<(Vec<&'a str>, &'a execute::Observation)> {
    let mut groups: Vec<(Vec<&'a str>, &'a execute::Observation)> = Vec::new();
    for (ctx, obs) in obs_list {
        let found = groups.iter_mut().find(|(_, existing)| {
            existing.stdout == obs.stdout
                && existing.stderr == obs.stderr
                && existing.exit_code == obs.exit_code
                && existing.fs_changes == obs.fs_changes
        });
        if let Some((names, _)) = found {
            names.push(ctx);
        } else {
            groups.push((vec![ctx], obs));
        }
    }
    groups
}
