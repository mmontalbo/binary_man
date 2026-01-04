//! Help-text parsing into surface-level option claims.
//!
//! This module intentionally keeps parsing non-regex and staged so that each phase
//! can evolve independently. Help output varies widely across binaries; a small
//! grammar with typed tokens is easier to extend than a single, brittle pattern.
//!
//! ## Pipeline
//! - **Row detection**: isolate the spec column from the description.
//! - **Tokenization**: turn the spec segment into option/arg tokens.
//! - **Grammar parse**: assemble an [`OptionSpec`] with a canonical option and arg.
//! - **Claim emission**: emit `:exists` and (when present) `:binding` claims.
//!
//! ## Design notes
//! - Prefer explicit tokens over regex capture groups to keep intent readable.
//! - Keep parser versioned (`parse:help:v1`) so heuristics can change safely.
//! - Attach confidence scores so downstream tools can weight heuristic claims.
//!
//! ## Example walkthrough
//! Given a typical help row with a trailing arg:
//! ```text
//!   -o, --output FILE  write results to FILE
//! ```
//! Stage 1 (row detection) keeps the raw line and extracts the spec segment:
//! ```text
//! spec_segment: "-o, --output FILE"
//! ```
//! Stage 2 (tokenization) yields typed tokens:
//! ```text
//! Option: -o
//! Option: --output
//! Arg: FILE (required, trailing)
//! ```
//! Stage 3 (grammar parse) assembles an option spec:
//! ```text
//! options: [-o, --output]
//! arg: FILE (required, trailing)
//! ```
//! Stage 4 (claim emission) produces:
//! ```text
//! claim:option:opt=--output:exists
//! claim:option:opt=--output:binding   form="--output FILE"
//! ```
//!
//! An attached optional arg takes a different path at Stage 2:
//! ```text
//!   --color[=WHEN]  colorize output
//! ```
//! Tokenization sees an attached arg:
//! ```text
//! Option: --color
//! Arg: WHEN (optional, attached)
//! ```

use crate::schema::{Claim, ClaimKind, ClaimSource, ClaimSourceType, ClaimStatus};
use std::collections::{BTreeMap, HashSet};

const EXTRACTOR_HELP_V1: &str = "parse:help:v1";
const CONFIDENCE_EXISTS: f32 = 0.9;
const CONFIDENCE_BINDING_REQUIRED_ATTACHED: f32 = 0.7;
const CONFIDENCE_BINDING_OPTIONAL_ATTACHED: f32 = 0.6;
const CONFIDENCE_BINDING_REQUIRED_TRAILING: f32 = 0.65;
const CONFIDENCE_BINDING_OPTIONAL_TRAILING: f32 = 0.55;
const SINGLE_SPACE_SPLIT_MAX_LEN: usize = 72;

