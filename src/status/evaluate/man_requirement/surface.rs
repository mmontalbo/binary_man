use crate::surface;

pub(super) fn surface_is_multi_command(surface: &surface::SurfaceInventory) -> bool {
    // Multi-command surfaces have entry points: items whose id is in their context_argv
    surface
        .items
        .iter()
        .any(|item| item.context_argv.last().map(|s| s.as_str()) == Some(item.id.as_str()))
        || surface
            .blockers
            .iter()
            .any(|blocker| blocker.code == "surface_entry_points_missing")
}
