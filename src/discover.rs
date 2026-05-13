//! Flag discovery and probe skeleton generation from --help text.

use anyhow::{Context, Result};
use regex::Regex;
use std::collections::{HashMap, HashSet};

use crate::parse::{Arg, NamedContext, Run, Script};
use crate::sandbox::Sandbox;

/// Extracted flag info from --help text.
pub struct FlagInfo {
    pub descs: HashMap<String, String>,   // flag -> description
    pub aliases: HashMap<String, String>, // short -> long (and long -> short)
    pub all_flags: HashSet<String>,       // every flag discovered
    pub extracted_values: HashMap<String, Vec<String>>, // flag -> values mined from help text
}

/// Extract values from a flag description using multiple patterns:
/// - Single-quoted values: 'auto', 'always', 'never'
/// - Brace enumerations: {all,none,older}
/// - Pipe-separated braces: {big|little}
/// - Bracket character sets: [doxn] (individual chars)
/// - "one of" lists: one of X, Y, or Z
fn mine_description_values(desc: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut seen = HashSet::new();

    // Single-quoted values: 'auto', 'always'
    let quoted_re = Regex::new(r"'([a-zA-Z][-a-zA-Z0-9]*)'").unwrap();
    for cap in quoted_re.captures_iter(desc) {
        let v = cap[1].to_string();
        if seen.insert(v.clone()) { values.push(v); }
    }

    // Brace enumerations: {all,none,older(default)} or {big|little}
    let brace_re = Regex::new(r"\{([^}]+)\}").unwrap();
    for cap in brace_re.captures_iter(desc) {
        let inner = &cap[1];
        for item in inner.split([',', '|']) {
            // Strip annotations like "(default)"
            let clean = item.trim().split('(').next().unwrap_or("").trim();
            if !clean.is_empty() && clean.chars().all(|c| c.is_alphanumeric() || c == '-') {
                let v = clean.to_string();
                if seen.insert(v.clone()) { values.push(v); }
            }
        }
    }

    // Bracket character sets: [doxn] → individual chars as values
    let bracket_re = Regex::new(r"\[([a-zA-Z]{2,8})\]").unwrap();
    for cap in bracket_re.captures_iter(desc) {
        for ch in cap[1].chars() {
            let v = ch.to_string();
            if seen.insert(v.clone()) { values.push(v); }
        }
    }

    values
}

