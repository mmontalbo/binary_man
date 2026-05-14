//! Flag discovery and behavioral probing from --help text.

use anyhow::{Context, Result};
use regex::Regex;
use std::collections::{HashMap, HashSet};

use crate::parse::{Arg, Run, Script};
use crate::sandbox::{Sandbox, shell_escape};

// --- Batched probe infrastructure ---
// Runs many probes in a single bwrap invocation using the same shell-script
// pattern as grid execution: each probe gets its own cell directory, all run
// in parallel within one sandbox, results read from output files.

/// Result of a single probe within a batch.
struct ProbeResult {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// A batch of probe commands executed in a single bwrap invocation.
struct ProbeBatch<'a> {
    sandbox: &'a Sandbox,
    binary: &'a str,
    work_template: &'a std::path::Path,
    tasks: Vec<(Vec<String>, Option<Vec<u8>>)>, // (args, stdin_data)
}

impl<'a> ProbeBatch<'a> {
    fn new(sandbox: &'a Sandbox, binary: &'a str, work_template: &'a std::path::Path) -> Self {
        ProbeBatch { sandbox, binary, work_template, tasks: Vec::new() }
    }

    /// Queue a probe. Returns the task index for matching results.
    fn add(&mut self, args: Vec<String>, stdin: Option<Vec<u8>>) -> usize {
        let idx = self.tasks.len();
        self.tasks.push((args, stdin));
        idx
    }

    /// Execute all queued probes in one bwrap and return results.
    fn run(self) -> Vec<ProbeResult> {
        if self.tasks.is_empty() {
            return Vec::new();
        }

        let batch_dir = tempfile::Builder::new()
            .prefix("bgrid_probe_")
            .tempdir()
            .expect("create probe batch dir");
        let out_dir = batch_dir.path().join("out");
        let _ = std::fs::create_dir(&out_dir);

        let mut script = String::new();
        for (i, (args, stdin_data)) in self.tasks.iter().enumerate() {
            // Each probe gets its own cell directory copied from the template
            let cell_dir = batch_dir.path().join(format!("c{}", i));
            let _ = std::fs::create_dir(&cell_dir);
            copy_dir_shallow(self.work_template, &cell_dir);

            let stdin_part = if let Some(data) = stdin_data {
                // Write stdin data to a file in the batch dir
                let stdin_file = batch_dir.path().join(format!("s{}", i));
                let _ = std::fs::write(&stdin_file, data);
                format!("cat /batch/s{} | ", i)
            } else {
                String::new()
            };

            let args_str = args.iter()
                .map(|a| shell_escape(a))
                .collect::<Vec<_>>()
                .join(" ");

            script.push_str(&format!(
                "(cd /batch/c{i} && {stdin}timeout {t} {bin} {args} >/batch/out/{i}.out 2>/batch/out/{i}.err; echo $? >/batch/out/{i}.rc) &\n",
                i = i, stdin = stdin_part,
                t = crate::execute::CELL_TIMEOUT_SECS,
                bin = shell_escape(self.binary),
                args = args_str,
            ));
            if (i + 1) % 64 == 0 {
                script.push_str("wait\n");
            }
        }
        script.push_str("wait\n");

        let script_path = batch_dir.path().join("run.sh");
        let _ = std::fs::write(&script_path, &script);

        let env = HashMap::new();
        let mut cmd = self.sandbox.batch_command(batch_dir.path(), "run.sh", &env);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe { cmd.pre_exec(|| { libc::setpgid(0, 0); Ok(()) }); }
        }

        let timeout_secs = crate::execute::CELL_TIMEOUT_SECS * (self.tasks.len() as u64 + 1);
        match cmd.spawn() {
            Ok(mut child) => {
                let child_id = child.id();
                let timer = std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(timeout_secs));
                    #[cfg(unix)]
                    unsafe { libc::kill(-(child_id as i32), libc::SIGKILL); }
                });
                let _ = child.wait();
                drop(timer);
            }
            Err(_) => {
                return (0..self.tasks.len())
                    .map(|_| ProbeResult { exit_code: None, stdout: String::new(), stderr: String::new() })
                    .collect();
            }
        }

        // Read results
        (0..self.tasks.len()).map(|i| {
            let exit_code = std::fs::read_to_string(out_dir.join(format!("{}.rc", i)))
                .ok()
                .and_then(|s| s.trim().parse().ok());
            let stdout = std::fs::read_to_string(out_dir.join(format!("{}.out", i)))
                .unwrap_or_default();
            let stderr = std::fs::read_to_string(out_dir.join(format!("{}.err", i)))
                .unwrap_or_default();
            ProbeResult { exit_code, stdout, stderr }
        }).collect()
    }
}

