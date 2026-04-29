//! Bootstrap initialization for simplified verification.
//!
//! Bootstrapping discovers surface items from help output. Primary extraction
//! uses an LM to handle non-standard help formats (e.g., `find`'s predicate
//! style). Regex parsing is the offline/no-LM fallback. LM-extracted options
//! are probe-validated by running the binary to reject hallucinations.
//!
//! Results are cached keyed on help-text hash so repeat bootstraps work offline.

use super::config::{CONTEXT_WINDOW_SIZE, DESC_MAX_LEN, EXTRACT_CHUNK_TARGET_SIZE, MAX_CONCURRENT_PROBES};
use super::evidence::{prepare_sandbox, run_in_sandbox, sanitize_id, write_evidence};
use super::types::{
    Attempt, DiffKind, FileEntry, Outcome, Seed, State, Status, SurfaceCategory, SurfaceEntry,
    VerifiedSeed, STATE_SCHEMA_VERSION,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Command;

/// Build a State from discovered surfaces and help outputs.
pub(super) fn build_state_from_surfaces(
    binary: &str,
    context_argv: &[String],
    surfaces: Vec<DiscoveredSurface>,
    help_outputs: &[String],
) -> Result<State> {
    let preamble = help_outputs
        .first()
        .map(|text| extract_help_preamble(text))
        .unwrap_or_default();

    let examples = help_outputs
        .first()
        .map(|text| extract_examples_section(text))
        .unwrap_or_default();

    let entries: Vec<SurfaceEntry> = surfaces
        .into_iter()
        .map(|s| {
            let category = classify_surface_mechanical(&s.id);
            SurfaceEntry {
                id: s.id,
                description: s.description,
                context: s.context,
                value_hint: s.value_hint,
                category,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![],
                retried: false,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }
        })
        .collect();

    Ok(State {
        schema_version: STATE_SCHEMA_VERSION,
        binary: binary.to_string(),
        context_argv: context_argv.to_vec(),
        baseline: None,
        entries,
        cycle: 0,
        seed_bank: vec![],
        help_preamble: preamble,
        examples_section: examples,
        experiment_params: None,
        invocation_hint: None,
    })
}

/// Discovered surface from help output.
#[derive(Debug, Clone)]
pub(super) struct DiscoveredSurface {
    /// Option name (e.g., "--stat", "-v").
    pub id: String,
    /// Full description from help text (multi-line descriptions joined).
    pub description: String,
    /// Surrounding context (nearby options) for additional hints.
    pub context: Option<String>,
    /// Value hint (e.g., "<n>", "<file>").
    pub value_hint: Option<String>,
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
pub(super) fn parse_surfaces_from_help(help_text: &str) -> Vec<DiscoveredSurface> {
    // First pass: parse all option blocks with full multi-line descriptions
    let mut blocks = parse_option_blocks(help_text);

    // Also extract options from usage/synopsis lines (e.g. "Usage: find [-H] [-L] [-P]")
    blocks.extend(parse_usage_line_options(help_text));

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

/// Extract options from usage/synopsis lines.
///
/// Many commands list key options in their usage line in bracketed form:
///   `Usage: find [-H] [-L] [-P] [-Olevel] [-D debugopts] [path...]`
///
/// These are often missed by the indented-option-block parser because they
/// don't follow the standard option documentation format.
fn parse_usage_line_options(help_text: &str) -> Vec<OptionBlock> {
    let mut blocks = Vec::new();

    // Match bracketed single-letter options: [-X], [-Xvalue], [-X value]
    // The flag is a dash + single alphanumeric char. Anything after it
    // (lowercase attached text or space-separated word) is the value hint.
    let bracket_opt = regex::Regex::new(
        r"\[-([a-zA-Z0-9])(?:([a-z]\S*)|\s+([a-z]\S*))?\]",
    )
    .expect("valid regex");

    for line in help_text.lines() {
        let trimmed = line.trim_start().to_lowercase();
        // Only scan usage/synopsis lines
        if !(trimmed.starts_with("usage:") || trimmed.starts_with("synopsis:")) {
            continue;
        }

        for caps in bracket_opt.captures_iter(line) {
            let flag_char = &caps[1];
            let id = format!("-{}", flag_char);

            if is_help_option(&id) || is_combinator(&id) {
                continue;
            }

            // Value hint from attached text ([-Olevel]) or space-separated ([-D debugopts])
            let value_hint = caps
                .get(2)
                .or_else(|| caps.get(3))
                .map(|m| m.as_str().to_string());

            blocks.push(OptionBlock {
                id,
                description: String::new(),
                value_hint,
            });
        }
    }

    blocks
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
                let value_hint = caps.name("value").map(|m| normalize_value_hint(m.as_str()));
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
///
/// Continues past blank lines as long as the next non-blank line is still at
/// description indentation (not a new option or section header). This captures
/// multi-paragraph man page descriptions instead of truncating at the first
/// blank line.
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

        // On blank line: peek ahead to see if description continues
        if next_trimmed.is_empty() {
            if let Some(resume_idx) = peek_past_blank(lines, *i, line_indent) {
                // Skip blank lines, continue collecting from resume point
                if !description.is_empty() {
                    description.push(' ');
                }
                *i = resume_idx;
                continue;
            } else {
                *i += 1;
                break;
            }
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

    // Cap description length — man page descriptions can be very long
    if description.len() > DESC_MAX_LEN {
        if let Some(boundary) = description[..DESC_MAX_LEN].rfind(". ") {
            description.truncate(boundary + 1);
        } else {
            description.truncate(DESC_MAX_LEN);
        }
    }

    description
}

/// Peek past blank lines to see if the description continues.
///
/// Returns `Some(index)` of the next non-blank continuation line if it's
/// still at description indent. Returns `None` if the blank line ends the
/// description (next non-blank is a new option, section header, or at
/// lower indentation).
fn peek_past_blank(lines: &[&str], blank_idx: usize, option_indent: usize) -> Option<usize> {
    let mut j = blank_idx + 1;
    // Allow up to 1 consecutive blank line
    while j < lines.len() && lines[j].trim().is_empty() {
        if j - blank_idx > 1 {
            // Two+ consecutive blanks = section break
            return None;
        }
        j += 1;
    }
    if j >= lines.len() {
        return None;
    }
    let next = lines[j];
    let next_trimmed = next.trim_start();
    // Stop if it's an option definition
    if next_trimmed.starts_with('-')
        && next_trimmed
            .chars()
            .nth(1)
            .is_some_and(|c| c.is_alphanumeric() || c == '-')
    {
        return None;
    }
    // Stop if it's a section header (all-caps line or line at base indent)
    let next_indent = leading_whitespace_count(next);
    if next_indent <= option_indent && !next_trimmed.is_empty() {
        return None;
    }
    // Continuation: still indented past the option line
    if next_indent > option_indent || (option_indent == 0 && next_indent >= 8) {
        Some(j)
    } else {
        None
    }
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
pub(super) fn classify_surface_mechanical(id: &str) -> SurfaceCategory {
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
pub(super) fn extract_help_preamble(help_text: &str) -> String {
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

/// Extract the EXAMPLES section from help/man page text.
///
/// Looks for a line matching "EXAMPLES" (with optional leading whitespace)
/// and captures until the next section header (all-caps word at the same or
/// lower indent level).
pub(super) fn extract_examples_section(help_text: &str) -> String {
    let lines: Vec<&str> = help_text.lines().collect();
    let mut start = None;
    let mut header_indent = 0;

    // Find the EXAMPLES header
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == "EXAMPLES" || trimmed == "EXAMPLES:" {
            start = Some(i + 1);
            header_indent = leading_whitespace_count(line);
            break;
        }
    }

    let start = match start {
        Some(s) => s,
        None => return String::new(),
    };

    // Collect until the next section header at the same indent level
    let mut example_lines = Vec::new();
    for line in &lines[start..] {
        let trimmed = line.trim();
        let indent = leading_whitespace_count(line);
        // A new section header: all-caps word at header indent level
        if indent <= header_indent
            && !trimmed.is_empty()
            && trimmed
                .chars()
                .all(|c| c.is_uppercase() || c.is_whitespace())
        {
            break;
        }
        example_lines.push(*line);
    }

    // Trim trailing blank lines
    while example_lines.last().is_some_and(|l| l.trim().is_empty()) {
        example_lines.pop();
    }

    let result = example_lines.join("\n").trim().to_string();

    // Cap length
    const EXAMPLES_MAX_LEN: usize = 2000;
    if result.len() > EXAMPLES_MAX_LEN {
        if let Some(boundary) = result[..EXAMPLES_MAX_LEN].rfind('\n') {
            result[..boundary].to_string()
        } else {
            result[..EXAMPLES_MAX_LEN].to_string()
        }
    } else {
        result
    }
}

/// Check if an option is a help/version option that shouldn't be verified.
fn is_help_option(opt: &str) -> bool {
    matches!(opt, "--help" | "-h" | "--version" | "-V" | "--usage" | "-?")
}

/// Check if an option is a logical combinator that shouldn't be verified.
fn is_combinator(opt: &str) -> bool {
    matches!(opt, "-and" | "-or" | "-not" | "-a" | "-o")
}

// ==================== Pipeline helpers ====================

/// Result of preparing for surface extraction.
///
/// Contains everything the pipeline needs to drive extraction. If cache hits,
/// `cached_surfaces` is populated and `chunks` is empty — skip LM extraction.
pub(super) struct ExtractionPrep {
    /// Collected help outputs from the binary.
    pub help_outputs: Vec<String>,
    /// Hash of help text for cache keying.
    pub help_hash: String,
    /// Raw help text chunks for LM extraction. Empty if cache hit.
    pub chunks: Vec<String>,
    /// Cached surfaces if cache hit.
    pub cached_surfaces: Option<Vec<DiscoveredSurface>>,
}

/// Prepare for surface extraction: collect help, check cache, build chunks.
///
/// Returns everything the pipeline needs to drive extraction independently.
/// If the cache hits, `cached_surfaces` is populated and `chunks` is empty.
pub(super) fn prepare_extraction(
    binary: &str,
    context_argv: &[String],
    pack_path: Option<&Path>,
    verbose: bool,
) -> Result<ExtractionPrep> {
    let help_outputs = collect_all_help_outputs(binary, context_argv)?;
    let combined_help = help_outputs.join("\n---\n");
    let help_hash = compute_help_hash(&combined_help);

    if let Some(pp) = pack_path {
        if let Some(cached) = load_surface_cache(pp, &help_hash) {
            if verbose {
                eprintln!(
                    "Using cached surface extraction ({} surfaces, hash {})",
                    cached.len(),
                    &help_hash[..12]
                );
            }
            return Ok(ExtractionPrep {
                help_outputs,
                help_hash,
                chunks: vec![],
                cached_surfaces: Some(cached),
            });
        }
    }

    let combined = help_outputs.join("\n\n---\n\n");
    let chunks = split_help_into_chunks(&combined, EXTRACT_CHUNK_TARGET_SIZE);

    Ok(ExtractionPrep {
        help_outputs,
        help_hash,
        chunks,
        cached_surfaces: None,
    })
}

/// Add discovered surfaces to state, deduplicating against existing entries.
///
/// Surfaces whose ID already exists in state are skipped. New surfaces are
/// classified mechanically and added as Pending.
pub(super) fn add_surfaces_to_state(state: &mut State, surfaces: Vec<DiscoveredSurface>) {
    let existing_ids: std::collections::HashSet<String> =
        state.entries.iter().map(|e| e.id.clone()).collect();
    for surface in surfaces {
        if existing_ids.contains(&surface.id) {
            continue;
        }
        let category = classify_surface_mechanical(&surface.id);
        state.entries.push(SurfaceEntry {
            id: surface.id,
            description: surface.description,
            context: surface.context,
            value_hint: surface.value_hint,
            category,
            status: Status::Pending,
            probes: vec![],
            attempts: vec![],
            retried: false,
            critique_feedback: None,
            critique_demotions: 0,
            characterization: None,
        });
    }
}

// ==================== LM-based surface extraction ====================



/// Cached surface extraction result.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SurfaceCache {
    /// Hash of the help text used to produce this cache.
    help_hash: String,
    /// Extracted surfaces.
    surfaces: Vec<CachedSurface>,
}

/// A cached discovered surface (serializable subset of DiscoveredSurface).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSurface {
    id: String,
    description: String,
    value_hint: Option<String>,
}

/// Compute a stable hash of help text for cache keying.
fn compute_help_hash(text: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Load cached surface extraction if the hash matches.
fn load_surface_cache(pack_path: &Path, help_hash: &str) -> Option<Vec<DiscoveredSurface>> {
    let cache_path = pack_path.join("surface_cache.json");
    let content = std::fs::read_to_string(&cache_path).ok()?;
    let cache: SurfaceCache = serde_json::from_str(&content).ok()?;

    if cache.help_hash != help_hash {
        return None;
    }

    Some(
        cache
            .surfaces
            .into_iter()
            .filter(|s| !is_help_option(&s.id) && !is_combinator(&s.id))
            .map(|s| DiscoveredSurface {
                id: s.id,
                description: s.description,
                context: None,
                value_hint: s.value_hint,
            })
            .collect(),
    )
}

/// Save surface extraction results to cache.
pub(super) fn save_surface_cache(pack_path: &Path, help_hash: &str, surfaces: &[DiscoveredSurface]) {
    let cache = SurfaceCache {
        help_hash: help_hash.to_string(),
        surfaces: surfaces
            .iter()
            .map(|s| CachedSurface {
                id: s.id.clone(),
                description: s.description.clone(),
                value_hint: s.value_hint.clone(),
            })
            .collect(),
    };

    let cache_path = pack_path.join("surface_cache.json");
    // Best-effort cache write — failure is not fatal
    if let Ok(content) = serde_json::to_string_pretty(&cache) {
        let _ = std::fs::write(&cache_path, content);
    }
}

/// Build the extraction prompt for the LM.
pub(super) fn build_extraction_prompt(binary: &str, context_argv: &[String], help_text: &str) -> String {
    let cmd_name = if context_argv.is_empty() {
        binary.to_string()
    } else {
        format!("{} {}", binary, context_argv.join(" "))
    };

    format!(
        r#"Extract all command-line options from the following help text for `{cmd}`.

Rules:
1. The "id" field MUST appear verbatim in the help text (copy it exactly).
2. Handle ALL format styles:
   - GNU long: --verbose
   - GNU short: -v
   - Combined: -v, --verbose (emit BOTH as separate entries)
   - Predicate-style: -name PATTERN, -maxdepth LEVELS
   - Packed multiple per line: extract each one separately
3. Skip --help, --version, -h, -V, -?, and --usage.
4. Skip logical operators/combinators: -and, -or, -not, -a, -o.
5. For value hints, capture the placeholder (e.g., PATTERN, <n>, FILE).
6. Description should be one concise sentence from the help text.

Respond with ONLY a JSON array, no prose:

```json
[
  {{"id": "-name", "value_hint": "PATTERN", "description": "Match files by name pattern"}},
  {{"id": "--verbose", "value_hint": null, "description": "Enable verbose output"}}
]
```

Help text for `{cmd}`:

```
{help}
```"#,
        cmd = cmd_name,
        help = help_text,
    )
}

/// LM-extracted surface entry from JSON response.
#[derive(Debug, Clone, Deserialize)]
struct LmExtractedSurface {
    id: String,
    #[serde(default)]
    value_hint: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// Split help text into chunks at section boundaries.
///
/// Sections are delimited by blank lines followed by a line at low indentation
/// that looks like a header (all-caps, ends with colon, or is a `---` separator).
/// Chunks are greedily merged until they exceed `target_size`.
fn split_help_into_chunks(help_text: &str, target_size: usize) -> Vec<String> {
    let lines: Vec<&str> = help_text.lines().collect();
    if lines.is_empty() {
        return vec![help_text.to_string()];
    }

    // Find section break points (line indices where a new section starts)
    let mut breaks = vec![0usize];
    let mut prev_blank = false;

    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            prev_blank = true;
            continue;
        }
        if prev_blank && i > 0 && is_section_header(line) {
            breaks.push(i);
        }
        prev_blank = false;
    }

    // Build sections from break points
    let mut sections: Vec<String> = Vec::new();
    for w in breaks.windows(2) {
        let section: String = lines[w[0]..w[1]].to_vec().join("\n");
        sections.push(section);
    }
    // Last section
    let last_start = *breaks.last().unwrap();
    let section: String = lines[last_start..].to_vec().join("\n");
    sections.push(section);

    // Greedily merge sections into chunks up to target_size
    let mut chunks: Vec<String> = Vec::new();
    let mut current_chunk = String::new();

    for section in sections {
        if !current_chunk.is_empty()
            && current_chunk.len() + section.len() > target_size
        {
            chunks.push(std::mem::take(&mut current_chunk));
        }
        if !current_chunk.is_empty() {
            current_chunk.push('\n');
        }
        current_chunk.push_str(&section);
    }
    if !current_chunk.is_empty() {
        chunks.push(current_chunk);
    }

    // If we ended up with just one chunk, return it as-is
    if chunks.is_empty() {
        chunks.push(help_text.to_string());
    }

    chunks
}

/// Check if a line looks like a section header.
fn is_section_header(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    // Leading whitespace check: headers are at low indentation
    let indent = line.len() - line.trim_start().len();
    if indent > 4 {
        return false;
    }

    // "---" separator
    if trimmed.starts_with("---") {
        return true;
    }

    // Ends with colon (e.g., "Positional options (always true):")
    if trimmed.ends_with(':') {
        return true;
    }

    // All-caps word(s) at start (e.g., "OPTIONS", "DESCRIPTION")
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    if first_word.len() >= 3
        && first_word.chars().all(|c| c.is_ascii_uppercase() || c == '-')
    {
        return true;
    }

    false
}

/// Parse the LM extraction response into discovered surfaces.
pub(super) fn parse_extraction_response(response: &str) -> Result<Vec<DiscoveredSurface>> {
    // Extract JSON array from response (may be wrapped in code fences or prose)
    let json_text = extract_json_array(response)
        .ok_or_else(|| anyhow::anyhow!("no JSON array found in LM response"))?;

    let extracted: Vec<LmExtractedSurface> = serde_json::from_str(&json_text).with_context(
        || format!("parse extraction JSON: {}", &json_text[..json_text.len().min(200)]),
    )?;

    let mut surfaces = Vec::new();
    for entry in extracted {
        let id = entry.id.trim().to_string();

        // Skip empty ids, help options, and combinators
        if id.is_empty() || is_help_option(&id) || is_combinator(&id) {
            continue;
        }

        // The id must start with a dash
        if !id.starts_with('-') {
            continue;
        }

        surfaces.push(DiscoveredSurface {
            id,
            description: entry.description.unwrap_or_default(),
            context: None,
            value_hint: entry.value_hint,
        });
    }

    Ok(surfaces)
}

/// Extract a JSON array from text that may include code fences or prose.
fn extract_json_array(text: &str) -> Option<String> {
    let text = text.trim();

    // Try extracting from code fences first
    let inner = if let Some(fence_start) = text.find("```json") {
        let content_start = fence_start + 7;
        let content_start = text[content_start..]
            .find('\n')
            .map(|i| content_start + i + 1)
            .unwrap_or(content_start);
        if let Some(end) = text[content_start..].find("```") {
            &text[content_start..content_start + end]
        } else {
            &text[content_start..]
        }
    } else if let Some(fence_start) = text.find("```") {
        let after_fence = fence_start + 3;
        let content_start = text[after_fence..]
            .find('\n')
            .map(|i| after_fence + i + 1)
            .unwrap_or(after_fence);
        if let Some(end) = text[content_start..].find("```") {
            &text[content_start..content_start + end]
        } else {
            &text[content_start..]
        }
    } else {
        text
    };

    let inner = inner.trim();

    // Find the opening '[' and its matching ']'
    let start = inner.find('[')?;
    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in inner[start..].char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '[' if !in_string => depth += 1,
            ']' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(inner[start..start + i + 1].to_string());
                }
            }
            _ => {}
        }
    }

    None
}

