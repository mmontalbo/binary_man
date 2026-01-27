//! Deterministic man page renderer from usage evidence.

use crate::pack::PackContext;
use crate::scenarios::ExamplesReport;
use crate::semantics;
use crate::surface;
use anyhow::{anyhow, Result};
use regex::{Regex, RegexBuilder};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Serialize, Clone)]
pub struct RenderSummary {
    pub schema_version: u32,
    pub synopsis_lines: usize,
    pub description_lines: usize,
    pub options_entries: usize,
    pub commands_entries: usize,
    pub exit_status_lines: usize,
    pub notes_lines: usize,
    pub env_vars: usize,
    pub see_also_entries: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub semantics_unmet: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub struct RenderedManPage {
    pub man_page: String,
    pub summary: RenderSummary,
}

pub fn render_man_page(
    context: &PackContext,
    semantics: &semantics::Semantics,
    examples_report: Option<&ExamplesReport>,
    surface: Option<&surface::SurfaceInventory>,
) -> Result<RenderedManPage> {
    let compiled = CompiledSemantics::new(semantics)?;
    let binary_name = context.manifest.binary_name.as_str();
    let upper = binary_name.to_uppercase();
    let help_text = context.help_text.as_str();
    let commands = collect_commands(surface);
    let surface_options = collect_surface_options(surface);

    let usage_lines = extract_usage_lines(help_text, &compiled);
    let synopsis_lines = select_synopsis_lines(&usage_lines, &compiled);
    let sections = parse_help_text(help_text, &compiled);
    let description_lines = select_description(
        help_text,
        &sections.description_fallback,
        &compiled,
        binary_name,
    );
    let notes_lines = filter_notes(&sections.notes, &description_lines, binary_name, &compiled);
    let name_desc = name_description(
        &description_lines,
        &sections.description_fallback,
        help_text,
        binary_name,
        &compiled,
    );
    let env_vars = extract_env_vars(help_text, &compiled);
    let see_also = extract_see_also(help_text, &compiled);

    let options_items = if !surface_options.is_empty() {
        surface_options
            .into_iter()
            .map(OptionItem::Option)
            .collect::<Vec<_>>()
    } else {
        sections.options
    };

    let mut out = String::new();
    out.push_str(&format!(
        ".TH {} 1 \"generated\" \"binary_man\" \"User Commands\"\n",
        upper
    ));
    out.push_str(".SH NAME\n");
    if let Some(desc) = name_desc {
        out.push_str(&format!(
            "{} \\- {}\n",
            escape_text(binary_name),
            escape_text(&desc)
        ));
    } else {
        out.push_str(&format!("{}\n", escape_text(binary_name)));
    }

    out.push_str(".SH SYNOPSIS\n");
    if synopsis_lines.is_empty() {
        out.push_str(&format!(".B {}\n", escape_text(binary_name)));
    } else {
        for (idx, usage) in synopsis_lines.iter().enumerate() {
            let (cmd, rest) = split_usage_line(usage, binary_name);
            out.push_str(&format!(".B {}\n", escape_text(&cmd)));
            if !rest.is_empty() {
                out.push_str(&format!(".RI \" {}\"\n", escape_text(&rest)));
            }
            if idx + 1 < synopsis_lines.len() {
                out.push_str(".br\n");
            }
        }
    }

    if !description_lines.is_empty() {
        out.push_str(".SH DESCRIPTION\n");
        out.push_str(&paragraphs_to_roff(&description_lines));
    }

    if !commands.is_empty() {
        out.push_str(".SH COMMANDS\n");
        for cmd in &commands {
            out.push_str(".TP\n");
            out.push_str(&format!(".B {}\n", escape_option(&cmd.name)));
            if let Some(desc) = cmd.description.as_ref() {
                out.push_str(&format!("{}\n", escape_text(desc)));
            }
        }
    }

    if !options_items.is_empty() {
        out.push_str(".SH OPTIONS\n");
        out.push_str(&options_to_roff(&options_items));
    }

    if let Some(examples_report) = examples_report {
        let passing: Vec<_> = examples_report
            .scenarios
            .iter()
            .filter(|scenario| scenario.pass && scenario.publish)
            .collect();
        if !passing.is_empty() {
            out.push_str(".SH EXAMPLES\n");
            for scenario in passing {
                out.push_str(".PP\n");
                out.push_str(".nf\n");
                append_verbatim_line(&mut out, &format!("$ {}", scenario.command_line));
                append_verbatim_snippet(&mut out, &scenario.stdout_snippet);
                if !scenario.stderr_snippet.trim().is_empty() {
                    append_verbatim_line(&mut out, "[stderr]");
                    append_verbatim_snippet(&mut out, &scenario.stderr_snippet);
                }
                out.push_str(".fi\n");

                if let Some(code) = scenario.observed_exit_code {
                    if code != 0 {
                        let exit_label = ["Exit", "status"].join(" ");
                        out.push_str(&format!(".PP\n{}: {}.\n", exit_label, code));
                    }
                } else if let Some(signal) = scenario.observed_exit_signal {
                    out.push_str(&format!(".PP\nTerminated by signal {}.\n", signal));
                }
            }
        }
    }

    if !sections.exit_status.is_empty() {
        out.push_str(".SH EXIT STATUS\n");
        for line in &sections.exit_status {
            out.push_str(&format!(".TP\n{}\n", escape_text(line)));
        }
    }

    if !env_vars.is_empty() {
        out.push_str(".SH ENVIRONMENT\n");
        for var in &env_vars {
            out.push_str(&format!(".TP\n.B {}\n", escape_option(var)));
        }
    }

    if !notes_lines.is_empty() {
        out.push_str(".SH NOTES\n");
        out.push_str(&paragraphs_to_roff(&notes_lines));
    }

    if !see_also.is_empty() {
        out.push_str(".SH SEE ALSO\n");
        for (idx, entry) in see_also.iter().enumerate() {
            if idx > 0 {
                out.push_str(".br\n");
            }
            out.push_str(&format!(".BR {}\n", escape_text(entry)));
        }
    }

    let options_count = options_items
        .iter()
        .filter(|item| matches!(item, OptionItem::Option(_)))
        .count();
    let summary = build_render_summary(
        &compiled,
        synopsis_lines.len(),
        description_lines.len(),
        options_count,
        commands.len(),
        sections.exit_status.len(),
        notes_lines.len(),
        env_vars.len(),
        see_also.len(),
    );

    Ok(RenderedManPage {
        man_page: out,
        summary,
    })
}

fn append_verbatim_snippet(out: &mut String, snippet: &str) {
    if snippet.trim().is_empty() {
        return;
    }
    for chunk in snippet.split_inclusive('\n') {
        if let Some(line) = chunk.strip_suffix('\n') {
            append_verbatim_line(out, line);
        } else {
            append_verbatim_line(out, chunk);
        }
    }
}

fn append_verbatim_line(out: &mut String, line: &str) {
    out.push_str(&escape_option(line));
    out.push('\n');
}

struct HelpSections {
    description_fallback: Vec<String>,
    options: Vec<OptionItem>,
    exit_status: Vec<String>,
    notes: Vec<String>,
}

struct CommandEntry {
    name: String,
    description: Option<String>,
}

enum OptionItem {
    Heading(String),
    Option(OptionEntry),
}

struct OptionEntry {
    names: String,
    desc: String,
}

struct CompiledSemantics {
    usage_line_rules: Vec<CompiledCapture>,
    usage_prefer_rules: Vec<Matcher>,
    description_capture_blocks: Vec<CompiledDescriptionBlock>,
    description_section_headers: Vec<Matcher>,
    options_section_headers: Vec<Matcher>,
    options_heading_rules: Vec<Matcher>,
    options_entry_rules: Vec<CompiledOptionRule>,
    exit_status_section_headers: Vec<Matcher>,
    exit_status_line_rules: Vec<CompiledCapture>,
    notes_section_headers: Vec<Matcher>,
    boilerplate_exclude_lines: Vec<Matcher>,
    see_also_rules: Vec<CompiledSeeAlsoRule>,
    env_paragraph_matchers: Vec<Matcher>,
    env_variable_regex: Option<Regex>,
    requirements: semantics::RenderRequirements,
    options_allow_continuation: bool,
    exit_status_stop_on_blank: bool,
    notes_capture_after_options: bool,
    boilerplate_exclude_binary_name: bool,
    description_fallback: semantics::DescriptionFallback,
}

impl CompiledSemantics {
    fn new(semantics: &semantics::Semantics) -> Result<Self> {
        let usage_line_rules = compile_capture_rules(&semantics.usage.line_rules)?;
        let usage_prefer_rules = compile_matchers(&semantics.usage.prefer_rules)?;
        let description_capture_blocks =
            compile_description_blocks(&semantics.description.capture_blocks)?;
        let description_section_headers = compile_matchers(&semantics.description.section_headers)?;
        let options_section_headers = compile_matchers(&semantics.options.section_headers)?;
        let options_heading_rules = compile_matchers(&semantics.options.heading_rules)?;
        let options_entry_rules = compile_option_rules(&semantics.options.entry_rules)?;
        let exit_status_section_headers = compile_matchers(&semantics.exit_status.section_headers)?;
        let exit_status_line_rules = compile_capture_rules(&semantics.exit_status.line_rules)?;
        let notes_section_headers = compile_matchers(&semantics.notes.section_headers)?;
        let boilerplate_exclude_lines = compile_matchers(&semantics.boilerplate.exclude_lines)?;
        let see_also_rules = compile_see_also_rules(&semantics.see_also.rules)?;
        let env_paragraph_matchers = compile_matchers(&semantics.env_vars.paragraph_matchers)?;
        let env_variable_regex = if let Some(pattern) = semantics.env_vars.variable_regex.as_ref() {
            Some(compile_regex(pattern, true)?)
        } else {
            None
        };

        Ok(Self {
            usage_line_rules,
            usage_prefer_rules,
            description_capture_blocks,
            description_section_headers,
            options_section_headers,
            options_heading_rules,
            options_entry_rules,
            exit_status_section_headers,
            exit_status_line_rules,
            notes_section_headers,
            boilerplate_exclude_lines,
            see_also_rules,
            env_paragraph_matchers,
            env_variable_regex,
            requirements: semantics.requirements.clone(),
            options_allow_continuation: semantics.options.allow_continuation,
            exit_status_stop_on_blank: semantics.exit_status.stop_on_blank,
            notes_capture_after_options: semantics.notes.capture_after_options,
            boilerplate_exclude_binary_name: semantics.boilerplate.exclude_binary_name,
            description_fallback: semantics.description.fallback.clone(),
        })
    }
}

struct CompiledCapture {
    regex: Regex,
    capture_group: Option<usize>,
}

struct CompiledOptionRule {
    regex: Regex,
    names_group: Option<usize>,
    desc_group: Option<usize>,
}

struct CompiledDescriptionBlock {
    start: Matcher,
    end: Option<Matcher>,
    include_start: bool,
    include_end: bool,
}

struct CompiledSeeAlsoRule {
    when: Matcher,
    entries: Vec<String>,
}

enum Matcher {
    Prefix {
        value: String,
        value_lower: String,
        case_sensitive: bool,
    },
    Contains {
        value: String,
        value_lower: String,
        case_sensitive: bool,
    },
    Exact {
        value: String,
        value_lower: String,
        case_sensitive: bool,
    },
    Regex(Regex),
}

impl Matcher {
    fn is_match(&self, text: &str) -> bool {
        match self {
            Matcher::Prefix {
                value,
                value_lower,
                case_sensitive,
            } => {
                if *case_sensitive {
                    text.starts_with(value)
                } else {
                    text.to_lowercase().starts_with(value_lower)
                }
            }
            Matcher::Contains {
                value,
                value_lower,
                case_sensitive,
            } => {
                if *case_sensitive {
                    text.contains(value)
                } else {
                    text.to_lowercase().contains(value_lower)
                }
            }
            Matcher::Exact {
                value,
                value_lower,
                case_sensitive,
            } => {
                if *case_sensitive {
                    text == value
                } else {
                    text.to_lowercase() == value_lower.as_str()
                }
            }
            Matcher::Regex(regex) => regex.is_match(text),
        }
    }
}

fn compile_capture_rules(rules: &[semantics::LineCapture]) -> Result<Vec<CompiledCapture>> {
    let mut compiled = Vec::new();
    for rule in rules {
        let regex = compile_regex(&rule.pattern, rule.case_sensitive)?;
        compiled.push(CompiledCapture {
            regex,
            capture_group: rule.capture_group,
        });
    }
    Ok(compiled)
}

fn compile_option_rules(rules: &[semantics::OptionEntryRule]) -> Result<Vec<CompiledOptionRule>> {
    let mut compiled = Vec::new();
    for rule in rules {
        let regex = compile_regex(&rule.pattern, rule.case_sensitive)?;
        compiled.push(CompiledOptionRule {
            regex,
            names_group: rule.names_group,
            desc_group: rule.desc_group,
        });
    }
    Ok(compiled)
}

fn compile_description_blocks(
    blocks: &[semantics::DescriptionCaptureBlock],
) -> Result<Vec<CompiledDescriptionBlock>> {
    let mut compiled = Vec::new();
    for block in blocks {
        compiled.push(CompiledDescriptionBlock {
            start: compile_matcher(&block.start)?,
            end: match block.end.as_ref() {
                Some(end) => Some(compile_matcher(end)?),
                None => None,
            },
            include_start: block.include_start,
            include_end: block.include_end,
        });
    }
    Ok(compiled)
}

fn compile_see_also_rules(rules: &[semantics::SeeAlsoRule]) -> Result<Vec<CompiledSeeAlsoRule>> {
    let mut compiled = Vec::new();
    for rule in rules {
        compiled.push(CompiledSeeAlsoRule {
            when: compile_matcher(&rule.when)?,
            entries: rule.entries.clone(),
        });
    }
    Ok(compiled)
}

fn compile_matchers(matchers: &[semantics::LineMatcher]) -> Result<Vec<Matcher>> {
    let mut compiled = Vec::new();
    for matcher in matchers {
        compiled.push(compile_matcher(matcher)?);
    }
    Ok(compiled)
}

fn compile_matcher(matcher: &semantics::LineMatcher) -> Result<Matcher> {
    match matcher {
        semantics::LineMatcher::Prefix {
            value,
            case_sensitive,
        } => Ok(Matcher::Prefix {
            value: value.clone(),
            value_lower: value.to_lowercase(),
            case_sensitive: *case_sensitive,
        }),
        semantics::LineMatcher::Contains {
            value,
            case_sensitive,
        } => Ok(Matcher::Contains {
            value: value.clone(),
            value_lower: value.to_lowercase(),
            case_sensitive: *case_sensitive,
        }),
        semantics::LineMatcher::Exact {
            value,
            case_sensitive,
        } => Ok(Matcher::Exact {
            value: value.clone(),
            value_lower: value.to_lowercase(),
            case_sensitive: *case_sensitive,
        }),
        semantics::LineMatcher::Regex {
            pattern,
            case_sensitive,
        } => Ok(Matcher::Regex(compile_regex(pattern, *case_sensitive)?)),
    }
}

fn compile_regex(pattern: &str, case_sensitive: bool) -> Result<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|err| anyhow!("invalid regex {pattern:?}: {err}"))
}

