//! Pack-owned render and verification semantics.
//!
//! Semantics define how help text is interpreted and how verification evidence
//! is classified, keeping the logic out of Rust and inside JSON.
use crate::templates;
use anyhow::{ensure, Context, Result};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Current schema version for `enrich/semantics.json`.
pub const SEMANTICS_SCHEMA_VERSION: u32 = 5;

fn default_true() -> bool {
    true
}

fn default_synopsis_min_lines() -> usize {
    1
}

/// Root semantics schema for rendering and verification.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Semantics {
    pub schema_version: u32,
    #[serde(default)]
    pub usage: UsageSemantics,
    #[serde(default)]
    pub description: DescriptionSemantics,
    #[serde(default)]
    pub options: OptionsSemantics,
    #[serde(default)]
    pub exit_status: ExitStatusSemantics,
    #[serde(default)]
    pub notes: NotesSemantics,
    #[serde(default)]
    pub boilerplate: BoilerplateSemantics,
    #[serde(default)]
    pub see_also: SeeAlsoSemantics,
    #[serde(default)]
    pub env_vars: EnvVarsSemantics,
    #[serde(default)]
    pub requirements: RenderRequirements,
    #[serde(default)]
    pub verification: VerificationSemantics,
    #[serde(default)]
    pub behavior_assertions: BehaviorAssertionSemantics,
}

/// Rules for extracting usage/synopsis lines.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct UsageSemantics {
    #[serde(default)]
    pub line_rules: Vec<LineCapture>,
    #[serde(default)]
    pub prefer_rules: Vec<LineMatcher>,
}

/// Rules for extracting and selecting descriptions.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct DescriptionSemantics {
    #[serde(default)]
    pub capture_blocks: Vec<DescriptionCaptureBlock>,
    #[serde(default)]
    pub section_headers: Vec<LineMatcher>,
    #[serde(default)]
    pub fallback: DescriptionFallback,
}

/// Fallback behavior when description extraction is ambiguous.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub enum DescriptionFallback {
    #[default]
    Leading,
    Section,
    None,
}

/// Rules for parsing option sections.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct OptionsSemantics {
    #[serde(default)]
    pub section_headers: Vec<LineMatcher>,
    #[serde(default)]
    pub heading_rules: Vec<LineMatcher>,
    #[serde(default)]
    pub entry_rules: Vec<OptionEntryRule>,
    #[serde(default = "default_true")]
    pub allow_continuation: bool,
}

/// Rules for parsing exit status sections.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct ExitStatusSemantics {
    #[serde(default)]
    pub section_headers: Vec<LineMatcher>,
    #[serde(default)]
    pub line_rules: Vec<LineCapture>,
    #[serde(default = "default_true")]
    pub stop_on_blank: bool,
}

/// Rules for parsing notes sections.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct NotesSemantics {
    #[serde(default)]
    pub section_headers: Vec<LineMatcher>,
    #[serde(default = "default_true")]
    pub capture_after_options: bool,
}

/// Boilerplate exclusion rules for synopsis/description parsing.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct BoilerplateSemantics {
    #[serde(default)]
    pub exclude_lines: Vec<LineMatcher>,
    #[serde(default = "default_true")]
    pub exclude_binary_name: bool,
}

/// Rules for parsing "See Also" sections.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct SeeAlsoSemantics {
    #[serde(default)]
    pub rules: Vec<SeeAlsoRule>,
}

/// Rules for parsing environment variable sections.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct EnvVarsSemantics {
    #[serde(default)]
    pub paragraph_matchers: Vec<LineMatcher>,
    #[serde(default)]
    pub variable_regex: Option<String>,
}

/// Rules for classifying verification evidence.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct VerificationSemantics {
    #[serde(default)]
    pub accepted: Vec<VerificationRule>,
    #[serde(default)]
    pub rejected: Vec<VerificationRule>,
    #[serde(default)]
    pub option_existence_argv_prefix: Vec<String>,
    #[serde(default)]
    pub option_existence_argv_suffix: Vec<String>,
    #[serde(default)]
    pub subcommand_existence_argv_prefix: Vec<String>,
    #[serde(default)]
    pub subcommand_existence_argv_suffix: Vec<String>,
}

/// Normalization rules applied to behavior assertion evaluation.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct BehaviorAssertionSemantics {
    #[serde(default = "default_true")]
    pub strip_ansi: bool,
    #[serde(default = "default_true")]
    pub trim_whitespace: bool,
    #[serde(default)]
    pub collapse_internal_whitespace: bool,
    #[serde(default)]
    pub confounded_coverage_gate: bool,
}

impl Default for BehaviorAssertionSemantics {
    fn default() -> Self {
        BehaviorAssertionSemantics {
            strip_ansi: true,
            trim_whitespace: true,
            collapse_internal_whitespace: false,
            confounded_coverage_gate: false,
        }
    }
}