// ==================== Probe validation ====================

/// Rejection patterns in stderr that indicate the binary doesn't recognize an option.
const REJECTION_PATTERNS: &[&str] = &[
    "unknown option",
    "unrecognized option",
    "invalid option",
    "not recognized",
    "unknown predicate",
    "invalid predicate",
    "bad option",
    "illegal option",
];

/// Probe-validate LM-extracted surfaces by running the binary.
///
/// For each extracted option, runs `binary [context_argv...] <option>` in an
/// empty tmpdir and checks stderr for rejection patterns. Options that the
/// binary rejects are filtered out as hallucinations.
pub(super) fn probe_validate_surfaces(
    binary: &str,
    context_argv: &[String],
    candidates: Vec<DiscoveredSurface>,
    verbose: bool,
) -> Vec<DiscoveredSurface> {
    let mut validated = Vec::new();

    // Process in batches to limit concurrent process spawns
    for batch in candidates.chunks(MAX_CONCURRENT_PROBES) {
        let batch_results: Vec<(DiscoveredSurface, ProbeOutcome)> =
            std::thread::scope(|s| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|surface| {
                        let surface = surface.clone();
                        s.spawn(move || {
                            let outcome = probe_option(binary, context_argv, &surface.id);
                            (surface, outcome)
                        })
                    })
                    .collect();

                handles
                    .into_iter()
                    .filter_map(|h| h.join().ok())
                    .collect()
            });

        for (surface, outcome) in batch_results {
            match outcome {
                ProbeOutcome::Accepted | ProbeOutcome::Error => {
                    validated.push(surface);
                }
                ProbeOutcome::Rejected => {
                    if verbose {
                        eprintln!(
                            "  Probe rejected: {} (binary doesn't recognize it)",
                            surface.id
                        );
                    }
                }
            }
        }
    }

    validated
}