#[derive(Debug, Clone)]
struct OptionRow {
    line_no: usize,
    raw_line: String,
    spec_segment: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedHelpRow {
    pub(crate) options: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OptionToken {
    raw: String,
    kind: OptionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionKind {
    Long,
    Short,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArgToken {
    raw: String,
    optional: bool,
    source: ArgSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgSource {
    Attached,
    Trailing,
}

#[derive(Debug, Clone)]
enum SpecToken {
    Option(OptionToken),
    Arg(ArgToken),
    Separator,
}

#[derive(Debug, Clone)]
struct OptionSpec {
    options: Vec<OptionToken>,
    arg: Option<ArgSpec>,
}

#[derive(Debug, Clone)]
enum ArgSpec {
    Required { raw: String, source: ArgSource },
    Optional { raw: String, source: ArgSource },
}

/// Parse help output into option existence and parameter-binding claims.
///
/// This parser is intentionally conservative: it only consumes help text that
/// looks like an option table, and it prefers attached argument forms over
/// trailing heuristics when both appear on a line.
///
/// ## Recognized forms
/// - `--long`, `-s`
/// - Attached: `--opt=ARG`, `--opt[=ARG]`
/// - Trailing: `--opt FILE`, `--opt [ARG]`, `--opt <fmt>`
///
/// ## Output
/// Produces stable claim IDs like `claim:option:opt=--all:exists`.
///
/// # Examples
/// ```ignore
/// let claims = parse_help_text("<captured:--help>", "  -a, --all  include dotfiles\n");
/// assert!(claims.iter().any(|claim| claim.id == "claim:option:opt=--all:exists"));
/// ```
pub fn parse_help_text(source_path: &str, content: &str) -> Vec<Claim> {
    parse_option_claims(
        ClaimSourceType::Help,
        EXTRACTOR_HELP_V1,
        source_path,
        content,
        looks_like_option_table,
    )
}

fn parse_help_row(line: &str) -> Option<ParsedHelpRow> {
    let spec = option_spec_segment(line);
    if spec.is_empty() {
        return None;
    }
    let tokens = tokenize_spec(spec);
    let Some(spec) = parse_option_spec(&tokens) else {
        return None;
    };
    if spec.options.is_empty() {
        return None;
    }
    let mut options = Vec::new();
    let mut seen = HashSet::new();
    for option in spec.options.into_iter().map(|opt| opt.raw) {
        if seen.insert(option.clone()) {
            options.push(option);
        }
    }
    Some(ParsedHelpRow { options })
}

pub(crate) fn parse_help_row_options(line: &str) -> Vec<String> {
    parse_help_row(line)
        .map(|parsed| parsed.options)
        .unwrap_or_default()
}

// Parse rows into claims using a staged pipeline and preserve provenance.
fn parse_option_claims<F>(
    source_type: ClaimSourceType,
    extractor: &str,
    source_path: &str,
    content: &str,
    line_selector: F,
) -> Vec<Claim>
where
    F: Fn(&str) -> bool,
{
    let mut claims_by_id: BTreeMap<String, Claim> = BTreeMap::new();
    let path_str = source_path.to_string();

    for row in detect_option_rows(content, line_selector) {
        let tokens = tokenize_spec(&row.spec_segment);
        let Some(spec) = parse_option_spec(&tokens) else {
            continue;
        };
        let canonical = choose_canonical(&spec.options);

        let source = ClaimSource {
            source_type: source_type.clone(),
            path: path_str.clone(),
            line: Some(row.line_no as u64),
        };
        let raw_excerpt = row.raw_line.clone();

        let exists_id = format!("claim:option:opt={}:exists", canonical.raw);
        claims_by_id
            .entry(exists_id.clone())
            .or_insert_with(|| Claim {
                id: exists_id,
                text: format!(
                    "Option {} is listed in {}.",
                    canonical.raw,
                    source_label(&source_type)
                ),
                kind: ClaimKind::Option,
                source: source.clone(),
                status: ClaimStatus::Unvalidated,
                extractor: extractor.to_string(),
                raw_excerpt: raw_excerpt.clone(),
                confidence: Some(CONFIDENCE_EXISTS),
            });

        if let Some(arg_spec) = spec.arg {
            let (form_text, confidence, qualifier) = match arg_spec {
                ArgSpec::Required { raw, source } => (
                    format_binding_form(&canonical.raw, &raw, false, source),
                    binding_confidence(false, source),
                    "requires a value",
                ),
                ArgSpec::Optional { raw, source } => (
                    format_binding_form(&canonical.raw, &raw, true, source),
                    binding_confidence(true, source),
                    "accepts an optional value",
                ),
            };

            let binding_id = format!("claim:option:opt={}:binding", canonical.raw);
            claims_by_id
                .entry(binding_id.clone())
                .or_insert_with(|| Claim {
                    id: binding_id,
                    text: format!(
                        "Option {} {} in `{}` form.",
                        canonical.raw, qualifier, form_text
                    ),
                    kind: ClaimKind::Option,
                    source: source.clone(),
                    status: ClaimStatus::Unvalidated,
                    extractor: extractor.to_string(),
                    raw_excerpt: raw_excerpt.clone(),
                    confidence: Some(confidence),
                });
        }
    }

    claims_by_id.into_values().collect()
}

// Extract rows that appear to be option-table entries.
fn detect_option_rows<F>(content: &str, line_selector: F) -> Vec<OptionRow>
where
    F: Fn(&str) -> bool,
{
    let mut rows = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if !line_selector(line) {
            continue;
        }
        let spec = option_spec_segment(line);
        if spec.is_empty() {
            continue;
        }
        rows.push(OptionRow {
            line_no: idx + 1,
            raw_line: line.to_string(),
            spec_segment: spec.to_string(),
        });
    }
    rows
}

fn source_label(source_type: &ClaimSourceType) -> &'static str {
    match source_type {
        ClaimSourceType::Help => "help output",
    }
}

// Return true when a line looks like the start of an option spec row.
// Example: "  -a, --all  include" -> true; "Examples:" -> false.
fn looks_like_option_table(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('-') && !trimmed.starts_with("---")
}

// Split a line into the "spec" segment (options + args) and description.
// Prefer the typical double-space column split, with a guarded single-space
// fallback for short, spec-heavy lines.
// Example (double-space): "  -o, --output FILE  write results" -> "-o, --output FILE"
// Example (single-space): "-o, --output FILE write results" -> "-o, --output FILE"
fn option_spec_segment(line: &str) -> &str {
    let trimmed = line.trim();
    split_on_double_space_index(trimmed)
        .or_else(|| split_on_single_space_fallback(trimmed))
        .map(|idx| trimmed[..idx].trim_end())
        .unwrap_or(trimmed)
}

fn split_on_double_space_index(line: &str) -> Option<usize> {
    line.as_bytes()
        .windows(2)
        .position(|pair| is_whitespace(pair[0]) && is_whitespace(pair[1]))
}

// Heuristic split for short lines that use single-space separation.
// Example: "-o, --output FILE write results" -> split before "write".
fn split_on_single_space_fallback(line: &str) -> Option<usize> {
    if line.len() > SINGLE_SPACE_SPLIT_MAX_LEN {
        return None;
    }
    let mut saw_option = false;
    let mut spec_tokens = 0;
    let mut non_spec_tokens = 0;
    let mut split_at = None;

    for (start, end) in token_spans(line) {
        let token = &line[start..end];
        let is_option = looks_like_option_token(token);
        if is_option {
            saw_option = true;
        }
        let is_spec = is_option || looks_like_arg_token(token) || looks_like_separator_token(token);
        if is_spec {
            spec_tokens += 1;
        } else {
            non_spec_tokens += 1;
            if saw_option && split_at.is_none() {
                split_at = Some(start);
            }
        }
    }

    if !saw_option {
        return None;
    }
    let split_at = split_at?;
    if spec_tokens >= non_spec_tokens {
        Some(split_at)
    } else {
        None
    }
}

fn token_spans(line: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut iter = line.char_indices().peekable();
    std::iter::from_fn(move || {
        while let Some((start, ch)) = iter.next() {
            if ch == ' ' || ch == '\t' {
                continue;
            }
            while let Some(&(_, next)) = iter.peek() {
                if next == ' ' || next == '\t' {
                    break;
                }
                iter.next();
            }
            let end = iter.peek().map(|(idx, _)| *idx).unwrap_or(line.len());
            return Some((start, end));
        }
        None
    })
}

fn is_whitespace(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

// Convert a spec segment into typed tokens without regex.
// Example: "-o, --output FILE" -> Option(-o), Option(--output), Arg(FILE).
fn tokenize_spec(spec: &str) -> Vec<SpecToken> {
    let mut tokens = Vec::new();
    for word in spec.split_whitespace() {
        tokenize_word(word, &mut tokens);
    }
    tokens
}

fn tokenize_word(word: &str, tokens: &mut Vec<SpecToken>) {
    let mut segment = String::new();
    for ch in word.chars() {
        match ch {
            ',' | ';' => {
                flush_spec_segment(&mut segment, tokens);
                tokens.push(SpecToken::Separator);
            }
            ':' => {
                flush_spec_segment(&mut segment, tokens);
            }
            _ => segment.push(ch),
        }
    }
    flush_spec_segment(&mut segment, tokens);
}

fn flush_spec_segment(segment: &mut String, tokens: &mut Vec<SpecToken>) {
    if segment.is_empty() {
        return;
    }
    if let Some((option, arg)) = parse_option_segment(segment) {
        tokens.push(SpecToken::Option(option));
        if let Some(arg) = arg {
            tokens.push(SpecToken::Arg(arg));
        }
        return;
    }
    if let Some(arg) = parse_trailing_arg_segment(segment) {
        tokens.push(SpecToken::Arg(arg));
    }
}

// Parse tokens into a compact option spec with at most one arg.
fn parse_option_spec(tokens: &[SpecToken]) -> Option<OptionSpec> {
    let mut options = Vec::new();
    let mut arg: Option<ArgSpec> = None;

    for token in tokens {
        match token {
            SpecToken::Option(option) => options.push(option.clone()),
            SpecToken::Arg(arg_token) => {
                let candidate = ArgSpec::from_token(arg_token);
                arg = match arg {
                    None => Some(candidate),
                    Some(existing) => Some(prefer_arg_spec(existing, candidate)),
                };
            }
            SpecToken::Separator => {}
        }
    }

    if options.is_empty() {
        None
    } else {
        Some(OptionSpec { options, arg })
    }
}

// Prefer attached args over trailing heuristics when both are present.
// Example: "--color[=WHEN] --color WHEN" -> attached wins.
fn prefer_arg_spec(existing: ArgSpec, candidate: ArgSpec) -> ArgSpec {
    if existing.source() == ArgSource::Trailing && candidate.source() == ArgSource::Attached {
        candidate
    } else {
        existing
    }
}

// Prefer a long option as the canonical claim key when present.
fn choose_canonical(tokens: &[OptionToken]) -> &OptionToken {
    tokens
        .iter()
        .find(|t| t.kind == OptionKind::Long)
        .unwrap_or(&tokens[0])
}

// Format the binding form shown in claim text.
// Example: attached optional -> "--color[=WHEN]"; trailing required -> "--output FILE".
fn format_binding_form(option: &str, arg: &str, optional: bool, source: ArgSource) -> String {
    match source {
        ArgSource::Attached => {
            if optional {
                format!("{option}[={arg}]")
            } else {
                format!("{option}={arg}")
            }
        }
        ArgSource::Trailing => {
            if optional {
                format!("{option} [{arg}]")
            } else {
                format!("{option} {arg}")
            }
        }
    }
}

// Lower confidence for trailing args because they are heuristic-derived.
fn binding_confidence(optional: bool, source: ArgSource) -> f32 {
    match (optional, source) {
        (false, ArgSource::Attached) => CONFIDENCE_BINDING_REQUIRED_ATTACHED,
        (true, ArgSource::Attached) => CONFIDENCE_BINDING_OPTIONAL_ATTACHED,
        (false, ArgSource::Trailing) => CONFIDENCE_BINDING_REQUIRED_TRAILING,
        (true, ArgSource::Trailing) => CONFIDENCE_BINDING_OPTIONAL_TRAILING,
    }
}

impl ArgSpec {
    fn from_token(token: &ArgToken) -> Self {
        if token.optional {
            ArgSpec::Optional {
                raw: token.raw.clone(),
                source: token.source,
            }
        } else {
            ArgSpec::Required {
                raw: token.raw.clone(),
                source: token.source,
            }
        }
    }

    fn source(&self) -> ArgSource {
        match self {
            ArgSpec::Required { source, .. } | ArgSpec::Optional { source, .. } => *source,
        }
    }
}

// Parse a single token segment into an option and optional attached arg.
fn parse_option_segment(segment: &str) -> Option<(OptionToken, Option<ArgToken>)> {
    if let Some(parsed) = parse_long_option_segment(segment) {
        return Some(parsed);
    }
    parse_short_option_segment(segment)
}

fn parse_long_option_segment(segment: &str) -> Option<(OptionToken, Option<ArgToken>)> {
    if !segment.starts_with("--") {
        return None;
    }
    if segment.len() <= 2 {
        return None;
    }

    let (opt_part, arg_form) = split_attached_arg_form(segment)?;
    let name = &opt_part[2..];
    if name.is_empty() {
        return None;
    }
    let mut chars = name.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphanumeric() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }

    Some((
        OptionToken {
            raw: opt_part.to_string(),
            kind: OptionKind::Long,
        },
        arg_form,
    ))
}

fn parse_short_option_segment(segment: &str) -> Option<(OptionToken, Option<ArgToken>)> {
    if !segment.starts_with('-') || segment.starts_with("--") {
        return None;
    }
    if segment.len() < 2 {
        return None;
    }

    let (opt_part, arg_form) = split_attached_arg_form(segment)?;
    let name = &opt_part[1..];
    if name.len() != 1 {
        return None;
    }
    let ch = name.chars().next()?;
    if !ch.is_ascii_alphanumeric() {
        return None;
    }

    Some((
        OptionToken {
            raw: opt_part.to_string(),
            kind: OptionKind::Short,
        },
        arg_form,
    ))
}

// Detect attached arg forms (`--opt=ARG`, `--opt[=ARG]`).
// Example: "--color[=WHEN]" -> ("--color", Arg(WHEN, optional)).
fn split_attached_arg_form(token: &str) -> Option<(&str, Option<ArgToken>)> {
    if let Some(idx) = token.find("[=") {
        if token.ends_with(']') {
            let opt_part = &token[..idx];
            let arg = &token[idx + 2..token.len() - 1];
            if arg.is_empty() {
                return None;
            }
            return Some((
                opt_part,
                Some(ArgToken {
                    raw: arg.to_string(),
                    optional: true,
                    source: ArgSource::Attached,
                }),
            ));
        }
    }

    if let Some(idx) = token.find('=') {
        let opt_part = &token[..idx];
        let arg = &token[idx + 1..];
        if arg.is_empty() {
            return None;
        }
        return Some((
            opt_part,
            Some(ArgToken {
                raw: arg.to_string(),
                optional: false,
                source: ArgSource::Attached,
            }),
        ));
    }

    Some((token, None))
}

// Detect trailing arg placeholders (`FILE`, `<fmt>`, `[ARG]`) as heuristics.
// Example: "FILE" or "[ARG]" or "<fmt>" -> trailing arg token.
fn parse_trailing_arg_segment(segment: &str) -> Option<ArgToken> {
    let (raw, optional) = classify_arg_token(segment)?;
    Some(ArgToken {
        raw,
        optional,
        source: ArgSource::Trailing,
    })
}

// Classify a token as a required/optional arg placeholder.
// Example: "[ARG]" -> optional, "<fmt>" -> required.
fn classify_arg_token(token: &str) -> Option<(String, bool)> {
    if token.is_empty() {
        return None;
    }
    if let Some(inner) = token
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        if inner.is_empty() {
            return None;
        }
        return Some((inner.to_string(), true));
    }
    if let Some(inner) = token
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
    {
        if inner.is_empty() {
            return None;
        }
        return Some((format!("<{inner}>"), false));
    }
    if is_upper_placeholder(token) {
        return Some((token.to_string(), false));
    }
    None
}

fn is_upper_placeholder(token: &str) -> bool {
    let mut has_alpha = false;
    for ch in token.chars() {
        if ch.is_ascii_uppercase() {
            has_alpha = true;
        } else if ch.is_ascii_digit() || ch == '-' || ch == '_' {
            continue;
        } else {
            return false;
        }
    }
    has_alpha
}

fn trim_token_punct(token: &str) -> &str {
    token.trim_end_matches(|c: char| matches!(c, ',' | ';' | ':'))
}

fn looks_like_option_token(token: &str) -> bool {
    let trimmed = trim_token_punct(token);
    parse_option_segment(trimmed).is_some()
}

fn looks_like_arg_token(token: &str) -> bool {
    let trimmed = trim_token_punct(token);
    classify_arg_token(trimmed).is_some()
}

fn looks_like_separator_token(token: &str) -> bool {
    matches!(token, "," | ";")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding_claim<'a>(claims: &'a [Claim], id: &str) -> &'a Claim {
        claims
            .iter()
            .find(|claim| claim.id == id)
            .unwrap_or_else(|| panic!("missing binding claim {}", id))
    }

