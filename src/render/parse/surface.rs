use super::super::model::{CommandEntry, OptionEntry};
use crate::surface;

pub(crate) fn collect_commands(surface: Option<&surface::SurfaceInventory>) -> Vec<CommandEntry> {
    let mut entries = Vec::new();
    let Some(surface) = surface else {
        return entries;
    };
    for item in surface
        .items
        .iter()
        .filter(|item| matches!(item.kind.as_str(), "command" | "subcommand"))
    {
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
    for item in surface.items.iter().filter(|item| item.kind == "option") {
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
