mod description;
mod metadata;
mod sections;
mod surface;
mod usage;

pub(super) use description::{filter_notes, name_description, select_description};
pub(super) use metadata::{extract_env_vars, extract_see_also};
pub(super) use sections::parse_help_text;
pub(super) use surface::{collect_commands, collect_surface_options};
pub(super) use usage::{extract_usage_lines, select_synopsis_lines};
