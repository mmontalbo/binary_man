use anyhow::{Context, Result};
use std::path::PathBuf;

mod delta;
mod execute;
mod parse;
mod sandbox;
mod validate;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 2 && args[1] == "init" {
        return cmd_init(&args[2..]);
    }

    if args.len() < 3 {
        eprintln!("Usage: bman-probe <binary> <test-file>");
        eprintln!("       bman-probe init <binary> <surface-dir>");
        std::process::exit(1);
    }

    let binary = &args[1];
    let test_path = PathBuf::from(&args[2]);

    cmd_run(binary, &test_path)
}

/// Run a test file: execute tests, report to stderr, append results to file.
fn cmd_run(binary: &str, test_path: &PathBuf) -> Result<()> {
    let source = std::fs::read_to_string(test_path)
        .with_context(|| format!("read {}", test_path.display()))?;

    // Strip old results block before parsing
    let base_source = strip_results_block(&source);

    let mut script = parse::parse_script(&base_source)
        .with_context(|| format!("parse {}", test_path.display()))?;

    // Load shared contexts from setup.test in the same directory
    if let Some(parent) = test_path.parent() {
        let setup_path = parent.join("setup.test");
        if setup_path.exists() && setup_path != *test_path {
            let setup_source = std::fs::read_to_string(&setup_path)
                .with_context(|| format!("read {}", setup_path.display()))?;
            let setup_script = parse::parse_script(&setup_source)
                .with_context(|| format!("parse {}", setup_path.display()))?;

            // If the test file has only the default context with no commands,
            // replace it with the setup contexts
            let has_own_contexts = script.contexts.len() > 1
                || (script.contexts.len() == 1 && script.contexts[0].name != "(default)")
                || (script.contexts.len() == 1 && !script.contexts[0].commands.is_empty());

            if !has_own_contexts {
                script.contexts = setup_script.contexts;
            }

            // Merge setup tests (baseline invocations) into the test file's tests
            for setup_test in setup_script.tests {
                if !script.tests.iter().any(|t| t.args == setup_test.args) {
                    script.tests.insert(0, setup_test);
                }
            }
        }
    }

    let ctx_names: Vec<&str> = script.contexts.iter().map(|c| c.name.as_str()).collect();
    eprintln!("Binary: {}", binary);
    eprintln!(
        "Contexts: {} ({})",
        script.contexts.len(),
        ctx_names.join(", ")
    );
    eprintln!("Tests: {}", script.tests.len());

    let results = execute::run_script(binary, &script)?;

    // Build results text for appending to file
    let mut results_lines: Vec<String> = Vec::new();
    results_lines.push(String::new());
    results_lines.push("#> --- results ---".to_string());

    let mut total_checks = 0;
    let mut total_passed = 0;

    for result in &results {
        let num_contexts = result.context_results.len();
        if num_contexts == 0 {
            continue;
        }

        let num_checks = result.context_results[0].checks.len();
        let args_str = format!("{:?}", result.args);

        for cr in &result.context_results {
            let stdout_lines: Vec<&str> = cr.observation.stdout.lines().collect();
            let stderr_str = if cr.observation.stderr.is_empty() {
                "(empty)".to_string()
            } else {
                cr.observation.stderr.trim().to_string()
            };

            results_lines.push(format!(
                "#> test {} in {}:",
                args_str, cr.context_name
            ));

            // Always show observation
            if stdout_lines.is_empty() {
                results_lines.push("#>   stdout: (empty)".to_string());
            } else {
                results_lines.push(format!(
                    "#>   stdout ({} lines):",
                    stdout_lines.len()
                ));
                for line in stdout_lines.iter().take(10) {
                    results_lines.push(format!("#>     {}", line));
                }
                if stdout_lines.len() > 10 {
                    results_lines.push(format!(
                        "#>     ... ({} more)",
                        stdout_lines.len() - 10
                    ));
                }
            }
            if !cr.observation.stderr.is_empty() {
                results_lines.push(format!("#>   stderr: {}", stderr_str));
            }
            results_lines.push(format!(
                "#>   exit: {}",
                cr.observation.exit_code.unwrap_or(-1)
            ));

            // Show check results
            for check in &cr.checks {
                let mark = if check.passed { "passed" } else { "FAILED" };
                results_lines.push(format!("#>   {}: {}", mark, check.detail));
                if !check.passed {
                    for ctx_line in &check.context {
                        results_lines.push(format!("#>     {}", ctx_line));
                    }
                }
            }
        }

        // For observation-only blocks, suggest expectations and validate across contexts
        if num_checks == 0 {
            let baseline_args: Option<&Vec<String>> = results
                .iter()
                .filter(|r| r.args != result.args && !r.context_results.is_empty())
                .min_by_key(|r| r.args.len())
                .map(|r| &r.args);

            if let Some(base_args) = baseline_args {
                // Generate suggestions from first non-empty context
                let mut suggestions = Vec::new();
                let mut source_ctx = "";

                for cr in &result.context_results {
                    if cr.observation.stdout.trim().is_empty() {
                        continue;
                    }
                    let base_result = results.iter().find(|r| r.args == *base_args);
                    let base_obs = base_result.and_then(|r| {
                        r.context_results
                            .iter()
                            .find(|bcr| bcr.context_name == cr.context_name)
                            .map(|bcr| &bcr.observation)
                    });
                    if let Some(base) = base_obs {
                        if !base.stdout.trim().is_empty() {
                            suggestions =
                                suggest_expectations(base, &cr.observation, base_args);
                            source_ctx = &cr.context_name;
                            break;
                        }
                    }
                }

                if !suggestions.is_empty() {
                    // Validate each suggestion across all contexts
                    results_lines.push(format!(
                        "#> suggested (from {}):",
                        source_ctx
                    ));

                    let base_result = results.iter().find(|r| r.args == *base_args);

                    for sugg in &suggestions {
                        // Try to parse and validate the suggestion across contexts
                        let mut holds_in = Vec::new();
                        let mut fails_in = Vec::new();

                        for cr in &result.context_results {
                            let base_obs = base_result.and_then(|r| {
                                r.context_results
                                    .iter()
                                    .find(|bcr| bcr.context_name == cr.context_name)
                                    .map(|bcr| &bcr.observation)
                            });

                            if let Some(base) = base_obs {
                                let holds = check_suggestion(sugg, base, &cr.observation);
                                if holds {
                                    holds_in.push(cr.context_name.as_str());
                                } else {
                                    fails_in.push(cr.context_name.as_str());
                                }
                            }
                        }

                        let total_ctx = holds_in.len() + fails_in.len();
                        if holds_in.len() == total_ctx {
                            results_lines.push(format!(
                                "#>   {} — all {} contexts",
                                sugg, total_ctx
                            ));
                        } else if !holds_in.is_empty() {
                            results_lines.push(format!(
                                "#>   {} — holds in: {} (fails in: {})",
                                sugg,
                                holds_in.join(", "),
                                fails_in.join(", ")
                            ));
                        } else {
                            // Fails everywhere — still show but mark
                            results_lines.push(format!(
                                "#>   {} — fails in all contexts",
                                sugg
                            ));
                        }
                    }
                }
            }
        }

        // Cross-context summary for multi-context tests
        if num_contexts > 1 && num_checks > 0 {
            results_lines.push(format!("#> summary {}:", args_str));
            for ci in 0..num_checks {
                let detail = &result.context_results[0].checks[ci].detail;
                let passed_in: Vec<&str> = result
                    .context_results
                    .iter()
                    .filter(|cr| cr.checks[ci].passed)
                    .map(|cr| cr.context_name.as_str())
                    .collect();
                let failed_in: Vec<&str> = result
                    .context_results
                    .iter()
                    .filter(|cr| !cr.checks[ci].passed)
                    .map(|cr| cr.context_name.as_str())
                    .collect();

                if failed_in.is_empty() {
                    results_lines
                        .push(format!("#>   {}: all {} contexts", detail, num_contexts));
                } else if passed_in.is_empty() {
                    results_lines.push(format!("#>   {}: failed in all contexts", detail));
                } else {
                    results_lines.push(format!(
                        "#>   {}: passed in {} (failed in: {})",
                        detail,
                        passed_in.join(", "),
                        failed_in.join(", ")
                    ));
                }
            }
        }

        total_checks += num_checks * num_contexts;
        for cr in &result.context_results {
            total_passed += cr.checks.iter().filter(|c| c.passed).count();
        }
    }

    results_lines.push(format!(
        "#> {}/{} checks passed",
        total_passed, total_checks
    ));

    // Also report to stderr
    for result in &results {
        let num_contexts = result.context_results.len();
        if num_contexts == 0 {
            continue;
        }
        let num_checks = result.context_results[0].checks.len();

        if num_checks == 0 {
            // Observation-only block
            eprintln!("  ? test args {:?}: observation only", result.args);
            for cr in &result.context_results {
                let lines = cr.observation.stdout.lines().count();
                let exit = cr.observation.exit_code.unwrap_or(-1);
                eprintln!(
                    "    {}: {} stdout lines, exit {}",
                    cr.context_name, lines, exit
                );
            }
        } else if num_contexts == 1 {
            let cr = &result.context_results[0];
            let passed = cr.checks.iter().filter(|c| c.passed).count();
            let status = if passed == num_checks { "✓" } else { "✗" };
            eprintln!(
                "  {} test args {:?}: {}/{} passed",
                status, result.args, passed, num_checks
            );
            for check in &cr.checks {
                if !check.passed {
                    eprintln!("    ✗ {}", check.detail);
                    for ctx_line in &check.context {
                        eprintln!("      {}", ctx_line);
                    }
                }
            }
        } else {
            let all_pass = result.context_results.iter().all(|cr| {
                cr.checks.iter().all(|c| c.passed)
            });
            let status = if all_pass { "✓" } else { "✗" };
            eprintln!(
                "  {} test args {:?}: {} checks across {} contexts",
                status, result.args, num_checks, num_contexts
            );
            for ci in 0..num_checks {
                let detail = &result.context_results[0].checks[ci].detail;
                let n_passed = result
                    .context_results
                    .iter()
                    .filter(|cr| cr.checks[ci].passed)
                    .count();
                if n_passed == num_contexts {
                    eprintln!("    ✓ {} — all {}", detail, num_contexts);
                } else if n_passed == 0 {
                    eprintln!("    ✗ {} — none", detail);
                } else {
                    eprintln!(
                        "    ~ {} — {}/{}",
                        detail, n_passed, num_contexts
                    );
                }
            }
        }
    }

    eprintln!(
        "\nResult: {}/{} checks passed",
        total_passed, total_checks
    );

    // Write results back to file
    let mut output = base_source.trim_end().to_string();
    output.push('\n');
    for line in &results_lines {
        output.push_str(line);
        output.push('\n');
    }

    let tmp_path = test_path.with_extension("test.tmp");
    std::fs::write(&tmp_path, &output)
        .with_context(|| format!("write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, test_path)
        .with_context(|| format!("rename to {}", test_path.display()))?;

    // Exit 1 only if any check failed in ALL contexts it ran in (totally wrong).
    // Context-dependent checks (pass in some, fail in others) are informational.
    let mut any_total_failure = false;
    for result in &results {
        if result.context_results.is_empty() {
            continue;
        }
        let num_checks = result.context_results[0].checks.len();
        for ci in 0..num_checks {
            let all_failed = result
                .context_results
                .iter()
                .all(|cr| !cr.checks[ci].passed);
            if all_failed && !result.context_results.is_empty() {
                any_total_failure = true;
            }
        }
    }
    if any_total_failure {
        std::process::exit(1);
    }

    Ok(())
}

/// Strip the #> results block from the end of a file.
fn strip_results_block(source: &str) -> String {
    let mut lines: Vec<&str> = source.lines().collect();

    // Find the results marker and strip from there
    if let Some(pos) = lines.iter().position(|l| l.trim() == "#> --- results ---") {
        // Also strip any blank lines immediately before the marker
        let mut start = pos;
        while start > 0 && lines[start - 1].trim().is_empty() {
            start -= 1;
        }
        lines.truncate(start);
    }

    let mut result = lines.join("\n");
    result.push('\n');
    result
}

/// Initialize a surface directory by running --help and generating stubs.
fn cmd_init(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        eprintln!("Usage: bman-probe init <binary> <surface-dir>");
        std::process::exit(1);
    }

    let binary = &args[0];
    let dir = PathBuf::from(&args[1]);

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create {}", dir.display()))?;

    // Run --help
    let help_output = std::process::Command::new(binary)
        .arg("--help")
        .output()
        .with_context(|| format!("run {} --help", binary))?;

    let help_text = String::from_utf8_lossy(&help_output.stdout).to_string();
    let help_stderr = String::from_utf8_lossy(&help_output.stderr).to_string();
    // Some binaries print help to stderr
    let help_combined = if help_text.is_empty() {
        &help_stderr
    } else {
        &help_text
    };

    // Write bootstrap file
    let mut bootstrap = format!("# {}: bootstrap observations\n\n", binary);
    bootstrap.push_str(&format!(
        "# --help output ({} lines):\n",
        help_combined.lines().count()
    ));
    for line in help_combined.lines() {
        bootstrap.push_str(&format!("# {}\n", line));
    }
    bootstrap.push('\n');
    bootstrap.push_str("test args \"--help\"\n");
    bootstrap.push_str("  expect stdout not-empty\n");
    bootstrap.push_str("  expect exit 0\n");

    std::fs::write(dir.join("_bootstrap.test"), &bootstrap)?;

    // Extract flags from help text
    let flags = extract_flags(help_combined);
    eprintln!("Binary: {}", binary);
    eprintln!("Flags found: {}", flags.len());

    // Write setup.test with a basic context
    let mut setup = format!("# {}: shared contexts\n\n", binary);
    setup.push_str("context \"base\"\n");
    setup.push_str("  file \"file1.txt\" \"hello\"\n");
    setup.push_str("  file \"file2.txt\" \"world\"\n");
    setup.push_str("  dir \"subdir\"\n");
    std::fs::write(dir.join("setup.test"), &setup)?;

    // Write stub files for each flag
    for (flag, description) in &flags {
        let filename = format!("{}.test", flag);
        let stub_path = dir.join(&filename);
        if stub_path.exists() {
            eprintln!("  skip {} (exists)", filename);
            continue;
        }

        let mut stub = format!("# {} {}: {}\n\n", binary, flag, description);
        stub.push_str(&format!("test args \".\" \"{}\"\n", flag));

        std::fs::write(&stub_path, &stub)?;
        eprintln!("  stub {}", filename);
    }

    eprintln!("\nInitialized {} with {} stubs", dir.display(), flags.len());
    Ok(())
}

/// Extract flags and descriptions from --help output.
/// Looks for patterns like "  -a, --all   description" or "  -a   description".
fn extract_flags(help_text: &str) -> Vec<(String, String)> {
    let mut flags = Vec::new();

    for line in help_text.lines() {
        let trimmed = line.trim_start();

        // Match: -X, --long-name   description
        // or:    -X                description
        // or:        --long-name   description
        if !trimmed.starts_with('-') {
            continue;
        }

        // Extract the flag part (everything before the description)
        // Flags end where there are 2+ spaces followed by text
        let mut parts = trimmed.splitn(2, "  ");
        let flag_part = parts.next().unwrap_or("");
        let desc_part = parts
            .next()
            .unwrap_or("")
            .trim();

        if desc_part.is_empty() {
            continue;
        }

        // Extract the short flag (-X) if present
        let short = flag_part
            .split(',')
            .find(|s| {
                let s = s.trim();
                s.len() == 2 && s.starts_with('-') && s.chars().nth(1).is_some_and(|c| c != '-')
            })
            .map(|s| s.trim().to_string());

        if let Some(flag) = short {
            flags.push((flag, desc_part.to_string()));
        }
    }

    flags
}

/// Generate suggested expectations by comparing a baseline and option observation.
fn suggest_expectations(
    baseline: &execute::Observation,
    option: &execute::Observation,
    baseline_args: &[String],
) -> Vec<String> {
    let mut suggestions = Vec::new();
    let vs_str = baseline_args
        .iter()
        .map(|a| format!("\"{}\"", a))
        .collect::<Vec<_>>()
        .join(" ");

    let base_lines: Vec<&str> = baseline.stdout.lines().collect();
    let opt_lines: Vec<&str> = option.stdout.lines().collect();
    let base_set: std::collections::HashSet<&str> = base_lines.iter().copied().collect();
    let opt_set: std::collections::HashSet<&str> = opt_lines.iter().copied().collect();

    let rel = delta::classify_stdout(&baseline.stdout, &option.stdout);
    let is_preserved = matches!(
        rel,
        delta::EntryRelation::Preserved
            | delta::EntryRelation::PreservedPrefixAdded
            | delta::EntryRelation::PreservedFieldsExpanded
            | delta::EntryRelation::PreservedWrapped
    );

    // Structural suggestion
    match rel {
        delta::EntryRelation::Identical => {}
        delta::EntryRelation::Superset => {
            suggestions.push(format!("expect stdout superset vs {}", vs_str));
        }
        delta::EntryRelation::Subset => {
            suggestions.push(format!("expect stdout subset vs {}", vs_str));
        }
        delta::EntryRelation::Reordered => {
            suggestions.push(format!("expect stdout reordered vs {}", vs_str));
            // For reordered: suggest line 1 contains (pins the sort order)
            let non_empty: Vec<&str> = opt_lines
                .iter()
                .filter(|l| !l.is_empty())
                .copied()
                .collect();
            if !non_empty.is_empty() {
                suggestions.push(format!(
                    "expect stdout line 1 contains \"{}\"",
                    non_empty[0]
                ));
            }
        }
        _ if is_preserved => {
            suggestions.push(format!("expect stdout preserved vs {}", vs_str));
        }
        _ => {}
    }

    // Line count comparison
    let base_count = base_lines.iter().filter(|l| !l.is_empty()).count();
    let opt_count = opt_lines.iter().filter(|l| !l.is_empty()).count();
    if opt_count > base_count {
        suggestions.push(format!("expect stdout lines more than {}", vs_str));
    } else if opt_count < base_count {
        suggestions.push(format!("expect stdout lines fewer than {}", vs_str));
    } else if opt_count == base_count
        && !matches!(rel, delta::EntryRelation::Identical)
    {
        suggestions.push(format!("expect stdout lines same as {}", vs_str));
    }

    // Content suggestions: lines only in option (added entries)
    let only_in_option: Vec<&&str> = opt_lines
        .iter()
        .filter(|l| !l.is_empty() && !base_set.contains(**l))
        .collect();
    // For superset/subset: suggest contains for added entries
    // For preserved: skip — the entries are reformatted, not new
    if !is_preserved {
        for line in only_in_option.iter().take(5) {
            suggestions.push(format!("expect stdout contains \"{}\"", line));
        }
    }

    // Not-contains for removed entries — only for subset (entries actually removed)
    // Skip for preserved (entries are still there, just reformatted)
    if matches!(rel, delta::EntryRelation::Subset) {
        let only_in_baseline: Vec<&&str> = base_lines
            .iter()
            .filter(|l| !l.is_empty() && !opt_set.contains(**l))
            .collect();
        for line in only_in_baseline.iter().take(3) {
            // Only suggest if the text is truly absent from option output
            if !option.stdout.contains(**line) {
                suggestions.push(format!("expect stdout not-contains \"{}\"", line));
            }
        }
    }

    // Exit code
    let exit = option.exit_code.unwrap_or(-1);
    suggestions.push(format!("expect exit {}", exit));

    // Stderr
    if !option.stderr.is_empty() {
        suggestions.push("expect stderr not-empty".to_string());
    }

    suggestions
}

/// Check if a suggestion string holds for a given baseline/option pair.
/// Quick-and-dirty: parse the suggestion and evaluate it.
fn check_suggestion(
    suggestion: &str,
    baseline: &execute::Observation,
    option: &execute::Observation,
) -> bool {
    let s = suggestion.trim();

    if let Some(rest) = s.strip_prefix("expect stdout superset vs ") {
        let _ = rest;
        let rel = delta::classify_stdout(&baseline.stdout, &option.stdout);
        return rel == delta::EntryRelation::Superset;
    }
    if let Some(rest) = s.strip_prefix("expect stdout subset vs ") {
        let _ = rest;
        let rel = delta::classify_stdout(&baseline.stdout, &option.stdout);
        return rel == delta::EntryRelation::Subset;
    }
    if let Some(rest) = s.strip_prefix("expect stdout reordered vs ") {
        let _ = rest;
        let rel = delta::classify_stdout(&baseline.stdout, &option.stdout);
        return rel == delta::EntryRelation::Reordered;
    }
    if let Some(rest) = s.strip_prefix("expect stdout preserved vs ") {
        let _ = rest;
        let rel = delta::classify_stdout(&baseline.stdout, &option.stdout);
        return matches!(
            rel,
            delta::EntryRelation::Preserved
                | delta::EntryRelation::PreservedPrefixAdded
                | delta::EntryRelation::PreservedFieldsExpanded
                | delta::EntryRelation::PreservedWrapped
        );
    }
    if let Some(rest) = s.strip_prefix("expect stdout lines more than ") {
        let _ = rest;
        let opt_c = option.stdout.lines().filter(|l| !l.is_empty()).count();
        let base_c = baseline.stdout.lines().filter(|l| !l.is_empty()).count();
        return opt_c > base_c;
    }
    if let Some(rest) = s.strip_prefix("expect stdout lines fewer than ") {
        let _ = rest;
        let opt_c = option.stdout.lines().filter(|l| !l.is_empty()).count();
        let base_c = baseline.stdout.lines().filter(|l| !l.is_empty()).count();
        return opt_c < base_c;
    }
    if let Some(rest) = s.strip_prefix("expect stdout lines same as ") {
        let _ = rest;
        let opt_c = option.stdout.lines().filter(|l| !l.is_empty()).count();
        let base_c = baseline.stdout.lines().filter(|l| !l.is_empty()).count();
        return opt_c == base_c;
    }
    if let Some(rest) = s.strip_prefix("expect stdout contains \"") {
        if let Some(text) = rest.strip_suffix('"') {
            return option.stdout.contains(text);
        }
    }
    if let Some(rest) = s.strip_prefix("expect stdout not-contains \"") {
        if let Some(text) = rest.strip_suffix('"') {
            return !option.stdout.contains(text);
        }
    }
    if let Some(rest) = s.strip_prefix("expect stdout line 1 contains \"") {
        if let Some(text) = rest.strip_suffix('"') {
            let first = option.stdout.lines().find(|l| !l.is_empty());
            return first.is_some_and(|f| f.contains(text));
        }
    }
    if let Some(rest) = s.strip_prefix("expect exit ") {
        if let Ok(code) = rest.trim().parse::<i32>() {
            return option.exit_code == Some(code);
        }
    }
    if s == "expect stderr not-empty" {
        return !option.stderr.trim().is_empty();
    }

    // Unknown suggestion format — assume it holds
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_flags() {
        let help = r#"
Usage: ls [OPTION]... [FILE]...
  -a, --all                  do not ignore entries starting with .
  -A, --almost-all           do not list implied . and ..
      --author               with -l, print the author of each file
  -B, --ignore-backups       do not list implied entries ending with ~
  -r, --reverse              reverse order while sorting
"#;
        let flags = extract_flags(help);
        assert_eq!(flags.len(), 4); // -a, -A, -B, -r (not --author, no short form)
        assert_eq!(flags[0].0, "-a");
        assert!(flags[0].1.contains("do not ignore"));
        assert_eq!(flags[1].0, "-A");
        assert_eq!(flags[2].0, "-B");
        assert_eq!(flags[3].0, "-r");
    }

    #[test]
    fn test_strip_results_block() {
        let source = "file \"a\" \"b\"\n\ntest args \".\"\n  expect exit 0\n\n#> --- results ---\n#> test [\".\"] in (default):\n#>   exit: 0\n";
        let stripped = strip_results_block(source);
        assert!(!stripped.contains("#>"));
        assert!(stripped.contains("expect exit 0"));
    }
}