/// Result of probing a single option.
enum ProbeOutcome {
    /// Binary recognized the option (no rejection pattern in stderr).
    Accepted,
    /// Binary rejected the option (stderr contains rejection pattern).
    Rejected,
    /// Probe itself failed (couldn't run binary). Keep the surface.
    Error,
}

/// Probe a single option by running it against the binary.
fn probe_option(binary: &str, context_argv: &[String], option: &str) -> ProbeOutcome {
    let tmpdir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(_) => return ProbeOutcome::Error,
    };

    let mut cmd = Command::new(binary);
    cmd.args(context_argv);
    cmd.arg(option);
    cmd.current_dir(tmpdir.path());
    // Prevent interactive behavior
    cmd.env("GIT_PAGER", "cat");
    cmd.env("PAGER", "cat");
    cmd.env("TERM", "dumb");
    // Capture output
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());

    let output = match cmd.output() {
        Ok(o) => o,
        Err(_) => return ProbeOutcome::Error,
    };

    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();

    // Check for rejection patterns
    for pattern in REJECTION_PATTERNS {
        if stderr.contains(pattern) {
            return ProbeOutcome::Rejected;
        }
    }

    // No rejection found — the binary recognizes this option.
    // This includes "missing argument" / "requires a value" errors,
    // which confirm the option is real.
    ProbeOutcome::Accepted
}