/// Extract flag descriptions and aliases from --help text.
pub fn extract_flag_info(help_text: &str) -> FlagInfo {
    let mut descs: HashMap<String, String> = HashMap::new();
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut all_flags: HashSet<String> = HashSet::new();
    let mut extracted_values: HashMap<String, Vec<String>> = HashMap::new();

    let flag_re = Regex::new(
        r"^\s+(-[a-zA-Z0-9](?:,\s*--[a-zA-Z][-a-zA-Z0-9]*(?:=\S+)?)?|--[a-zA-Z][-a-zA-Z0-9]*(?:=\S+)?)\s{2,}(.+)"
    ).unwrap();

    // Two-pass: first collect full multi-line descriptions per flag group,
    // then mine all patterns from the complete description.
    let lines: Vec<&str> = help_text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if let Some(cap) = flag_re.captures(lines[i]) {
            let flags_part = cap[1].trim();
            let mut desc = cap[2].trim().to_string();
            let mut names: Vec<String> = Vec::new();

            // Also extract values from the flag definition itself (e.g., --endian={big|little})
            let flag_line_values = mine_description_values(flags_part);

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

            // Collect continuation lines (indented 20+ spaces, no flag prefix)
            while i + 1 < lines.len() {
                let next = lines[i + 1];
                let trimmed = next.trim_start();
                let indent = next.len() - trimmed.len();
                if indent >= 20 && !trimmed.starts_with('-') {
                    desc.push(' ');
                    desc.push_str(trimmed);
                    i += 1;
                } else {
                    break;
                }
            }

            // Update descriptions with full multi-line text
            for name in &names {
                descs.insert(name.clone(), desc.clone());
            }

            // Record alias pairs
            if names.len() == 2 {
                aliases.insert(names[0].clone(), names[1].clone());
                aliases.insert(names[1].clone(), names[0].clone());
            }

            // Mine values from the full description + flag definition
            let mut values = mine_description_values(&desc);
            for v in flag_line_values {
                if !values.contains(&v) { values.push(v); }
            }
            if values.len() >= 2 {
                for name in &names {
                    extracted_values.insert(name.clone(), values.clone());
                }
            }
        }
        i += 1;
    }

    FlagInfo { descs, aliases, all_flags, extracted_values }
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

    // Probe stdin: try piping content (bare, then with "-" marker)
    let stdin_works = [sub_args.to_vec(), { let mut a = sub_args.to_vec(); a.push("-"); a }]
        .iter().any(|args| {
            let mut cmd = sandbox.command(binary, args, work_dir, &env, None);
            cmd.stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let Ok(mut child) = cmd.spawn() else { return false };
            if let Some(mut si) = child.stdin.take() {
                use std::io::Write;
                let _ = si.write_all(b"cherry\napple\nbanana\n");
            }
            child.wait_with_output().map(|o| {
                let exit = o.status.code().unwrap_or(-1);
                (exit == 0 || exit == 1) && !String::from_utf8_lossy(&o.stdout).trim().is_empty()
            }).unwrap_or(false)
        });

    // Usage-line mining: detect structural args the binary expects after flags.
    // E.g., "Usage: xargs [OPTION]... COMMAND" → try "echo" as trailing command.
    if let Ok(help_text) = try_help(binary, sub_args, sandbox) {
        let usage_line = help_text.lines()
            .find(|l| l.starts_with("Usage:") || l.starts_with("usage:"))
            .unwrap_or("");
        // Look for uppercase structural placeholders after [OPTION]
        let structural_candidates: Vec<Vec<&str>> = if usage_line.contains("COMMAND") {
            vec![
                vec!["echo"],
                vec!["echo", "input.txt"],
            ]
        } else if usage_line.contains("[expression]") || usage_line.contains("EXPRESSION") {
            vec![
                vec![".", "-print"],
                vec![".", "-name", "*.txt"],
                vec![".", "-type", "f"],
            ]
        } else {
            vec![]
        };
        for candidate in &structural_candidates {
            let mut args: Vec<&str> = sub_args.to_vec();
            args.extend(candidate.iter());
            let mut cmd = sandbox.command(binary, &args, work_dir, &env, None);
            cmd.stdin(std::process::Stdio::null());
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            if let Ok(output) = cmd.output() {
                let exit = output.status.code().unwrap_or(-1);
                let has_output = !String::from_utf8_lossy(&output.stdout).trim().is_empty();
                if exit == 0 || (exit <= 1 && has_output) {
                    working.push(candidate.iter().map(|s| s.to_string()).collect());
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

/// Candidate values for a metavar (value placeholder from --help, e.g. NUM, FILE).
/// Returns an ordered list; the first working candidate becomes the flag's value.
/// List order is stable — new candidates are appended, never reordered.
pub fn candidates(metavar: &str) -> Vec<&'static str> {
    let upper = metavar.to_uppercase();
    let upper = upper.as_str();
    match upper {
        "NUM" | "NUMBER" | "N" | "SIZE" | "COLS" | "WIDTH" | "COUNT" | "LINES" | "BYTES"
        | "MAX" | "PROCS" | "DEPTH" | "JOBS" | "LEVEL" =>
            vec!["1", "0", "2", "10", "100"],
        "FILE" | "PATH" | "FILENAME" =>
            vec!["input.txt", "other.txt", "/dev/null"],
        "DIR" | "DIRECTORY" =>
            vec![".", "subdir", "/tmp"],
        "PATTERN" | "PAT" | "REGEX" =>
            vec![".*", "a", "^$", "[0-9]+"],
        "LIST" | "FIELDS" | "FIELD_LIST" =>
            vec!["1", "1,2", "1-3"],
        "RANGE" | "SET1" | "SET2" | "CHARS" =>
            vec!["1-3", "a-z", "1"],
        "CHAR" | "DELIM" | "SEP" | "CHARACTER" =>
            vec![",", ":", "\t", " "],
        "FORMAT" | "FMT" =>
            vec!["%s", "%d", "%f"],
        "MODE" =>
            vec!["644", "755", "600"],
        "WORD" | "STYLE" | "TYPE" | "METHOD" | "WHEN" | "CONTROL" =>
            vec!["auto", "always", "never", "none"],
        "KEYDEF" | "KEY" | "POS" =>
            vec!["1", "1,2", "2", "1,1"],
        "PROG" | "PROGRAM" | "COMMAND" =>
            vec!["cat", "true", "echo"],
        "END" | "EOF" =>
            vec!["EOF", ""],
        "R" | "REPLACE" =>
            vec!["{}", "X"],
        "TIME_STYLE" =>
            vec!["full-iso", "long-iso", "iso", "locale"],
        "VAR" | "NAME" | "PREFIX" | "SUFFIX" | "STRING" | "STR" | "LABEL" | "TAG" =>
            vec!["test", "x", ""],
        _ => {
            // Handle compound metavars like "MAX-LINES", "MAX-PROCS", "MAX-CHARS"
            let numeric_words = ["MAX", "NUM", "COUNT", "SIZE", "LINES", "BYTES",
                                 "PROCS", "ARGS", "CHARS", "DEPTH", "JOBS", "WIDTH"];
            if upper.split('-').any(|part| numeric_words.contains(&part)) {
                return vec!["1", "0", "2", "10"];
            }
            // Unknown metavar — try generic values
            vec!["1", "auto", ",", "input.txt", ".", "0"]
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
            println!("  {}", crate::output::format_setup_cmd(cmd));
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
                println!("  {}", crate::output::format_setup_cmd(cmd));
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

/// Extract valid values from error output.
/// Parses the GNU coreutils format: "Valid arguments are:\n  - 'value'\n  ..."
fn mine_valid_values(stderr: &str) -> Vec<String> {
    let mut values = Vec::new();
    let re = Regex::new(r"'([a-zA-Z][-a-zA-Z0-9]*)'").unwrap();
    let mut in_valid_section = false;
    for line in stderr.lines() {
        if line.contains("Valid arguments are") || line.contains("valid arguments are") {
            in_valid_section = true;
            continue;
        }
        if in_valid_section {
            if line.starts_with("  ") || line.starts_with("\t") {
                for cap in re.captures_iter(line) {
                    values.push(cap[1].to_string());
                }
            } else {
                break; // End of valid arguments section
            }
        }
    }
    values
}

/// Push a flag (with optional value) as Arg::Literal(s).
/// Short flags use space-separated values (-A 1), long flags use = (--after-context=1).
fn push_flag_arg(args: &mut Vec<Arg>, flag: &str, value: Option<&str>) {
    match value {
        Some(v) if flag.starts_with("--") => {
            args.push(Arg::Literal(format!("{}={}", flag, v)));
        }
        Some(v) => {
            args.push(Arg::Literal(flag.to_string()));
            args.push(Arg::Literal(v.to_string()));
        }
        None => {
            args.push(Arg::Literal(flag.to_string()));
        }
    }
}

/// Probe a flag+value combination. Returns exit code if it ran, None if spawn failed.
/// Shared context for probing flag values against a binary.
struct ProbeCtx<'a> {
    sandbox: &'a Sandbox,
    binary: &'a str,
    sub_args: &'a [&'a str],
    pattern: &'a [String],
    work_dir: &'a std::path::Path,
}

impl<'a> ProbeCtx<'a> {
    /// Probe a flag+value. Returns exit code if it ran.
    fn exit_code(&self, flag: &str, value: Option<&str>,
        companion: Option<(&str, &str)>, stdin_data: Option<&[u8]>,
    ) -> Option<i32> {
        let env = std::collections::HashMap::new();
        let mut args: Vec<String> = self.sub_args.iter().map(|s| s.to_string()).collect();
        if let Some((cf, cv)) = companion {
            if cf.starts_with("--") { args.push(format!("{}={}", cf, cv)); }
            else { args.push(cf.to_string()); args.push(cv.to_string()); }
        }
        if let Some(v) = value {
            if flag.starts_with("--") { args.push(format!("{}={}", flag, v)); }
            else { args.push(flag.to_string()); args.push(v.to_string()); }
        } else {
            args.push(flag.to_string());
        }
        args.extend(self.pattern.iter().cloned());
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let _ = std::fs::write(self.work_dir.join("input.txt"), "cherry\napple\nbanana\n");
        let mut cmd = self.sandbox.command(self.binary, &refs, self.work_dir, &env, None);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        if let Some(data) = stdin_data {
            cmd.stdin(std::process::Stdio::piped());
            let mut child = cmd.spawn().ok()?;
            if let Some(mut si) = child.stdin.take() {
                use std::io::Write;
                let _ = si.write_all(data);
            }
            child.wait_with_output().ok()?.status.code()
        } else {
            cmd.stdin(std::process::Stdio::null());
            cmd.output().ok()?.status.code()
        }
    }

    /// Probe succeeds if exit code ≤ 1.
    fn succeeds(&self, flag: &str, value: Option<&str>,
        companion: Option<(&str, &str)>,
    ) -> bool {
        self.exit_code(flag, value, companion, None).is_some_and(|c| c <= 1)
    }

    /// Try invalid value and mine stderr for valid alternatives.
    fn error_mine(&self, flag: &str) -> Vec<String> {
        let env = std::collections::HashMap::new();
        let mut args: Vec<String> = self.sub_args.iter().map(|s| s.to_string()).collect();
        if flag.starts_with("--") {
            args.push(format!("{}=__bgrid_invalid__", flag));
        } else {
            args.push(flag.to_string());
            args.push("__bgrid_invalid__".into());
        }
        args.extend(self.pattern.iter().cloned());
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let _ = std::fs::write(self.work_dir.join("input.txt"), "cherry\napple\nbanana\n");
        let mut cmd = self.sandbox.command(self.binary, &refs, self.work_dir, &env, None);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        match cmd.output() {
            Ok(output) => mine_valid_values(&String::from_utf8_lossy(&output.stderr)),
            Err(_) => Vec::new(),
        }
    }
}

/// Parse flags from help text into a unified list with metavars.
/// Handles short flags, long flags, short-flag metavar capture, and alias propagation.
fn parse_flags(help_text: &str, flag_info: &FlagInfo) -> (Vec<(String, Option<String>)>, HashSet<String>) {
    let short_re = Regex::new(r"(?:^|\s)-([a-zA-Z0-9])\b").unwrap();
    let short_metavar_re = Regex::new(r"^\s{2,}.*-([a-zA-Z0-9])\s+([A-Z][-A-Z_]*)(?:\s|,|$)").unwrap();
    let long_re = Regex::new(r"--([a-zA-Z][a-zA-Z0-9-]*)(?:[=\s]([A-Z][A-Z_]*))?").unwrap();

    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let mut seen = HashSet::new();
    let mut long_metavars: HashMap<String, String> = HashMap::new();
    let mut short_metavars: HashMap<String, String> = HashMap::new();

    for line in help_text.lines() {
        for cap in short_metavar_re.captures_iter(line) {
            short_metavars.insert(format!("-{}", &cap[1]), cap[2].to_string());
        }
    }

    for line in help_text.lines() {
        for cap in short_re.captures_iter(line) {
            let flag = format!("-{}", &cap[1]);
            if seen.insert(flag.clone()) {
                flags.push((flag.clone(), short_metavars.get(&flag).cloned()));
            }
        }
        for cap in long_re.captures_iter(line) {
            let name = format!("--{}", &cap[1]);
            if name == "--help" || name == "--version" { continue; }
            let metavar = cap.get(2).map(|m| m.as_str().to_string());
            if let Some(mv) = &metavar { long_metavars.insert(name.clone(), mv.clone()); }
            if seen.insert(name.clone()) { flags.push((name, metavar)); }
        }
    }

    // Propagate metavars from long aliases to short flags
    for (flag, metavar) in &mut flags {
        if metavar.is_none() && flag.len() == 2 {
            if let Some(long_alias) = flag_info.aliases.get(flag) {
                if let Some(mv) = long_metavars.get(long_alias) {
                    *metavar = Some(mv.clone());
                }
            }
        }
    }

    (flags, seen)
}

/// Pilot study: determine working factor levels for each flag.
/// Tries candidates solo, then with companions, then mutual compounds.
/// Returns (extra_solo_values, prerequisites).
#[allow(clippy::type_complexity)]
fn probe_values(
    ctx: &ProbeCtx,
    flags: &mut [(String, Option<String>)],
    flag_info: &FlagInfo,
    stdin_works: bool,
) -> (HashMap<String, Vec<String>>, HashMap<String, (String, String)>) {
    let mut extra_solo_values: HashMap<String, Vec<String>> = HashMap::new();
    let mut prerequisites: HashMap<String, (String, String)> = HashMap::new();

    let original_metavars: HashMap<String, String> = flags.iter()
        .filter_map(|(f, mv)| mv.as_ref().map(|m| (f.clone(), m.clone())))
        .collect();

    // Solo probing: try all candidates per flag
    #[allow(clippy::needless_range_loop)]
    for i in 0..flags.len() {
        let (ref flag, ref metavar) = flags[i];
        let has_extracted = flag_info.extracted_values.contains_key(flag);
        if metavar.is_none() && !has_extracted { continue; }

        let mut cands: Vec<String> = Vec::new();
        if let Some(extracted) = flag_info.extracted_values.get(flag) {
            cands.extend(extracted.iter().cloned());
        }
        if let Some(mv) = metavar.as_ref() {
            for c in candidates(mv) {
                if !cands.iter().any(|e| e == c) { cands.push(c.to_string()); }
            }
        }

        let mut working: Vec<String> = Vec::new();
        let mut has_exit0 = false;
        for c in &cands {
            if let Some(exit) = ctx.exit_code(flag, Some(c), None, None) {
                if exit <= 1 {
                    working.push(c.clone());
                    if exit == 0 { has_exit0 = true; }
                }
            }
        }

        // Error mining: try when no candidates worked OR when all exit 1
        if working.is_empty() || !has_exit0 {
            let mined = ctx.error_mine(flag);
            for val in mined {
                if let Some(exit) = ctx.exit_code(flag, Some(&val), None, None) {
                    if exit <= 1 {
                        if exit == 0 && !has_exit0 {
                            working.insert(0, val);
                            has_exit0 = true;
                        } else if !working.contains(&val) {
                            working.push(val);
                        }
                    }
                }
            }
        }

        // Stdin retry for stdin-primary tools
        if stdin_works && !has_exit0 {
            let stdin_data = b"cherry\napple\nbanana\n";
            let empty_pattern: Vec<String> = vec![];
            let stdin_ctx = ProbeCtx { pattern: &empty_pattern, ..*ctx };
            for c in &cands {
                if !working.contains(c)
                    && stdin_ctx.exit_code(flag, Some(c), None, Some(stdin_data)) == Some(0)
                {
                    working.insert(0, c.clone());
                    break;
                }
            }
        }

        if let Some(first) = working.first() {
            flags[i].1 = Some(first.clone());
            if working.len() > 1 {
                extra_solo_values.insert(flags[i].0.clone(), working[1..].to_vec());
            }
        } else {
            flags[i].1 = None;
        }
    }

    // Companion probing: try failing flags with each working flag as companion
    let working_flags: Vec<(String, String)> = flags.iter()
        .filter_map(|(f, v)| v.as_ref().map(|val| (f.clone(), val.clone())))
        .collect();
    #[allow(clippy::needless_range_loop)]
    for i in 0..flags.len() {
        if flags[i].1.is_some() { continue; }
        let flag = flags[i].0.clone();
        let mut target_cands: Vec<String> = flag_info.extracted_values
            .get(&flag).cloned().unwrap_or_default();
        if target_cands.is_empty() {
            if let Some(mv) = original_metavars.get(&flag) {
                target_cands = candidates(mv).into_iter().map(String::from).collect();
            }
        }

        'companion: for (cf, cv) in &working_flags {
            if *cf == flag { continue; }
            let companion = Some((cf.as_str(), cv.as_str()));
            if target_cands.is_empty() {
                if ctx.succeeds(&flag, None, companion) {
                    break 'companion;
                }
            } else {
                for val in &target_cands {
                    if ctx.succeeds(&flag, Some(val), companion) {
                        flags[i].1 = Some(val.clone());
                        break 'companion;
                    }
                }
            }
        }
    }

    // Mutual compound probing: try pairs of both-failing flags together
    let still_failing: Vec<(usize, Vec<String>)> = (0..flags.len())
        .filter(|&i| flags[i].1.is_none())
        .filter_map(|i| {
            let f = &flags[i].0;
            let mut c: Vec<String> = flag_info.extracted_values
                .get(f).cloned().unwrap_or_default();
            if c.is_empty() {
                if let Some(mv) = original_metavars.get(f) {
                    c = candidates(mv).into_iter().map(String::from).collect();
                }
            }
            if c.is_empty() { return None; }
            Some((i, c))
        })
        .collect();

    for (ai, ref a_cands) in &still_failing {
        if flags[*ai].1.is_some() { continue; }
        for (bi, ref b_cands) in &still_failing {
            if bi == ai || flags[*bi].1.is_some() { continue; }
            if let (Some(va), Some(vb)) = (a_cands.first(), b_cands.first()) {
                let companion = Some((flags[*bi].0.as_str(), vb.as_str()));
                if ctx.succeeds(&flags[*ai].0, Some(va), companion) {
                    flags[*ai].1 = Some(va.clone());
                    flags[*bi].1 = Some(vb.clone());
                    prerequisites.insert(flags[*ai].0.clone(), (flags[*bi].0.clone(), vb.clone()));
                    prerequisites.insert(flags[*bi].0.clone(), (flags[*ai].0.clone(), va.clone()));
                    break;
                }
            }
        }
    }

    (extra_solo_values, prerequisites)
}

/// Generate the experimental design: discover factors and construct the grid.
///
/// DoE workflow:
///   1. Factor identification — parse flags from --help, discover invocation patterns
///   2. Level determination — probe flag values (candidates, error mining, compounds)
///   3. Design construction — cross flags × patterns × contexts into runs
///
/// The grid (runs × contexts) is fully determined before any behavioral observation.
pub fn generate_initial_script(
    binary: &str,
    sub_args: &[&str],
    sandbox: &Sandbox,
) -> Result<(Script, FlagInfo)> {
    // --- Factor identification ---
    // Parse help text for flags (treatment factors) and their metavars (value hints).
    // Discover invocation patterns (positional arg templates that work).
    let help_text = try_help(binary, sub_args, sandbox)?;
    let flag_info = extract_flag_info(&help_text);
    let (mut flags, seen) = parse_flags(&help_text, &flag_info);
    let mut flag_info = flag_info;
    flag_info.all_flags = seen;

    let (working_patterns, stdin_works, probe_pattern) = probe_arg_patterns(binary, sub_args, sandbox);
    if stdin_works { eprintln!("  stdin: accepted"); }
    if probe_pattern.is_some() { eprintln!("  pattern: context-derived"); }

    // --- Level determination (pilot study) ---
    // Probe each flag with candidate values to determine working levels.
    // Sequential and adaptive (standard pilot study practice) — but the
    // main experiment (the grid) is fixed once levels are determined.
    let original_metavars: HashMap<String, String> = flags.iter()
        .filter_map(|(f, mv)| mv.as_ref().map(|m| (f.clone(), m.clone())))
        .collect();
    let numeric_metavar_names = ["NUM", "NUMBER", "N", "SIZE", "COLS", "WIDTH", "COUNT", "LINES", "BYTES"];
    let numeric_flags: HashSet<String> = original_metavars.iter()
        .filter(|(_, mv)| numeric_metavar_names.contains(&mv.to_uppercase().as_str()))
        .map(|(f, _)| f.clone())
        .collect();

    let (extra_solo_values, prerequisites) = if let Some(first_pattern) = working_patterns.first() {
        let probe_dir = tempfile::Builder::new().prefix("bgrid_val_").tempdir()
            .expect("create probe dir");
        let work_dir = probe_dir.path();
        let _ = std::fs::write(work_dir.join("input.txt"), "cherry\napple\nbanana\n");
        let _ = std::fs::write(work_dir.join("other.txt"), "hello world\n");
        let _ = std::fs::create_dir(work_dir.join("subdir"));
        let sub_refs: Vec<&str> = sub_args.to_vec();
        let ctx = ProbeCtx { sandbox, binary, sub_args: &sub_refs, pattern: first_pattern, work_dir };
        probe_values(&ctx, &mut flags, &flag_info, stdin_works)
    } else {
        (HashMap::new(), HashMap::new())
    };

    // --- Design construction ---
    // Cross all flags × invocation patterns × contexts into a fixed grid.
    // No adaptation from here — the design is determined.
    let contexts = crate::data::build_contexts();

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

    // Solo runs: one base + one per flag, for each invocation pattern.
    // Each flag run diffs against the base to isolate the flag's effect.
    for pattern in &working_patterns {
        let base_args: Vec<Arg> = sub_prefix.iter().cloned()
            .chain(pattern.iter().map(&to_arg))
            .collect();
        runs.push(Run { args: base_args.clone(), in_contexts: None, diff_from: None });
        for (flag, metavar) in flags.iter() {
            let mut args = sub_prefix.clone();
            // Include prerequisite companion if this flag needs one
            if let Some((cf, cv)) = prerequisites.get(flag) {
                push_flag_arg(&mut args, cf, Some(cv));
            }
            push_flag_arg(&mut args, flag, metavar.as_deref());
            args.extend(pattern.iter().map(&to_arg));
            runs.push(Run { args, in_contexts: None, diff_from: Some(base_args.clone()) });
        }
        // Extra solo runs for additional working values
        for (flag, extra_vals) in &extra_solo_values {
            for val in extra_vals {
                let mut args = sub_prefix.clone();
                push_flag_arg(&mut args, flag, Some(val));
                args.extend(pattern.iter().map(&to_arg));
                runs.push(Run { args, in_contexts: None, diff_from: Some(base_args.clone()) });
            }
        }
    }
    // Bare-args run (no positional args) — for stdin contexts
    if stdin_works {
        let bare_args: Vec<Arg> = sub_prefix.clone();
        runs.push(Run { args: bare_args.clone(), in_contexts: None, diff_from: None });
        for (flag, metavar) in flags.iter() {
            let mut args = sub_prefix.clone();
            push_flag_arg(&mut args, flag, metavar.as_deref());
            runs.push(Run { args, in_contexts: None, diff_from: Some(bare_args.clone()) });
        }
    }

    // Boundary runs for numeric flags (using first working pattern)
    if let Some(first_pattern) = working_patterns.first() {
        let zero_flags: Vec<&(String, Option<String>)> = flags.iter()
            .filter(|(f, _)| numeric_flags.contains(f))
            .collect();
        if !zero_flags.is_empty() {
            let base_args: Vec<Arg> = sub_prefix.iter().cloned()
                .chain(first_pattern.iter().map(&to_arg))
                .collect();
            for (flag, _) in &zero_flags {
                let mut args = sub_prefix.clone();
                push_flag_arg(&mut args, flag, Some("0"));
                args.extend(first_pattern.iter().map(&to_arg));
                runs.push(Run {
                    args, in_contexts: None,                    diff_from: Some(base_args.clone()),
                });
            }
            for (flag, _) in zero_flags.iter().take(3) {
                let mut args1 = sub_prefix.clone();
                push_flag_arg(&mut args1, flag, Some("-1"));
                args1.extend(first_pattern.iter().map(&to_arg));
                runs.push(Run {
                    args: args1, in_contexts: None,                    diff_from: Some(base_args.clone()),
                });
                let mut args2 = sub_prefix.clone();
                push_flag_arg(&mut args2, flag, Some("2147483647"));
                args2.extend(first_pattern.iter().map(&to_arg));
                runs.push(Run {
                    args: args2, in_contexts: None,                    diff_from: Some(base_args.clone()),
                });
            }
        }
    }

    // Pairwise interaction runs: all flag pairs in both orderings.
    // Detects flags distinguishable only through interaction effects.
    // Uses richest pattern to ensure the tool has input to process.
    let combo_pattern = working_patterns.iter()
        .max_by_key(|p| p.len())
        .or(working_patterns.first());
    if let Some(pattern) = combo_pattern {
        let base_args: Vec<Arg> = sub_prefix.iter().cloned()
            .chain(pattern.iter().map(&to_arg))
            .collect();

        // Build deduplicated flag arg groups (resolve aliases to keep only one per pair).
        // Each group is a Vec<Arg> because short flags with values produce two args (e.g., -A 1).
        let mut all_flag_args: Vec<Vec<Arg>> = Vec::new();
        let mut seen_stems: HashSet<String> = HashSet::new();
        for (flag, metavar) in flags.iter() {
            let canon = flag_info.aliases.get(flag).unwrap_or(flag).clone();
            let key = if *flag < canon { flag.clone() } else { canon };
            if seen_stems.insert(key) {
                let mut group = Vec::new();
                push_flag_arg(&mut group, flag, metavar.as_deref());
                all_flag_args.push(group);
            }
        }

        // Generate all pairwise combos in BOTH orderings.
        // Tools with last-flag-wins semantics (head -q -v ≠ head -v -q)
        // produce different output depending on argument order.
        // Testing both orderings detects order-sensitivity and prevents
        // false positives where alias flags at different list positions
        // get different orderings against a third flag.
        let pair_count = all_flag_args.len() * (all_flag_args.len() - 1);
        eprintln!("  pairs: {} flags, {} combinations (both orderings)", all_flag_args.len(), pair_count);
        for i in 0..all_flag_args.len() {
            for j in 0..all_flag_args.len() {
                if i == j { continue; }
                let mut args = sub_prefix.clone();
                args.extend(all_flag_args[i].iter().cloned());
                args.extend(all_flag_args[j].iter().cloned());
                args.extend(pattern.iter().map(&to_arg));
                runs.push(Run {
                    args,
                    in_contexts: None,
                                       diff_from: Some(base_args.clone()),
                });
            }
        }
    }

    // Error provocation: nonexistent file
    {
        let mut err_args = sub_prefix.clone();
        err_args.push(Arg::Literal("nonexistent-file.txt".into()));
        runs.push(Run { args: err_args, in_contexts: None, diff_from: None });
    }

    Ok((Script { contexts, runs }, flag_info))
}

