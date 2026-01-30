use crate::enrich;
use crate::scenarios;
use crate::semantics;
use crate::surface;
use anyhow::{Context, Result};

pub(super) enum SurfaceLoadError {
    Missing,
    Parse(String),
    Invalid(String),
}

pub(super) struct SurfaceLoadResult {
    pub(super) evidence: enrich::EvidenceRef,
    pub(super) surface: Option<surface::SurfaceInventory>,
    pub(super) error: Option<SurfaceLoadError>,
}

pub(super) enum ScenarioPlanLoadError {
    Missing,
    Invalid(String),
}

pub(super) struct ScenarioPlanLoadResult {
    pub(super) evidence: enrich::EvidenceRef,
    pub(super) plan: Option<scenarios::ScenarioPlan>,
    pub(super) error: Option<ScenarioPlanLoadError>,
}

pub(super) enum SemanticsLoadError {
    Missing,
    Invalid(String),
}

pub(super) struct SemanticsLoadResult {
    pub(super) evidence: enrich::EvidenceRef,
    pub(super) error: Option<SemanticsLoadError>,
}

pub(super) fn load_surface_inventory_state(
    paths: &enrich::DocPackPaths,
    missing_artifacts: &mut Vec<String>,
) -> Result<SurfaceLoadResult> {
    let surface_path = paths.surface_path();
    let evidence = paths.evidence_from_path(&surface_path)?;
    if !surface_path.is_file() {
        missing_artifacts.push(evidence.path.clone());
        return Ok(SurfaceLoadResult {
            evidence,
            surface: None,
            error: Some(SurfaceLoadError::Missing),
        });
    }
    let surface = match surface::load_surface_inventory(&surface_path) {
        Ok(surface) => surface,
        Err(err) => {
            return Ok(SurfaceLoadResult {
                evidence,
                surface: None,
                error: Some(SurfaceLoadError::Parse(err.to_string())),
            })
        }
    };
    if let Err(err) = surface::validate_surface_inventory(&surface) {
        return Ok(SurfaceLoadResult {
            evidence,
            surface: None,
            error: Some(SurfaceLoadError::Invalid(err.to_string())),
        });
    }
    Ok(SurfaceLoadResult {
        evidence,
        surface: Some(surface),
        error: None,
    })
}

pub(super) fn load_scenario_plan_state(
    paths: &enrich::DocPackPaths,
    missing_artifacts: &mut Vec<String>,
) -> Result<ScenarioPlanLoadResult> {
    let plan_path = paths.scenarios_plan_path();
    let evidence = paths.evidence_from_path(&plan_path)?;
    match scenarios::load_plan_if_exists(&plan_path, paths.root()) {
        Ok(Some(plan)) => Ok(ScenarioPlanLoadResult {
            evidence,
            plan: Some(plan),
            error: None,
        }),
        Ok(None) => {
            missing_artifacts.push(evidence.path.clone());
            Ok(ScenarioPlanLoadResult {
                evidence,
                plan: None,
                error: Some(ScenarioPlanLoadError::Missing),
            })
        }
        Err(err) => Ok(ScenarioPlanLoadResult {
            evidence,
            plan: None,
            error: Some(ScenarioPlanLoadError::Invalid(err.to_string())),
        }),
    }
}

pub(super) fn load_semantics_state(
    paths: &enrich::DocPackPaths,
    missing_artifacts: &mut Vec<String>,
) -> Result<SemanticsLoadResult> {
    let semantics_path = paths.semantics_path();
    let evidence = paths.evidence_from_path(&semantics_path)?;
    if !semantics_path.is_file() {
        missing_artifacts.push(evidence.path.clone());
        return Ok(SemanticsLoadResult {
            evidence,
            error: Some(SemanticsLoadError::Missing),
        });
    }
    let bytes = std::fs::read(&semantics_path)
        .with_context(|| format!("read {}", semantics_path.display()))?;
    let semantics = match serde_json::from_slice::<semantics::Semantics>(&bytes) {
        Ok(semantics) => semantics,
        Err(err) => {
            return Ok(SemanticsLoadResult {
                evidence,
                error: Some(SemanticsLoadError::Invalid(err.to_string())),
            })
        }
    };
    if let Err(err) = semantics::validate_semantics(&semantics) {
        return Ok(SemanticsLoadResult {
            evidence,
            error: Some(SemanticsLoadError::Invalid(err.to_string())),
        });
    }
    Ok(SemanticsLoadResult {
        evidence,
        error: None,
    })
}
