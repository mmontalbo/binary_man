//! Bootstrap initialization for simplified verification.
//!
//! Bootstrapping discovers surface items from help output using only built-in
//! knowledge (--help or -h flags). This is intentionally simple - no SQL lenses
//! or multi-stage discovery pipelines.

use super::types::{State, Status, SurfaceCategory, SurfaceEntry, STATE_SCHEMA_VERSION};
use anyhow::{Context, Result};
use crate::lm::LmConfig;
use std::collections::HashMap;
use std::process::Command;

/// Maximum number of surrounding options to include as context.
const CONTEXT_WINDOW_SIZE: usize = 2;

/// Bootstrap a new verification state for a binary.
///
/// This runs both `--help` and `-h` to discover surface items, merging results
/// to capture both "common options" and full man page options. An LM call
/// classifies surfaces into categories for scheduling and execution strategy.
pub(super) fn bootstrap(binary: &str, context_argv: &[String], _lm_config: &LmConfig) -> Result<State> {
    // 1. Run help discovery - collect from BOTH -h and --help
    let help_outputs = collect_all_help_outputs(binary, context_argv)?;

    // 2. Parse and merge surfaces from all outputs
    let mut seen: HashMap<String, DiscoveredSurface> = HashMap::new();
    for output in &help_outputs {
        for surface in parse_surfaces_from_help(output) {
            // First discovery wins (usually has better description)
            seen.entry(surface.id.clone()).or_insert(surface);
        }
    }

    // 3. Extract help preamble (synopsis, description) for LM context
    let preamble = help_outputs
        .first()
        .map(|text| extract_help_preamble(text))
        .unwrap_or_default();

    // 4. Create initial entries with mechanical --no-X classification only
    let entries: Vec<SurfaceEntry> = seen
        .into_values()
        .map(|s| {
            let category = classify_surface_mechanical(&s.id);
            SurfaceEntry {
                id: s.id,
                description: s.description,
                context: s.context,
                value_hint: s.value_hint,
                category,
                status: Status::Pending,
                attempts: vec![],
                retried: false,
                critique_feedback: None,
            }
        })
        .collect();

    // Note: LM classification is deferred to the session loop. Each session
    // classifies its surfaces in parallel using its own LM instance.
    // Surfaces start as General (or --no-X Modifier from mechanical classification).

    Ok(State {
        schema_version: STATE_SCHEMA_VERSION,
        binary: binary.to_string(),
        context_argv: context_argv.to_vec(),
        baseline: None,
        entries,
        cycle: 0,
        seed_bank: vec![],
        help_preamble: preamble,
    })
}

/// Discovered surface from help output.
#[derive(Debug, Clone)]
struct DiscoveredSurface {
    /// Option name (e.g., "--stat", "-v").
    id: String,
    /// Full description from help text (multi-line descriptions joined).
    description: String,
    /// Surrounding context (nearby options) for additional hints.
    context: Option<String>,
    /// Value hint (e.g., "<n>", "<file>").
    value_hint: Option<String>,
}

/// Collect help output from both -h and --help flags.
///
/// Returns all outputs that look like help text, allowing us to merge
/// surfaces from both "common options" (-h) and full man pages (--help).
fn collect_all_help_outputs(binary: &str, context_argv: &[String]) -> Result<Vec<String>> {
    let mut outputs = Vec::new();

    for help_flag in ["--help", "-h"] {
        let mut argv = vec![binary.to_string()];
        argv.extend(context_argv.iter().cloned());
        argv.push(help_flag.to_string());

        let output = Command::new(&argv[0])
            .args(&argv[1..])
            .env("GIT_PAGER", "cat") // Prevent git from opening pager
            .env("PAGER", "cat")
            .env("MANPAGER", "cat")
            .env("TERM", "dumb") // Reduce ANSI codes
            .output()
            .with_context(|| format!("run {} {}", binary, help_flag))?;

        // Accept both success and common "help" exit codes
        // Common codes: 0 (success), 1 (error), 2 (usage), 129 (git usage)
        let is_success = output.status.success();
        let is_help_exit = matches!(output.status.code(), Some(1) | Some(2) | Some(129));

        if !(is_success || is_help_exit) {
            continue;
        }

        // Check stdout first, then stderr
        let text = if !output.stdout.is_empty() {
            String::from_utf8_lossy(&output.stdout).to_string()
        } else if !output.stderr.is_empty() {
            String::from_utf8_lossy(&output.stderr).to_string()
        } else {
            continue;
        };

        // Strip ANSI escape codes (man pages use these for formatting)
        let text = strip_ansi_codes(&text);

        // Only include if it looks like help output
        if help_likelihood_score(&text) > 0 {
            outputs.push(text);
        }
    }

    if outputs.is_empty() {
        anyhow::bail!("Could not discover help for {}", binary);
    }

    Ok(outputs)
}

