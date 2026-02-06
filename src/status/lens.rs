use crate::enrich;
use crate::surface;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Deserialize, Default)]
pub(crate) struct ManMeta {
    #[serde(default)]
    pub(crate) warnings: Vec<String>,
    #[serde(default)]
    pub(crate) usage_lens_source_path: Option<String>,
}

pub(crate) fn read_man_meta(paths: &enrich::DocPackPaths) -> Option<ManMeta> {
    let meta_path = paths.man_dir().join("meta.json");
    if !meta_path.is_file() {
        return None;
    }
    let bytes = std::fs::read(&meta_path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn build_lens_summary(
    paths: &enrich::DocPackPaths,
    config: &enrich::EnrichConfig,
    warnings: &mut Vec<String>,
    man_meta: Option<&ManMeta>,
) -> Vec<enrich::LensSummary> {
    let mut summary = Vec::new();
    let used_template = man_meta.and_then(|meta| meta.usage_lens_source_path.as_deref());
    let usage_present = man_meta.is_some();

    let rel = config.usage_lens_template.as_str();
    let template_path = paths.root().join(rel);
    let evidence = lens_evidence(paths, &template_path, warnings);
    let status = if !template_path.is_file() {
        "error"
    } else if used_template == Some(rel) {
        "used"
    } else {
        "empty"
    };
    let message = if !template_path.is_file() {
        Some("usage lens template missing".to_string())
    } else if !usage_present {
        Some("man/meta.json missing".to_string())
    } else {
        None
    };
    summary.push(enrich::LensSummary {
        kind: "usage".to_string(),
        template_path: rel.to_string(),
        status: status.to_string(),
        evidence,
        message,
    });

    let surface_path = paths.surface_path();
    let surface_state = if surface_path.is_file() {
        surface::load_surface_inventory(&surface_path)
            .map(|surface| {
                surface
                    .discovery
                    .into_iter()
                    .map(|entry| (entry.code.clone(), entry))
                    .collect::<BTreeMap<_, _>>()
            })
            .map_err(|err| err.to_string())
    } else {
        Err("surface inventory missing".to_string())
    };

    for rel in enrich::SURFACE_LENS_TEMPLATE_RELS {
        let template_path = paths.root().join(rel);
        let fallback_evidence = lens_evidence(paths, &template_path, warnings);
        let (status, evidence, message) = match surface_state.as_ref() {
            Ok(entries) => match entries.get(rel) {
                Some(entry) => {
                    let normalized = normalize_lens_status(&entry.status);
                    (normalized, entry.evidence.clone(), entry.message.clone())
                }
                None => (
                    "error".to_string(),
                    fallback_evidence,
                    Some("surface lens not found in discovery".to_string()),
                ),
            },
            Err(err) => (
                "error".to_string(),
                fallback_evidence,
                Some(err.to_string()),
            ),
        };
        summary.push(enrich::LensSummary {
            kind: "surface".to_string(),
            template_path: rel.to_string(),
            status,
            evidence,
            message,
        });
    }

    summary
}

fn lens_evidence(
    paths: &enrich::DocPackPaths,
    template_path: &Path,
    warnings: &mut Vec<String>,
) -> Vec<enrich::EvidenceRef> {
    match paths.evidence_from_path(template_path) {
        Ok(evidence) => vec![evidence],
        Err(err) => {
            warnings.push(err.to_string());
            Vec::new()
        }
    }
}

fn normalize_lens_status(raw: &str) -> String {
    match raw {
        "used" => "used",
        "empty" => "empty",
        "error" => "error",
        "missing" => "error",
        "skipped" => "empty",
        _ => "error",
    }
    .to_string()
}
