use super::super::compile::{capture_with_rule, matches_any, CompiledSemantics};

pub(crate) fn extract_usage_lines(help_text: &str, compiled: &CompiledSemantics) -> Vec<String> {
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

pub(crate) fn select_synopsis_lines(lines: &[String], compiled: &CompiledSemantics) -> Vec<String> {
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
