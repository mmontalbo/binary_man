use super::types::SurfaceInvocation;
use super::{
    is_supported_surface_kind, merge_surface_item, SurfaceDiscovery, SurfaceItem, SurfaceState,
};
use crate::enrich;
use crate::pack;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceLensRow {
    kind: String,
    id: String,
    display: String,
    #[serde(default)]
    description: Option<String>,
    forms: Vec<String>,
    invocation: SurfaceInvocation,
    scenario_path: String,
    multi_command_hint: bool,
}

type ScenarioHit<T> = (T, PathBuf);

struct ScenarioQueryRun<T> {
    hits: Vec<T>,
    ran: bool,
    errors: Vec<String>,
}

fn run_scenario_query<T, F>(
    pack_root: &Path,
    staging_root: &Path,
    template_sql: &str,
    pack_has_scenarios: bool,
    staging_has_scenarios: bool,
    mut run_query: F,
) -> ScenarioQueryRun<T>
where
    F: FnMut(&Path, &str) -> Result<Vec<T>>,
{
    let mut hits = Vec::new();
    let mut ran = false;
    let mut errors = Vec::new();

    if pack_has_scenarios {
        ran = true;
        match run_query(pack_root, template_sql) {
            Ok(mut rows) => hits.append(&mut rows),
            Err(err) => errors.push(err.to_string()),
        }
    }
    if staging_has_scenarios {
        ran = true;
        match run_query(staging_root, template_sql) {
            Ok(mut rows) => hits.append(&mut rows),
            Err(err) => errors.push(err.to_string()),
        }
    }

    ScenarioQueryRun { hits, ran, errors }
}

fn query_status(ran: bool, found: bool, has_errors: bool) -> &'static str {
    if has_errors {
        "error"
    } else if ran && found {
        "used"
    } else if ran {
        "empty"
    } else {
        "skipped"
    }
}

pub(super) fn run_surface_lenses(
    doc_pack_root: &Path,
    staging_root: &Path,
    pack_has_scenarios: bool,
    staging_has_scenarios: bool,
    paths: &enrich::DocPackPaths,
    state: &mut SurfaceState,
) -> Result<()> {
    for template_rel in enrich::SURFACE_LENS_TEMPLATE_RELS {
        let template_path = doc_pack_root.join(template_rel);
        let template_evidence = paths.evidence_from_path(&template_path)?;
        if template_path.is_file() {
            match fs::read_to_string(&template_path) {
                Ok(template_sql) => {
                    let run = run_scenario_query(
                        doc_pack_root,
                        staging_root,
                        &template_sql,
                        pack_has_scenarios,
                        staging_has_scenarios,
                        run_surface_lens_query,
                    );
                    let mut query_errors = run.errors;
                    let mut found_any = false;
                    for (row, source_root) in run.hits {
                        let scenario_path = row.scenario_path.trim();
                        if scenario_path.is_empty()
                            || !scenario_path.starts_with("inventory/scenarios/")
                            || scenario_path.contains("..")
                        {
                            query_errors.push(format!(
                                "lens row has invalid scenario_path {scenario_path:?} (template {template_rel})"
                            ));
                            continue;
                        }
                        let evidence = match enrich::evidence_from_rel(&source_root, scenario_path)
                        {
                            Ok(evidence) => evidence,
                            Err(err) => {
                                query_errors.push(err.to_string());
                                continue;
                            }
                        };
                        if row.multi_command_hint {
                            state.subcommand_hint_evidence.push(evidence.clone());
                            found_any = true;
                        }
                        let kind = row.kind.trim();
                        let id = row.id.trim();
                        if kind.is_empty() || id.is_empty() {
                            continue;
                        }
                        if !is_supported_surface_kind(kind) {
                            query_errors.push(format!(
                                "unsupported surface kind {kind:?} (template {template_rel})"
                            ));
                            continue;
                        }
                        let display_value = row.display.trim();
                        let display = if display_value.is_empty() {
                            id.to_string()
                        } else {
                            display_value.to_string()
                        };
                        let description = row
                            .description
                            .as_deref()
                            .map(str::trim)
                            .filter(|desc| !desc.is_empty())
                            .map(|desc| desc.to_string());
                        let item = SurfaceItem {
                            kind: kind.to_string(),
                            id: id.to_string(),
                            display,
                            description,
                            forms: row.forms.clone(),
                            invocation: row.invocation.clone(),
                            evidence: vec![evidence],
                        };
                        merge_surface_item(&mut state.items, &mut state.seen, item);
                        found_any = true;
                    }
                    let status = query_status(run.ran, found_any, !query_errors.is_empty());
                    state.discovery.push(SurfaceDiscovery {
                        code: template_rel.to_string(),
                        status: status.to_string(),
                        evidence: vec![template_evidence.clone()],
                        message: if query_errors.is_empty() {
                            None
                        } else {
                            Some(query_errors.join("; "))
                        },
                    });
                    if !query_errors.is_empty() {
                        state.blockers.push(enrich::Blocker {
                            code: "surface_lens_query_error".to_string(),
                            message: format!("surface lens query failed ({template_rel})"),
                            evidence: vec![template_evidence.clone()],
                            next_action: Some(format!("fix {template_rel}")),
                        });
                    }
                }
                Err(err) => {
                    state.discovery.push(SurfaceDiscovery {
                        code: template_rel.to_string(),
                        status: "error".to_string(),
                        evidence: vec![template_evidence.clone()],
                        message: Some(err.to_string()),
                    });
                    state.blockers.push(enrich::Blocker {
                        code: "surface_lens_template_read_error".to_string(),
                        message: err.to_string(),
                        evidence: vec![template_evidence.clone()],
                        next_action: Some(format!("fix {template_rel}")),
                    });
                }
            }
        } else {
            state.discovery.push(SurfaceDiscovery {
                code: template_rel.to_string(),
                status: "missing".to_string(),
                evidence: vec![template_evidence.clone()],
                message: Some("surface lens template missing".to_string()),
            });
        }
    }
    Ok(())
}

pub(super) fn add_subcommand_missing_blocker(state: &mut SurfaceState) {
    if state.subcommand_hint_evidence.is_empty() {
        return;
    }
    let has_subcommands = state.items.iter().any(|item| item.kind == "subcommand");
    if has_subcommands {
        return;
    }
    enrich::dedupe_evidence_refs(&mut state.subcommand_hint_evidence);
    state.blockers.push(enrich::Blocker {
        code: "surface_subcommands_missing".to_string(),
        message: "multi-command usage detected but no subcommands extracted".to_string(),
        evidence: std::mem::take(&mut state.subcommand_hint_evidence),
        next_action: Some(
            "add help scenarios in scenarios/plan.json or adjust queries/subcommands_from_scenarios.sql"
                .to_string(),
        ),
    });
}

fn run_surface_lens_query(
    root: &Path,
    template_sql: &str,
) -> Result<Vec<ScenarioHit<SurfaceLensRow>>> {
    let output = pack::run_duckdb_query(template_sql, root)?;
    let rows: Vec<SurfaceLensRow> =
        if output.is_empty() || output.iter().all(|byte| byte.is_ascii_whitespace()) {
            Vec::new()
        } else {
            serde_json::from_slice(&output).context("parse surface lens query output")?
        };
    Ok(rows
        .into_iter()
        .map(|row| (row, root.to_path_buf()))
        .collect())
}
