//! Deterministic man page renderer from usage evidence.

use crate::pack::PackContext;
use crate::scenarios::ExamplesReport;
use std::collections::BTreeSet;

pub fn render_man_page(context: &PackContext, examples_report: Option<&ExamplesReport>) -> String {
    let binary_name = context.manifest.binary_name.as_str();
    let upper = binary_name.to_uppercase();
    let help_text = context.help_text.as_str();

    let sections = parse_help_text(help_text);
    let synopsis_lines = select_synopsis_lines(&sections.usage_lines);
    let description_lines = select_description(help_text, &sections.description);
    let notes_lines = filter_notes(&sections.notes, &description_lines, binary_name);
    let name_desc = name_description(&description_lines, &sections.description, help_text);
    let env_vars = extract_env_vars(help_text);

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

    if !sections.options.is_empty() {
        out.push_str(".SH OPTIONS\n");
        out.push_str(&options_to_roff(&sections.options));
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
                        out.push_str(&format!(".PP\nExit status: {}.\n", code));
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
        for var in env_vars {
            out.push_str(&format!(".TP\n.B {}\n", escape_option(&var)));
        }
    }

    if !notes_lines.is_empty() {
        out.push_str(".SH NOTES\n");
        out.push_str(&paragraphs_to_roff(&notes_lines));
    }
    if help_text.contains("dircolors") {
        out.push_str(".SH SEE ALSO\n");
        out.push_str(".BR dircolors (1)\n");
    }

    out
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
    usage_lines: Vec<String>,
    description: Vec<String>,
    options: Vec<OptionItem>,
    exit_status: Vec<String>,
    notes: Vec<String>,
}

enum OptionItem {
    Heading(String),
    Option(OptionEntry),
}

struct OptionEntry {
    names: String,
    desc: String,
}

fn parse_help_text(help_text: &str) -> HelpSections {
    let mut usage_lines = Vec::new();
    let mut description = Vec::new();
    let mut options = Vec::new();
    let mut exit_status = Vec::new();
    let mut notes = Vec::new();
    let mut current_option: Option<OptionEntry> = None;
    let mut seen_options = false;
    let mut in_exit = false;

    for raw in help_text.lines() {
        let trimmed = raw.trim_end();
        let stripped = trimmed.trim_start();

        if stripped.is_empty() {
            if let Some(entry) = current_option.take() {
                options.push(OptionItem::Option(entry));
            }
            if in_exit {
                break;
            }
            continue;
        }

        if stripped.starts_with("Exit status:") {
            if let Some(entry) = current_option.take() {
                options.push(OptionItem::Option(entry));
            }
            in_exit = true;
            continue;
        }

        if in_exit {
            exit_status.push(stripped.to_string());
            continue;
        }

        if is_usage_line(stripped) {
            usage_lines.push(stripped.to_string());
            continue;
        }

        if is_option_line(stripped) {
            if let Some(entry) = current_option.take() {
                options.push(OptionItem::Option(entry));
            }
            let entry = parse_option_line(stripped);
            current_option = Some(entry);
            seen_options = true;
            continue;
        }

        if let Some(ref mut entry) = current_option {
            if raw.starts_with(' ') || raw.starts_with('\t') {
                if !entry.desc.is_empty() {
                    entry.desc.push(' ');
                }
                entry.desc.push_str(stripped);
                continue;
            }
        }

        if stripped.ends_with(':') && seen_options {
            if let Some(entry) = current_option.take() {
                options.push(OptionItem::Option(entry));
            }
            if stripped.to_lowercase().contains("option") {
                options.push(OptionItem::Heading(
                    stripped.trim_end_matches(':').to_string(),
                ));
            } else {
                notes.push(stripped.to_string());
            }
            continue;
        }

        if seen_options {
            notes.push(stripped.to_string());
        } else {
            description.push(stripped.to_string());
        }
    }

    if let Some(entry) = current_option.take() {
        options.push(OptionItem::Option(entry));
    }

    HelpSections {
        usage_lines,
        description,
        options,
        exit_status,
        notes,
    }
}

fn is_usage_line(line: &str) -> bool {
    line.starts_with("Usage:") || line.starts_with("or:")
}