fn matches_any(matchers: &[Matcher], text: &str) -> bool {
    matchers.iter().any(|matcher| matcher.is_match(text))
}

fn capture_with_rule(rule: &CompiledCapture, text: &str) -> Option<String> {
    let caps = rule.regex.captures(text)?;
    let value = match rule.capture_group {
        Some(group) => caps.get(group).or_else(|| caps.get(0)),
        None => caps.get(0),
    }?;
    let trimmed = value.as_str().trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_option_line(line: &str, rules: &[CompiledOptionRule]) -> Option<OptionEntry> {
    for rule in rules {
        let caps = match rule.regex.captures(line) {
            Some(caps) => caps,
            None => continue,
        };
        let names = match rule.names_group {
            Some(group) => caps.get(group).or_else(|| caps.get(0)),
            None => caps.get(0),
        };
        let Some(names) = names
            .map(|m| m.as_str().trim())
            .filter(|val| !val.is_empty())
        else {
            continue;
        };
        let desc = match rule.desc_group {
            Some(group) => caps.get(group).map(|m| m.as_str().trim()).unwrap_or(""),
            None => "",
        };
        return Some(OptionEntry {
            names: names.to_string(),
            desc: desc.to_string(),
        });
    }
    None
}

fn extract_usage_lines(help_text: &str, compiled: &CompiledSemantics) -> Vec<String> {
    let mut lines = Vec::new();
    for raw in help_text.lines() {
        let stripped = raw.trim_end();
        for rule in &compiled.usage_line_rules {
            if let Some(value) = capture_with_rule(rule, stripped) {
                lines.push(value);
            }
        }
    }
    lines
}

fn select_synopsis_lines(lines: &[String], compiled: &CompiledSemantics) -> Vec<String> {
    if compiled.usage_prefer_rules.is_empty() {
        return lines.to_vec();
    }
    let mut preferred = Vec::new();
    for line in lines {
        if matches_any(&compiled.usage_prefer_rules, line) {
            preferred.push(line.clone());
        }
    }
    if preferred.is_empty() {
        lines.to_vec()
    } else {
        preferred
    }
}

fn parse_help_text(help_text: &str, compiled: &CompiledSemantics) -> HelpSections {
    let mut description_fallback = Vec::new();
    let mut options = Vec::new();
    let mut exit_status = Vec::new();
    let mut notes = Vec::new();
    let mut current_option: Option<OptionEntry> = None;
    let mut seen_options = false;
    let mut in_exit = false;
    let mut in_notes_section = false;

    for raw in help_text.lines() {
        let trimmed = raw.trim_end();
        let stripped = trimmed.trim_start();

        if stripped.is_empty() {
            if let Some(entry) = current_option.take() {
                options.push(OptionItem::Option(entry));
            }
            if in_exit && compiled.exit_status_stop_on_blank {
                in_exit = false;
            }
            continue;
        }

        if matches_any(&compiled.exit_status_section_headers, stripped) {
            if let Some(entry) = current_option.take() {
                options.push(OptionItem::Option(entry));
            }
            in_exit = true;
            in_notes_section = false;
            continue;
        }

        if matches_any(&compiled.options_section_headers, stripped) {
            if let Some(entry) = current_option.take() {
                options.push(OptionItem::Option(entry));
            }
            seen_options = true;
            in_notes_section = false;
            if matches_any(&compiled.options_heading_rules, stripped) {
                options.push(OptionItem::Heading(
                    stripped.trim_end_matches(':').to_string(),
                ));
            }
            continue;
        }

        if matches_any(&compiled.notes_section_headers, stripped) {
            if let Some(entry) = current_option.take() {
                options.push(OptionItem::Option(entry));
            }
            in_notes_section = true;
            in_exit = false;
            notes.push(stripped.to_string());
            continue;
        }

        if in_exit {
            if compiled.exit_status_line_rules.is_empty() {
                exit_status.push(stripped.to_string());
            } else {
                for rule in &compiled.exit_status_line_rules {
                    if let Some(value) = capture_with_rule(rule, stripped) {
                        exit_status.push(value);
                        break;
                    }
                }
            }
            continue;
        }

        if let Some(entry) = parse_option_line(stripped, &compiled.options_entry_rules) {
            if let Some(entry) = current_option.take() {
                options.push(OptionItem::Option(entry));
            }
            current_option = Some(entry);
            seen_options = true;
            continue;
        }

        if let Some(ref mut entry) = current_option {
            if compiled.options_allow_continuation
                && (raw.starts_with(' ') || raw.starts_with('\t'))
            {
                if !entry.desc.is_empty() {
                    entry.desc.push(' ');
                }
                entry.desc.push_str(stripped);
                continue;
            }
        }

        if seen_options && matches_any(&compiled.options_heading_rules, stripped) {
            if let Some(entry) = current_option.take() {
                options.push(OptionItem::Option(entry));
            }
            options.push(OptionItem::Heading(
                stripped.trim_end_matches(':').to_string(),
            ));
            continue;
        }

        if in_notes_section || (compiled.notes_capture_after_options && seen_options) {
            if !is_usage_line(stripped, compiled) && !is_option_line(stripped, compiled) {
                notes.push(stripped.to_string());
            }
            continue;
        }

        if !seen_options {
            if !is_usage_line(stripped, compiled) && !is_option_line(stripped, compiled) {
                description_fallback.push(stripped.to_string());
            }
        }
    }

    if let Some(entry) = current_option.take() {
        options.push(OptionItem::Option(entry));
    }

    HelpSections {
        description_fallback,
        options,
        exit_status,
        notes,
    }
}

fn capture_description_blocks(help_text: &str, compiled: &CompiledSemantics) -> Vec<String> {
    if compiled.description_capture_blocks.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut active: Option<&CompiledDescriptionBlock> = None;

    for raw in help_text.lines() {
        let trimmed = raw.trim_end();
        let stripped = trimmed.trim_start();
        if stripped.is_empty() {
            if let Some(block) = active.take() {
                if block.include_end {
                    lines.push(String::new());
                }
            }
            continue;
        }

        if let Some(block) = active {
            let end_hit = match block.end.as_ref() {
                Some(end) => end.is_match(stripped),
                None => false,
            };
            if end_hit {
                if block.include_end {
                    lines.push(stripped.to_string());
                }
                active = None;
                continue;
            }
            lines.push(stripped.to_string());
            continue;
        }

        for block in &compiled.description_capture_blocks {
            if block.start.is_match(stripped) {
                active = Some(block);
                if block.include_start {
                    lines.push(stripped.to_string());
                }
                break;
            }
        }
    }

    lines
}

fn capture_description_section(help_text: &str, compiled: &CompiledSemantics) -> Vec<String> {
    if compiled.description_section_headers.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut capture = false;

    for raw in help_text.lines() {
        let stripped = raw.trim_end().trim_start();
        if stripped.is_empty() {
            if capture {
                break;
            }
            continue;
        }
        if !capture && matches_any(&compiled.description_section_headers, stripped) {
            capture = true;
            continue;
        }
        if capture {
            if matches_any(&compiled.exit_status_section_headers, stripped)
                || matches_any(&compiled.options_section_headers, stripped)
                || matches_any(&compiled.notes_section_headers, stripped)
            {
                break;
            }
            lines.push(stripped.to_string());
        }
    }
    lines
}

fn select_description(
    help_text: &str,
    fallback: &[String],
    compiled: &CompiledSemantics,
    binary_name: &str,
) -> Vec<String> {
    let mut lines = capture_description_blocks(help_text, compiled);
    if lines.is_empty() {
        lines = match compiled.description_fallback {
            semantics::DescriptionFallback::Leading => fallback.to_vec(),
            semantics::DescriptionFallback::Section => {
                capture_description_section(help_text, compiled)
            }
            semantics::DescriptionFallback::None => Vec::new(),
        };
    }

    lines
        .into_iter()
        .filter(|line| is_description_line(line, binary_name, compiled))
        .collect()
}

fn name_description(
    description_lines: &[String],
    fallback: &[String],
    help_text: &str,
    binary_name: &str,
    compiled: &CompiledSemantics,
) -> Option<String> {
    for line in description_lines.iter().chain(fallback.iter()) {
        let trimmed = line.trim();
        if is_description_line(trimmed, binary_name, compiled) {
            return Some(trimmed.to_string());
        }
    }

    for raw in help_text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_usage_line(trimmed, compiled)
            || is_option_line(trimmed, compiled)
            || is_boilerplate_line(trimmed, binary_name, compiled)
        {
            continue;
        }
        return Some(trimmed.to_string());
    }
    None
}

