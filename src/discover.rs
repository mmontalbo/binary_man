//! Flag discovery and probe skeleton generation from --help text.

use anyhow::{Context, Result};
use regex::Regex;
use std::collections::{HashMap, HashSet};

use crate::parse::{
    self, FileContent, NamedContext, Property, Run, Script, SetupCommand,
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
                if name.starts_with('-') {
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

/// Infer positional arguments from usage line.
/// Returns (pattern_arg, file_arg) — pattern_arg is Some for tools like grep/awk/sed.
pub fn infer_base_args(help_text: &str) -> (Option<String>, Option<String>) {
    let pattern_words = ["PATTERN", "PATTERNS", "EXPRESSION", "REGEX", "REGEXP",
                         "BRE", "ERE", "SCRIPT", "PROGRAM"];

    for line in help_text.lines().take(10) {
        let upper = line.to_uppercase();

        // Check for pattern-before-file: "PATTERN [FILE]", "PATTERNS [FILE]..."
        let has_pattern = pattern_words.iter().any(|p| {
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

/// Map a value hint from --help to a reasonable default.
pub fn default_value(hint: &str) -> String {
    match hint.to_uppercase().as_str() {
        "NUM" | "NUMBER" | "N" | "SIZE" | "COLS" | "WIDTH" => "10".into(),
        "FILE" | "PATH" | "FILENAME" => "input.txt".into(),
        "DIR" | "DIRECTORY" => ".".into(),
        "PATTERN" | "PAT" | "REGEX" => ".*".into(),
        _ => hint.to_lowercase(),
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
    let mut current_from: Option<&Vec<String>> = None;
    for run in &script.runs {
        let args_str = run.args.iter().map(|a| format!("\"{}\"", a)).collect::<Vec<_>>().join(" ");

        match (&run.diff_from, current_from) {
            (Some(ref from), Some(prev)) if from == prev => {
                // Inside an existing from block
                println!("  run {}", args_str);
            }
            (Some(ref from), _) => {
                // New from block
                let from_str = from.iter().map(|a| format!("\"{}\"", a)).collect::<Vec<_>>().join(" ");
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

    let (pattern_arg, file_arg) = infer_base_args(&help_text);

    // --- Latin square base contexts ---
    // Three factors, three levels each. Each level appears once per row and column.
    // Main effects (content, structure, properties) are estimable without aliasing.
    //
    //              minimal         standard              deep
    // alpha        default         varied-perms          varied-times
    // numeric      varied-times    default               varied-perms
    // fielded      varied-perms    varied-times          default

    // Content levels
    let content_alpha: Vec<String> = vec!["cherry", "apple", "banana", "date", "elderberry"]
        .into_iter().map(String::from).collect();
    let content_numeric: Vec<String> = vec!["100", "2", "30", "1", "20", "3", "10"]
        .into_iter().map(String::from).collect();
    let content_fielded: Vec<String> = vec!["bob:30:sales", "alice:25:eng", "charlie:35:sales"]
        .into_iter().map(String::from).collect();

    // Structure builders — each returns the setup commands for that structure level
    let structure_minimal = |content: &[String]| -> Vec<SetupCommand> {
        vec![
            SetupCommand::CreateFile { path: "input.txt".into(),
                content: FileContent::Lines(content.to_vec()) },
            SetupCommand::CreateFile { path: "other.txt".into(),
                content: FileContent::Lines(vec!["hello world".into()]) },
        ]
    };

    let structure_standard = |content: &[String]| -> Vec<SetupCommand> {
        vec![
            SetupCommand::CreateFile { path: "input.txt".into(),
                content: FileContent::Lines(content.to_vec()) },
            SetupCommand::CreateFile { path: "a.txt".into(),
                content: FileContent::Lines(vec!["first".into()]) },
            SetupCommand::CreateFile { path: "b.txt".into(),
                content: FileContent::Lines(vec!["second".into()]) },
            SetupCommand::CreateFile { path: ".hidden".into(),
                content: FileContent::Lines(vec!["secret".into()]) },
            SetupCommand::CreateDir { path: "subdir".into() },
            SetupCommand::CreateFile { path: "subdir/nested.txt".into(),
                content: FileContent::Lines(vec!["nested".into()]) },
            SetupCommand::CreateLink { path: "link.txt".into(), target: "input.txt".into() },
            SetupCommand::CreateFile { path: "exec.sh".into(),
                content: FileContent::Lines(vec!["#!/bin/sh\necho hello".into()]) },
            SetupCommand::SetProps { path: "exec.sh".into(), props: vec![Property::Executable] },
        ]
    };

    let structure_deep = |content: &[String]| -> Vec<SetupCommand> {
        vec![
            SetupCommand::CreateFile { path: "input.txt".into(),
                content: FileContent::Lines(content.to_vec()) },
            SetupCommand::CreateDir { path: "level1".into() },
            SetupCommand::CreateDir { path: "level1/level2".into() },
            SetupCommand::CreateFile { path: "level1/a.txt".into(),
                content: FileContent::Lines(vec!["depth one".into()]) },
            SetupCommand::CreateFile { path: "level1/level2/b.txt".into(),
                content: FileContent::Lines(vec!["depth two".into()]) },
            SetupCommand::CreateLink { path: "link_to_dir".into(), target: "level1".into() },
        ]
    };

    // Property modifiers — appended after structure commands
    let props_default = |_cmds: &mut Vec<SetupCommand>| {};

    let props_perms = |cmds: &mut Vec<SetupCommand>| {
        cmds.push(SetupCommand::CreateFile { path: "readonly.dat".into(),
            content: FileContent::Lines(vec!["protected".into()]) });
        cmds.push(SetupCommand::SetProps { path: "readonly.dat".into(), props: vec![Property::ReadOnly] });
        cmds.push(SetupCommand::CreateFile { path: "-rf".into(),
            content: FileContent::Lines(vec!["flag-like filename".into()]) });
    };

    let props_times = |cmds: &mut Vec<SetupCommand>| {
        cmds.push(SetupCommand::CreateFile { path: "old.txt".into(),
            content: FileContent::Lines(vec!["ancient".into()]) });
        cmds.push(SetupCommand::SetProps { path: "old.txt".into(), props: vec![Property::MtimeOld] });
        cmds.push(SetupCommand::CreateFile { path: "big.bin".into(), content: FileContent::Size(10000) });
    };

    // Latin square: 9 contexts
    let build_ctx = |name: &str, content: &[String],
                         structure_fn: &dyn Fn(&[String]) -> Vec<SetupCommand>,
                         props_fn: &dyn Fn(&mut Vec<SetupCommand>)| -> NamedContext {
        let mut cmds = structure_fn(content);
        props_fn(&mut cmds);
        NamedContext { name: name.into(), extends: None, commands: cmds }
    };

    let mut contexts: Vec<NamedContext> = vec![
        // Row: alpha       | minimal + default    | standard + perms     | deep + times
        build_ctx("alpha_minimal",  &content_alpha,   &structure_minimal,  &props_default),
        build_ctx("alpha_standard", &content_alpha,   &structure_standard, &props_perms),
        build_ctx("alpha_deep",     &content_alpha,   &structure_deep,     &props_times),
        // Row: numeric     | minimal + times      | standard + default   | deep + perms
        build_ctx("numeric_minimal",  &content_numeric,  &structure_minimal,  &props_times),
        build_ctx("numeric_standard", &content_numeric,  &structure_standard, &props_default),
        build_ctx("numeric_deep",     &content_numeric,  &structure_deep,     &props_perms),
        // Row: fielded     | minimal + perms      | standard + times     | deep + default
        build_ctx("fielded_minimal",  &content_fielded,  &structure_minimal,  &props_perms),
        build_ctx("fielded_standard", &content_fielded,  &structure_standard, &props_times),
        build_ctx("fielded_deep",     &content_fielded,  &structure_deep,     &props_default),
        // Error context: empty workspace
        NamedContext { name: "empty_dir".into(), extends: None, commands: vec![] },
    ];

    // --- Single-factor perturbations from numeric_standard (richest base) ---
    let vary_base = "numeric_standard";
    let perturbations = vec![
        SetupCommand::Remove { path: ".hidden".into() },
        SetupCommand::Remove { path: "subdir".into() },
        SetupCommand::Remove { path: "link.txt".into() },
        SetupCommand::CreateFile { path: "input.txt".into(), content: FileContent::Empty },
        SetupCommand::SetProps { path: "input.txt".into(), props: vec![Property::ReadOnly] },
        SetupCommand::SetProps { path: "input.txt".into(), props: vec![Property::MtimeOld] },
        SetupCommand::CreateFile { path: "input.txt".into(), content: FileContent::Size(1) },
    ];

    let base_ctx = contexts.iter().find(|c| c.name == vary_base).unwrap().clone();
    for perturbation in &perturbations {
        let variant_name = format!("{} / {}", vary_base, parse::describe_perturbation(perturbation));
        let mut cmds = base_ctx.commands.clone();
        cmds.push(perturbation.clone());
        contexts.push(NamedContext { name: variant_name, extends: None, commands: cmds });
    }

    // --- Build runs ---
    let mut runs: Vec<Run> = Vec::new();
    let sub_prefix: Vec<String> = sub_args.iter().map(|s| s.to_string()).collect();

    // Helper: build args with a specific positional target
    let build_args = |flag: Option<&str>, flag_value: Option<&str>,
                      pat: Option<&str>, target: &str| -> Vec<String> {
        let mut args = sub_prefix.clone();
        if let Some(f) = flag {
            if let Some(v) = flag_value {
                args.push(format!("{}={}", f, v));
            } else {
                args.push(f.to_string());
            }
        }
        if let Some(p) = pat { args.push(p.to_string()); }
        args.push(target.to_string());
        args
    };

    // Determine positional targets
    let file_target = file_arg.as_deref().unwrap_or("input.txt");
    let has_file_arg = file_arg.is_some() || pattern_arg.is_some();
    let use_dir = has_file_arg && file_target != ".";

    // Pattern archetypes: when the tool takes a PATTERN, vary it to exercise
    // different regex/matching behaviors (case, word boundary, metachar, non-matching)
    let patterns: Vec<&str> = if pattern_arg.is_some() {
        vec!["alpha", "Alpha", "a.*e", "zzzzz"]
    } else {
        vec![]
    };
    let has_patterns = !patterns.is_empty();

    // Helper: emit a set of flag runs for a given (pattern, target) combination
    let emit_flag_runs = |pat: Option<&str>, target: &str, runs: &mut Vec<Run>| {
        let base = build_args(None, None, pat, target);
        runs.push(Run { args: base.clone(), in_contexts: None, stdin: None, diff_from: None });

        for flag in &short_flags {
            runs.push(Run {
                args: build_args(Some(flag), None, pat, target),
                in_contexts: None, stdin: None, diff_from: Some(base.clone()),
            });
        }
        for (flag, hint) in &long_flags {
            let val = hint.as_ref().map(|h| default_value(h));
            runs.push(Run {
                args: build_args(Some(flag), val.as_deref(), pat, target),
                in_contexts: None, stdin: None, diff_from: Some(base.clone()),
            });
        }
    };

    if has_patterns {
        // Pattern-taking tool: emit runs for each pattern × target
        for pat in &patterns {
            emit_flag_runs(Some(pat), file_target, &mut runs);
        }
        if use_dir {
            // Only use the primary pattern for directory runs (avoid combinatorial explosion)
            emit_flag_runs(Some(patterns[0]), ".", &mut runs);
        }
    } else if has_file_arg {
        // File-taking tool: emit file + directory runs
        emit_flag_runs(None, file_target, &mut runs);
        if use_dir {
            emit_flag_runs(None, ".", &mut runs);
        }
    } else {
        // No positional arg — flat run list
        for flag in &short_flags {
            let mut args = sub_prefix.clone();
            args.push(flag.clone());
            runs.push(Run { args, in_contexts: None, stdin: None, diff_from: None });
        }
        for (flag, hint) in &long_flags {
            let mut args = sub_prefix.clone();
            let val = hint.as_ref().map(|h| default_value(h));
            if let Some(v) = val {
                args.push(format!("{}={}", flag, v));
            } else {
                args.push(flag.clone());
            }
            runs.push(Run { args, in_contexts: None, stdin: None, diff_from: None });
        }
    }

    // --- Boundary runs for numeric flags (primary pattern only) ---
    if has_file_arg {
        let boundary_pat = if has_patterns { Some(patterns[0]) } else { None };
        let numeric_hints = ["NUM", "NUMBER", "N", "SIZE", "COLS", "WIDTH", "COUNT", "LINES", "BYTES"];
        let zero_flags: Vec<&(String, Option<String>)> = long_flags.iter()
            .filter(|(_, hint)| hint.as_ref().is_some_and(|h| numeric_hints.contains(&h.to_uppercase().as_str())))
            .collect();
        let base_file = build_args(None, None, boundary_pat, file_target);
        for (flag, _) in &zero_flags {
            runs.push(Run {
                args: build_args(Some(&format!("{}=0", flag)), None, boundary_pat, file_target),
                in_contexts: None, stdin: None, diff_from: Some(base_file.clone()),
            });
        }
        for (flag, _) in zero_flags.iter().take(3) {
            runs.push(Run {
                args: build_args(Some(&format!("{}=-1", flag)), None, boundary_pat, file_target),
                in_contexts: None, stdin: None, diff_from: Some(base_file.clone()),
            });
            runs.push(Run {
                args: build_args(Some(&format!("{}=2147483647", flag)), None, boundary_pat, file_target),
                in_contexts: None, stdin: None, diff_from: Some(base_file.clone()),
            });
        }
    }

    // Error provocation
    {
        let mut err_args = sub_prefix.clone();
        if let Some(ref p) = pattern_arg { err_args.push(p.clone()); }
        err_args.push("nonexistent-file.txt".into());
        runs.push(Run { args: err_args, in_contexts: None, stdin: None, diff_from: None });
    }

    Ok((Script { contexts, runs }, flag_info))
}

