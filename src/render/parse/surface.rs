use super::super::model::{CommandEntry, OptionEntry};
use crate::surface::{self, SurfaceItem};

/// Check if item is an entry point (context_argv includes its own id).
fn is_entry_point(item: &SurfaceItem) -> bool {
    item.context_argv.last().map(|s| s.as_str()) == Some(item.id.as_str())
}

/// Check if item looks like an option (id starts with -).
fn looks_like_option(item: &SurfaceItem) -> bool {
    item.id.starts_with('-')
}

pub(crate) fn collect_commands(surface: Option<&surface::SurfaceInventory>) -> Vec<CommandEntry> {
    let mut entries = Vec::new();
    let Some(surface) = surface else {
        return entries;
    };
    // Entry points are commands/subcommands
    for item in surface.items.iter().filter(|item| is_entry_point(item)) {
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

pub(crate) fn collect_surface_options(
    surface: Option<&surface::SurfaceInventory>,
) -> Vec<OptionEntry> {
    let mut entries = Vec::new();
    let Some(surface) = surface else {
        return entries;
    };
    // Non-entry-points that look like options
    for item in surface
        .items
        .iter()
        .filter(|item| !is_entry_point(item) && looks_like_option(item))
    {
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
