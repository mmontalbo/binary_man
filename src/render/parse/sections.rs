use super::super::compile::{capture_with_rule, matches_any, parse_option_line, CompiledSemantics};
use super::super::model::{HelpSections, OptionEntry, OptionItem};
use super::description::{is_option_line, is_usage_line};

#[derive(Default)]
struct ParseState {
    description_fallback: Vec<String>,
    options: Vec<OptionItem>,
    exit_status: Vec<String>,
    notes: Vec<String>,
    current_option: Option<OptionEntry>,
    seen_options: bool,
    in_exit: bool,
    in_notes_section: bool,
}

impl ParseState {
    fn flush_option(&mut self) {
        if let Some(entry) = self.current_option.take() {
            self.options.push(OptionItem::Option(entry));
        }
    }

    fn finish(mut self) -> HelpSections {
        self.flush_option();
        HelpSections {
            description_fallback: self.description_fallback,
            options: self.options,
            exit_status: self.exit_status,
            notes: self.notes,
        }
    }
}

pub(crate) fn parse_help_text(help_text: &str, compiled: &CompiledSemantics) -> HelpSections {
    let mut state = ParseState::default();
    for raw in help_text.lines() {
        let trimmed = raw.trim_end();
        let stripped = trimmed.trim_start();

        if handle_blank_line(stripped, &mut state, compiled) {
            continue;
        }

        if handle_exit_header(stripped, &mut state, compiled) {
            continue;
        }

        if handle_options_header(stripped, &mut state, compiled) {
            continue;
        }

        if handle_notes_header(stripped, &mut state, compiled) {
            continue;
        }

        if handle_exit_line(stripped, &mut state, compiled) {
            continue;
        }

        if handle_option_line(stripped, &mut state, compiled) {
            continue;
        }

        if handle_option_continuation(raw, stripped, &mut state, compiled) {
            continue;
        }

        if handle_option_heading(stripped, &mut state, compiled) {
            continue;
        }

        if handle_notes_line(stripped, &mut state, compiled) {
            continue;
        }

        handle_description_fallback(stripped, &mut state, compiled);
    }

    state.finish()
}

fn handle_blank_line(stripped: &str, state: &mut ParseState, compiled: &CompiledSemantics) -> bool {
    if !stripped.is_empty() {
        return false;
    }
    state.flush_option();
    if state.in_exit && compiled.exit_status_stop_on_blank {
        state.in_exit = false;
    }
    true
}

fn handle_exit_header(
    stripped: &str,
    state: &mut ParseState,
    compiled: &CompiledSemantics,
) -> bool {
    if !matches_any(&compiled.exit_status_section_headers, stripped) {
        return false;
    }
    state.flush_option();
    state.in_exit = true;
    state.in_notes_section = false;
    true
}

fn handle_options_header(
    stripped: &str,
    state: &mut ParseState,
    compiled: &CompiledSemantics,
) -> bool {
    if !matches_any(&compiled.options_section_headers, stripped) {
        return false;
    }
    state.flush_option();
    state.seen_options = true;
    state.in_notes_section = false;
    if matches_any(&compiled.options_heading_rules, stripped) {
        state.options.push(OptionItem::Heading(
            stripped.trim_end_matches(':').to_string(),
        ));
    }
    true
}

fn handle_notes_header(
    stripped: &str,
    state: &mut ParseState,
    compiled: &CompiledSemantics,
) -> bool {
    if !matches_any(&compiled.notes_section_headers, stripped) {
        return false;
    }
    state.flush_option();
    state.in_notes_section = true;
    state.in_exit = false;
    state.notes.push(stripped.to_string());
    true
}

fn handle_exit_line(stripped: &str, state: &mut ParseState, compiled: &CompiledSemantics) -> bool {
    if !state.in_exit {
        return false;
    }
    if compiled.exit_status_line_rules.is_empty() {
        state.exit_status.push(stripped.to_string());
    } else {
        for rule in &compiled.exit_status_line_rules {
            if let Some(value) = capture_with_rule(rule, stripped) {
                state.exit_status.push(value);
                break;
            }
        }
    }
    true
}

fn handle_option_line(
    stripped: &str,
    state: &mut ParseState,
    compiled: &CompiledSemantics,
) -> bool {
    let Some(entry) = parse_option_line(stripped, &compiled.options_entry_rules) else {
        return false;
    };
    state.flush_option();
    state.current_option = Some(entry);
    state.seen_options = true;
    true
}

fn handle_option_continuation(
    raw: &str,
    stripped: &str,
    state: &mut ParseState,
    compiled: &CompiledSemantics,
) -> bool {
    let Some(ref mut entry) = state.current_option else {
        return false;
    };
    if !compiled.options_allow_continuation || !(raw.starts_with(' ') || raw.starts_with('\t')) {
        return false;
    }
    if !entry.desc.is_empty() {
        entry.desc.push(' ');
    }
    entry.desc.push_str(stripped);
    true
}

fn handle_option_heading(
    stripped: &str,
    state: &mut ParseState,
    compiled: &CompiledSemantics,
) -> bool {
    if !state.seen_options || !matches_any(&compiled.options_heading_rules, stripped) {
        return false;
    }
    state.flush_option();
    state.options.push(OptionItem::Heading(
        stripped.trim_end_matches(':').to_string(),
    ));
    true
}

fn handle_notes_line(stripped: &str, state: &mut ParseState, compiled: &CompiledSemantics) -> bool {
    if !(state.in_notes_section || (compiled.notes_capture_after_options && state.seen_options)) {
        return false;
    }
    if !is_usage_line(stripped, compiled) && !is_option_line(stripped, compiled) {
        state.notes.push(stripped.to_string());
    }
    true
}

fn handle_description_fallback(
    stripped: &str,
    state: &mut ParseState,
    compiled: &CompiledSemantics,
) {
    if !state.seen_options
        && !is_usage_line(stripped, compiled)
        && !is_option_line(stripped, compiled)
    {
        state.description_fallback.push(stripped.to_string());
    }
}
