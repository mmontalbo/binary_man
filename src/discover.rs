//! Flag discovery and probe skeleton generation from --help text.

use anyhow::{Context, Result};
use regex::Regex;
use std::collections::{HashMap, HashSet};

use crate::parse::{
    self, Arg, FileContent, NamedContext, Property, Run, Script, SetupCommand,
};
use crate::sandbox::Sandbox;

/// Extracted flag info from --help text.
pub struct FlagInfo {
    pub descs: HashMap<String, String>,   // flag -> description
    pub aliases: HashMap<String, String>, // short -> long (and long -> short)
    pub all_flags: HashSet<String>,       // every flag discovered
}

/// Extract flag descriptions and aliases from --help text.
pub fn extract_flag_info(help_text: &str) -> FlagInfo {
    let mut descs: HashMap<String, String> = HashMap::new();
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut all_flags: HashSet<String> = HashSet::new();

    let re = Regex::new(
        r"^\s+(-[a-zA-Z0-9](?:,\s*--[a-zA-Z][-a-zA-Z0-9]*(?:=\S+)?)?|--[a-zA-Z][-a-zA-Z0-9]*(?:=\S+)?)\s{2,}(.+)"
    ).unwrap();

    for line in help_text.lines() {
        if let Some(cap) = re.captures(line) {
            let flags_part = cap[1].trim();
            let desc = cap[2].trim().to_string();
            let mut names: Vec<String> = Vec::new();
            for flag in flags_part.split(',') {
                let flag = flag.trim();
                let name = if let Some(eq) = flag.find('=') {
                    &flag[..eq]
                } else {
                    flag
                };
                if name.starts_with('-') && name != "--help" && name != "--version" {
                    descs.insert(name.to_string(), desc.clone());
                    all_flags.insert(name.to_string());
                    names.push(name.to_string());
                }
            }
            // Record alias pairs (e.g., -a <-> --all)
            if names.len() == 2 {
                aliases.insert(names[0].clone(), names[1].clone());
                aliases.insert(names[1].clone(), names[0].clone());
            }
        }
    }
    FlagInfo { descs, aliases, all_flags }
}