fn is_usage_line(line: &str, compiled: &CompiledSemantics) -> bool {
    compiled
        .usage_line_rules
        .iter()
        .any(|rule| rule.regex.is_match(line))
}

fn is_option_line(line: &str, compiled: &CompiledSemantics) -> bool {
    compiled
        .options_entry_rules
        .iter()
        .any(|rule| rule.regex.is_match(line))
}

fn is_boilerplate_line(line: &str, binary_name: &str, compiled: &CompiledSemantics) -> bool {
    if compiled.boilerplate_exclude_binary_name && line == binary_name {
        return true;
    }
    matches_any(&compiled.boilerplate_exclude_lines, line)
}

fn is_description_line(line: &str, binary_name: &str, compiled: &CompiledSemantics) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    if is_usage_line(trimmed, compiled) || is_option_line(trimmed, compiled) {
        return false;
    }
    if is_boilerplate_line(trimmed, binary_name, compiled) {
        return false;
    }
    true
}

fn filter_notes(
    lines: &[String],
    description_lines: &[String],
    binary_name: &str,
    compiled: &CompiledSemantics,
) -> Vec<String> {
    let mut filtered = Vec::new();
    let mut description = BTreeSet::new();
    for line in description_lines {
        description.insert(line.trim().to_string());
    }

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_usage_line(trimmed, compiled) || is_option_line(trimmed, compiled) {
            continue;
        }
        if is_boilerplate_line(trimmed, binary_name, compiled) {
            continue;
        }
        if description.contains(trimmed) {
            continue;
        }
        filtered.push(trimmed.to_string());
    }
    filtered
}