/// Strip ANSI escape codes from text.
fn strip_ansi_codes(text: &str) -> String {
    // Match ANSI escape sequences: ESC [ ... m (color/style)
    // and other common sequences
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip ESC and the sequence that follows
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                              // Skip until we hit a letter (end of sequence)
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Score how much a text looks like help output (higher = more likely help).
fn help_likelihood_score(text: &str) -> usize {
    let mut score = 0;

    // Count lines that look like option definitions
    for line in text.lines() {
        let trimmed = line.trim_start();
        // Lines starting with - followed by letter/digit
        if trimmed.starts_with('-')
            && trimmed.len() > 1
            && trimmed
                .chars()
                .nth(1)
                .map(|c| c.is_alphanumeric())
                .unwrap_or(false)
        {
            score += 10;
        }
        // Lines containing common help keywords
        if trimmed.contains("--help")
            || trimmed.contains("--version")
            || trimmed.contains("Usage:")
            || trimmed.contains("Options:")
        {
            score += 5;
        }
    }

    score
}

/// An option block parsed from help text.
#[derive(Debug, Clone)]
struct OptionBlock {
    /// Option ID (e.g., "--stat", "-v").
    id: String,
    /// Full description with continuation lines joined.
    description: String,
    /// Value hint (e.g., "<n>", "<file>").
    value_hint: Option<String>,
}

/// Parse surface items from help output text.
///
/// This parser handles multi-line descriptions by detecting continuation lines
/// (lines that are indented more than the option line or start with whitespace only).
///
/// It also captures surrounding context from neighboring options.
fn parse_surfaces_from_help(help_text: &str) -> Vec<DiscoveredSurface> {
    // First pass: parse all option blocks with full multi-line descriptions
    let blocks = parse_option_blocks(help_text);

    // Second pass: convert to surfaces with surrounding context
    let mut surfaces = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (i, block) in blocks.iter().enumerate() {
        if seen.contains(&block.id) || is_help_option(&block.id) {
            continue;
        }
        seen.insert(block.id.clone());

        // Build surrounding context from neighboring options
        let context = build_context(&blocks, i);

        surfaces.push(DiscoveredSurface {
            id: block.id.clone(),
            description: block.description.clone(),
            context,
            value_hint: block.value_hint.clone(),
        });
    }

    surfaces
}

/// Parse help text into option blocks, joining multi-line descriptions.
///
/// This function handles several common option formats:
/// - Combined short and long: `-B, --break-rewrites[=<n>]`
/// - Long only: `--verbose`, `--stat=<width>`
/// - Short only: `-v`, `-S<string>`, `-n <num>`
///
/// When both short and long forms appear on the same line, TWO OptionBlocks
/// are emitted (one for each form) with the same description.
fn parse_option_blocks(help_text: &str) -> Vec<OptionBlock> {
    let mut blocks = Vec::new();

    // Pattern for lines with both short and long options
    // Examples: -B, --break-rewrites[=<n>]  OR  -v, --verbose
    // Captures short option, optional short value, long option, optional long value
    let combined_pattern = regex::Regex::new(
        r"^\s*(?P<short>-[a-zA-Z0-9])(?P<short_value>[<\[]\S*[>\]])?(?:\s*,\s*)(?P<long>--[a-zA-Z0-9][a-zA-Z0-9_-]*)(?P<long_value>[=\[]\S*[>\]])?(?:\s+(?P<desc>.*))?",
    )
    .expect("valid regex");

    // Pattern for long-only options (--verbose, --stat=<width>, --stat[=<width>], --diff-algorithm=(x|y))
    let long_only_pattern = regex::Regex::new(
        r"^\s*(?P<long>--[a-zA-Z0-9][a-zA-Z0-9_-]*)(?P<value>(?:[=\[]\S*[>\]]|=\([^)]+\)))?(?:\s+(?P<desc>.*))?",
    )
    .expect("valid regex");

    // Pattern for short-only options (-v, -S<string>, -n <num>)
    // Handles both attached values (-S<string>) and space-separated values (-n <num>)
    let short_only_pattern = regex::Regex::new(
        r"^\s*(?P<short>-[a-zA-Z0-9])(?P<value>[<\[]\S+[>\]])?(?:\s+(?P<value2>[<\[]\S+[>\]]))?(?:\s+(?P<desc>.*))?$",
    )
    .expect("valid regex");

    let lines: Vec<&str> = help_text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        // Skip lines that don't look like option definitions
        if !trimmed.starts_with('-') {
            i += 1;
            continue;
        }

        let line_indent = leading_whitespace_count(line);

        // Try combined pattern first (short and long on same line)
        if let Some(caps) = combined_pattern.captures(line) {
            let short_opt = caps.name("short").map(|m| m.as_str().to_string());
            let long_opt = caps.name("long").map(|m| m.as_str().to_string());
            let short_value = caps.name("short_value").map(|m| m.as_str().to_string());
            let long_value = caps
                .name("long_value")
                .map(|m| normalize_value_hint(m.as_str()));
            let desc_start = caps
                .name("desc")
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();

            // Collect continuation lines for description
            i += 1;
            let description = collect_continuation_lines(&lines, &mut i, line_indent, desc_start);

            // Determine the value hint - prefer long_value, fall back to short_value
            let value_hint = long_value.or(short_value);

            // Emit short form
            if let Some(short) = short_opt {
                blocks.push(OptionBlock {
                    id: short,
                    description: description.clone(),
                    value_hint: value_hint.clone(),
                });
            }

            // Emit long form
            if let Some(long) = long_opt {
                blocks.push(OptionBlock {
                    id: long,
                    description,
                    value_hint,
                });
            }
        } else if let Some(caps) = long_only_pattern.captures(line) {
            // Long-only option
            if let Some(long) = caps.name("long") {
                let value_hint = caps
                    .name("value")
                    .map(|m| normalize_value_hint(m.as_str()));
                let desc_start = caps
                    .name("desc")
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default();

                i += 1;
                let description =
                    collect_continuation_lines(&lines, &mut i, line_indent, desc_start);

                blocks.push(OptionBlock {
                    id: long.as_str().to_string(),
                    description,
                    value_hint,
                });
            } else {
                i += 1;
            }
        } else if let Some(caps) = short_only_pattern.captures(line) {
            // Short-only option
            if let Some(short) = caps.name("short") {
                // Value can be attached (-S<string>) or space-separated (-n <num>)
                let value_hint = caps
                    .name("value")
                    .or_else(|| caps.name("value2"))
                    .map(|m| m.as_str().to_string());
                let desc_start = caps
                    .name("desc")
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default();

                i += 1;
                let description =
                    collect_continuation_lines(&lines, &mut i, line_indent, desc_start);

                blocks.push(OptionBlock {
                    id: short.as_str().to_string(),
                    description,
                    value_hint,
                });
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    blocks
}

/// Collect continuation lines for a multi-line description.
fn collect_continuation_lines(
    lines: &[&str],
    i: &mut usize,
    line_indent: usize,
    mut description: String,
) -> String {
    while *i < lines.len() {
        let next_line = lines[*i];
        let next_trimmed = next_line.trim_start();

        // Stop if we hit another option definition
        // Check for -X (short) or --foo (long) patterns
        if next_trimmed.starts_with('-')
            && next_trimmed
                .chars()
                .nth(1)
                .map(|c| c.is_alphanumeric() || c == '-')
                .unwrap_or(false)
        {
            break;
        }

        // Stop if we hit an empty line
        if next_trimmed.is_empty() {
            *i += 1;
            break;
        }

        let next_indent = leading_whitespace_count(next_line);

        // Continuation lines are typically more indented or at description indent
        // For man-page style, descriptions start on the next line with more indent
        if next_indent > line_indent || (line_indent == 0 && next_indent >= 8) {
            // Join with space, trimming excessive whitespace
            if !description.is_empty() {
                description.push(' ');
            }
            description.push_str(next_trimmed);
            *i += 1;
        } else {
            break;
        }
    }

    description
}

/// Normalize a value hint by extracting the actual hint from various formats.
///
/// Handles:
/// - `=<value>` -> `<value>`
/// - `[=<value>]` -> `<value>`
/// - `<value>` -> `<value>` (unchanged)
/// - `=(a|b|c)` -> `(a|b|c)`
fn normalize_value_hint(hint: &str) -> String {
    let hint = hint.trim();

    // Remove leading = or [=
    let hint = hint.trim_start_matches('[').trim_start_matches('=');

    // Remove trailing ]
    let hint = hint.trim_end_matches(']');

    hint.to_string()
}

/// Count leading whitespace characters.
fn leading_whitespace_count(s: &str) -> usize {
    s.chars().take_while(|c| c.is_whitespace()).count()
}

/// Build context string from surrounding option blocks.
fn build_context(blocks: &[OptionBlock], current_idx: usize) -> Option<String> {
    if blocks.len() <= 1 {
        return None;
    }

    let mut context_parts = Vec::new();

    // Get previous options (up to CONTEXT_WINDOW_SIZE)
    let start = current_idx.saturating_sub(CONTEXT_WINDOW_SIZE);
    for block in blocks.iter().skip(start).take(current_idx - start) {
        let short_desc = truncate_context_desc(&block.description, 60);
        if short_desc.is_empty() {
            context_parts.push(block.id.clone());
        } else {
            context_parts.push(format!("{}: {}", block.id, short_desc));
        }
    }

    // Get next options (up to CONTEXT_WINDOW_SIZE)
    for block in blocks
        .iter()
        .skip(current_idx + 1)
        .take(CONTEXT_WINDOW_SIZE)
    {
        let short_desc = truncate_context_desc(&block.description, 60);
        if short_desc.is_empty() {
            context_parts.push(block.id.clone());
        } else {
            context_parts.push(format!("{}: {}", block.id, short_desc));
        }
    }

    if context_parts.is_empty() {
        None
    } else {
        Some(format!("Related options: {}", context_parts.join("; ")))
    }
}

/// Truncate a description for context display.
fn truncate_context_desc(desc: &str, max_len: usize) -> String {
    if desc.len() <= max_len {
        desc.to_string()
    } else {
        // Find a word boundary
        let truncated = &desc[..max_len];
        if let Some(last_space) = truncated.rfind(' ') {
            format!("{}...", &desc[..last_space])
        } else {
            format!("{}...", truncated)
        }
    }
}

/// Mechanical classification — only handles the reliable syntactic pattern.
///
/// `--no-X` → `Modifier { base: "--X" }`. Everything else defaults to `General`
/// and is enriched by the LM classification pass.
fn classify_surface_mechanical(id: &str) -> SurfaceCategory {
    if let Some(base_name) = id.strip_prefix("--no-") {
        return SurfaceCategory::Modifier {
            base: format!("--{}", base_name),
        };
    }
    SurfaceCategory::General
}

/// Extract the preamble (synopsis, description) from help text.
///
/// Returns everything before the first option definition line. This gives the
/// LM context about what the command does when classifying surfaces.
fn extract_help_preamble(help_text: &str) -> String {
    let mut preamble_lines = Vec::new();
    for line in help_text.lines() {
        let trimmed = line.trim_start();
        // Stop at the first option definition
        if trimmed.starts_with('-')
            && trimmed.len() > 1
            && trimmed
                .chars()
                .nth(1)
                .map(|c| c.is_alphanumeric() || c == '-')
                .unwrap_or(false)
        {
            break;
        }
        preamble_lines.push(line);
    }
    let preamble = preamble_lines.join("\n").trim().to_string();
    // Cap at ~1000 chars to avoid bloating the prompt
    if preamble.len() > 1000 {
        preamble[..1000].to_string()
    } else {
        preamble
    }
}

/// Check if an option is a help/version option that shouldn't be verified.
fn is_help_option(opt: &str) -> bool {
    matches!(opt, "--help" | "-h" | "--version" | "-V" | "--usage" | "-?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_options() {
        let help = r#"
Usage: test [OPTIONS]

Options:
  -v, --verbose    Enable verbose output
  --color=<when>   Colorize output
  -n <num>         Number of items
  --dry-run        Don't actually do anything
"#;

        let surfaces = parse_surfaces_from_help(help);

        assert!(surfaces.iter().any(|s| s.id == "--verbose"));
        assert!(surfaces.iter().any(|s| s.id == "--color"));
        assert!(surfaces.iter().any(|s| s.id == "--dry-run"));
        assert!(surfaces.iter().any(|s| s.id == "-n"));
    }

    #[test]
    fn test_parse_value_hints() {
        let help = r#"
  --output=<file>   Output file
  --count <n>       Count
"#;

        let surfaces = parse_surfaces_from_help(help);

        let output = surfaces.iter().find(|s| s.id == "--output").unwrap();
        assert_eq!(output.value_hint, Some("<file>".to_string()));
    }

    #[test]
    fn test_skip_help_options() {
        let help = r#"
  -h, --help        Show help
  -V, --version     Show version
  --verbose         Be verbose
"#;

        let surfaces = parse_surfaces_from_help(help);

        assert!(!surfaces.iter().any(|s| s.id == "--help"));
        assert!(!surfaces.iter().any(|s| s.id == "--version"));
        assert!(!surfaces.iter().any(|s| s.id == "-h"));
        assert!(surfaces.iter().any(|s| s.id == "--verbose"));
    }

    #[test]
    fn test_parse_git_diff_help_sample() {
        let help = r#"
usage: git diff [<options>] [<commit>] [--] [<path>...]

    --stat[=<width>[,<name-width>[,<count>]]]
                          output diffstat
    --numstat             output machine-readable format
    -z                    output diff-raw with NUL termination
    --name-only           show only names of changed files
    --name-status         show names and status of changed files
    --color[=<when>]      show colored diff
    --no-color            turn off colored diff
"#;

        let surfaces = parse_surfaces_from_help(help);

        assert!(surfaces.iter().any(|s| s.id == "--stat"));
        assert!(surfaces.iter().any(|s| s.id == "--numstat"));
        assert!(surfaces.iter().any(|s| s.id == "--name-only"));
        assert!(surfaces.iter().any(|s| s.id == "--color"));
    }

    // Note: test_bootstrap_echo removed — bootstrap now requires an LM plugin
    // which makes live-binary tests unsuitable for unit tests.
    // The mechanical parsing is tested thoroughly by the parse_* tests below.

    #[test]
    fn test_strip_ansi_codes() {
        // ANSI bold and color codes
        let input = "\x1b[1mBOLD\x1b[0m and \x1b[32mGREEN\x1b[0m";
        let output = strip_ansi_codes(input);
        assert_eq!(output, "BOLD and GREEN");

        // Underline codes (common in man pages)
        let input = "\x1b[4mUNDERLINE\x1b[24m";
        let output = strip_ansi_codes(input);
        assert_eq!(output, "UNDERLINE");

        // No codes
        let input = "plain text";
        let output = strip_ansi_codes(input);
        assert_eq!(output, "plain text");
    }

    #[test]
    fn test_parse_man_page_format() {
        // Simulate stripped man page output (after ANSI removal)
        let help = r#"
       --stat[=<width>[,<name-width>[,<count>]]]
           output diffstat instead of patch.
       --compact-summary
           Output a condensed summary of extended header information
       --numstat
           Similar to --stat, but shows number of added and deleted
"#;

        let surfaces = parse_surfaces_from_help(help);

        assert!(surfaces.iter().any(|s| s.id == "--stat"));
        assert!(surfaces.iter().any(|s| s.id == "--compact-summary"));
        assert!(surfaces.iter().any(|s| s.id == "--numstat"));
    }

    #[test]
    fn test_parse_multiline_descriptions() {
        // Test that multi-line descriptions are joined properly
        let help = r#"
  -L, --dereference          when showing file information for a symbolic
                               link, show information for the file the link
                               references rather than for the link itself
  -H                         follow symbolic links on command line
"#;

        let surfaces = parse_surfaces_from_help(help);

        let deref = surfaces.iter().find(|s| s.id == "--dereference").unwrap();
        // Description should be joined into a single string
        assert!(deref.description.contains("when showing file information"));
        assert!(deref.description.contains("references rather than"));
        // Should not have excessive newlines
        assert!(!deref.description.contains('\n'));
    }

    #[test]
    fn test_surrounding_context() {
        let help = r#"
  -a, --all                  do not ignore entries starting with .
  -A, --almost-all           do not list implied . and ..
  -B, --ignore-backups       do not list implied entries ending with ~
  -C                         list entries by columns
  -d, --directory            list directories themselves, not their contents
"#;

        let surfaces = parse_surfaces_from_help(help);

        // Find --ignore-backups which is in the middle
        let backups = surfaces
            .iter()
            .find(|s| s.id == "--ignore-backups")
            .unwrap();

        // Should have context from surrounding options
        assert!(backups.context.is_some());
        let ctx = backups.context.as_ref().unwrap();
        assert!(ctx.contains("Related options:"));
        // Should include neighbors
        assert!(ctx.contains("--almost-all") || ctx.contains("--all"));
        assert!(ctx.contains("-C") || ctx.contains("--directory"));
    }

    #[test]
    fn test_man_page_multiline() {
        // Man page style where description is on next line
        let help = r#"
       --stat[=<width>[,<name-width>[,<count>]]]
           Generate a diffstat. By default, as much space as necessary
           will be used for the filename part, and the rest for the graph
           part. Maximum width defaults to terminal width.
       --compact-summary
           Output a condensed summary of extended header information
"#;

        let surfaces = parse_surfaces_from_help(help);

        let stat = surfaces.iter().find(|s| s.id == "--stat").unwrap();
        // Should capture the full multi-line description
        assert!(stat.description.contains("Generate a diffstat"));
        assert!(stat.description.contains("Maximum width defaults"));
    }

    #[test]
    fn test_leading_whitespace_count() {
        assert_eq!(leading_whitespace_count("hello"), 0);
        assert_eq!(leading_whitespace_count("  hello"), 2);
        assert_eq!(leading_whitespace_count("\thello"), 1);
        assert_eq!(leading_whitespace_count("    "), 4);
    }

    #[test]
    fn test_truncate_context_desc() {
        assert_eq!(truncate_context_desc("short", 10), "short");
        assert_eq!(
            truncate_context_desc("this is a longer description", 15),
            "this is a..."
        );
        assert_eq!(truncate_context_desc("nospaces", 5), "nospa...");
    }

    #[test]
    fn test_parse_combined_short_long() {
        let help = "  -B, --break-rewrites   Detect complete rewrites";
        let surfaces = parse_surfaces_from_help(help);

        assert!(
            surfaces.iter().any(|s| s.id == "-B"),
            "Should have -B, got: {:?}",
            surfaces.iter().map(|s| &s.id).collect::<Vec<_>>()
        );
        assert!(
            surfaces.iter().any(|s| s.id == "--break-rewrites"),
            "Should have --break-rewrites"
        );

        // Both should have the same description
        let short = surfaces.iter().find(|s| s.id == "-B").unwrap();
        let long = surfaces.iter().find(|s| s.id == "--break-rewrites").unwrap();
        assert_eq!(short.description, long.description);
    }

    #[test]
    fn test_parse_short_with_attached_value() {
        let help = "  -S<string>   Find string in diff";
        let surfaces = parse_surfaces_from_help(help);

        let s = surfaces
            .iter()
            .find(|s| s.id == "-S")
            .expect("Should have -S");
        assert_eq!(
            s.value_hint,
            Some("<string>".to_string()),
            "Should have value hint <string>"
        );
    }

    #[test]
    fn test_parse_long_with_optional_value() {
        let help = "  --stat[=<width>]   Show diffstat";
        let surfaces = parse_surfaces_from_help(help);

        let s = surfaces
            .iter()
            .find(|s| s.id == "--stat")
            .expect("Should have --stat");
        assert!(s.value_hint.is_some(), "Should have value hint");
        // The normalized value hint should be <width>
        assert!(
            s.value_hint.as_ref().unwrap().contains("width"),
            "Value hint should contain 'width', got: {:?}",
            s.value_hint
        );
    }

    #[test]
    fn test_parse_combined_with_value_hints() {
        // Test combined form where long option has a value hint
        let help = "  -B, --break-rewrites[=<n>]   Detect complete rewrites";
        let surfaces = parse_surfaces_from_help(help);

        assert!(surfaces.iter().any(|s| s.id == "-B"));
        assert!(surfaces.iter().any(|s| s.id == "--break-rewrites"));

        // Both forms should have the value hint
        let short = surfaces.iter().find(|s| s.id == "-B").unwrap();
        let long = surfaces.iter().find(|s| s.id == "--break-rewrites").unwrap();

        assert!(short.value_hint.is_some(), "-B should have value hint");
        assert!(
            long.value_hint.is_some(),
            "--break-rewrites should have value hint"
        );
    }

    #[test]
    fn test_normalize_value_hint() {
        assert_eq!(normalize_value_hint("=<value>"), "<value>");
        assert_eq!(normalize_value_hint("[=<value>]"), "<value>");
        assert_eq!(normalize_value_hint("<value>"), "<value>");
        assert_eq!(normalize_value_hint("[=<width>]"), "<width>");
    }

    #[test]
    fn test_parse_git_diff_style_options() {
        // Real git diff -h style output with various option formats
        let help = r#"
usage: git diff [<options>] [<commit>] [--] [<path>...]

    -p                    generate patch
    -s                    suppress diff output
    -S<string>            find string in diff
    -G<regex>             find regex in diff
    --stat[=<width>[,<name-width>[,<count>]]]
                          output diffstat
    -B, --break-rewrites[=<n>/<m>]
                          detect complete rewrites
    -M, --find-renames[=<n>]
                          detect renames
    --color[=<when>]      show colored diff
"#;

        let surfaces = parse_surfaces_from_help(help);

        // Short options with attached values
        assert!(
            surfaces.iter().any(|s| s.id == "-S"),
            "Should have -S, got: {:?}",
            surfaces.iter().map(|s| &s.id).collect::<Vec<_>>()
        );
        assert!(surfaces.iter().any(|s| s.id == "-G"), "Should have -G");

        // Combined short+long options
        assert!(surfaces.iter().any(|s| s.id == "-B"), "Should have -B");
        assert!(
            surfaces.iter().any(|s| s.id == "--break-rewrites"),
            "Should have --break-rewrites"
        );
        assert!(surfaces.iter().any(|s| s.id == "-M"), "Should have -M");
        assert!(
            surfaces.iter().any(|s| s.id == "--find-renames"),
            "Should have --find-renames"
        );

        // Value hints on short options
        let s_opt = surfaces.iter().find(|s| s.id == "-S").unwrap();
        assert_eq!(s_opt.value_hint, Some("<string>".to_string()));

        let g_opt = surfaces.iter().find(|s| s.id == "-G").unwrap();
        assert_eq!(g_opt.value_hint, Some("<regex>".to_string()));
    }

    #[test]
    fn test_mechanical_classifier_no_prefix() {
        // --no-X should be classified as Modifier
        let cat = classify_surface_mechanical("--no-color");
        assert_eq!(
            cat,
            SurfaceCategory::Modifier {
                base: "--color".to_string()
            }
        );

        // Regular options should be General
        assert_eq!(
            classify_surface_mechanical("--verbose"),
            SurfaceCategory::General
        );
        assert_eq!(
            classify_surface_mechanical("-S"),
            SurfaceCategory::General
        );
        assert_eq!(
            classify_surface_mechanical("--pickaxe-all"),
            SurfaceCategory::General
        );
    }

    #[test]
    fn test_extract_help_preamble() {
        let help = r#"usage: git diff [<options>] [<commit>] [--] [<path>...]

Show changes between commits, commit and working tree, etc.

    --stat[=<width>]          output diffstat
    --numstat                 machine-readable format
"#;
        let preamble = extract_help_preamble(help);
        assert!(preamble.contains("usage: git diff"));
        assert!(preamble.contains("Show changes between"));
        assert!(!preamble.contains("--stat"));
        assert!(!preamble.contains("--numstat"));
    }

    #[test]
    fn test_extract_help_preamble_empty() {
        let help = "  --stat   output diffstat\n";
        let preamble = extract_help_preamble(help);
        assert!(preamble.is_empty());
    }

    #[test]
    fn test_parse_paren_value_hint() {
        let help = "  --diff-algorithm=(patience|minimal|histogram|myers)   Choose a diff algorithm";
        let surfaces = parse_surfaces_from_help(help);

        let alg = surfaces
            .iter()
            .find(|s| s.id == "--diff-algorithm")
            .expect("Should have --diff-algorithm");
        assert!(
            alg.value_hint.is_some(),
            "diff-algorithm should have a value_hint, got: {:?}",
            alg.value_hint
        );
        let hint = alg.value_hint.as_ref().unwrap();
        assert!(
            hint.contains("patience"),
            "value_hint should contain 'patience', got: {}",
            hint
        );
    }
}