/// Try --help, then -h to get help text from a binary.
pub fn try_help(binary: &str, sub_args: &[&str], sandbox: &Sandbox) -> Result<String> {
    let tmp = tempfile::Builder::new().prefix("bgrid_help_").tempdir()
        .context("create help sandbox")?;

    for help_flag in &["--help", "-h"] {
        let mut args: Vec<&str> = sub_args.to_vec();
        args.push(help_flag);
        let env = HashMap::new();
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


/// Probe the binary with candidate arg patterns to discover which invocation
/// patterns succeed. Returns the list of working arg patterns (each is a vec
/// of positional args). Replaces help-text parsing with behavioral observation.
/// Returns (working_arg_patterns, stdin_works, probe_pattern).
/// `probe_pattern` is Some if a pattern-taking candidate (e.g. grep PATTERN FILE)
/// worked. The value is the concrete pattern string used during probing, which
/// callers should replace with `Arg::Extract` for context-derived matching.
pub fn probe_arg_patterns(
    binary: &str,
    sub_args: &[&str],
    sandbox: &Sandbox,
) -> (Vec<Vec<String>>, bool, Option<String>) {
    // Create a minimal workspace for probing
    let probe_dir = match tempfile::Builder::new().prefix("bgrid_probe_").tempdir() {
        Ok(d) => d,
        Err(_) => return (vec![vec!["input.txt".into()]], false, None), // fallback
    };
    let work_dir = probe_dir.path();

    // Set up minimal files for probing
    let probe_content = "cherry\napple\nbanana\n";
    let _ = std::fs::write(work_dir.join("input.txt"), probe_content);
    let _ = std::fs::write(work_dir.join("other.txt"), "hello world\n");
    let _ = std::fs::create_dir(work_dir.join("subdir"));
    let _ = std::fs::write(work_dir.join("subdir/nested.txt"), "nested\n");

    // Extract pattern from probe content — guaranteed to match input.txt
    let probe_pattern = probe_content.lines().next().unwrap_or("test").to_string();

    // Build candidates dynamically: replace the hardcoded "alpha" placeholder
    // with the actual first line of input.txt so pattern-taking tools (grep, sed)
    // get a pattern that matches their input.
    let candidates: Vec<Vec<&str>> = vec![
        vec![],                                       // no args
        vec!["input.txt"],                            // single file
        vec!["."],                                    // directory
        vec!["input.txt", "other.txt"],               // two files (diff, paste)
        vec!["input.txt", "subdir"],                  // file to directory
    ];
    // Pattern candidates use the probe_pattern (which matches content)
    let pattern_str = probe_pattern.as_str();
    let pattern_candidates: Vec<Vec<&str>> = vec![
        vec![pattern_str, "input.txt"],               // pattern + file (grep, sed)
        vec![pattern_str, "."],                       // pattern + directory (grep -r)
    ];

    let env = std::collections::HashMap::new();
    let mut working = Vec::new();
    let mut found_pattern_candidate = false;

    let all_candidates: Vec<&Vec<&str>> = candidates.iter().chain(pattern_candidates.iter()).collect();

    for candidate in &all_candidates {
        let mut args: Vec<&str> = sub_args.to_vec();
        args.extend(candidate.iter());

        let mut cmd = sandbox.command(binary, &args, work_dir, &env, None);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        if let Ok(output) = cmd.output() {
            let exit = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let has_output = !stdout.trim().is_empty();
            let has_fs_effect = {
                // Quick check: did any file get created or modified?
                // Compare against known initial files
                let after_count = std::fs::read_dir(work_dir)
                    .map(|d| d.count()).unwrap_or(0);
                after_count > 3 // started with input.txt, other.txt, subdir
            };

            // Pattern works if: exit 0, or exit 0 with output, or exit 0 with fs effect
            // Also accept exit 1 with output (grep no-match, diff differences-found)
            if exit == 0 || (exit <= 1 && has_output) || has_fs_effect {
                let pattern: Vec<String> = candidate.iter().map(|s| s.to_string()).collect();
                // Track whether this was a pattern-taking candidate
                if candidate.first() == Some(&pattern_str) {
                    found_pattern_candidate = true;
                }
                working.push(pattern);
            }

            // Reset workspace for next probe (in case a command modified files)
            let _ = std::fs::write(work_dir.join("input.txt"), probe_content);
            let _ = std::fs::write(work_dir.join("other.txt"), "hello world\n");
        }
    }

    // Probe stdin: try piping content with no file arg and with sub_args only
    let mut stdin_works = false;
    {
        let mut args: Vec<&str> = sub_args.to_vec();
        // Some tools need a flag to signal stdin (like sort with no args reads stdin)
        // Try with no args first
        let mut cmd = sandbox.command(binary, &args, work_dir, &env, None);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        if let Ok(mut child) = cmd.spawn() {
            // Write content to stdin
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(b"cherry\napple\nbanana\n");
            }
            // Wait with a short timeout
            if let Ok(output) = child.wait_with_output() {
                let exit = output.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&output.stdout);
                if (exit == 0 || exit == 1) && !stdout.trim().is_empty() {
                    stdin_works = true;
                }
            }
        }

        // Also try with "-" as explicit stdin marker
        if !stdin_works {
            args.push("-");
            let mut cmd2 = sandbox.command(binary, &args, work_dir, &env, None);
            cmd2.stdin(std::process::Stdio::piped());
            cmd2.stdout(std::process::Stdio::piped());
            cmd2.stderr(std::process::Stdio::piped());

            if let Ok(mut child) = cmd2.spawn() {
                if let Some(mut stdin) = child.stdin.take() {
                    use std::io::Write;
                    let _ = stdin.write_all(b"cherry\napple\nbanana\n");
                }
                if let Ok(output) = child.wait_with_output() {
                    let exit = output.status.code().unwrap_or(-1);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if (exit == 0 || exit == 1) && !stdout.trim().is_empty() {
                        stdin_works = true;
                    }
                }
            }
        }
    }

    if working.is_empty() {
        working.push(vec!["input.txt".into()]);
    }

    let probe_pat = if found_pattern_candidate { Some(probe_pattern) } else { None };
    (working, stdin_works, probe_pat)
}