fn extract_env_vars(text: &str, compiled: &CompiledSemantics) -> Vec<String> {
    let mut vars = BTreeSet::new();
    let Some(regex) = compiled.env_variable_regex.as_ref() else {
        return Vec::new();
    };

    let mut paragraph = String::new();
    let flush_paragraph =
        |para: &str, vars: &mut BTreeSet<String>, compiled: &CompiledSemantics| {
            let normalized = para.split_whitespace().collect::<Vec<_>>().join(" ");
            if !matches_any(&compiled.env_paragraph_matchers, &normalized) {
                return;
            }
            for caps in regex.find_iter(para) {
                let token = caps.as_str();
                if !token.is_empty() {
                    vars.insert(token.to_string());
                }
            }
        };

    for line in text.lines() {
        if line.trim().is_empty() {
            if !paragraph.trim().is_empty() {
                flush_paragraph(&paragraph, &mut vars, compiled);
                paragraph.clear();
            }
            continue;
        }
        if !paragraph.is_empty() {
            paragraph.push('\n');
        }
        paragraph.push_str(line);
    }
    if !paragraph.trim().is_empty() {
        flush_paragraph(&paragraph, &mut vars, compiled);
    }

    vars.into_iter().collect()
}

fn extract_see_also(text: &str, compiled: &CompiledSemantics) -> Vec<String> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    let lines: Vec<&str> = text.lines().collect();

    for rule in &compiled.see_also_rules {
        let hit = lines.iter().any(|line| rule.when.is_match(line));
        if !hit {
            continue;
        }
        for entry in &rule.entries {
            if seen.insert(entry.clone()) {
                entries.push(entry.clone());
            }
        }
    }

    entries
}

