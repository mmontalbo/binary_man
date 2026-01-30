use super::super::compile::{matches_any, CompiledDescriptionBlock, CompiledSemantics};
use crate::semantics;
use std::collections::BTreeSet;

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

pub(crate) fn select_description(
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

pub(crate) fn name_description(
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

pub(crate) fn filter_notes(
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

pub(super) fn is_usage_line(line: &str, compiled: &CompiledSemantics) -> bool {
    compiled
        .usage_line_rules
        .iter()
        .any(|rule| rule.regex.is_match(line))
}

pub(super) fn is_option_line(line: &str, compiled: &CompiledSemantics) -> bool {
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