/// Discovered subcommand with its behavioral classification.
#[derive(Debug)]
pub struct SubcommandInfo {
    pub name: String,
    pub exits_ok: bool,          // exit 0 in empty workspace
    pub modifies_fs: bool,       // created/modified files (state builder)
    pub recognized: bool,        // different error from "unknown command"
}

/// Probe the binary for subcommands by trying common verbs.
/// Returns subcommands classified as working, state-building, or recognized.
pub fn probe_subcommands(
    binary: &str,
    sandbox: &Sandbox,
) -> Vec<SubcommandInfo> {
    use crate::data;

    let probe_dir = match tempfile::Builder::new().prefix("bgrid_subcmd_").tempdir() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let work_dir = probe_dir.path();

    // Set up minimal files
    let _ = std::fs::write(work_dir.join("input.txt"), "cherry\napple\nbanana\n");
    let _ = std::fs::write(work_dir.join("other.txt"), "hello world\n");
    let _ = std::fs::create_dir(work_dir.join("subdir"));

    let env = std::collections::HashMap::new();

    // First, get the "unknown command" baseline by trying a nonsense word
    let mut baseline_cmd = sandbox.command(binary, &["xyzzy_not_a_command"], work_dir, &env, None);
    baseline_cmd.stdin(std::process::Stdio::null());
    baseline_cmd.stdout(std::process::Stdio::piped());
    baseline_cmd.stderr(std::process::Stdio::piped());
    let baseline_stderr = baseline_cmd.output()
        .map(|o| String::from_utf8_lossy(&o.stderr).to_string())
        .unwrap_or_default();

    let mut results = Vec::new();

    for &verb in data::SUBCOMMAND_CANDIDATES {
        // Reset workspace for each probe
        let _ = std::fs::write(work_dir.join("input.txt"), "cherry\napple\nbanana\n");
        let _ = std::fs::write(work_dir.join("other.txt"), "hello world\n");

        let before_count = count_files(work_dir);

        let mut cmd = sandbox.command(binary, &[verb], work_dir, &env, None);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let output = match cmd.output() {
            Ok(o) => o,
            Err(_) => continue,
        };

        let exit = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let after_count = count_files(work_dir);
        let modifies_fs = after_count != before_count;

        // A subcommand is "recognized" if it produces a structurally different
        // error than the unknown-command baseline (not just the command name echoed)
        let normalized_stderr = stderr.replace(verb, "___");
        let normalized_baseline = baseline_stderr.replace("xyzzy_not_a_command", "___");
        let recognized = exit == 0 || normalized_stderr != normalized_baseline;

        if recognized {
            results.push(SubcommandInfo {
                name: verb.to_string(),
                exits_ok: exit == 0,
                modifies_fs,
                recognized,
            });
        }
    }

    results
}

fn count_files(dir: &std::path::Path) -> usize {
    walkdir_count(dir).unwrap_or(0)
}

fn walkdir_count(dir: &std::path::Path) -> Option<usize> {
    let mut count = 0;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        count += 1;
        if entry.path().is_dir() && !entry.path().is_symlink() {
            count += walkdir_count(&entry.path()).unwrap_or(0);
        }
    }
    Some(count)
}