/// Build a rich filesystem fixture seed for batch probing.
///
/// Creates ~15 files with varied names, types, timestamps, permissions,
/// sizes, and content. Works for find, ls, grep, stat, chmod, etc.
fn build_rich_fixture() -> Seed {
    Seed {
        files: vec![
            FileEntry {
                path: "hello.txt".to_string(),
                content: "Hello, world!\nThis is a test file.\n".to_string(),
            },
            FileEntry {
                path: "data.csv".to_string(),
                content: "name,age,city\nAlice,30,NYC\nBob,25,LA\nCharlie,35,Chicago\n"
                    .to_string(),
            },
            FileEntry {
                path: "app.log".to_string(),
                content: "2024-01-01 INFO Starting up\n2024-01-01 ERROR Failed to connect\n2024-01-01 WARN Retrying\n".to_string(),
            },
            FileEntry {
                path: ".hidden".to_string(),
                content: "secret config\n".to_string(),
            },
            FileEntry {
                path: "noext".to_string(),
                content: "file without extension\n".to_string(),
            },
            FileEntry {
                path: "empty.txt".to_string(),
                content: String::new(),
            },
            FileEntry {
                path: "multi_line.txt".to_string(),
                content: "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n"
                    .to_string(),
            },
            FileEntry {
                path: "binary.bin".to_string(),
                content: "\x00\x01\x02\x7fELF".to_string(),
            },
            FileEntry {
                path: "subdir/nested.txt".to_string(),
                content: "nested file content\n".to_string(),
            },
            FileEntry {
                path: "subdir/deep/level3.txt".to_string(),
                content: "deeply nested\n".to_string(),
            },
            FileEntry {
                path: "script.sh".to_string(),
                content: "#!/bin/sh\necho hello\n".to_string(),
            },
            FileEntry {
                path: "pattern.txt".to_string(),
                content: "foo bar baz\nfoo qux\nbar baz foo\nno match here\n".to_string(),
            },
            FileEntry {
                path: "spaces in name.txt".to_string(),
                content: "file with spaces\n".to_string(),
            },
            FileEntry {
                path: "UPPER.TXT".to_string(),
                content: "uppercase extension\n".to_string(),
            },
        ],
        setup: vec![
            // Create a symlink
            vec![
                "ln".to_string(),
                "-s".to_string(),
                "hello.txt".to_string(),
                "link.txt".to_string(),
            ],
            // Make script executable
            vec![
                "chmod".to_string(),
                "+x".to_string(),
                "script.sh".to_string(),
            ],
            // Set old timestamp on a file
            vec![
                "touch".to_string(),
                "-t".to_string(),
                "200001010000".to_string(),
                "noext".to_string(),
            ],
            // Remove all permissions on one file
            vec![
                "chmod".to_string(),
                "000".to_string(),
                "empty.txt".to_string(),
            ],
            // Create an empty directory
            vec!["mkdir".to_string(), "emptydir".to_string()],
        ],
    }
}