fn split_usage_line(line: &str, binary_name: &str) -> (String, String) {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix(binary_name) {
        let rest = rest.trim();
        if rest.is_empty() {
            return (binary_name.to_string(), String::new());
        }
        return (binary_name.to_string(), rest.to_string());
    }

    let mut parts = trimmed.splitn(2, ' ');
    let cmd = parts.next().unwrap_or(binary_name);
    let rest = parts.next().unwrap_or("").trim();
    (cmd.to_string(), rest.to_string())
}

fn collect_commands(surface: Option<&surface::SurfaceInventory>) -> Vec<CommandEntry> {
    let mut entries = Vec::new();
    let Some(surface) = surface else {
        return entries;
    };
    for item in surface
        .items
        .iter()
        .filter(|item| matches!(item.kind.as_str(), "command" | "subcommand"))
    {
        let name = if !item.display.trim().is_empty() {
            item.display.trim().to_string()
        } else {
            item.id.trim().to_string()
        };
        if name.is_empty() {
            continue;
        }
        let description = item
            .description
            .as_ref()
            .map(|desc| desc.trim().to_string())
            .filter(|desc| !desc.is_empty());
        entries.push(CommandEntry { name, description });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

fn collect_surface_options(surface: Option<&surface::SurfaceInventory>) -> Vec<OptionEntry> {
    let mut entries = Vec::new();
    let Some(surface) = surface else {
        return entries;
    };
    for item in surface.items.iter().filter(|item| item.kind == "option") {
        let names = if !item.display.trim().is_empty() {
            item.display.trim().to_string()
        } else {
            item.id.trim().to_string()
        };
        if names.is_empty() {
            continue;
        }
        let desc = item
            .description
            .as_ref()
            .map(|desc| desc.trim().to_string())
            .unwrap_or_default();
        entries.push(OptionEntry { names, desc });
    }
    entries.sort_by(|a, b| a.names.cmp(&b.names));
    entries
}

fn options_to_roff(items: &[OptionItem]) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            OptionItem::Heading(title) => {
                out.push_str(&format!(".SS {}\n", escape_text(title)));
            }
            OptionItem::Option(opt) => {
                out.push_str(".TP\n");
                out.push_str(&format!(".B {}\n", escape_option(&opt.names)));
                if !opt.desc.is_empty() {
                    out.push_str(&format!("{}\n", escape_text(&opt.desc)));
                }
            }
        }
    }
    out
}