    fn assert_binding_form(claims: &[Claim], id: &str, expected_form: &str, optional: bool) {
        let claim = binding_claim(claims, id);
        if optional {
            assert!(claim.text.contains("optional value"));
        } else {
            assert!(claim.text.contains("requires a value"));
        }
        assert!(claim.text.contains(&format!("`{expected_form}`")));
    }

    #[test]
    fn ignores_non_option_lines() {
        let content = "Examples:\n  ls --color=auto\n";
        let claims = parse_help_text("/tmp/help.txt", content);
        assert!(claims.is_empty());
    }

    #[test]
    fn uses_captured_source_path_label() {
        let content = "  -a, --all  include dotfiles\n";
        let claims = parse_help_text("<captured:--help>", content);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].source.path, "<captured:--help>");
    }

    #[test]
    fn parses_trailing_arg_for_output() {
        let content = "  -o, --output FILE\n";
        let claims = parse_help_text("<captured:--help>", content);
        assert!(claims
            .iter()
            .any(|claim| claim.id == "claim:option:opt=--output:exists"));
        assert_binding_form(
            &claims,
            "claim:option:opt=--output:binding",
            "--output FILE",
            false,
        );
    }

    #[test]
    fn parse_help_row_extracts_options() {
        let row = "  -a, --all  include dotfiles";
        let parsed = parse_help_row(row).expect("parse help row");
        assert_eq!(parsed.options, vec!["-a", "--all"]);
    }

    #[test]
    fn parses_optional_attached_arg() {
        let content = "  --color[=WHEN]\n";
        let claims = parse_help_text("<captured:--help>", content);
        assert_binding_form(
            &claims,
            "claim:option:opt=--color:binding",
            "--color[=WHEN]",
            true,
        );
    }

    #[test]
    fn parses_required_attached_arg() {
        let content = "  --ignore=PATTERN\n";
        let claims = parse_help_text("<captured:--help>", content);
        assert_binding_form(
            &claims,
            "claim:option:opt=--ignore:binding",
            "--ignore=PATTERN",
            false,
        );
    }

    #[test]
    fn parses_angle_bracket_arg() {
        let content = "  --format <fmt>\n";
        let claims = parse_help_text("<captured:--help>", content);
        assert_binding_form(
            &claims,
            "claim:option:opt=--format:binding",
            "--format <fmt>",
            false,
        );
    }

    #[test]
    fn parses_optional_bracket_arg() {
        let content = "  --foo [BAR]\n";
        let claims = parse_help_text("<captured:--help>", content);
        assert_binding_form(
            &claims,
            "claim:option:opt=--foo:binding",
            "--foo [BAR]",
            true,
        );
    }

    #[test]
    fn parses_no_binding() {
        let content = "  --bar\n";
        let claims = parse_help_text("<captured:--help>", content);
        assert!(claims
            .iter()
            .any(|claim| claim.id == "claim:option:opt=--bar:exists"));
        assert!(!claims
            .iter()
            .any(|claim| claim.id == "claim:option:opt=--bar:binding"));
    }

    #[test]
    fn single_space_split_extracts_spec() {
        let line = "-o, --output FILE write results";
        assert_eq!(option_spec_segment(line), "-o, --output FILE");
    }

    #[test]
    fn single_space_split_skipped_when_line_too_long() {
        let spec = "-o, --output FILE";
        let desc_len = SINGLE_SPACE_SPLIT_MAX_LEN - spec.len() - 1;
        let desc = "x".repeat(desc_len);
        let line = format!("{spec} {desc}");
        assert_eq!(line.len(), SINGLE_SPACE_SPLIT_MAX_LEN);
        assert_eq!(option_spec_segment(&line), spec);

        let desc_len = SINGLE_SPACE_SPLIT_MAX_LEN - spec.len();
        let desc = "x".repeat(desc_len);
        let line = format!("{spec} {desc}");
        assert_eq!(line.len(), SINGLE_SPACE_SPLIT_MAX_LEN + 1);
        assert_eq!(option_spec_segment(&line), line);
    }
}