/// Infer plausible arguments for a surface entry based on its value_hint.
///
/// Returns `Some(args)` if we can guess reasonable args, `None` to skip.
fn infer_batch_args(entry: &SurfaceEntry) -> Option<Vec<String>> {
    let hint = match &entry.value_hint {
        None => return Some(vec![]),
        Some(h) => h.to_lowercase(),
    };

    if hint.is_empty() {
        return Some(vec![]);
    }

    let hint = hint.trim();

    // Numeric hints
    if hint == "n"
        || hint == "num"
        || hint == "number"
        || hint == "count"
        || hint == "depth"
        || hint == "level"
        || hint == "max-depth"
    {
        return Some(vec!["1".to_string()]);
    }

    // Pattern/glob hints
    if hint == "pattern" || hint == "glob" || hint == "regex" || hint == "expr" {
        return Some(vec!["*.txt".to_string()]);
    }

    // Type hints (find -type)
    if hint == "type" || hint == "filetype" {
        return Some(vec!["f".to_string()]);
    }

    // File/path hints
    if hint == "file"
        || hint == "path"
        || hint == "dir"
        || hint == "directory"
        || hint == "name"
        || hint == "filename"
    {
        return Some(vec![".".to_string()]);
    }

    // Format/mode/style — try without a value (flag-like)
    if hint == "format"
        || hint == "fmt"
        || hint == "mode"
        || hint == "style"
        || hint == "when"
        || hint == "color"
    {
        return Some(vec![]);
    }

    // Unknown hint — skip
    None
}