/// Map a value hint from --help to a reasonable default.
pub fn default_value(hint: &str) -> String {
    let upper = hint.to_uppercase();
    let upper = upper.as_str();
    match upper {
        "NUM" | "NUMBER" | "N" | "SIZE" | "COLS" | "WIDTH" | "COUNT" | "LINES" | "BYTES"
        | "MAX" | "PROCS" | "DEPTH" | "JOBS" | "LEVEL" => "10".into(),
        "FILE" | "PATH" | "FILENAME" => "input.txt".into(),
        "DIR" | "DIRECTORY" => ".".into(),
        "PATTERN" | "PAT" | "REGEX" => ".*".into(),
        "LIST" | "FIELDS" | "FIELD_LIST" => "1".into(),
        "RANGE" | "SET1" | "SET2" | "CHARS" => "1-3".into(),
        "CHAR" | "DELIM" | "SEP" | "CHARACTER" => ",".into(),
        "FORMAT" | "FMT" => "%s".into(),
        "MODE" => "644".into(),
        "WORD" | "STYLE" | "TYPE" | "METHOD" | "WHEN" => "auto".into(),
        "VAR" | "NAME" | "PREFIX" | "SUFFIX" | "STRING" | "STR" | "LABEL" | "TAG" => "test".into(),
        _ => {
            // Handle compound hints like "MAX-LINES", "MAX-PROCS", "MAX-CHARS"
            // by checking if any component is a known numeric keyword
            let numeric_words = ["MAX", "NUM", "COUNT", "SIZE", "LINES", "BYTES",
                                 "PROCS", "ARGS", "CHARS", "DEPTH", "JOBS", "WIDTH"];
            if upper.split('-').any(|part| numeric_words.contains(&part)) {
                return "10".into();
            }
            hint.to_lowercase()
        }
    }
}

/// Print a probe skeleton to stdout for manual authoring.
/// Generates the same Script as `generate_initial_script` and serializes it to probe text.
pub fn print_skeleton(
    binary: &str,
    sub_args: &[&str],
    sandbox: &Sandbox,
) -> Result<()> {
    let (script, flag_info) = generate_initial_script(binary, sub_args, sandbox)?;

    let cmd_label = if sub_args.is_empty() {
        binary.to_string()
    } else {
        format!("{} {}", binary, sub_args.join(" "))
    };

    println!("# Discovered from: {} --help", cmd_label);
    println!("# {} flags found", flag_info.all_flags.len());
    println!();

    // Serialize contexts
    for ctx in &script.contexts {
        // Skip vary-generated contexts (contain " / " in name) — print vary blocks instead below
        if ctx.name.contains(" / ") { continue; }
        println!("context \"{}\"", ctx.name);
        for cmd in &ctx.commands {
            println!("  {}", format_setup_cmd(cmd));
        }
        println!();
    }

    // Reconstruct vary blocks from generated contexts
    let vary_base = "many_files";
    let vary_contexts: Vec<&NamedContext> = script.contexts.iter()
        .filter(|c| c.name.starts_with(&format!("{} / ", vary_base)))
        .collect();
    if !vary_contexts.is_empty() {
        println!("vary from \"{}\"", vary_base);
        for ctx in &vary_contexts {
            // The last command is the perturbation
            if let Some(cmd) = ctx.commands.last() {
                println!("  {}", format_setup_cmd(cmd));
            }
        }
        println!();
    }

    // Serialize runs
    let mut current_from: Option<&Vec<Arg>> = None;
    for run in &script.runs {
        let args_str = run.args.iter().map(|a| a.display()).collect::<Vec<_>>().join(" ");

        match (&run.diff_from, current_from) {
            (Some(ref from), Some(prev)) if from == prev => {
                // Inside an existing from block
                println!("  run {}", args_str);
            }
            (Some(ref from), _) => {
                // New from block
                let from_str = from.iter().map(|a| a.display()).collect::<Vec<_>>().join(" ");
                println!();
                println!("from {}", from_str);
                println!("  run {}", args_str);
                current_from = Some(from);
            }
            (None, _) => {
                println!("run {}", args_str);
                current_from = None;
            }
        }
    }

    Ok(())
}