fn is_option_line(line: &str) -> bool {
    if !line.starts_with('-') {
        return false;
    }
    let mut chars = line.chars();
    chars.next();
    match chars.next() {
        Some(ch) if ch.is_whitespace() => false,
        Some(_) => true,
        None => false,
    }
}

fn parse_option_line(line: &str) -> OptionEntry {
    let mut split_at = None;
    let bytes = line.as_bytes();
    for idx in 0..bytes.len().saturating_sub(1) {
        if bytes[idx] == b' ' && bytes[idx + 1] == b' ' {
            split_at = Some(idx);
            break;
        }
    }

    let (names, desc) = if let Some(idx) = split_at {
        let left = line[..idx].trim();
        let right = line[idx..].trim();
        (left, right)
    } else {
        (line.trim(), "")
    };

    OptionEntry {
        names: names.to_string(),
        desc: desc.to_string(),
    }
}

fn split_usage_line(line: &str, binary_name: &str) -> (String, String) {
    let mut trimmed = line;
    if let Some(rest) = trimmed.strip_prefix("Usage:") {
        trimmed = rest.trim();
    } else if let Some(rest) = trimmed.strip_prefix("or:") {
        trimmed = rest.trim();
    }

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

fn select_synopsis_lines(lines: &[String]) -> Vec<String> {
    let mut preferred = Vec::new();
    for line in lines {
        if line.contains("[FILE]") || line.contains("[FILE]...") || line.contains("FILE...") {
            preferred.push(line.clone());
        }
    }
    if preferred.is_empty() {
        lines.to_vec()
    } else {
        preferred
    }
}

fn select_description(help_text: &str, fallback: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut capture = false;
    for raw in help_text.lines() {
        let line = raw.trim_end();
        if line.contains("List information about the FILEs") {
            capture = true;
        }
        if capture {
            if line.trim().is_empty() {
                break;
            }
            let trimmed = line.trim();
            if is_description_line(trimmed) {
                lines.push(trimmed.to_string());
            }
        }
    }
    if lines.is_empty() {
        fallback
            .iter()
            .filter(|line| is_description_line(line))
            .cloned()
            .collect()
    } else {
        lines
    }
}

fn name_description(
    description_lines: &[String],
    fallback: &[String],
    help_text: &str,
) -> Option<String> {
    for line in description_lines {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    for line in fallback {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    for raw in help_text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_usage_line(trimmed) || is_option_line(trimmed) {
            continue;
        }
        return Some(trimmed.to_string());
    }
    None
}

fn is_description_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    if lower.starts_with("try '") || lower.starts_with("use: '") {
        return false;
    }
    if lower.starts_with("ls online help")
        || lower.starts_with("full documentation")
        || lower.starts_with("report any translation bugs")
    {
        return false;
    }
    if is_usage_line(line) {
        return false;
    }
    if is_option_line(line) {
        return false;
    }
    true
}

fn filter_notes(lines: &[String], description_lines: &[String], binary_name: &str) -> Vec<String> {
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
        let lower = trimmed.to_lowercase();
        if lower.starts_with("try '")
            || lower.starts_with("use: '")
            || lower.starts_with("ls online help")
            || lower.starts_with("full documentation")
            || lower.starts_with("or available locally via")
            || lower.starts_with("report any translation bugs")
            || lower.starts_with("built-in programs")
            || lower.starts_with("execute the program_name")
        {
            continue;
        }
        if trimmed == binary_name {
            continue;
        }
        if description.contains(trimmed) {
            continue;
        }
        if is_usage_line(trimmed) {
            continue;
        }
        filtered.push(trimmed.to_string());
    }
    filtered
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

fn extract_env_vars(text: &str) -> Vec<String> {
    let mut vars = BTreeSet::new();

    let mut paragraph = String::new();
    let flush_paragraph = |para: &str, vars: &mut BTreeSet<String>| {
        let normalized = para
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if !normalized.contains("environment variable") {
            return;
        }
        for token in para.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_') {
            if token.len() < 4 {
                continue;
            }
            if !token
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
            {
                continue;
            }
            if token.contains('_') {
                vars.insert(token.to_string());
            }
        }
    };

    for line in text.lines() {
        if line.trim().is_empty() {
            if !paragraph.trim().is_empty() {
                flush_paragraph(&paragraph, &mut vars);
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
        flush_paragraph(&paragraph, &mut vars);
    }

    vars.into_iter().collect()
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
