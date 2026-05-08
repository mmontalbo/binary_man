//! Flag discovery and probe skeleton generation from --help text.

use anyhow::{Context, Result};
use regex::Regex;
use std::collections::{HashMap, HashSet};

use crate::parse::{
    FileContent, NamedContext, Property, Run, Script, SetupCommand,
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

/// Binary inspection results from scanning printable strings.
struct BinaryHints {
    env_vars: Vec<String>,
    config_paths: Vec<String>,
}

/// Scan a binary file for environment variable names and config paths.
fn inspect_binary(binary: &str) -> BinaryHints {
    let path = which::which(binary).ok();
    let data = path.and_then(|p| std::fs::read(p).ok()).unwrap_or_default();

    let mut env_vars = HashSet::new();
    let mut config_paths = HashSet::new();

    // Extract printable strings (runs of >= 4 printable ASCII chars)
    let mut current = String::new();
    for &byte in &data {
        if (0x20..0x7f).contains(&byte) {
            current.push(byte as char);
        } else {
            if current.len() >= 4 {
                classify_string(&current, &mut env_vars, &mut config_paths);
            }
            current.clear();
        }
    }
    if current.len() >= 4 {
        classify_string(&current, &mut env_vars, &mut config_paths);
    }

    // Filter env vars to likely-real ones
    let skip_env = ["DESCRIPTION", "COMMAND", "ERROR", "WARNING", "VERSION",
                    "OPTIONS", "USAGE", "ARGUMENTS", "SYNOPSIS", "EXAMPLE",
                    "DEFAULT", "REQUIRED", "OPTIONAL", "INTERNAL", "ENABLED",
                    "DISABLED", "SUCCESS", "FAILURE", "UNKNOWN", "INVALID",
                    "TRUE", "FALSE", "NULL", "NONE", "AUTO"];

    let mut sorted_env: Vec<String> = env_vars.into_iter()
        .filter(|v| !skip_env.contains(&v.as_str()))
        .filter(|v| v.len() >= 3 && v.len() <= 40)
        .collect();
    sorted_env.sort();

    let mut sorted_paths: Vec<String> = config_paths.into_iter().collect();
    sorted_paths.sort();

    BinaryHints {
        env_vars: sorted_env,
        config_paths: sorted_paths,
    }
}

fn classify_string(s: &str, env_vars: &mut HashSet<String>, config_paths: &mut HashSet<String>) {
    // Environment variable pattern: all uppercase with underscores, 3+ chars
    let env_suffixes = ["_DIR", "_PATH", "_HOME", "_FILE", "_CONFIG", "_ROOT",
                        "_PREFIX", "_EDITOR", "_PAGER", "_AUTHOR", "_EMAIL",
                        "_NAME", "_ENCODING", "_LANG", "_OPTS", "_FLAGS"];
    if s.chars().all(|c| c.is_ascii_uppercase() || c == '_') && s.len() >= 3
        && (env_suffixes.iter().any(|suf| s.ends_with(suf))
            || s.starts_with("LC_")
            || s.starts_with("XDG_")) {
        env_vars.insert(s.to_string());
    }

    // Config path pattern: contains /etc/, .config/, or ~/
    if (s.contains("/etc/") || s.contains(".config/") || s.starts_with("~/"))
        && s.len() >= 6 && s.len() <= 80 && !s.contains(' ') && !s.contains("%s") && !s.contains("..")
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "/-_.~".contains(c)) {
        config_paths.insert(s.to_string());
    }
    // Dotfile pattern: starts with . and looks like a config file
    if s.starts_with('.') && !s.starts_with("..") && s.len() >= 3 && s.len() <= 40
        && !s.contains(' ') && !s.contains('(')
        && (s.contains("rc") || s.contains("config") || s.contains("ignore")
            || s.contains("profile") || s.ends_with(".conf")
            || s.ends_with(".cfg") || s.ends_with(".ini")) {
        config_paths.insert(s.to_string());
    }
}