fn format_setup_cmd(cmd: &SetupCommand) -> String {
    match cmd {
        SetupCommand::CreateFile { path, content } => {
            match content {
                FileContent::Lines(lines) => {
                    let quoted: Vec<String> = lines.iter().map(|l| format!("\"{}\"", l)).collect();
                    format!("file \"{}\" {}", path, quoted.join(" "))
                }
                FileContent::Size(n) => format!("file \"{}\" size {}", path, n),
                FileContent::Empty => format!("file \"{}\" empty", path),
                FileContent::From(src) => format!("file \"{}\" from \"{}\"", path, src),
            }
        }
        SetupCommand::CreateDir { path } => format!("dir \"{}\"", path),
        SetupCommand::CreateLink { path, target } => format!("file \"{}\" -> \"{}\"", path, target),
        SetupCommand::SetProps { path, props } => {
            let p: Vec<&str> = props.iter().map(|p| match p {
                Property::Executable => "executable",
                Property::MtimeOld => "mtime old",
                Property::MtimeRecent => "mtime recent",
                Property::ReadOnly => "readonly",
            }).collect();
            format!("props \"{}\" {}", path, p.join(" "))
        }
        SetupCommand::SetEnv { var, value } => format!("env {} \"{}\"", var, value),
        SetupCommand::Remove { path } => format!("remove \"{}\"", path),
        SetupCommand::RemoveEnv { var } => format!("remove env {}", var),
        SetupCommand::Invoke { args } => {
            let quoted: Vec<String> = args.iter().map(|a| format!("\"{}\"", a)).collect();
            format!("invoke {}", quoted.join(" "))
        }
    }
}