/// Copy files (non-recursive, top-level only) from src to dst.
fn copy_dir_shallow(src: &std::path::Path, dst: &std::path::Path) {
    if let Ok(entries) = std::fs::read_dir(src) {
        for entry in entries.flatten() {
            let path = entry.path();
            let dest = dst.join(entry.file_name());
            if path.is_dir() {
                let _ = std::fs::create_dir(&dest);
                copy_dir_shallow(&path, &dest);
            } else {
                let _ = std::fs::copy(&path, &dest);
            }
        }
    }
}

/// Extracted flag info from --help text.
pub struct FlagInfo {
    pub descs: HashMap<String, String>,   // flag -> description
    pub aliases: HashMap<String, String>, // short -> long (and long -> short)
    pub all_flags: HashSet<String>,       // every flag discovered
    pub extracted_values: HashMap<String, Vec<String>>, // flag -> values mined from help text
    /// Ordered flag list with resolved metavars (short flags inherit from long aliases).
    pub flags: Vec<(String, Option<String>)>,
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

    // Flag regexes for the unified pass
    let flag_re = Regex::new(
        r"^\s+(-[a-zA-Z0-9](?:,\s*--[a-zA-Z][-a-zA-Z0-9]*(?:=\S+)?)?|--[a-zA-Z][-a-zA-Z0-9]*(?:=\S+)?)\s{2,}(.+)"
    ).unwrap();
    let short_re = Regex::new(r"(?:^|\s)-([a-zA-Z0-9])\b").unwrap();
    let short_metavar_re = Regex::new(r"^\s{2,}.*-([a-zA-Z0-9])\s+([A-Z][-A-Z_]*)(?:\s|,|$)").unwrap();
    let long_re = Regex::new(r"--([a-zA-Z][a-zA-Z0-9-]*)(?:[=\s]([A-Z][A-Z_]*))?").unwrap();

    // Collect flags with metavars in insertion order
    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let mut seen_flags: HashSet<String> = HashSet::new();
    let mut long_metavars: HashMap<String, String> = HashMap::new();
    let mut short_metavars: HashMap<String, String> = HashMap::new();

    // Pre-pass: capture short flag metavars (only on indented flag lines)
    for line in help_text.lines() {
        for cap in short_metavar_re.captures_iter(line) {
            short_metavars.insert(format!("-{}", &cap[1]), cap[2].to_string());
        }
    }

    // Two-pass: first collect full multi-line descriptions per flag group,
    // then mine all patterns from the complete description.
    let lines: Vec<&str> = help_text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        // Collect flags with metavars (permissive matching)
        for cap in short_re.captures_iter(lines[i]) {
            let flag = format!("-{}", &cap[1]);
            if seen_flags.insert(flag.clone()) {
                flags.push((flag.clone(), short_metavars.get(&flag).cloned()));
            }
        }
        for cap in long_re.captures_iter(lines[i]) {
            let name = format!("--{}", &cap[1]);
            if name == "--help" || name == "--version" { continue; }
            let metavar = cap.get(2).map(|m| m.as_str().to_string());
            if let Some(mv) = &metavar { long_metavars.insert(name.clone(), mv.clone()); }
            if seen_flags.insert(name.clone()) { flags.push((name, metavar)); }
        }

        // Extract descriptions, aliases, and values from flag-description lines
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

    // Propagate metavars from long aliases to short flags
    for (flag, metavar) in &mut flags {
        if metavar.is_none() && flag.len() == 2 {
            if let Some(long_alias) = aliases.get(flag) {
                if let Some(mv) = long_metavars.get(long_alias) {
                    *metavar = Some(mv.clone());
                }
            }
        }
    }

    all_flags = seen_flags;
    FlagInfo { descs, aliases, all_flags, extracted_values, flags }
}