/// Run batch probe against all pending surfaces using a rich fixture.
///
/// For each surface, runs the binary with and without the option in the
/// same sandbox. Surfaces that show differing output get a starter seed
/// added to the seed bank.
/// A batch probe hit: a surface that showed differing output from control.
pub(super) struct BatchProbeHit {
    pub surface_id: String,
    pub args: Vec<String>,
    pub diff_kind: DiffKind,
    pub stdout_preview: Option<String>,
    pub control_stdout_preview: Option<String>,
    pub evidence_path: String,
}

/// Run batch probe against all pending surfaces using a rich fixture.
///
/// For each surface, runs the binary with and without the option in the
/// same sandbox. Returns hits for surfaces that show differing output.
pub(super) fn batch_probe_surfaces(
    state: &State,
    pack_path: &Path,
    verbose: bool,
) -> Vec<BatchProbeHit> {
    let fixture_seed = build_rich_fixture();

    // Collect pending surfaces with inferred args
    let candidates: Vec<(String, Vec<String>)> = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Pending))
        .filter_map(|e| {
            let args = infer_batch_args(e)?;
            Some((e.id.clone(), args))
        })
        .collect();

    if candidates.is_empty() {
        return Vec::new();
    }

    if verbose {
        eprintln!(
            "Batch probe: testing {} surfaces against rich fixture",
            candidates.len()
        );
    }

    // Prepare sandbox once
    let sandbox = match prepare_sandbox("batch_probe", &fixture_seed) {
        Ok(s) => s,
        Err(e) => {
            if verbose {
                eprintln!("Batch probe: sandbox setup failed: {}", e);
            }
            return Vec::new();
        }
    };

    if sandbox.setup_failed {
        if verbose {
            eprintln!(
                "Batch probe: fixture setup failed: {}",
                sandbox.setup_error.as_deref().unwrap_or("unknown")
            );
        }
        return Vec::new();
    }

    // Build base argv: context_argv + invocation args (required positional args)
    let mut base_argv = state.context_argv.clone();
    if let Some(hint) = &state.invocation_hint {
        base_argv.extend(hint.required_args.iter().cloned());
    }

    // Run a single shared control
    let control = match run_in_sandbox(&sandbox, &state.binary, &base_argv, false, true) {
        Ok(e) => e,
        Err(e) => {
            if verbose {
                eprintln!("Batch probe: control run failed: {}", e);
            }
            return Vec::new();
        }
    };

    let mut hits = Vec::new();

    // Probe each surface
    for (surface_id, extra_args) in &candidates {
        // Option argv: base_argv + surface_id + extra_args
        let mut argv = base_argv.clone();
        argv.push(surface_id.clone());
        argv.extend(extra_args.iter().cloned());

        let evidence = match run_in_sandbox(&sandbox, &state.binary, &argv, false, true) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Skip if setup/execution failed
        if evidence.setup_failed || evidence.execution_error.is_some() {
            continue;
        }

        let stdout_differs = evidence.stdout != control.stdout;
        let stderr_differs = evidence.stderr != control.stderr;
        let exit_code_differs = evidence.exit_code != control.exit_code;

        if !stdout_differs && !stderr_differs && !exit_code_differs {
            continue;
        }

        // Reject stderr-only diffs when both runs failed — these are just error message
        // variations, not behavioral differences. Matches compute_outcome's filter.
        let both_failed = evidence.exit_code.unwrap_or(0) != 0
            && control.exit_code.unwrap_or(0) != 0;
        if stderr_differs && !stdout_differs && !exit_code_differs && both_failed {
            continue;
        }

        // Filter out error-only diffs: if the option crashed (non-zero exit, no stdout)
        // while control succeeded, this isn't a useful seed
        let option_errored = evidence.exit_code.is_some_and(|c| c != 0)
            && evidence.stdout.is_empty()
            && control.exit_code.is_some_and(|c| c == 0);
        if option_errored {
            continue;
        }

        // Reject empty-stdout hits: option producing empty output while control
        // has output is degenerate — it means "matched nothing", not meaningful
        // behavior verification. These are the most common false positive in
        // batch probe (58% of find's hits were this pattern).
        if evidence.stdout.is_empty() && !control.stdout.is_empty() && stdout_differs {
            continue;
        }

        let diff_kind = match (stdout_differs, stderr_differs, exit_code_differs) {
            (true, false, false) => DiffKind::Stdout,
            (false, true, false) => DiffKind::Stderr,
            (false, false, true) => DiffKind::ExitCode,
            _ => DiffKind::Multiple,
        };

        // Write evidence files for critique
        let sanitized = sanitize_id(surface_id);
        let ev_path = format!("evidence/batch_probe_{}.json", sanitized);
        let ctrl_path = format!("evidence/batch_probe_{}_control.json", sanitized);
        if let Err(e) = write_evidence(pack_path, &ev_path, &evidence) {
            if verbose {
                eprintln!("Batch probe: failed to write evidence for {}: {}", surface_id, e);
            }
        }
        if let Err(e) = write_evidence(pack_path, &ctrl_path, &control) {
            if verbose {
                eprintln!("Batch probe: failed to write control evidence for {}: {}", surface_id, e);
            }
        }

        let stdout_preview = if evidence.stdout.is_empty() {
            None
        } else {
            Some(evidence.stdout.chars().take(200).collect())
        };
        let control_stdout_preview = if control.stdout.is_empty() {
            None
        } else {
            Some(control.stdout.chars().take(200).collect())
        };

        hits.push(BatchProbeHit {
            surface_id: surface_id.clone(),
            args: argv,
            diff_kind,
            stdout_preview,
            control_stdout_preview,
            evidence_path: ev_path,
        });
    }

    eprintln!(
        "Batch probe: {} hits from {} candidates",
        hits.len(),
        candidates.len()
    );

    hits
}