/// Print a probe skeleton to stdout for manual authoring.
pub fn print_skeleton(
    binary: &str,
    sub_args: &[&str],
    sandbox: &Sandbox,
) -> Result<()> {
    let help_text = try_help(binary, sub_args, sandbox)?;

    // Extract flags
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
    let cmd_label = if sub_args.is_empty() {
        binary.to_string()
    } else {
        format!("{} {}", binary, sub_args.join(" "))
    };

    // Inspect binary for env vars and config paths
    let hints = inspect_binary(binary);

    // Output skeleton
    println!("# Discovered from: {} --help", cmd_label);
    println!("# {} short flags, {} long flags found", short_flags.len(), long_flags.len());
    if !hints.env_vars.is_empty() || !hints.config_paths.is_empty() {
        println!("#");
        println!("# Binary inspection:");
        if !hints.env_vars.is_empty() {
            println!("#   env vars: {}", hints.env_vars.join(", "));
        }
        if !hints.config_paths.is_empty() {
            println!("#   config paths: {}", hints.config_paths.join(", "));
        }
    }
    println!();

    // --- Orthogonal base contexts ---
    println!("context \"few_files\"");
    println!("  file \"input.txt\" \"cherry\" \"apple\" \"banana\" \"date\" \"elderberry\"");
    println!("  file \"other.txt\" \"hello world\"");
    println!();

    println!("context \"many_files\"");
    println!("  file \"input.txt\" \"100\" \"2\" \"30\" \"1\" \"20\" \"3\" \"10\"");
    println!("  file \"a.txt\" \"first\"");
    println!("  file \"b.txt\" \"second\"");
    println!("  file \"c.log\" \"log entry\"");
    println!("  file \"data.csv\" \"name,age\" \"alice,25\" \"bob,30\"");
    println!("  file \".hidden\" \"secret content\"");
    println!("  file \".config\" \"key=value\"");
    println!("  dir \"subdir\"");
    println!("  file \"subdir/nested.txt\" \"nested content\"");
    println!();

    println!("context \"deep_tree\"");
    println!("  file \"input.txt\" \"bob:30:sales\" \"alice:25:eng\" \"charlie:35:sales\"");
    println!("  dir \"level1\"");
    println!("  dir \"level1/level2\"");
    println!("  file \"level1/a.txt\" \"depth one\"");
    println!("  file \"level1/level2/b.txt\" \"depth two\"");
    println!("  file \"level1/level2/deep.log\" \"deep log\"");
    println!("  file \"link_to_dir\" -> \"level1\"");
    println!();

    println!("context \"mixed_types\"");
    println!("  file \"input.txt\" \"Apple\" \"BANANA\" \"cherry\" \"apple\" \"Cherry\" \"APPLE\"");
    println!("  file \"empty.txt\" empty");
    println!("  file \"exec.sh\" \"#!/bin/sh\\necho hello\"");
    println!("  props \"exec.sh\" executable");
    println!("  file \"readonly.dat\" \"protected\"");
    println!("  props \"readonly.dat\" readonly");
    println!("  file \"link.txt\" -> \"input.txt\"");
    println!("  file \"broken.lnk\" -> \"nonexistent\"");
    println!("  file \"-rf\" \"flag-like filename\"");
    println!();

    println!("context \"timestamped\"");
    println!("  file \"input.txt\" \"aaa\" \"aaa\" \"bbb\" \"bbb\" \"bbb\" \"ccc\" \"aaa\"");
    println!("  file \"old.txt\" \"ancient content\"");
    println!("  props \"old.txt\" mtime old");
    println!("  file \"big.bin\" size 10000");
    println!("  file \"tiny.txt\" \"x\"");
    println!("  file \"medium.txt\" \"line1\" \"line2\" \"line3\" \"line4\" \"line5\"");
    println!("  dir \"subdir\"");
    println!("  file \"subdir/recent.txt\" \"fresh\"");
    println!("  props \"subdir/recent.txt\" mtime recent");
    println!();

    println!("context \"empty_dir\"");
    println!("  # empty workspace — error path exerciser");
    println!();

    // Vary blocks (from many_files)
    println!("vary from \"many_files\"");
    println!("  remove \".hidden\"");
    println!("  remove \".config\"");
    println!("  remove \"subdir\"");
    println!("  file \"input.txt\" empty");
    println!("  props \"input.txt\" readonly");
    println!("  props \"input.txt\" mtime old");
    println!("  file \"input.txt\" size 1");
    println!();

    // Helper: build a run arg list
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
    let fil = file_arg.as_deref().unwrap_or("input.txt");
    let has_file_arg = file_arg.is_some() || pattern_arg.is_some();
    let use_dir = has_file_arg && fil != ".";

    // --- File-targeted runs ---
    if has_file_arg {
        let base_file = build_run(None, None, pat, Some(fil));
        println!("run {}", base_file);
        println!();
        println!("from {}", base_file);
        for flag in &short_flags {
            println!("  run {}", build_run(Some(flag), None, pat, Some(fil)));
        }
        for (flag, hint) in &long_flags {
            let val = hint.as_ref().map(|h| default_value(h));
            println!("  run {}", build_run(Some(flag), val.as_deref(), pat, Some(fil)));
        }
    } else {
        for flag in &short_flags {
            println!("run {}", build_run(Some(flag), None, None, None));
        }
        for (flag, hint) in &long_flags {
            let val = hint.as_ref().map(|h| default_value(h));
            println!("run {}", build_run(Some(flag), val.as_deref(), None, None));
        }
    }

    // --- Directory-targeted runs ---
    if use_dir {
        println!();
        println!("# Directory-targeted: exercises filesystem structure");
        let base_dir = build_run(None, None, pat, Some("."));
        println!("run {}", base_dir);
        println!();
        println!("from {}", base_dir);
        for flag in &short_flags {
            println!("  run {}", build_run(Some(flag), None, pat, Some(".")));
        }
        for (flag, hint) in &long_flags {
            let val = hint.as_ref().map(|h| default_value(h));
            println!("  run {}", build_run(Some(flag), val.as_deref(), pat, Some(".")));
        }
    }

    // Boundary runs
    if has_file_arg {
        let numeric_hints = ["NUM", "NUMBER", "N", "SIZE", "COLS", "WIDTH", "COUNT", "LINES", "BYTES"];
        let zero_flags: Vec<&(String, Option<String>)> = long_flags.iter()
            .filter(|(_, hint)| hint.as_ref().is_some_and(|h| numeric_hints.contains(&h.to_uppercase().as_str())))
            .collect();
        if !zero_flags.is_empty() {
            println!();
            println!("# Boundary: zero value for numeric flags");
            for (flag, _) in &zero_flags {
                println!("run {}", build_run(Some(&format!("{}=0", flag)), None, pat, Some(fil)));
            }
            for (flag, _) in zero_flags.iter().take(3) {
                println!("run {}", build_run(Some(&format!("{}=-1", flag)), None, pat, Some(fil)));
                println!("run {}", build_run(Some(&format!("{}=2147483647", flag)), None, pat, Some(fil)));
            }
        }
    }

    // Error provocation
    println!();
    println!("# Error: nonexistent file");
    println!("run {}", build_run(None, None, pat, Some("nonexistent-file.txt")));

    Ok(())
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

    // --- Orthogonal base contexts ---
    // Each varies structure, content, properties, and topology simultaneously.
    // Collapsing across these reveals which dimensions each flag is sensitive to.
    // --- Orthogonal base contexts ---
    // Each varies structure, content, properties, and topology simultaneously.
    let mut contexts: Vec<NamedContext> = vec![
        // few_files: minimal — 2 files, alpha content. Control context.
        NamedContext {
            name: "few_files".into(), extends: None,
            commands: vec![
                SetupCommand::CreateFile { path: "input.txt".into(),
                    content: FileContent::Lines(vec!["cherry".into(), "apple".into(), "banana".into(), "date".into(), "elderberry".into()]) },
                SetupCommand::CreateFile { path: "other.txt".into(),
                    content: FileContent::Lines(vec!["hello world".into()]) },
            ],
        },
        // many_files: crowded — 8 files, hidden files, subdir, numeric content.
        NamedContext {
            name: "many_files".into(), extends: None,
            commands: vec![
                SetupCommand::CreateFile { path: "input.txt".into(),
                    content: FileContent::Lines(vec!["100".into(), "2".into(), "30".into(), "1".into(), "20".into(), "3".into(), "10".into()]) },
                SetupCommand::CreateFile { path: "a.txt".into(),
                    content: FileContent::Lines(vec!["first".into()]) },
                SetupCommand::CreateFile { path: "b.txt".into(),
                    content: FileContent::Lines(vec!["second".into()]) },
                SetupCommand::CreateFile { path: "c.log".into(),
                    content: FileContent::Lines(vec!["log entry".into()]) },
                SetupCommand::CreateFile { path: "data.csv".into(),
                    content: FileContent::Lines(vec!["name,age".into(), "alice,25".into(), "bob,30".into()]) },
                SetupCommand::CreateFile { path: ".hidden".into(),
                    content: FileContent::Lines(vec!["secret content".into()]) },
                SetupCommand::CreateFile { path: ".config".into(),
                    content: FileContent::Lines(vec!["key=value".into()]) },
                SetupCommand::CreateDir { path: "subdir".into() },
                SetupCommand::CreateFile { path: "subdir/nested.txt".into(),
                    content: FileContent::Lines(vec!["nested content".into()]) },
            ],
        },
        // deep_tree: 3-level nesting, directory symlink, fielded content.
        NamedContext {
            name: "deep_tree".into(), extends: None,
            commands: vec![
                SetupCommand::CreateFile { path: "input.txt".into(),
                    content: FileContent::Lines(vec!["bob:30:sales".into(), "alice:25:eng".into(), "charlie:35:sales".into()]) },
                SetupCommand::CreateDir { path: "level1".into() },
                SetupCommand::CreateDir { path: "level1/level2".into() },
                SetupCommand::CreateFile { path: "level1/a.txt".into(),
                    content: FileContent::Lines(vec!["depth one".into()]) },
                SetupCommand::CreateFile { path: "level1/level2/b.txt".into(),
                    content: FileContent::Lines(vec!["depth two".into()]) },
                SetupCommand::CreateFile { path: "level1/level2/deep.log".into(),
                    content: FileContent::Lines(vec!["deep log".into()]) },
                SetupCommand::CreateLink { path: "link_to_dir".into(), target: "level1".into() },
            ],
        },
        // mixed_types: symlinks, broken link, executable, readonly, flag-like name. Cased content.
        NamedContext {
            name: "mixed_types".into(), extends: None,
            commands: vec![
                SetupCommand::CreateFile { path: "input.txt".into(),
                    content: FileContent::Lines(vec!["Apple".into(), "BANANA".into(), "cherry".into(), "apple".into(), "Cherry".into(), "APPLE".into()]) },
                SetupCommand::CreateFile { path: "empty.txt".into(), content: FileContent::Empty },
                SetupCommand::CreateFile { path: "exec.sh".into(),
                    content: FileContent::Lines(vec!["#!/bin/sh\necho hello".into()]) },
                SetupCommand::SetProps { path: "exec.sh".into(), props: vec![Property::Executable] },
                SetupCommand::CreateFile { path: "readonly.dat".into(),
                    content: FileContent::Lines(vec!["protected".into()]) },
                SetupCommand::SetProps { path: "readonly.dat".into(), props: vec![Property::ReadOnly] },
                SetupCommand::CreateLink { path: "link.txt".into(), target: "input.txt".into() },
                SetupCommand::CreateLink { path: "broken.lnk".into(), target: "nonexistent".into() },
                SetupCommand::CreateFile { path: "-rf".into(),
                    content: FileContent::Lines(vec!["flag-like filename".into()]) },
            ],
        },
        // timestamped: varied sizes (0B-10KB) and timestamps (old/recent). Duplicated content.
        NamedContext {
            name: "timestamped".into(), extends: None,
            commands: vec![
                SetupCommand::CreateFile { path: "input.txt".into(),
                    content: FileContent::Lines(vec!["aaa".into(), "aaa".into(), "bbb".into(), "bbb".into(), "bbb".into(), "ccc".into(), "aaa".into()]) },
                SetupCommand::CreateFile { path: "old.txt".into(),
                    content: FileContent::Lines(vec!["ancient content".into()]) },
                SetupCommand::SetProps { path: "old.txt".into(), props: vec![Property::MtimeOld] },
                SetupCommand::CreateFile { path: "big.bin".into(), content: FileContent::Size(10000) },
                SetupCommand::CreateFile { path: "tiny.txt".into(),
                    content: FileContent::Lines(vec!["x".into()]) },
                SetupCommand::CreateFile { path: "medium.txt".into(),
                    content: FileContent::Lines(vec!["line1".into(), "line2".into(), "line3".into(), "line4".into(), "line5".into()]) },
                SetupCommand::CreateDir { path: "subdir".into() },
                SetupCommand::CreateFile { path: "subdir/recent.txt".into(),
                    content: FileContent::Lines(vec!["fresh".into()]) },
                SetupCommand::SetProps { path: "subdir/recent.txt".into(), props: vec![Property::MtimeRecent] },
            ],
        },
        // empty_dir: nothing at all. Universal error-path exerciser.
        NamedContext {
            name: "empty_dir".into(), extends: None,
            commands: vec![],
        },
    ];

    // --- Vary blocks (single-factor perturbations from many_files) ---
    let vary_base = "many_files";
    let perturbations = vec![
        SetupCommand::Remove { path: ".hidden".into() },
        SetupCommand::Remove { path: ".config".into() },
        SetupCommand::Remove { path: "subdir".into() },
        SetupCommand::CreateFile { path: "input.txt".into(), content: FileContent::Empty },
        SetupCommand::SetProps { path: "input.txt".into(), props: vec![Property::ReadOnly] },
        SetupCommand::SetProps { path: "input.txt".into(), props: vec![Property::MtimeOld] },
        SetupCommand::CreateFile { path: "input.txt".into(), content: FileContent::Size(1) },
    ];

    let base_ctx = contexts.iter().find(|c| c.name == vary_base).unwrap().clone();
    for perturbation in &perturbations {
        let variant_name = format!("{} / {}", vary_base, describe_perturbation(perturbation));
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

fn describe_perturbation(cmd: &SetupCommand) -> String {
    match cmd {
        SetupCommand::Remove { path } => format!("remove {}", path),
        SetupCommand::RemoveEnv { var } => format!("remove env {}", var),
        SetupCommand::CreateFile { path, content } => {
            match content {
                FileContent::Size(n) => format!("{}=size:{}", path, n),
                FileContent::Lines(l) if l.len() == 1 => format!("{}={:?}", path, l[0]),
                FileContent::Lines(l) => format!("{}={}lines", path, l.len()),
                FileContent::Empty => format!("{}=empty", path),
                FileContent::From(src) => format!("{}=from:{}", path, src),
            }
        }
        SetupCommand::CreateLink { path, target } => format!("{} -> {}", path, target),
        SetupCommand::SetProps { path, props } => {
            let p: Vec<&str> = props.iter().map(|p| match p {
                Property::Executable => "executable",
                Property::MtimeOld => "mtime=old",
                Property::MtimeRecent => "mtime=recent",
                Property::ReadOnly => "readonly",
            }).collect();
            format!("{} {}", path, p.join(" "))
        }
        SetupCommand::SetEnv { var, value } => format!("env {}={}", var, value),
        SetupCommand::CreateDir { path } => format!("dir {}", path),
        SetupCommand::Invoke { args } => format!("invoke {:?}", args),
    }
}