/// Single verification rule for accepted/rejected classification.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct VerificationRule {
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub exit_signal: Option<i32>,
    #[serde(default)]
    pub stdout_contains_all: Vec<String>,
    #[serde(default)]
    pub stdout_contains_any: Vec<String>,
    #[serde(default)]
    pub stdout_regex_all: Vec<String>,
    #[serde(default)]
    pub stdout_regex_any: Vec<String>,
    #[serde(default)]
    pub stderr_contains_all: Vec<String>,
    #[serde(default)]
    pub stderr_contains_any: Vec<String>,
    #[serde(default)]
    pub stderr_regex_all: Vec<String>,
    #[serde(default)]
    pub stderr_regex_any: Vec<String>,
}

/// Minimum render requirements used to decide if a man page is complete.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct RenderRequirements {
    #[serde(default = "default_synopsis_min_lines")]
    pub synopsis_min_lines: usize,
    #[serde(default)]
    pub description_min_lines: Option<usize>,
    #[serde(default)]
    pub commands_min_entries: Option<usize>,
    #[serde(default)]
    pub options_min_entries: Option<usize>,
    #[serde(default)]
    pub exit_status_min_lines: Option<usize>,
}

impl Default for RenderRequirements {
    fn default() -> Self {
        RenderRequirements {
            synopsis_min_lines: default_synopsis_min_lines(),
            description_min_lines: None,
            commands_min_entries: None,
            options_min_entries: None,
            exit_status_min_lines: None,
        }
    }
}

/// A line matcher used to locate structured help text segments.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LineMatcher {
    Prefix {
        value: String,
        #[serde(default)]
        case_sensitive: bool,
    },
    Contains {
        value: String,
        #[serde(default)]
        case_sensitive: bool,
    },
    Exact {
        value: String,
        #[serde(default)]
        case_sensitive: bool,
    },
    Regex {
        pattern: String,
        #[serde(default)]
        case_sensitive: bool,
    },
}

/// Rule capturing a line once matched.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LineCapture {
    pub pattern: String,
    #[serde(default)]
    pub capture_group: Option<usize>,
    #[serde(default)]
    pub case_sensitive: bool,
}

/// Rule describing how to parse a single option entry.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct OptionEntryRule {
    pub pattern: String,
    #[serde(default)]
    pub names_group: Option<usize>,
    #[serde(default)]
    pub desc_group: Option<usize>,
    #[serde(default)]
    pub case_sensitive: bool,
}

/// Capture block used for structured description extraction.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DescriptionCaptureBlock {
    pub start: LineMatcher,
    #[serde(default)]
    pub end: Option<LineMatcher>,
    #[serde(default)]
    pub include_start: bool,
    #[serde(default)]
    pub include_end: bool,
}

/// Rule for emitting a See Also entry.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SeeAlsoRule {
    pub when: LineMatcher,
    #[serde(default)]
    pub entries: Vec<String>,
}

/// Load semantics from `enrich/semantics.json`.
pub fn load_semantics(doc_pack_root: &Path) -> Result<Semantics> {
    let path = doc_pack_root.join("enrich").join("semantics.json");
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let semantics: Semantics =
        serde_json::from_slice(&bytes).context("parse enrich semantics JSON")?;
    validate_semantics(&semantics)?;
    Ok(semantics)
}

