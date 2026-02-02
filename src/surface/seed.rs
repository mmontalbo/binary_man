use super::types::SurfaceInvocation;
use super::{
    is_supported_surface_kind, merge_surface_item, SurfaceDiscovery, SurfaceItem, SurfaceState,
    SURFACE_SEED_SCHEMA_VERSION,
};
use crate::enrich;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Serialize, Deserialize, Clone)]
struct SurfaceSeed {
    schema_version: u32,
    #[serde(default)]
    items: Vec<SurfaceSeedItem>,
    #[serde(default)]
    overlays: Vec<SurfaceSeedOverlay>,
}

#[derive(Serialize, Deserialize, Clone)]
struct SurfaceSeedItem {
    kind: String,
    id: String,
    #[serde(default)]
    display: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct SurfaceSeedOverlay {
    kind: String,
    id: String,
    #[serde(default)]
    invocation: SurfaceSeedInvocationOverlay,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct SurfaceSeedInvocationOverlay {
    #[serde(default)]
    value_examples: Vec<String>,
    #[serde(default)]
    requires_argv: Vec<String>,
}

pub(super) fn apply_surface_seed(
    paths: &enrich::DocPackPaths,
    state: &mut SurfaceState,
) -> Result<()> {
    let seed_path = paths.surface_seed_path();
    if !seed_path.is_file() {
        return Ok(());
    }
    let evidence = paths.evidence_from_path(&seed_path)?;
    match load_surface_seed(&seed_path) {
        Ok(seed) => {
            state.discovery.push(SurfaceDiscovery {
                code: "seed:surface".to_string(),
                status: "used".to_string(),
                evidence: vec![evidence.clone()],
                message: None,
            });
            let mut has_invalid_items = false;
            for item in seed.items {
                if !is_supported_surface_kind(&item.kind) || item.id.trim().is_empty() {
                    has_invalid_items = true;
                    continue;
                }
                let surface_item = SurfaceItem {
                    kind: item.kind,
                    id: item.id.trim().to_string(),
                    display: item.display.unwrap_or_else(|| item.id.trim().to_string()),
                    description: item.description,
                    forms: Vec::new(),
                    invocation: SurfaceInvocation::default(),
                    evidence: vec![evidence.clone()],
                };
                merge_surface_item(&mut state.items, &mut state.seen, surface_item);
            }
            let mut missing_overlays = Vec::new();
            let mut has_invalid_overlays = false;
            for overlay in seed.overlays {
                if !is_supported_surface_kind(&overlay.kind) || overlay.id.trim().is_empty() {
                    has_invalid_overlays = true;
                    continue;
                }
                let key = format!("{}:{}", overlay.kind, overlay.id.trim());
                if !state.seen.contains_key(&key) {
                    missing_overlays.push(overlay.id.trim().to_string());
                    continue;
                }
                let surface_item = SurfaceItem {
                    kind: overlay.kind,
                    id: overlay.id.trim().to_string(),
                    display: String::new(),
                    description: None,
                    forms: Vec::new(),
                    invocation: SurfaceInvocation {
                        value_examples: overlay.invocation.value_examples,
                        requires_argv: overlay.invocation.requires_argv,
                        ..SurfaceInvocation::default()
                    },
                    evidence: vec![evidence.clone()],
                };
                merge_surface_item(&mut state.items, &mut state.seen, surface_item);
            }
            if has_invalid_items {
                state.blockers.push(enrich::Blocker {
                    code: "surface_seed_items_invalid".to_string(),
                    message: "surface seed contains unsupported items".to_string(),
                    evidence: vec![evidence.clone()],
                    next_action: Some("fix inventory/surface.seed.json".to_string()),
                });
            }
            if has_invalid_overlays {
                state.blockers.push(enrich::Blocker {
                    code: "surface_seed_overlays_invalid".to_string(),
                    message: "surface seed overlays contain unsupported items".to_string(),
                    evidence: vec![evidence.clone()],
                    next_action: Some("fix inventory/surface.seed.json".to_string()),
                });
            }
            if !missing_overlays.is_empty() {
                state.blockers.push(enrich::Blocker {
                    code: "surface_seed_overlays_missing".to_string(),
                    message: format!(
                        "surface seed overlays missing from inventory: {}",
                        missing_overlays.join(", ")
                    ),
                    evidence: vec![evidence],
                    next_action: Some(
                        "fix inventory/surface.json or inventory/surface.seed.json".to_string(),
                    ),
                });
            }
        }
        Err(err) => {
            state.blockers.push(enrich::Blocker {
                code: "surface_seed_parse_error".to_string(),
                message: err.to_string(),
                evidence: vec![evidence],
                next_action: Some("fix inventory/surface.seed.json".to_string()),
            });
        }
    }
    Ok(())
}

fn load_surface_seed(path: &Path) -> Result<SurfaceSeed> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let seed: SurfaceSeed = serde_json::from_slice(&bytes).context("parse surface seed")?;
    if seed.schema_version != SURFACE_SEED_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported surface seed schema_version {}",
            seed.schema_version
        ));
    }
    Ok(seed)
}