/// Try --help, then -h to get help text from a binary.
pub fn try_help(binary: &str, sub_args: &[&str], sandbox: &Sandbox) -> Result<String> {
    let tmp = tempfile::Builder::new().prefix("bgrid_help_").tempdir()
        .context("create help sandbox")?;

    for help_flag in &["--help", "-h"] {
        let mut args: Vec<&str> = sub_args.to_vec();
        args.push(help_flag);
        let env = HashMap::new();
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
    help_text: &str,
) -> (Vec<Vec<String>>, bool, Option<String>) {
    // Create a minimal workspace template for probing
    let probe_dir = match tempfile::Builder::new().prefix("bgrid_probe_").tempdir() {
        Ok(d) => d,
        Err(_) => return (vec![vec!["input.txt".into()]], false, None),
    };
    let work_dir = probe_dir.path();
    let probe_content = "cherry\napple\nbanana\n";
    let _ = std::fs::write(work_dir.join("input.txt"), probe_content);
    let _ = std::fs::write(work_dir.join("other.txt"), "hello world\n");
    let _ = std::fs::create_dir(work_dir.join("subdir"));
    let _ = std::fs::write(work_dir.join("subdir/nested.txt"), "nested\n");

    let probe_pattern = probe_content.lines().next().unwrap_or("test").to_string();
    let pattern_str = probe_pattern.as_str();

    // All candidate arg patterns to try
    let candidates: Vec<Vec<&str>> = vec![
        vec![],                                       // no args
        vec!["input.txt"],                            // single file
        vec!["."],                                    // directory
        vec!["input.txt", "other.txt"],               // two files (diff, paste)
        vec!["input.txt", "subdir"],                  // file to directory
    ];
    let pattern_candidates: Vec<Vec<&str>> = vec![
        vec![pattern_str, "input.txt"],               // pattern + file (grep, sed)
        vec![pattern_str, "."],                       // pattern + directory (grep -r)
    ];

    // Structural candidates from usage line
    let usage_line = help_text.lines()
        .find(|l| l.starts_with("Usage:") || l.starts_with("usage:"))
        .unwrap_or("");
    let structural_candidates: Vec<Vec<&str>> = if usage_line.contains("COMMAND") {
        vec![vec!["echo"], vec!["echo", "input.txt"]]
    } else if usage_line.contains("[expression]") || usage_line.contains("EXPRESSION") {
        vec![vec![".", "-print"], vec![".", "-name", "*.txt"], vec![".", "-type", "f"]]
    } else {
        vec![]
    };

    let stdin_data = b"cherry\napple\nbanana\n".to_vec();

    // Batch all probes into one bwrap invocation
    let mut batch = ProbeBatch::new(sandbox, binary, work_dir);

    // Track which task indices correspond to which candidate type
    let n_regular = candidates.len();

    // Regular + pattern candidates
    let all_candidates: Vec<&Vec<&str>> = candidates.iter()
        .chain(pattern_candidates.iter())
        .collect();
    for candidate in &all_candidates {
        let args: Vec<String> = sub_args.iter().map(|s| s.to_string())
            .chain(candidate.iter().map(|s| s.to_string()))
            .collect();
        batch.add(args, None);
    }

    // Stdin probes: bare args, then with "-"
    let stdin_bare_idx = batch.add(
        sub_args.iter().map(|s| s.to_string()).collect(),
        Some(stdin_data.clone()),
    );
    let stdin_dash_idx = batch.add(
        sub_args.iter().map(|s| s.to_string()).chain(std::iter::once("-".to_string())).collect(),
        Some(stdin_data.clone()),
    );

    // Structural candidates
    let structural_start = batch.tasks.len();
    for candidate in &structural_candidates {
        let args: Vec<String> = sub_args.iter().map(|s| s.to_string())
            .chain(candidate.iter().map(|s| s.to_string()))
            .collect();
        batch.add(args, None);
    }

    let results = batch.run();

    // Process regular + pattern candidates
    let mut working = Vec::new();
    let mut found_pattern_candidate = false;
    for (i, candidate) in all_candidates.iter().enumerate() {
        let r = &results[i];
        let exit = r.exit_code.unwrap_or(-1);
        let has_output = !r.stdout.trim().is_empty();
        // Check for fs effect: each cell gets its own dir, so count entries beyond template
        let has_fs_effect = i < n_regular && {
            // The cell directory had 3 entries (input.txt, other.txt, subdir)
            // If more exist after the probe, the command created files
            false // fs effect detection deferred: batch cells are isolated
        };
        if exit == 0 || (exit <= 1 && has_output) || has_fs_effect {
            let pattern: Vec<String> = candidate.iter().map(|s| s.to_string()).collect();
            if i >= n_regular {
                found_pattern_candidate = true;
            }
            working.push(pattern);
        }
    }

    // Process stdin probes
    let stdin_works = [stdin_bare_idx, stdin_dash_idx].iter().any(|&idx| {
        let r = &results[idx];
        let exit = r.exit_code.unwrap_or(-1);
        (exit == 0 || exit == 1) && !r.stdout.trim().is_empty()
    });

    // Process structural candidates
    for (si, candidate) in structural_candidates.iter().enumerate() {
        let r = &results[structural_start + si];
        let exit = r.exit_code.unwrap_or(-1);
        let has_output = !r.stdout.trim().is_empty();
        if exit == 0 || (exit <= 1 && has_output) {
            working.push(candidate.iter().map(|s| s.to_string()).collect());
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

/// Insert a flag (with optional value) at a specific position.
fn push_flag_arg_at(args: &mut Vec<Arg>, pos: usize, flag: &str, value: Option<&str>) {
    match value {
        Some(v) if flag.starts_with("--") => {
            args.insert(pos, Arg::Literal(format!("{}={}", flag, v)));
        }
        Some(v) => {
            args.insert(pos, Arg::Literal(flag.to_string()));
            args.insert(pos + 1, Arg::Literal(v.to_string()));
        }
        None => {
            args.insert(pos, Arg::Literal(flag.to_string()));
        }
    }
}

/// Build probe args: sub_args + [companion] + flag[=value] + pattern.
fn build_probe_args(
    sub_args: &[&str], flag: &str, value: Option<&str>,
    companion: Option<(&str, &str)>, pattern: &[String],
) -> Vec<String> {
    let mut args: Vec<String> = sub_args.iter().map(|s| s.to_string()).collect();
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
    args.extend(pattern.iter().cloned());
    args
}

/// Pilot study: determine working factor levels for each flag.
/// Batches probes into phases, each a single bwrap invocation:
///   1. Solo candidates + error mine probes
///   2. Mined value probes + stdin retries
///   3. Companion probes
///   4. Mutual compound probes
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn probe_values(
    sandbox: &Sandbox,
    binary: &str,
    sub_args: &[&str],
    pattern: &[String],
    work_template: &std::path::Path,
    flags: &mut [(String, Option<String>)],
    flag_info: &FlagInfo,
    stdin_works: bool,
) -> (HashMap<String, Vec<String>>, HashMap<String, (String, String)>) {
    let mut extra_solo_values: HashMap<String, Vec<String>> = HashMap::new();
    let mut prerequisites: HashMap<String, (String, String)> = HashMap::new();

    let original_metavars: HashMap<String, String> = flags.iter()
        .filter_map(|(f, mv)| mv.as_ref().map(|m| (f.clone(), m.clone())))
        .collect();

    // Build candidate lists per flag (reused across phases)
    let flag_cands: Vec<Option<Vec<String>>> = (0..flags.len()).map(|i| {
        let (ref flag, ref metavar) = flags[i];
        let has_extracted = flag_info.extracted_values.contains_key(flag);
        if metavar.is_none() && !has_extracted { return None; }
        let mut cands: Vec<String> = Vec::new();
        if let Some(extracted) = flag_info.extracted_values.get(flag) {
            cands.extend(extracted.iter().cloned());
        }
        if let Some(mv) = metavar.as_ref() {
            for c in candidates(mv) {
                if !cands.iter().any(|e| e == c) { cands.push(c.to_string()); }
            }
        }
        Some(cands)
    }).collect();

    // === Phase 1: Solo candidates + error mine probes (1 bwrap) ===
    let mut batch1 = ProbeBatch::new(sandbox, binary, work_template);

    // Solo: (flag_index, value, task_index)
    let mut solo_tasks: Vec<(usize, String, usize)> = Vec::new();
    // Error mine: (flag_index, task_index)
    let mut mine_tasks: Vec<(usize, usize)> = Vec::new();

    for (i, cands) in flag_cands.iter().enumerate() {
        let Some(cands) = cands else { continue };
        let flag = &flags[i].0;
        for c in cands {
            let args = build_probe_args(sub_args, flag, Some(c), None, pattern);
            let idx = batch1.add(args, None);
            solo_tasks.push((i, c.clone(), idx));
        }
        // Error mine probe (submit unconditionally; decide whether to use results later)
        let mut mine_args: Vec<String> = sub_args.iter().map(|s| s.to_string()).collect();
        if flag.starts_with("--") {
            mine_args.push(format!("{}=__bgrid_invalid__", flag));
        } else {
            mine_args.push(flag.to_string());
            mine_args.push("__bgrid_invalid__".into());
        }
        mine_args.extend(pattern.iter().cloned());
        let idx = batch1.add(mine_args, None);
        mine_tasks.push((i, idx));
    }

    let r1 = batch1.run();

    // Collect solo results per flag
    struct FlagState { working: Vec<String>, has_exit0: bool }
    let mut states: HashMap<usize, FlagState> = HashMap::new();

    for &(fi, ref val, ti) in &solo_tasks {
        let exit = r1[ti].exit_code.unwrap_or(-1);
        let state = states.entry(fi).or_insert(FlagState { working: Vec::new(), has_exit0: false });
        if exit <= 1 {
            state.working.push(val.clone());
            if exit == 0 { state.has_exit0 = true; }
        }
    }

    // === Phase 2: Mined value probes + stdin retries (1 bwrap) ===
    let mut batch2 = ProbeBatch::new(sandbox, binary, work_template);
    let mut mined_tasks: Vec<(usize, String, usize)> = Vec::new();
    let mut stdin_tasks: Vec<(usize, String, usize)> = Vec::new();

    for &(fi, ti) in &mine_tasks {
        let state = states.get(&fi);
        let needs_mining = state.is_none_or(|s| s.working.is_empty() || !s.has_exit0);
        if !needs_mining { continue; }
        let mined = mine_valid_values(&r1[ti].stderr);
        for val in mined {
            let args = build_probe_args(sub_args, &flags[fi].0, Some(&val), None, pattern);
            let idx = batch2.add(args, None);
            mined_tasks.push((fi, val, idx));
        }
    }

    // Stdin retry for flags still without exit 0
    if stdin_works {
        let stdin_data = b"cherry\napple\nbanana\n".to_vec();
        for (i, cands) in flag_cands.iter().enumerate() {
            let Some(cands) = cands else { continue };
            let state = states.get(&i);
            if state.is_some_and(|s| s.has_exit0) { continue; }
            for c in cands {
                let args = build_probe_args(sub_args, &flags[i].0, Some(c), None, &[]);
                let idx = batch2.add(args, Some(stdin_data.clone()));
                stdin_tasks.push((i, c.clone(), idx));
            }
        }
    }

    if !batch2.tasks.is_empty() {
        let r2 = batch2.run();

        // Process mined value results
        for &(fi, ref val, ti) in &mined_tasks {
            let exit = r2[ti].exit_code.unwrap_or(-1);
            if exit <= 1 {
                let state = states.entry(fi).or_insert(FlagState { working: Vec::new(), has_exit0: false });
                if exit == 0 && !state.has_exit0 {
                    state.working.insert(0, val.clone());
                    state.has_exit0 = true;
                } else if !state.working.contains(val) {
                    state.working.push(val.clone());
                }
            }
        }

        // Process stdin retry results — only take first exit-0 per flag
        let mut stdin_resolved: HashSet<usize> = HashSet::new();
        for &(fi, ref val, ti) in &stdin_tasks {
            if stdin_resolved.contains(&fi) { continue; }
            if r2[ti].exit_code == Some(0) {
                let state = states.entry(fi).or_insert(FlagState { working: Vec::new(), has_exit0: false });
                state.working.insert(0, val.clone());
                state.has_exit0 = true;
                stdin_resolved.insert(fi);
            }
        }
    }

    // Apply results to flags
    for (fi, state) in &states {
        if let Some(first) = state.working.first() {
            flags[*fi].1 = Some(first.clone());
            if state.working.len() > 1 {
                extra_solo_values.insert(flags[*fi].0.clone(), state.working[1..].to_vec());
            }
        } else {
            flags[*fi].1 = None;
        }
    }

    // === Phase 3: Companion probing (1 bwrap) ===
    // Flags with values enter the grid, but only exit-0 flags are truly resolved.
    // Flags with exit-1-only values (e.g., cut -d , → errors without -f) still
    // need companion probing to find the enabling flag.
    let has_exit0: HashSet<usize> = states.iter()
        .filter(|(_, s)| s.has_exit0)
        .map(|(fi, _)| *fi)
        .collect();

    let working_flags: Vec<(String, String)> = flags.iter()
        .filter_map(|(f, v)| v.as_ref().map(|val| (f.clone(), val.clone())))
        .collect();

    // Build all companion probes for flags without exit-0 values
    let mut batch3 = ProbeBatch::new(sandbox, binary, work_template);
    // (flag_index, companion_flag, value_or_none, task_index)
    let mut companion_tasks: Vec<(usize, String, Option<String>, usize)> = Vec::new();

    #[allow(clippy::needless_range_loop)]
    for i in 0..flags.len() {
        if has_exit0.contains(&i) { continue; }
        let flag = &flags[i].0;
        let mut target_cands: Vec<String> = flag_info.extracted_values
            .get(flag).cloned().unwrap_or_default();
        if target_cands.is_empty() {
            if let Some(mv) = original_metavars.get(flag) {
                target_cands = candidates(mv).into_iter().map(String::from).collect();
            }
        }
        for (cf, cv) in &working_flags {
            if *cf == *flag { continue; }
            let companion = Some((cf.as_str(), cv.as_str()));
            if target_cands.is_empty() {
                let args = build_probe_args(sub_args, flag, None, companion, pattern);
                let idx = batch3.add(args, None);
                companion_tasks.push((i, cf.clone(), None, idx));
            } else {
                for val in &target_cands {
                    let args = build_probe_args(sub_args, flag, Some(val), companion, pattern);
                    let idx = batch3.add(args, None);
                    companion_tasks.push((i, cf.clone(), Some(val.clone()), idx));
                }
            }
        }
    }

    if !batch3.tasks.is_empty() {
        let r3 = batch3.run();
        // For each failing flag, find the first companion that produces exit 0.
        // Exit 0 required (not exit 1) — exit 1 with a companion often means the
        // companion masked the error rather than enabling the flag.
        let mut resolved: HashSet<usize> = HashSet::new();
        for &(fi, ref cf, ref val, ti) in &companion_tasks {
            if resolved.contains(&fi) { continue; }
            let exit = r3[ti].exit_code.unwrap_or(-1);
            if exit == 0 {
                if let Some(v) = val {
                    flags[fi].1 = Some(v.clone());
                }
                // The companion that made this flag work is a prerequisite
                let cv = working_flags.iter()
                    .find(|(f, _)| f == cf)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                prerequisites.insert(flags[fi].0.clone(), (cf.clone(), cv.clone()));
                resolved.insert(fi);
            }
        }
    }

    // === Phase 4: Mutual compound probing (1 bwrap) ===
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

    if still_failing.len() >= 2 {
        let mut batch4 = ProbeBatch::new(sandbox, binary, work_template);
        // (flag_a_idx, flag_b_idx, value_a, value_b, task_index)
        let mut compound_tasks: Vec<(usize, usize, String, String, usize)> = Vec::new();

        for (ai, ref a_cands) in &still_failing {
            for (bi, ref b_cands) in &still_failing {
                if bi == ai { continue; }
                if let (Some(va), Some(vb)) = (a_cands.first(), b_cands.first()) {
                    let companion = Some((flags[*bi].0.as_str(), vb.as_str()));
                    let args = build_probe_args(sub_args, &flags[*ai].0, Some(va), companion, pattern);
                    let idx = batch4.add(args, None);
                    compound_tasks.push((*ai, *bi, va.clone(), vb.clone(), idx));
                }
            }
        }

        if !batch4.tasks.is_empty() {
            let r4 = batch4.run();
            for &(ai, bi, ref va, ref vb, ti) in &compound_tasks {
                if flags[ai].1.is_some() || flags[bi].1.is_some() { continue; }
                let exit = r4[ti].exit_code.unwrap_or(-1);
                if exit <= 1 {
                    flags[ai].1 = Some(va.clone());
                    flags[bi].1 = Some(vb.clone());
                    prerequisites.insert(flags[ai].0.clone(), (flags[bi].0.clone(), vb.clone()));
                    prerequisites.insert(flags[bi].0.clone(), (flags[ai].0.clone(), va.clone()));
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
    let t0 = std::time::Instant::now();
    let help_text = try_help(binary, sub_args, sandbox)?;
    let flag_info = extract_flag_info(&help_text);
    let mut flags = flag_info.flags.clone();
    let t_parse = t0.elapsed();

    let (working_patterns, stdin_works, probe_pattern) = probe_arg_patterns(binary, sub_args, sandbox, &help_text);
    if stdin_works { eprintln!("  stdin: accepted"); }
    if probe_pattern.is_some() { eprintln!("  pattern: context-derived"); }
    let t_patterns = t0.elapsed();

    // --- Level determination (pilot study) ---
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
        probe_values(sandbox, binary, &sub_refs, first_pattern, work_dir,
            &mut flags, &flag_info, stdin_works)
    } else {
        (HashMap::new(), HashMap::new())
    };

    let t_probe = t0.elapsed();

    // --- Design construction ---
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
            // Include prerequisite companion if this flag needs one.
            // The diff baseline also includes the companion so the delta
            // isolates the flag's effect (not the companion's).
            let diff_base = if let Some((cf, cv)) = prerequisites.get(flag) {
                push_flag_arg(&mut args, cf, Some(cv));
                let mut prereq_base = base_args.clone();
                let insert_pos = sub_prefix.len(); // after sub_args, before positionals
                push_flag_arg_at(&mut prereq_base, insert_pos, cf, Some(cv));
                // Ensure the prerequisite base run exists
                if !runs.iter().any(|r| r.args == prereq_base) {
                    runs.push(Run { args: prereq_base.clone(), in_contexts: None, diff_from: None });
                }
                prereq_base
            } else {
                base_args.clone()
            };
            push_flag_arg(&mut args, flag, metavar.as_deref());
            args.extend(pattern.iter().map(&to_arg));
            runs.push(Run { args, in_contexts: None, diff_from: Some(diff_base) });
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
    // Scoped to a diverse subset of contexts — combos only need to
    // prove two flags are DIFFERENT, not measure sensitivity across
    // all contexts. One per content type + stdin for diversity.
    let combo_contexts: Vec<String> = vec![
        "words_minimal", "numbers_minimal", "passwd_minimal",
        "formatted_minimal", "csv_minimal", "words_minimal / stdin",
    ].into_iter().map(String::from).collect();

    let combo_pattern = working_patterns.iter()
        .max_by_key(|p| p.len())
        .or(working_patterns.first());
    if let Some(pattern) = combo_pattern {
        let base_args: Vec<Arg> = sub_prefix.iter().cloned()
            .chain(pattern.iter().map(&to_arg))
            .collect();

        // Base run must also exist in combo contexts for diff_from to work
        runs.push(Run {
            args: base_args.clone(),
            in_contexts: Some(combo_contexts.clone()),
            diff_from: None,
        });

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

        let pair_count = all_flag_args.len() * (all_flag_args.len() - 1);
        eprintln!("  pairs: {} flags, {} combinations (in {} contexts)", all_flag_args.len(), pair_count, combo_contexts.len());
        for i in 0..all_flag_args.len() {
            for j in 0..all_flag_args.len() {
                if i == j { continue; }
                let mut args = sub_prefix.clone();
                args.extend(all_flag_args[i].iter().cloned());
                args.extend(all_flag_args[j].iter().cloned());
                args.extend(pattern.iter().map(&to_arg));
                runs.push(Run {
                    args,
                    in_contexts: Some(combo_contexts.clone()),
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

    let t_total = t0.elapsed();
    eprintln!("  discovery: parse={}ms patterns={}ms probe={}ms design={}ms total={}ms",
        t_parse.as_millis(), (t_patterns - t_parse).as_millis(),
        (t_probe - t_patterns).as_millis(), (t_total - t_probe).as_millis(),
        t_total.as_millis());

    Ok((Script { contexts, runs }, flag_info))
}

