//! Deterministic man page renderer from usage evidence.
//!
//! Rendering stays schema-driven: usage, surface, and semantics are treated as
//! inputs so the output remains reproducible and pack-owned.

use crate::pack::PackContext;
use crate::scenarios::ExamplesReport;
use crate::semantics;
use crate::surface;
use anyhow::Result;
use serde::Serialize;

mod compile;
mod format;
mod model;
mod parse;

use compile::CompiledSemantics;
use format::{
    append_commands_section, append_description_section, append_env_section,
    append_examples_section, append_exit_status_section, append_header, append_name_section,
    append_notes_section, append_options_section, append_see_also_section, append_synopsis_section,
    build_render_summary, RenderCounts,
};
use model::OptionItem;
use parse::{
    collect_commands, collect_surface_options, extract_env_vars, extract_see_also,
    extract_usage_lines, filter_notes, name_description, parse_help_text, select_description,
    select_synopsis_lines,
};

/// Rendering summary used for status and diagnostics.
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

/// Rendered man page content plus a summary of extraction quality.
pub struct RenderedManPage {
    pub man_page: String,
    pub summary: RenderSummary,
}

/// Render a man page using usage evidence, semantics, and optional surface data.
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
    append_header(&mut out, &upper);
    append_name_section(&mut out, binary_name, name_desc.as_deref());
    append_synopsis_section(&mut out, binary_name, &synopsis_lines);
    append_description_section(&mut out, &description_lines);
    append_commands_section(&mut out, &commands);
    append_options_section(&mut out, &options_items);
    append_examples_section(&mut out, examples_report);
    append_exit_status_section(&mut out, &sections.exit_status);
    append_env_section(&mut out, &env_vars);
    append_notes_section(&mut out, &notes_lines);
    append_see_also_section(&mut out, &see_also);

    let options_count = options_items
        .iter()
        .filter(|item| matches!(item, OptionItem::Option(_)))
        .count();
    let counts = RenderCounts {
        synopsis_lines: synopsis_lines.len(),
        description_lines: description_lines.len(),
        options_entries: options_count,
        commands_entries: commands.len(),
        exit_status_lines: sections.exit_status.len(),
        notes_lines: notes_lines.len(),
        env_vars: env_vars.len(),
        see_also_entries: see_also.len(),
    };
    let summary = build_render_summary(&compiled, &counts);

    Ok(RenderedManPage {
        man_page: out,
        summary,
    })
}