fn paragraphs_to_roff(lines: &[String]) -> String {
    let mut out = String::new();
    let mut current = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            if !current.is_empty() {
                out.push_str(".PP\n");
                out.push_str(&format!("{}\n", escape_text(&current.join(" "))));
                current.clear();
            }
            continue;
        }
        current.push(line.trim().to_string());
    }
    if !current.is_empty() {
        out.push_str(".PP\n");
        out.push_str(&format!("{}\n", escape_text(&current.join(" "))));
    }
    out
}

fn build_render_summary(
    compiled: &CompiledSemantics,
    synopsis_lines: usize,
    description_lines: usize,
    options_entries: usize,
    commands_entries: usize,
    exit_status_lines: usize,
    notes_lines: usize,
    env_vars: usize,
    see_also_entries: usize,
) -> RenderSummary {
    let mut semantics_unmet = Vec::new();
    if synopsis_lines < compiled.requirements.synopsis_min_lines {
        semantics_unmet.push("synopsis_missing".to_string());
    }
    if let Some(min) = compiled.requirements.description_min_lines {
        if description_lines < min {
            semantics_unmet.push("description_missing".to_string());
        }
    }
    if let Some(min) = compiled.requirements.options_min_entries {
        if options_entries < min {
            semantics_unmet.push("options_missing".to_string());
        }
    }
    if let Some(min) = compiled.requirements.commands_min_entries {
        if commands_entries < min {
            semantics_unmet.push("commands_missing".to_string());
        }
    }
    if let Some(min) = compiled.requirements.exit_status_min_lines {
        if exit_status_lines < min {
            semantics_unmet.push("exit_status_missing".to_string());
        }
    }

    RenderSummary {
        schema_version: 1,
        synopsis_lines,
        description_lines,
        options_entries,
        commands_entries,
        exit_status_lines,
        notes_lines,
        env_vars,
        see_also_entries,
        semantics_unmet,
        warnings: Vec::new(),
    }
}

fn escape_text(text: &str) -> String {
    let mut out = String::new();
    let mut iter = text.lines().peekable();
    while let Some(line) = iter.next() {
        let mut line_out = String::new();
        for ch in line.chars() {
            match ch {
                '\\' => line_out.push_str("\\\\"),
                _ => line_out.push(ch),
            }
        }
        if line_out.starts_with('.') || line_out.starts_with('\'') {
            line_out = format!("\\&{}", line_out);
        }
        out.push_str(&line_out);
        if iter.peek().is_some() {
            out.push('\n');
        }
    }
    out
}

fn escape_option(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '-' => out.push_str("\\-"),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    if out.starts_with('.') || out.starts_with('\'') {
        out = format!("\\&{}", out);
    }
    out
}
