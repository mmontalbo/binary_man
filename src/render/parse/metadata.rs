use super::super::compile::{matches_any, CompiledSemantics};
use std::collections::BTreeSet;

pub(crate) fn extract_env_vars(text: &str, compiled: &CompiledSemantics) -> Vec<String> {
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

pub(crate) fn extract_see_also(text: &str, compiled: &CompiledSemantics) -> Vec<String> {
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
