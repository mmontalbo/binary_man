use super::compile::CompiledSemantics;
use super::model::{CommandEntry, OptionItem};
use super::RenderSummary;
use crate::scenarios::ExamplesReport;

pub(super) fn append_header(out: &mut String, upper: &str) {
    out.push_str(&format!(
        ".TH {} 1 \"generated\" \"binary_man\" \"User Commands\"\n",
        upper
    ));
}

pub(super) fn append_name_section(out: &mut String, binary_name: &str, name_desc: Option<&str>) {
    out.push_str(".SH NAME\n");
    if let Some(desc) = name_desc {
        out.push_str(&format!(
            "{} \\- {}\n",
            escape_text(binary_name),
            escape_text(desc)
        ));
    } else {
        out.push_str(&format!("{}\n", escape_text(binary_name)));
    }
}

pub(super) fn append_synopsis_section(
    out: &mut String,
    binary_name: &str,
    synopsis_lines: &[String],
) {
    out.push_str(".SH SYNOPSIS\n");
    if synopsis_lines.is_empty() {
        out.push_str(&format!(".B {}\n", escape_text(binary_name)));
        return;
    }
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

pub(super) fn append_description_section(out: &mut String, description_lines: &[String]) {
    if description_lines.is_empty() {
        return;
    }
    out.push_str(".SH DESCRIPTION\n");
    out.push_str(&paragraphs_to_roff(description_lines));
}

pub(super) fn append_commands_section(out: &mut String, commands: &[CommandEntry]) {
    if commands.is_empty() {
        return;
    }
    out.push_str(".SH COMMANDS\n");
    for cmd in commands {
        out.push_str(".TP\n");
        out.push_str(&format!(".B {}\n", escape_option(&cmd.name)));
        if let Some(desc) = cmd.description.as_ref() {
            out.push_str(&format!("{}\n", escape_text(desc)));
        }
    }
}

pub(super) fn append_options_section(out: &mut String, options_items: &[OptionItem]) {
    if options_items.is_empty() {
        return;
    }
    out.push_str(".SH OPTIONS\n");
    out.push_str(&options_to_roff(options_items));
}

pub(super) fn append_examples_section(out: &mut String, examples_report: Option<&ExamplesReport>) {
    let Some(examples_report) = examples_report else {
        return;
    };
    let passing: Vec<_> = examples_report
        .scenarios
        .iter()
        .filter(|scenario| scenario.pass && scenario.publish)
        .collect();
    if passing.is_empty() {
        return;
    }
    out.push_str(".SH EXAMPLES\n");
    for scenario in passing {
        out.push_str(".PP\n");
        out.push_str(".nf\n");
        append_verbatim_line(out, &format!("$ {}", scenario.command_line));
        append_verbatim_snippet(out, &scenario.stdout_snippet);
        if !scenario.stderr_snippet.trim().is_empty() {
            append_verbatim_line(out, "[stderr]");
            append_verbatim_snippet(out, &scenario.stderr_snippet);
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

pub(super) fn append_exit_status_section(out: &mut String, exit_status: &[String]) {
    if exit_status.is_empty() {
        return;
    }
    out.push_str(".SH EXIT STATUS\n");
    for line in exit_status {
        out.push_str(&format!(".TP\n{}\n", escape_text(line)));
    }
}

pub(super) fn append_env_section(out: &mut String, env_vars: &[String]) {
    if env_vars.is_empty() {
        return;
    }
    out.push_str(".SH ENVIRONMENT\n");
    for var in env_vars {
        out.push_str(&format!(".TP\n.B {}\n", escape_option(var)));
    }
}

pub(super) fn append_notes_section(out: &mut String, notes_lines: &[String]) {
    if notes_lines.is_empty() {
        return;
    }
    out.push_str(".SH NOTES\n");
    out.push_str(&paragraphs_to_roff(notes_lines));
}

pub(super) fn append_see_also_section(out: &mut String, see_also: &[String]) {
    if see_also.is_empty() {
        return;
    }
    out.push_str(".SH SEE ALSO\n");
    for (idx, entry) in see_also.iter().enumerate() {
        if idx > 0 {
            out.push_str(".br\n");
        }
        out.push_str(&format!(".BR {}\n", escape_text(entry)));
    }
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

fn options_to_roff(items: &[OptionItem]) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            OptionItem::Heading(text) => {
                out.push_str(&format!(".SS {}\n", escape_text(text)));
            }
            OptionItem::Option(entry) => {
                out.push_str(".TP\n");
                out.push_str(&format!(".B {}\n", escape_option(&entry.names)));
                if !entry.desc.is_empty() {
                    out.push_str(&format!("{}\n", escape_text(&entry.desc)));
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

pub(super) struct RenderCounts {
    pub(super) synopsis_lines: usize,
    pub(super) description_lines: usize,
    pub(super) options_entries: usize,
    pub(super) commands_entries: usize,
    pub(super) exit_status_lines: usize,
    pub(super) notes_lines: usize,
    pub(super) env_vars: usize,
    pub(super) see_also_entries: usize,
}

pub(super) fn build_render_summary(
    compiled: &CompiledSemantics,
    counts: &RenderCounts,
) -> RenderSummary {
    let mut semantics_unmet = Vec::new();
    if counts.synopsis_lines < compiled.requirements.synopsis_min_lines {
        semantics_unmet.push("synopsis_missing".to_string());
    }
    if let Some(min) = compiled.requirements.description_min_lines {
        if counts.description_lines < min {
            semantics_unmet.push("description_missing".to_string());
        }
    }
    if let Some(min) = compiled.requirements.options_min_entries {
        if counts.options_entries < min {
            semantics_unmet.push("options_missing".to_string());
        }
    }
    if let Some(min) = compiled.requirements.commands_min_entries {
        if counts.commands_entries < min {
            semantics_unmet.push("commands_missing".to_string());
        }
    }
    if let Some(min) = compiled.requirements.exit_status_min_lines {
        if counts.exit_status_lines < min {
            semantics_unmet.push("exit_status_missing".to_string());
        }
    }

    RenderSummary {
        schema_version: 1,
        synopsis_lines: counts.synopsis_lines,
        description_lines: counts.description_lines,
        options_entries: counts.options_entries,
        commands_entries: counts.commands_entries,
        exit_status_lines: counts.exit_status_lines,
        notes_lines: counts.notes_lines,
        env_vars: counts.env_vars,
        see_also_entries: counts.see_also_entries,
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
    out
}