/// Apply batch probe hits to state: mark surfaces Verified and populate seed bank.
pub(super) fn apply_batch_probe_hits(state: &mut State, hits: Vec<BatchProbeHit>, _verbose: bool) -> Vec<String> {
    let fixture_seed = build_rich_fixture();
    let verified_count = hits.len();
    let mut verified_ids = Vec::new();

    for hit in hits {
        // Find entry and mark Verified
        if let Some(entry) = state.find_entry_mut(&hit.surface_id) {
            entry.status = Status::Verified;
            entry.attempts.push(Attempt {
                cycle: 0,
                args: hit.args.clone(),
                full_argv: hit.args.clone(),
                seed: fixture_seed.clone(),
                evidence_path: hit.evidence_path.clone(),
                outcome: Outcome::Verified {
                    diff_kind: hit.diff_kind.clone(),
                },
                stdout_preview: hit.stdout_preview,
                stderr_preview: None,
                control_stdout_preview: hit.control_stdout_preview,
                fs_diff: None,
                stdout_metrics: None,
                stderr_metrics: None,
                prediction: None,
                prediction_matched: None,
                prediction_channel_matched: None,
                delta_relation: None, // TODO: compute from evidence when available
            });
            verified_ids.push(hit.surface_id.clone());
        }

        // Add to seed bank
        let hint = match &hit.diff_kind {
            DiffKind::Stdout => "batch_probe:stdout",
            DiffKind::Stderr => "batch_probe:stderr",
            DiffKind::ExitCode => "batch_probe:exit_code",
            _ => "batch_probe:multiple",
        };
        state.seed_bank.push(VerifiedSeed {
            surface_id: hit.surface_id,
            args: hit.args,
            seed: fixture_seed.clone(),
            verified_at: 0,
            hint: Some(hint.to_string()),
        });
    }

    eprintln!("Batch probe: verified {} surfaces", verified_count);
    verified_ids
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
        let long = surfaces
            .iter()
            .find(|s| s.id == "--break-rewrites")
            .unwrap();
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
        let long = surfaces
            .iter()
            .find(|s| s.id == "--break-rewrites")
            .unwrap();

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
        assert_eq!(classify_surface_mechanical("-S"), SurfaceCategory::General);
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
        let help =
            "  --diff-algorithm=(patience|minimal|histogram|myers)   Choose a diff algorithm";
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

    #[test]
    fn test_parse_usage_line_options() {
        let help = r#"Usage: find [-H] [-L] [-P] [-Olevel] [-D debugopts] [path...] [expression]

Default path is the current directory; default expression is -print.
"#;

        let surfaces = parse_surfaces_from_help(help);
        let ids: Vec<&str> = surfaces.iter().map(|s| s.id.as_str()).collect();

        assert!(ids.contains(&"-H"), "Should find -H, got: {:?}", ids);
        assert!(ids.contains(&"-L"), "Should find -L, got: {:?}", ids);
        assert!(ids.contains(&"-P"), "Should find -P, got: {:?}", ids);
        assert!(ids.contains(&"-O"), "Should find -O, got: {:?}", ids);
        assert!(ids.contains(&"-D"), "Should find -D, got: {:?}", ids);

        // Value hints
        let o = surfaces.iter().find(|s| s.id == "-O").unwrap();
        assert_eq!(o.value_hint, Some("level".to_string()));
        let d = surfaces.iter().find(|s| s.id == "-D").unwrap();
        assert_eq!(d.value_hint, Some("debugopts".to_string()));

        // Non-option brackets should not be extracted
        assert!(!ids.iter().any(|id| id.contains("path")));
        assert!(!ids.iter().any(|id| id.contains("expression")));
    }

    #[test]
    fn test_usage_line_dedup_with_option_blocks() {
        // -H appears in both usage line and option block — should appear once
        let help = r#"Usage: cmd [-H] [-v]

Options:
  -H    Dereference symlinks
  -v    Verbose output
"#;

        let surfaces = parse_surfaces_from_help(help);
        let h_count = surfaces.iter().filter(|s| s.id == "-H").count();
        assert_eq!(h_count, 1, "-H should appear exactly once");
        // The option block version should win (has description)
        let h = surfaces.iter().find(|s| s.id == "-H").unwrap();
        assert!(
            h.description.contains("Dereference"),
            "Should have description from option block, got: {:?}",
            h.description
        );
    }
}
