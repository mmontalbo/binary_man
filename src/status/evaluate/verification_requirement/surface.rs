use crate::enrich;
use crate::surface;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn collect_surface_ids(
    surface: &surface::SurfaceInventory,
) -> (BTreeSet<String>, BTreeMap<String, Vec<enrich::EvidenceRef>>) {
    let mut surface_ids = BTreeSet::new();
    let mut surface_evidence_map: BTreeMap<String, Vec<enrich::EvidenceRef>> = BTreeMap::new();
    for item in surface
        .items
        .iter()
        .filter(|item| matches!(item.kind.as_str(), "option" | "command" | "subcommand"))
    {
        let id = item.id.trim();
        if id.is_empty() {
            continue;
        }
        surface_ids.insert(id.to_string());
        surface_evidence_map
            .entry(id.to_string())
            .or_default()
            .extend(item.evidence.iter().cloned());
    }
    (surface_ids, surface_evidence_map)
}
