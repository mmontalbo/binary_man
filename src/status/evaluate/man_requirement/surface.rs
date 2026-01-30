use crate::surface;

pub(super) fn surface_is_multi_command(surface: &surface::SurfaceInventory) -> bool {
    surface
        .items
        .iter()
        .any(|item| matches!(item.kind.as_str(), "command" | "subcommand"))
        || surface
            .blockers
            .iter()
            .any(|blocker| blocker.code == "surface_subcommands_missing")
}
