use super::model::OptionEntry;
use crate::semantics;
use anyhow::{anyhow, Result};
use regex::{Regex, RegexBuilder};

pub(super) struct CompiledSemantics {
    pub(super) usage_line_rules: Vec<CompiledCapture>,
    pub(super) usage_prefer_rules: Vec<Matcher>,
    pub(super) description_capture_blocks: Vec<CompiledDescriptionBlock>,
    pub(super) description_section_headers: Vec<Matcher>,
    pub(super) options_section_headers: Vec<Matcher>,
    pub(super) options_heading_rules: Vec<Matcher>,
    pub(super) options_entry_rules: Vec<CompiledOptionRule>,
    pub(super) exit_status_section_headers: Vec<Matcher>,
    pub(super) exit_status_line_rules: Vec<CompiledCapture>,
    pub(super) notes_section_headers: Vec<Matcher>,
    pub(super) boilerplate_exclude_lines: Vec<Matcher>,
    pub(super) see_also_rules: Vec<CompiledSeeAlsoRule>,
    pub(super) env_paragraph_matchers: Vec<Matcher>,
    pub(super) env_variable_regex: Option<Regex>,
    pub(super) requirements: semantics::RenderRequirements,
    pub(super) options_allow_continuation: bool,
    pub(super) exit_status_stop_on_blank: bool,
    pub(super) notes_capture_after_options: bool,
    pub(super) boilerplate_exclude_binary_name: bool,
    pub(super) description_fallback: semantics::DescriptionFallback,
}

impl CompiledSemantics {
    pub(super) fn new(semantics: &semantics::Semantics) -> Result<Self> {
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

pub(super) struct CompiledCapture {
    pub(super) regex: Regex,
    capture_group: Option<usize>,
}

pub(super) struct CompiledOptionRule {
    pub(super) regex: Regex,
    names_group: Option<usize>,
    desc_group: Option<usize>,
}

pub(super) struct CompiledDescriptionBlock {
    pub(super) start: Matcher,
    pub(super) end: Option<Matcher>,
    pub(super) include_start: bool,
    pub(super) include_end: bool,
}

pub(super) struct CompiledSeeAlsoRule {
    pub(super) when: Matcher,
    pub(super) entries: Vec<String>,
}

pub(super) enum Matcher {
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
    pub(super) fn is_match(&self, text: &str) -> bool {
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
                    text.eq_ignore_ascii_case(value_lower)
                }
            }
            Matcher::Regex(regex) => regex.is_match(text),
        }
    }
}

fn compile_capture_rules(rules: &[semantics::LineCapture]) -> Result<Vec<CompiledCapture>> {
    let mut compiled = Vec::new();
    for rule in rules {
        compiled.push(CompiledCapture {
            regex: compile_regex(&rule.pattern, rule.case_sensitive)?,
            capture_group: rule.capture_group,
        });
    }
    Ok(compiled)
}

fn compile_option_rules(rules: &[semantics::OptionEntryRule]) -> Result<Vec<CompiledOptionRule>> {
    let mut compiled = Vec::new();
    for rule in rules {
        compiled.push(CompiledOptionRule {
            regex: compile_regex(&rule.pattern, rule.case_sensitive)?,
            names_group: rule.names_group,
            desc_group: rule.desc_group,
        });
    }
    Ok(compiled)
}

fn compile_description_blocks(
    rules: &[semantics::DescriptionCaptureBlock],
) -> Result<Vec<CompiledDescriptionBlock>> {
    let mut compiled = Vec::new();
    for rule in rules {
        compiled.push(CompiledDescriptionBlock {
            start: compile_matcher(&rule.start)?,
            end: rule.end.as_ref().map(compile_matcher).transpose()?,
            include_start: rule.include_start,
            include_end: rule.include_end,
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
    let regex = RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|err| anyhow!("invalid regex: {pattern}: {err}"))?;
    Ok(regex)
}

pub(super) fn matches_any(matchers: &[Matcher], text: &str) -> bool {
    matchers.iter().any(|matcher| matcher.is_match(text))
}

pub(super) fn capture_with_rule(rule: &CompiledCapture, text: &str) -> Option<String> {
    let captures = rule.regex.captures(text)?;
    let value = if let Some(group) = rule.capture_group {
        captures.get(group)?.as_str()
    } else {
        captures.get(0)?.as_str()
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(super) fn parse_option_line(line: &str, rules: &[CompiledOptionRule]) -> Option<OptionEntry> {
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