/// Generate an initial Script from binary discovery.
/// Returns (Script, FlagInfo) for the iteration loop.
pub fn generate_initial_script(
    binary: &str,
    sub_args: &[&str],
    sandbox: &Sandbox,
) -> Result<(Script, FlagInfo)> {
    let help_text = try_help(binary, sub_args, sandbox)?;
    let flag_info = extract_flag_info(&help_text);

    let short_re = Regex::new(r"(?:^|\s)-([a-zA-Z0-9])\b").unwrap();
    let long_re = Regex::new(r"--([a-zA-Z][a-zA-Z0-9-]*)(?:[=\s]([A-Z][A-Z_]*))?").unwrap();

    let mut short_flags: Vec<String> = Vec::new();
    let mut long_flags: Vec<(String, Option<String>)> = Vec::new();
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

    // Use the discovered flags as the canonical set (not extract_flag_info's
    // restrictive parsing, which misses flags in non-coreutils help formats).
    let mut flag_info = flag_info;
    flag_info.all_flags = seen.clone();

    // Discover which invocation patterns work by probing the binary.
    // probe_pattern is Some("cherry") if the binary accepts pattern+file args
    // (e.g. grep, sed). In the grid, we replace this with Arg::Extract so the
    // pattern is derived from each context's input.txt at runtime.
    let (working_patterns, stdin_works, probe_pattern) = probe_arg_patterns(binary, sub_args, sandbox);
    if stdin_works {
        eprintln!("  stdin: accepted");
    }
    if probe_pattern.is_some() {
        eprintln!("  pattern: context-derived");
    }

    // Probe values for value-taking flags: try candidates and keep what works
    if let Some(first_pattern) = working_patterns.first() {
        let value_candidates = ["1", "auto", ",", ":", "input.txt", ".", "0"];
        let probe_dir = tempfile::Builder::new().prefix("bgrid_val_").tempdir().ok();
        if let Some(ref probe_dir) = probe_dir {
            let work_dir = probe_dir.path();
            let _ = std::fs::write(work_dir.join("input.txt"), "cherry\napple\nbanana\n");
            let _ = std::fs::write(work_dir.join("other.txt"), "hello world\n");
            let _ = std::fs::create_dir(work_dir.join("subdir"));
            let env = std::collections::HashMap::new();

            #[allow(clippy::needless_range_loop)]
            for i in 0..long_flags.len() {
                let (ref flag, ref hint) = long_flags[i];
                if hint.is_none() { continue; }

                let default_val = default_value(hint.as_ref().unwrap());
                let flag_arg = format!("{}={}", flag, default_val);
                let mut test_args: Vec<String> = sub_args.iter().map(|s| s.to_string()).collect();
                test_args.push(flag_arg);
                test_args.extend(first_pattern.iter().cloned());
                let test_refs: Vec<&str> = test_args.iter().map(|s| s.as_str()).collect();

                let mut cmd = sandbox.command(binary, &test_refs, work_dir, &env, None);
                cmd.stdin(std::process::Stdio::null());
                cmd.stdout(std::process::Stdio::piped());
                cmd.stderr(std::process::Stdio::piped());

                let default_works = cmd.output()
                    .map(|o| o.status.code().unwrap_or(-1) <= 1)
                    .unwrap_or(false);

                if default_works { continue; }

                // Default failed — try candidate values
                let mut found_value = None;
                for &candidate in &value_candidates {
                    let flag_arg2 = format!("{}={}", long_flags[i].0, candidate);
                    let mut test_args2: Vec<String> = sub_args.iter().map(|s| s.to_string()).collect();
                    test_args2.push(flag_arg2);
                    test_args2.extend(first_pattern.iter().cloned());
                    let test_refs2: Vec<&str> = test_args2.iter().map(|s| s.as_str()).collect();

                    let _ = std::fs::write(work_dir.join("input.txt"), "cherry\napple\nbanana\n");

                    let mut cmd2 = sandbox.command(binary, &test_refs2, work_dir, &env, None);
                    cmd2.stdin(std::process::Stdio::null());
                    cmd2.stdout(std::process::Stdio::piped());
                    cmd2.stderr(std::process::Stdio::piped());

                    if let Ok(output) = cmd2.output() {
                        if output.status.code().unwrap_or(-1) <= 1 {
                            found_value = Some(candidate.to_uppercase());
                            break;
                        }
                    }
                }
                if let Some(val) = found_value {
                    long_flags[i].1 = Some(val);
                }
            }
        }
    }

    // --- Base contexts ---
    // Five content levels × three structure levels × three property levels.
    // Property assignment cycles through Latin square pattern.
    // Data definitions live in data.rs; the assignment is here.
    //
    //              minimal         standard              deep
    // alpha        default         varied-perms          varied-times
    // numeric      varied-times    default               varied-perms
    // fielded      varied-perms    varied-times          default
    // formatted    default         varied-times          varied-perms
    // tabular      varied-times    varied-perms          default

    use crate::data;

    let content_alpha = data::content_alpha();
    let content_numeric = data::content_numeric();
    let content_fielded = data::content_fielded();
    let content_formatted = data::content_formatted();
    let content_tabular = data::content_tabular();

    let build_ctx = |name: &str, content: &[String],
                     structure_fn: fn(&[String]) -> Vec<SetupCommand>,
                     props_fn: fn(&mut Vec<SetupCommand>)| -> NamedContext {
        let mut cmds = structure_fn(content);
        props_fn(&mut cmds);
        NamedContext { name: name.into(), extends: None, commands: cmds }
    };

    let mut contexts: Vec<NamedContext> = vec![
        build_ctx("alpha_minimal",      &content_alpha,     data::structure_minimal,  data::props_default),
        build_ctx("alpha_standard",     &content_alpha,     data::structure_standard, data::props_perms),
        build_ctx("alpha_deep",         &content_alpha,     data::structure_deep,     data::props_times),
        build_ctx("numeric_minimal",    &content_numeric,   data::structure_minimal,  data::props_times),
        build_ctx("numeric_standard",   &content_numeric,   data::structure_standard, data::props_default),
        build_ctx("numeric_deep",       &content_numeric,   data::structure_deep,     data::props_perms),
        build_ctx("fielded_minimal",    &content_fielded,   data::structure_minimal,  data::props_perms),
        build_ctx("fielded_standard",   &content_fielded,   data::structure_standard, data::props_times),
        build_ctx("fielded_deep",       &content_fielded,   data::structure_deep,     data::props_default),
        build_ctx("formatted_minimal",  &content_formatted, data::structure_minimal,  data::props_default),
        build_ctx("formatted_standard", &content_formatted, data::structure_standard, data::props_times),
        build_ctx("formatted_deep",     &content_formatted, data::structure_deep,     data::props_perms),
        build_ctx("tabular_minimal",    &content_tabular,   data::structure_minimal,  data::props_times),
        build_ctx("tabular_standard",   &content_tabular,   data::structure_standard, data::props_perms),
        build_ctx("tabular_deep",       &content_tabular,   data::structure_deep,     data::props_default),
        NamedContext { name: "empty_dir".into(), extends: None, commands: vec![] },
    ];

    // --- Single-factor perturbations from numeric_standard (richest base) ---
    let vary_base = "numeric_standard";
    let perturbations = data::perturbations();

    let base_ctx = contexts.iter().find(|c| c.name == vary_base).unwrap().clone();
    for perturbation in &perturbations {
        let variant_name = format!("{} / {}", vary_base, parse::describe_perturbation(perturbation));
        let mut cmds = base_ctx.commands.clone();
        cmds.push(perturbation.clone());
        contexts.push(NamedContext { name: variant_name, extends: None, commands: cmds });
    }

    // Locale perturbation on alpha content (mixed case — sensitive to LC_ALL)
    let alpha_base = contexts.iter().find(|c| c.name == "alpha_minimal").unwrap().clone();
    let mut locale_cmds = alpha_base.commands.clone();
    locale_cmds.push(SetupCommand::SetEnv { var: "LC_ALL".into(), value: "en_US.UTF-8".into() });
    contexts.push(NamedContext {
        name: "alpha_minimal / env LC_ALL=en_US.UTF-8".into(),
        extends: None,
        commands: locale_cmds,
    });

    // --- Build runs from behaviorally-discovered arg patterns ---
    let mut runs: Vec<Run> = Vec::new();
    let sub_prefix: Vec<Arg> = sub_args.iter().map(|s| Arg::Literal(s.to_string())).collect();

    // Convert a positional arg string to Arg. If it matches the probe_pattern,
    // replace it with a context-derived extraction so the pattern comes from
    // each context's input.txt at runtime (guaranteed to match).
    let to_arg = |s: &String| -> Arg {
        if probe_pattern.as_ref() == Some(s) {
            Arg::Extract("head -n1 input.txt".into())
        } else {
            Arg::Literal(s.clone())
        }
    };

    // For each working arg pattern, generate a base run + flag runs
    for pattern in &working_patterns {
        // Build base args: sub_prefix + positional args
        let base_args: Vec<Arg> = sub_prefix.iter().cloned()
            .chain(pattern.iter().map(&to_arg))
            .collect();

        // Base run (no flags)
        runs.push(Run { args: base_args.clone(), in_contexts: None, stdin: None, diff_from: None });

        // Flag runs with from-reference to base
        for flag in &short_flags {
            let mut args = sub_prefix.clone();
            args.push(Arg::Literal(flag.clone()));
            args.extend(pattern.iter().map(&to_arg));
            runs.push(Run {
                args,
                in_contexts: None, stdin: None,
                diff_from: Some(base_args.clone()),
            });
        }
        for (flag, hint) in &long_flags {
            let mut args = sub_prefix.clone();
            let val = hint.as_ref().map(|h| default_value(h));
            if let Some(v) = val {
                args.push(Arg::Literal(format!("{}={}", flag, v)));
            } else {
                args.push(Arg::Literal(flag.clone()));
            }
            args.extend(pattern.iter().map(&to_arg));
            runs.push(Run {
                args,
                in_contexts: None, stdin: None,
                diff_from: Some(base_args.clone()),
            });
        }
    }

    // Stdin runs: if the binary accepts stdin, generate runs that pipe content
    // combined with each working arg pattern (not just bare stdin)
    if stdin_works {
        let stdin_content = parse::StdinSource::Lines(
            vec!["cherry".into(), "apple".into(), "banana".into()]
        );

        // Stdin with each working arg pattern
        for pattern in &working_patterns {
            let base_args: Vec<Arg> = sub_prefix.iter().cloned()
                .chain(pattern.iter().map(&to_arg))
                .collect();

            runs.push(Run {
                args: base_args.clone(),
                in_contexts: None,
                stdin: Some(stdin_content.clone()),
                diff_from: None,
            });

            for flag in &short_flags {
                let mut args = sub_prefix.clone();
                args.push(Arg::Literal(flag.clone()));
                args.extend(pattern.iter().map(&to_arg));
                runs.push(Run {
                    args,
                    in_contexts: None,
                    stdin: Some(stdin_content.clone()),
                    diff_from: Some(base_args.clone()),
                });
            }
            for (flag, hint) in &long_flags {
                let mut args = sub_prefix.clone();
                let val = hint.as_ref().map(|h| default_value(h));
                if let Some(v) = val {
                    args.push(Arg::Literal(format!("{}={}", flag, v)));
                } else {
                    args.push(Arg::Literal(flag.clone()));
                }
                args.extend(pattern.iter().map(&to_arg));
                runs.push(Run {
                    args,
                    in_contexts: None,
                    stdin: Some(stdin_content.clone()),
                    diff_from: Some(base_args.clone()),
                });
            }
        }

        // Also stdin with no positional args (bare stdin)
        let bare_args: Vec<Arg> = sub_prefix.clone();
        runs.push(Run {
            args: bare_args.clone(),
            in_contexts: None,
            stdin: Some(stdin_content.clone()),
            diff_from: None,
        });
        for flag in &short_flags {
            let mut args = sub_prefix.clone();
            args.push(Arg::Literal(flag.clone()));
            runs.push(Run {
                args,
                in_contexts: None,
                stdin: Some(stdin_content.clone()),
                diff_from: Some(bare_args.clone()),
            });
        }
        for (flag, hint) in &long_flags {
            let mut args = sub_prefix.clone();
            let val = hint.as_ref().map(|h| default_value(h));
            if let Some(v) = val {
                args.push(Arg::Literal(format!("{}={}", flag, v)));
            } else {
                args.push(Arg::Literal(flag.clone()));
            }
            runs.push(Run {
                args,
                in_contexts: None,
                stdin: Some(stdin_content.clone()),
                diff_from: Some(bare_args.clone()),
            });
        }
    }

    // Boundary runs for numeric flags (using first working pattern)
    if let Some(first_pattern) = working_patterns.first() {
        let numeric_hints = ["NUM", "NUMBER", "N", "SIZE", "COLS", "WIDTH", "COUNT", "LINES", "BYTES"];
        let zero_flags: Vec<&(String, Option<String>)> = long_flags.iter()
            .filter(|(_, hint)| hint.as_ref().is_some_and(|h| numeric_hints.contains(&h.to_uppercase().as_str())))
            .collect();
        if !zero_flags.is_empty() {
            let base_args: Vec<Arg> = sub_prefix.iter().cloned()
                .chain(first_pattern.iter().map(&to_arg))
                .collect();
            for (flag, _) in &zero_flags {
                let mut args = sub_prefix.clone();
                args.push(Arg::Literal(format!("{}=0", flag)));
                args.extend(first_pattern.iter().map(&to_arg));
                runs.push(Run {
                    args, in_contexts: None, stdin: None,
                    diff_from: Some(base_args.clone()),
                });
            }
            for (flag, _) in zero_flags.iter().take(3) {
                let mut args1 = sub_prefix.clone();
                args1.push(Arg::Literal(format!("{}=-1", flag)));
                args1.extend(first_pattern.iter().map(&to_arg));
                runs.push(Run {
                    args: args1, in_contexts: None, stdin: None,
                    diff_from: Some(base_args.clone()),
                });
                let mut args2 = sub_prefix.clone();
                args2.push(Arg::Literal(format!("{}=2147483647", flag)));
                args2.extend(first_pattern.iter().map(&to_arg));
                runs.push(Run {
                    args: args2, in_contexts: None, stdin: None,
                    diff_from: Some(base_args.clone()),
                });
            }
        }
    }

    // Error provocation: nonexistent file
    {
        let mut err_args = sub_prefix.clone();
        err_args.push(Arg::Literal("nonexistent-file.txt".into()));
        runs.push(Run { args: err_args, in_contexts: None, stdin: None, diff_from: None });
    }

    Ok((Script { contexts, runs }, flag_info))
}

