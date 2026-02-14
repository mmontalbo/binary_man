use super::types::SurfaceInvocation;
use super::{
    merge_surface_item, SurfaceDiscovery, SurfaceItem, SurfaceState,
    SURFACE_OVERLAYS_SCHEMA_VERSION,
};
use crate::enrich;
use crate::scenarios::ScenarioSeedSpec;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BehaviorExclusionReasonCode {
    UnsafeSideEffects,
    FixtureGap,
    AssertionGap,
    Nondeterministic,
    RequiresInteractiveTty,
}

impl BehaviorExclusionReasonCode {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            BehaviorExclusionReasonCode::UnsafeSideEffects => "unsafe_side_effects",
            BehaviorExclusionReasonCode::FixtureGap => "fixture_gap",
            BehaviorExclusionReasonCode::AssertionGap => "assertion_gap",
            BehaviorExclusionReasonCode::Nondeterministic => "nondeterministic",
            BehaviorExclusionReasonCode::RequiresInteractiveTty => "requires_interactive_tty",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct BehaviorExclusionEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_variant_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delta_ids: Vec<String>,
}

impl BehaviorExclusionEvidence {
    pub(crate) fn has_reference(&self) -> bool {
        self.delta_variant_path
            .as_deref()
            .is_some_and(|path| !path.trim().is_empty())
            || self.delta_ids.iter().any(|id| !id.trim().is_empty())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct BehaviorExclusion {
    pub reason_code: BehaviorExclusionReasonCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub evidence: BehaviorExclusionEvidence,
}

impl BehaviorExclusion {
    pub(crate) fn validate_shape(&self, surface_id: &str) -> Result<()> {
        if let Some(note) = self.note.as_deref() {
            if note.trim().is_empty() {
                return Err(anyhow!(
                    "behavior_exclusion note must not be empty for {surface_id}"
                ));
            }
            if note.chars().count() > 200 {
                return Err(anyhow!(
                    "behavior_exclusion note must be <= 200 chars for {surface_id}"
                ));
            }
        }
        if !self.evidence.has_reference() {
            return Err(anyhow!(
                "behavior_exclusion evidence requires at least one reference for {surface_id}"
            ));
        }
        if let Some(path) = self.evidence.delta_variant_path.as_deref() {
            if path.trim().is_empty() {
                return Err(anyhow!(
                    "behavior_exclusion evidence.delta_variant_path must not be empty for {surface_id}"
                ));
            }
        }
        for delta_id in &self.evidence.delta_ids {
            if delta_id.trim().is_empty() {
                return Err(anyhow!(
                    "behavior_exclusion evidence.delta_ids entries must not be empty for {surface_id}"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SurfaceBehaviorExclusion {
    pub surface_id: String,
    pub exclusion: BehaviorExclusion,
}

/// User override for inferred prereqs on a surface item.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct PrereqOverride {
    /// If true, exclude this item from auto-verify.
    #[serde(default)]
    pub exclude: bool,
    /// Custom seed to use instead of inferred seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<ScenarioSeedSpec>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SurfaceOverlays {
    schema_version: u32,
    #[serde(default)]
    items: Vec<SurfaceOverlaysItem>,
    #[serde(default)]
    overlays: Vec<SurfaceOverlaysOverlay>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct SurfaceOverlaysItem {
    id: String,
    #[serde(default)]
    display: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    context_argv: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct SurfaceOverlaysOverlay {
    id: String,
    #[serde(default)]
    invocation: SurfaceOverlaysInvocationOverlay,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    behavior_exclusion: Option<BehaviorExclusion>,
    /// LM-authored prereqs for this surface item (references keys in semantics.json.verification.prereqs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    prereqs: Vec<String>,
    /// User override for inferred prereqs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prereq_override: Option<PrereqOverride>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
struct SurfaceOverlaysInvocationOverlay {
    #[serde(default)]
    value_examples: Vec<String>,
    #[serde(default)]
    requires_argv: Vec<String>,
}

pub(super) fn apply_surface_overlays(
    paths: &enrich::DocPackPaths,
    state: &mut SurfaceState,
) -> Result<()> {
    let overlays_path = paths.surface_overlays_path();
    if !overlays_path.is_file() {
        return Ok(());
    }
    let evidence = paths.evidence_from_path(&overlays_path)?;
    match load_surface_overlays(&overlays_path) {
        Ok(overlays) => {
            state.discovery.push(SurfaceDiscovery {
                code: "overlays:surface".to_string(),
                status: "used".to_string(),
                evidence: vec![evidence.clone()],
                message: None,
            });
            for item in overlays.items {
                if item.id.trim().is_empty() {
                    continue;
                }
                let surface_item = SurfaceItem {
                    id: item.id.trim().to_string(),
                    display: item.display.unwrap_or_else(|| item.id.trim().to_string()),
                    description: item.description,
                    parent_id: item.parent_id,
                    context_argv: item.context_argv,
                    forms: Vec::new(),
                    invocation: SurfaceInvocation::default(),
                    evidence: vec![evidence.clone()],
                };
                merge_surface_item(&mut state.items, &mut state.seen, surface_item);
            }
            let mut missing_overlays = Vec::new();
            for overlay in overlays.overlays {
                if overlay.id.trim().is_empty() {
                    continue;
                }
                let key = overlay.id.trim().to_string();
                if !state.seen.contains_key(&key) {
                    missing_overlays.push(overlay.id.trim().to_string());
                    continue;
                }
                let surface_item = SurfaceItem {
                    id: overlay.id.trim().to_string(),
                    display: String::new(),
                    description: None,
                    parent_id: None,
                    context_argv: Vec::new(),
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
            if !missing_overlays.is_empty() {
                state.blockers.push(enrich::Blocker {
                    code: "surface_overlays_missing_targets".to_string(),
                    message: format!(
                        "surface overlays reference unknown items: {:?}",
                        missing_overlays
                    ),
                    evidence: vec![evidence.clone()],
                    next_action: Some("fix inventory/surface.overlays.json".to_string()),
                });
            }
            if !missing_overlays.is_empty() {
                state.blockers.push(enrich::Blocker {
                    code: "surface_overlays_missing".to_string(),
                    message: format!(
                        "surface overlays missing from inventory: {}",
                        missing_overlays.join(", ")
                    ),
                    evidence: vec![evidence],
                    next_action: Some(
                        "fix inventory/surface.json or inventory/surface.overlays.json".to_string(),
                    ),
                });
            }
        }
        Err(err) => {
            state.blockers.push(enrich::Blocker {
                code: "surface_overlays_parse_error".to_string(),
                message: err.to_string(),
                evidence: vec![evidence],
                next_action: Some("fix inventory/surface.overlays.json".to_string()),
            });
        }
    }
    Ok(())
}

fn load_surface_overlays(path: &Path) -> Result<SurfaceOverlays> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let overlays: SurfaceOverlays =
        serde_json::from_slice(&bytes).context("parse surface overlays")?;
    if overlays.schema_version != SURFACE_OVERLAYS_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported surface overlays schema_version {}",
            overlays.schema_version
        ));
    }
    Ok(overlays)
}

pub(crate) fn load_surface_overlays_if_exists(path: &Path) -> Result<Option<SurfaceOverlays>> {
    if !path.is_file() {
        return Ok(None);
    }
    load_surface_overlays(path).map(Some)
}

pub(crate) fn collect_behavior_exclusions(
    overlays: &SurfaceOverlays,
) -> Vec<SurfaceBehaviorExclusion> {
    let mut exclusions = Vec::new();
    for overlay in &overlays.overlays {
        let Some(behavior_exclusion) = overlay.behavior_exclusion.clone() else {
            continue;
        };
        exclusions.push(SurfaceBehaviorExclusion {
            surface_id: overlay.id.trim().to_string(),
            exclusion: behavior_exclusion,
        });
    }
    exclusions
}