/// Validate semantics schema for render/verification rules.
pub fn validate_semantics(semantics: &Semantics) -> Result<()> {
    ensure!(
        semantics.schema_version == SEMANTICS_SCHEMA_VERSION,
        "unsupported semantics schema_version {}",
        semantics.schema_version
    );

    for (idx, rule) in semantics.usage.line_rules.iter().enumerate() {
        validate_capture_rule(rule, &format!("usage.line_rules[{idx}]"))?;
    }
    for (idx, rule) in semantics.usage.prefer_rules.iter().enumerate() {
        validate_line_matcher(rule, &format!("usage.prefer_rules[{idx}]"))?;
    }
    for (idx, block) in semantics.description.capture_blocks.iter().enumerate() {
        validate_line_matcher(
            &block.start,
            &format!("description.capture_blocks[{idx}].start"),
        )?;
        if let Some(end) = block.end.as_ref() {
            validate_line_matcher(end, &format!("description.capture_blocks[{idx}].end"))?;
        }
    }
    for (idx, rule) in semantics.description.section_headers.iter().enumerate() {
        validate_line_matcher(rule, &format!("description.section_headers[{idx}]"))?;
    }
    for (idx, rule) in semantics.options.section_headers.iter().enumerate() {
        validate_line_matcher(rule, &format!("options.section_headers[{idx}]"))?;
    }
    for (idx, rule) in semantics.options.heading_rules.iter().enumerate() {
        validate_line_matcher(rule, &format!("options.heading_rules[{idx}]"))?;
    }
    for (idx, rule) in semantics.options.entry_rules.iter().enumerate() {
        validate_option_entry_rule(rule, &format!("options.entry_rules[{idx}]"))?;
    }
    for (idx, rule) in semantics.exit_status.section_headers.iter().enumerate() {
        validate_line_matcher(rule, &format!("exit_status.section_headers[{idx}]"))?;
    }
    for (idx, rule) in semantics.exit_status.line_rules.iter().enumerate() {
        validate_capture_rule(rule, &format!("exit_status.line_rules[{idx}]"))?;
    }
    for (idx, rule) in semantics.notes.section_headers.iter().enumerate() {
        validate_line_matcher(rule, &format!("notes.section_headers[{idx}]"))?;
    }
    for (idx, rule) in semantics.boilerplate.exclude_lines.iter().enumerate() {
        validate_line_matcher(rule, &format!("boilerplate.exclude_lines[{idx}]"))?;
    }
    for (idx, rule) in semantics.see_also.rules.iter().enumerate() {
        validate_line_matcher(&rule.when, &format!("see_also.rules[{idx}].when"))?;
    }
    if let Some(pattern) = semantics.env_vars.variable_regex.as_ref() {
        compile_regex(pattern, true, "env_vars.variable_regex")?;
    }
    for (idx, rule) in semantics.env_vars.paragraph_matchers.iter().enumerate() {
        validate_line_matcher(rule, &format!("env_vars.paragraph_matchers[{idx}]"))?;
    }
    for (idx, rule) in semantics.verification.accepted.iter().enumerate() {
        validate_verification_rule(rule, &format!("verification.accepted[{idx}]"))?;
    }
    for (idx, rule) in semantics.verification.rejected.iter().enumerate() {
        validate_verification_rule(rule, &format!("verification.rejected[{idx}]"))?;
    }
    validate_invocation_tokens(
        &semantics.verification.option_existence_argv_prefix,
        "verification.option_existence_argv_prefix",
    )?;
    validate_invocation_tokens(
        &semantics.verification.option_existence_argv_suffix,
        "verification.option_existence_argv_suffix",
    )?;
    validate_invocation_tokens(
        &semantics.verification.subcommand_existence_argv_prefix,
        "verification.subcommand_existence_argv_prefix",
    )?;
    validate_invocation_tokens(
        &semantics.verification.subcommand_existence_argv_suffix,
        "verification.subcommand_existence_argv_suffix",
    )?;

    Ok(())
}

/// Render a semantics stub for new packs or edit suggestions.
pub fn semantics_stub(_binary_name: Option<&str>) -> String {
    templates::ENRICH_SEMANTICS_JSON.to_string()
}

fn validate_line_matcher(matcher: &LineMatcher, label: &str) -> Result<()> {
    if let LineMatcher::Regex {
        pattern,
        case_sensitive,
    } = matcher
    {
        compile_regex(pattern, *case_sensitive, label)?;
    }
    Ok(())
}

fn validate_capture_rule(rule: &LineCapture, label: &str) -> Result<()> {
    let regex = compile_regex(&rule.pattern, rule.case_sensitive, label)?;
    if let Some(group) = rule.capture_group {
        ensure!(
            group < regex.captures_len(),
            "{label} capture_group {group} exceeds regex groups ({})",
            regex.captures_len().saturating_sub(1)
        );
    }
    Ok(())
}

fn validate_option_entry_rule(rule: &OptionEntryRule, label: &str) -> Result<()> {
    let regex = compile_regex(&rule.pattern, rule.case_sensitive, label)?;
    if let Some(group) = rule.names_group {
        ensure!(
            group < regex.captures_len(),
            "{label} names_group {group} exceeds regex groups ({})",
            regex.captures_len().saturating_sub(1)
        );
    }
    if let Some(group) = rule.desc_group {
        ensure!(
            group < regex.captures_len(),
            "{label} desc_group {group} exceeds regex groups ({})",
            regex.captures_len().saturating_sub(1)
        );
    }
    Ok(())
}

fn validate_verification_rule(rule: &VerificationRule, label: &str) -> Result<()> {
    for (idx, pattern) in rule.stdout_regex_all.iter().enumerate() {
        compile_regex(pattern, true, &format!("{label}.stdout_regex_all[{idx}]"))?;
    }
    for (idx, pattern) in rule.stdout_regex_any.iter().enumerate() {
        compile_regex(pattern, true, &format!("{label}.stdout_regex_any[{idx}]"))?;
    }
    for (idx, pattern) in rule.stderr_regex_all.iter().enumerate() {
        compile_regex(pattern, true, &format!("{label}.stderr_regex_all[{idx}]"))?;
    }
    for (idx, pattern) in rule.stderr_regex_any.iter().enumerate() {
        compile_regex(pattern, true, &format!("{label}.stderr_regex_any[{idx}]"))?;
    }
    Ok(())
}

fn validate_invocation_tokens(tokens: &[String], label: &str) -> Result<()> {
    for (idx, token) in tokens.iter().enumerate() {
        ensure!(!token.trim().is_empty(), "{label}[{idx}] must not be empty");
    }
    Ok(())
}

fn compile_regex(pattern: &str, case_sensitive: bool, label: &str) -> Result<regex::Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .with_context(|| format!("invalid regex for {label}"))
}
