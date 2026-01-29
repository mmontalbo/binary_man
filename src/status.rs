use crate::enrich;
use crate::scenarios;
use crate::semantics;
use crate::surface;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const TRIAGE_PREVIEW_LIMIT: usize = 10;

#[derive(Deserialize)]
struct RunsIndex {
    #[serde(default)]
    run_count: Option<usize>,
    #[serde(default)]
    runs: Vec<serde_json::Value>,
}

enum SurfaceLoadError {
    Missing,
    Parse(String),
    Invalid(String),
}

struct SurfaceLoadResult {
    evidence: enrich::EvidenceRef,
    surface: Option<surface::SurfaceInventory>,
    error: Option<SurfaceLoadError>,
}

enum ScenarioPlanLoadError {
    Missing,
    Invalid(String),
}

struct ScenarioPlanLoadResult {
    evidence: enrich::EvidenceRef,
    plan: Option<scenarios::ScenarioPlan>,
    error: Option<ScenarioPlanLoadError>,
}

enum SemanticsLoadError {
    Missing,
    Invalid(String),
}

struct SemanticsLoadResult {
    evidence: enrich::EvidenceRef,
    error: Option<SemanticsLoadError>,
}

#[derive(Debug, PartialEq, Eq)]
enum HelpUsageEvidenceState {
    MissingRuns,
    NoUsableHelp,
    UsableHelp,
}

struct EvalResult {
    requirements: Vec<enrich::RequirementStatus>,
    missing_artifacts: Vec<String>,
    blockers: Vec<enrich::Blocker>,
    decision: enrich::Decision,
    decision_reason: Option<String>,
    coverage_next_action: Option<enrich::NextAction>,
    verification_next_action: Option<enrich::NextAction>,
    man_semantics_next_action: Option<enrich::NextAction>,
    man_usage_next_action: Option<enrich::NextAction>,
}

pub fn build_status_summary(
    doc_pack_root: &Path,
    binary_name: Option<&str>,
    config: &enrich::EnrichConfig,
    config_exists: bool,
    lock_status: enrich::LockStatus,
    plan_status: enrich::PlanStatus,
    include_full: bool,
    force_used: bool,
) -> Result<enrich::StatusSummary> {
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    let mut eval = evaluate_requirements(&paths, binary_name, config, &lock_status, include_full)?;
    let mut warnings = Vec::new();
    if !config_exists {
        let config_rel = paths.rel_path(&paths.config_path())?;
        eval.missing_artifacts.push(config_rel);
        warnings.push("enrich/config.json missing".to_string());
    }
    if !paths.scenarios_plan_path().is_file() {
        let plan_rel = paths.rel_path(&paths.scenarios_plan_path())?;
        eval.missing_artifacts.push(plan_rel);
        warnings.push("scenarios/plan.json missing".to_string());
    }

    let scenario_failures = load_scenario_failures(&paths, &mut warnings)?;
    let missing_inputs = config_exists && enrich::resolve_inputs(config, doc_pack_root).is_err();
    let gating_ok = config_exists
        && lock_status.present
        && !lock_status.stale
        && plan_status.present
        && !plan_status.stale;
    let first_unmet = eval
        .requirements
        .iter()
        .find(|req| req.status != enrich::RequirementState::Met)
        .map(|req| req.id.clone());
    let first_unmet_is_scenarios = matches!(
        first_unmet.clone(),
        Some(
            enrich::RequirementId::Coverage
                | enrich::RequirementId::CoverageLedger
                | enrich::RequirementId::Verification
                | enrich::RequirementId::ExamplesReport
        )
    );
    let scenario_failure_next_action =
        if gating_ok && first_unmet_is_scenarios && !scenario_failures.is_empty() {
            let plan_content =
                scenarios::load_plan_if_exists(&paths.scenarios_plan_path(), paths.root())
                    .ok()
                    .flatten()
                    .and_then(|plan| serde_json::to_string_pretty(&plan).ok())
                    .unwrap_or_else(|| scenarios::plan_stub(binary_name));
            Some(enrich::NextAction::Edit {
                path: "scenarios/plan.json".to_string(),
                content: plan_content,
                reason: format!("edit scenario {}", scenario_failures[0].scenario_id),
            })
        } else {
            None
        };
    let next_action = if missing_inputs {
        next_action_for_missing_inputs(&paths, binary_name)
    } else if config_exists && eval.man_semantics_next_action.is_some() {
        eval.man_semantics_next_action.clone().unwrap()
    } else if gating_ok
        && matches!(first_unmet.clone(), Some(enrich::RequirementId::ManPage))
        && eval.man_usage_next_action.is_some()
    {
        eval.man_usage_next_action.clone().unwrap()
    } else if gating_ok
        && matches!(first_unmet.clone(), Some(enrich::RequirementId::Coverage))
        && eval.coverage_next_action.is_some()
    {
        eval.coverage_next_action.clone().unwrap()
    } else if gating_ok
        && matches!(
            first_unmet.clone(),
            Some(enrich::RequirementId::Verification)
        )
        && eval.verification_next_action.is_some()
    {
        eval.verification_next_action.clone().unwrap()
    } else if gating_ok && scenario_failure_next_action.is_some() {
        scenario_failure_next_action.clone().unwrap()
    } else {
        determine_next_action(
            doc_pack_root,
            config_exists,
            &lock_status,
            &plan_status,
            &eval.decision,
            &eval.requirements,
        )
    };
    let man_meta = read_man_meta(&paths);
    let man_warnings = man_meta
        .as_ref()
        .map(|meta| meta.warnings.clone())
        .unwrap_or_default();
    let lens_summary = build_lens_summary(&paths, config, &mut warnings, man_meta.as_ref());

    Ok(enrich::StatusSummary {
        schema_version: 1,
        generated_at_epoch_ms: enrich::now_epoch_ms()?,
        binary_name: binary_name.map(|name| name.to_string()),
        lock: lock_status,
        plan: plan_status.clone(),
        requirements: eval.requirements,
        missing_artifacts: eval.missing_artifacts,
        blockers: eval.blockers,
        scenario_failures,
        decision: eval.decision,
        decision_reason: eval.decision_reason,
        next_action,
        warnings,
        man_warnings,
        lens_summary,
        force_used,
    })
}

#[derive(Deserialize, Default)]
struct ManMeta {
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    usage_lens_source_path: Option<String>,
}

fn read_man_meta(paths: &enrich::DocPackPaths) -> Option<ManMeta> {
    let meta_path = paths.man_dir().join("meta.json");
    if !meta_path.is_file() {
        return None;
    }
    let bytes = std::fs::read(&meta_path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[derive(Deserialize, Default)]
struct HelpScenarioEvidence {
    #[serde(default)]
    scenario_id: String,
    #[serde(default)]
    argv: Vec<String>,
    #[serde(default)]
    timed_out: bool,
    #[serde(default)]
    stdout: String,
}

fn help_usage_evidence_state(paths: &enrich::DocPackPaths) -> HelpUsageEvidenceState {
    let scenarios_dir = paths.inventory_scenarios_dir();
    let Ok(entries) = std::fs::read_dir(&scenarios_dir) else {
        return HelpUsageEvidenceState::MissingRuns;
    };
    let mut saw_any = false;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| !ext.eq_ignore_ascii_case("json"))
            .unwrap_or(true)
        {
            continue;
        }
        saw_any = true;
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(evidence) = serde_json::from_slice::<HelpScenarioEvidence>(&bytes) else {
            continue;
        };
        if !evidence.scenario_id.starts_with("help--") {
            continue;
        }
        if evidence.timed_out || evidence.stdout.is_empty() {
            continue;
        }
        let Some(last) = evidence.argv.last() else {
            continue;
        };
        if last.eq_ignore_ascii_case("--help")
            || last.eq_ignore_ascii_case("--usage")
            || last.eq_ignore_ascii_case("-?")
        {
            return HelpUsageEvidenceState::UsableHelp;
        }
    }

    if saw_any {
        HelpUsageEvidenceState::NoUsableHelp
    } else {
        HelpUsageEvidenceState::MissingRuns
    }
}

fn build_lens_summary(
    paths: &enrich::DocPackPaths,
    config: &enrich::EnrichConfig,
    warnings: &mut Vec<String>,
    man_meta: Option<&ManMeta>,
) -> Vec<enrich::LensSummary> {
    let mut summary = Vec::new();
    let empty_warnings: &[String] = &[];
    let used_template = man_meta.and_then(|meta| meta.usage_lens_source_path.as_deref());
    let usage_warnings = man_meta
        .map(|meta| meta.warnings.as_slice())
        .unwrap_or(empty_warnings);
    let usage_present = man_meta.is_some();
    let usage_failures = usage_lens_failures(usage_warnings);

    for rel in &config.usage_lens_templates {
        let template_path = paths.root().join(rel);
        let evidence = match paths.evidence_from_path(&template_path) {
            Ok(evidence) => vec![evidence],
            Err(err) => {
                warnings.push(err.to_string());
                Vec::new()
            }
        };
        let status = if !template_path.is_file() {
            "error"
        } else if used_template == Some(rel.as_str()) {
            "used"
        } else if usage_failures.contains_key(rel) {
            "error"
        } else {
            "empty"
        };
        let message = if !template_path.is_file() {
            Some("usage lens template missing".to_string())
        } else if let Some(message) = usage_failures.get(rel) {
            Some(message.clone())
        } else if !usage_present {
            Some("man/meta.json missing".to_string())
        } else {
            None
        };
        summary.push(enrich::LensSummary {
            kind: "usage".to_string(),
            template_path: rel.clone(),
            status: status.to_string(),
            evidence,
            message,
        });
    }

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

    for rel in &config.surface_lens_templates {
        let template_path = paths.root().join(rel);
        let fallback_evidence = match paths.evidence_from_path(&template_path) {
            Ok(evidence) => vec![evidence],
            Err(err) => {
                warnings.push(err.to_string());
                Vec::new()
            }
        };
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
            template_path: rel.clone(),
            status,
            evidence,
            message,
        });
    }

    summary
}

fn usage_lens_failures(warnings: &[String]) -> BTreeMap<String, String> {
    let mut failures = BTreeMap::new();
    for warning in warnings {
        let Some(rest) = warning.strip_prefix("usage lens fallback: ") else {
            continue;
        };
        let Some((template, detail)) = rest.split_once(": ") else {
            continue;
        };
        failures.insert(template.to_string(), detail.to_string());
    }
    failures
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

fn preview_ids(ids: &[String]) -> Vec<String> {
    ids.iter().take(TRIAGE_PREVIEW_LIMIT).cloned().collect()
}

fn format_preview(count: usize, preview: &[String]) -> String {
    if count == 0 {
        return String::new();
    }
    if count <= preview.len() {
        return preview.join(", ");
    }
    if preview.is_empty() {
        return format!("{count} ids");
    }
    format!("{count} ids (preview: {})", preview.join(", "))
}

fn load_surface_inventory_state(
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

fn load_scenario_plan_state(
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

fn load_semantics_state(
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

fn evaluate_requirements(
    paths: &enrich::DocPackPaths,
    binary_name: Option<&str>,
    config: &enrich::EnrichConfig,
    lock_status: &enrich::LockStatus,
    include_full: bool,
) -> Result<EvalResult> {
    let mut requirements = Vec::new();
    let mut missing_artifacts = Vec::new();
    let mut blockers = Vec::new();
    let mut coverage_next_action = None;
    let mut verification_next_action = None;
    let mut man_semantics_next_action = None;
    let mut man_usage_next_action = None;

    for req in enrich::normalized_requirements(config) {
        match req {
            enrich::RequirementId::Surface => {
                let surface_state = load_surface_inventory_state(paths, &mut missing_artifacts)?;
                let evidence = surface_state.evidence.clone();
                let surface = match surface_state.error {
                    Some(SurfaceLoadError::Missing) => {
                        requirements.push(enrich::RequirementStatus {
                            id: req.clone(),
                            status: enrich::RequirementState::Unmet,
                            reason: "surface inventory missing".to_string(),
                            unverified_ids: Vec::new(),
                            unverified_count: None,
                            verification: None,
                            evidence: vec![evidence],
                            blockers: Vec::new(),
                        });
                        continue;
                    }
                    Some(SurfaceLoadError::Parse(message)) => {
                        let blocker = enrich::Blocker {
                            code: "surface_parse_error".to_string(),
                            message,
                            evidence: vec![evidence.clone()],
                            next_action: None,
                        };
                        blockers.push(blocker.clone());
                        requirements.push(enrich::RequirementStatus {
                            id: req.clone(),
                            status: enrich::RequirementState::Blocked,
                            reason: "surface inventory parse error".to_string(),
                            unverified_ids: Vec::new(),
                            unverified_count: None,
                            verification: None,
                            evidence: vec![evidence],
                            blockers: vec![blocker],
                        });
                        continue;
                    }
                    Some(SurfaceLoadError::Invalid(message)) => {
                        let blocker = enrich::Blocker {
                            code: "surface_schema_invalid".to_string(),
                            message,
                            evidence: vec![evidence.clone()],
                            next_action: Some("fix inventory/surface.json".to_string()),
                        };
                        blockers.push(blocker.clone());
                        requirements.push(enrich::RequirementStatus {
                            id: req.clone(),
                            status: enrich::RequirementState::Blocked,
                            reason: "surface inventory schema invalid".to_string(),
                            unverified_ids: Vec::new(),
                            unverified_count: None,
                            verification: None,
                            evidence: vec![evidence],
                            blockers: vec![blocker],
                        });
                        continue;
                    }
                    None => surface_state.surface.expect("surface inventory present"),
                };

                let local_blockers = surface.blockers.clone();
                blockers.extend(local_blockers.clone());
                let meaningful_items = surface::meaningful_surface_items(&surface);
                let is_stale = lock_status.present
                    && !lock_status.stale
                    && match (
                        surface.inputs_hash.as_deref(),
                        lock_status.inputs_hash.as_deref(),
                    ) {
                        (Some(surface_hash), Some(lock_hash)) => surface_hash != lock_hash,
                        (None, Some(_)) => true,
                        _ => false,
                    };
                let (status, reason, req_blockers) = if !local_blockers.is_empty() {
                    (
                        enrich::RequirementState::Blocked,
                        "surface blockers present".to_string(),
                        local_blockers,
                    )
                } else if meaningful_items < 1 {
                    (
                        enrich::RequirementState::Unmet,
                        "surface inventory missing items".to_string(),
                        Vec::new(),
                    )
                } else if is_stale {
                    (
                        enrich::RequirementState::Unmet,
                        "surface inventory stale relative to lock".to_string(),
                        Vec::new(),
                    )
                } else {
                    (
                        enrich::RequirementState::Met,
                        "surface inventory present".to_string(),
                        Vec::new(),
                    )
                };
                requirements.push(enrich::RequirementStatus {
                    id: req.clone(),
                    status,
                    reason,
                    unverified_ids: Vec::new(),
                    unverified_count: None,
                    verification: None,
                    evidence: vec![evidence],
                    blockers: req_blockers,
                });
            }
            enrich::RequirementId::Coverage => {
                let SurfaceLoadResult {
                    evidence: surface_evidence,
                    surface,
                    error,
                } = load_surface_inventory_state(paths, &mut missing_artifacts)?;
                let ScenarioPlanLoadResult {
                    evidence: scenarios_evidence,
                    plan,
                    error: plan_error,
                } = load_scenario_plan_state(paths, &mut missing_artifacts)?;
                let mut evidence = vec![surface_evidence.clone(), scenarios_evidence.clone()];
                let mut local_blockers = Vec::new();
                let mut missing = Vec::new();
                let mut uncovered_ids = Vec::new();
                let mut blocked_ids = BTreeSet::new();

                let surface = match error {
                    Some(SurfaceLoadError::Missing) => {
                        missing.push("surface inventory missing".to_string());
                        None
                    }
                    Some(SurfaceLoadError::Parse(message)) => {
                        let blocker = enrich::Blocker {
                            code: "surface_parse_error".to_string(),
                            message,
                            evidence: vec![surface_evidence.clone()],
                            next_action: None,
                        };
                        local_blockers.push(blocker);
                        None
                    }
                    Some(SurfaceLoadError::Invalid(message)) => {
                        let blocker = enrich::Blocker {
                            code: "surface_schema_invalid".to_string(),
                            message,
                            evidence: vec![surface_evidence.clone()],
                            next_action: Some("fix inventory/surface.json".to_string()),
                        };
                        local_blockers.push(blocker);
                        None
                    }
                    None => surface,
                };

                let plan = match plan_error {
                    Some(ScenarioPlanLoadError::Missing) => {
                        missing.push("scenarios plan missing".to_string());
                        None
                    }
                    Some(ScenarioPlanLoadError::Invalid(message)) => {
                        let blocker = enrich::Blocker {
                            code: "scenario_plan_invalid".to_string(),
                            message,
                            evidence: vec![scenarios_evidence.clone()],
                            next_action: Some("fix scenarios/plan.json".to_string()),
                        };
                        local_blockers.push(blocker);
                        None
                    }
                    None => plan,
                };

                if let (Some(surface), Some(plan)) = (surface.as_ref(), plan.as_ref()) {
                    let mut covered = BTreeSet::new();
                    for scenario in &plan.scenarios {
                        if scenario.coverage_ignore || scenario.covers.is_empty() {
                            continue;
                        }
                        for token in &scenario.covers {
                            let normalized = scenarios::normalize_surface_id(token);
                            if !normalized.is_empty() {
                                covered.insert(normalized);
                            }
                        }
                    }

                    if let Some(coverage) = plan.coverage.as_ref() {
                        for blocked in &coverage.blocked {
                            for item_id in &blocked.item_ids {
                                let normalized = scenarios::normalize_surface_id(item_id);
                                if normalized.is_empty() {
                                    continue;
                                }
                                blocked_ids.insert(normalized);
                            }
                        }
                    }

                    let mut surface_evidence_map: BTreeMap<String, Vec<enrich::EvidenceRef>> =
                        BTreeMap::new();
                    for item in surface.items.iter().filter(|item| {
                        matches!(item.kind.as_str(), "option" | "command" | "subcommand")
                    }) {
                        let normalized = scenarios::normalize_surface_id(&item.id);
                        if normalized.is_empty() {
                            continue;
                        }
                        let entry = surface_evidence_map.entry(normalized).or_default();
                        entry.extend(item.evidence.iter().cloned());
                    }

                    for (id, item_evidence) in surface_evidence_map {
                        if covered.contains(&id) || blocked_ids.contains(&id) {
                            continue;
                        }
                        uncovered_ids.push(id);
                        evidence.extend(item_evidence);
                    }

                    uncovered_ids.sort();
                    if !uncovered_ids.is_empty() {
                        if let Some(content) = coverage_stub_from_plan(plan, &uncovered_ids) {
                            coverage_next_action = Some(enrich::NextAction::Edit {
                                path: "scenarios/plan.json".to_string(),
                                content,
                                reason: format!(
                                    "add coverage claim (1 of {}): {}",
                                    uncovered_ids.len(),
                                    uncovered_ids[0]
                                ),
                            });
                        }
                    }
                }

                enrich::dedupe_evidence_refs(&mut evidence);
                let (status, reason) = if !local_blockers.is_empty() {
                    (
                        enrich::RequirementState::Blocked,
                        "coverage inputs blocked".to_string(),
                    )
                } else if !missing.is_empty() {
                    (
                        enrich::RequirementState::Unmet,
                        format!("coverage inputs missing: {}", missing.join("; ")),
                    )
                } else if !uncovered_ids.is_empty() {
                    (
                        enrich::RequirementState::Unmet,
                        format!("uncovered ids: {}", uncovered_ids.join(", ")),
                    )
                } else {
                    let reason = if blocked_ids.is_empty() {
                        "coverage complete".to_string()
                    } else {
                        format!("coverage complete (blocked ids: {})", blocked_ids.len())
                    };
                    (enrich::RequirementState::Met, reason)
                };

                blockers.extend(local_blockers.clone());
                requirements.push(enrich::RequirementStatus {
                    id: req.clone(),
                    status,
                    reason,
                    unverified_ids: Vec::new(),
                    unverified_count: None,
                    verification: None,
                    evidence,
                    blockers: local_blockers,
                });
            }
            enrich::RequirementId::Verification => {
                let SurfaceLoadResult {
                    evidence: surface_evidence,
                    surface,
                    error,
                } = load_surface_inventory_state(paths, &mut missing_artifacts)?;
                let ScenarioPlanLoadResult {
                    evidence: scenarios_evidence,
                    plan,
                    error: plan_error,
                } = load_scenario_plan_state(paths, &mut missing_artifacts)?;
                let template_path = paths
                    .root()
                    .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);
                let template_evidence = paths.evidence_from_path(&template_path)?;
                let semantics_path = paths.semantics_path();
                let semantics_evidence = paths.evidence_from_path(&semantics_path)?;

                let mut evidence = vec![
                    surface_evidence.clone(),
                    scenarios_evidence.clone(),
                    template_evidence.clone(),
                    semantics_evidence.clone(),
                ];
                let mut local_blockers = Vec::new();
                let mut missing = Vec::new();
                let mut unverified_ids = Vec::new();
                let mut triage_summary: Option<enrich::VerificationTriageSummary> = None;
                let verification_tier = config.verification_tier.as_deref().unwrap_or("accepted");
                let tier_label = if verification_tier == "behavior" {
                    "behavior"
                } else {
                    "acceptance"
                };

                let surface = match error {
                    Some(SurfaceLoadError::Missing) => {
                        missing.push("surface inventory missing".to_string());
                        None
                    }
                    Some(SurfaceLoadError::Parse(message)) => {
                        let blocker = enrich::Blocker {
                            code: "surface_parse_error".to_string(),
                            message,
                            evidence: vec![surface_evidence.clone()],
                            next_action: None,
                        };
                        local_blockers.push(blocker);
                        None
                    }
                    Some(SurfaceLoadError::Invalid(message)) => {
                        let blocker = enrich::Blocker {
                            code: "surface_schema_invalid".to_string(),
                            message,
                            evidence: vec![surface_evidence.clone()],
                            next_action: Some("fix inventory/surface.json".to_string()),
                        };
                        local_blockers.push(blocker);
                        None
                    }
                    None => surface,
                };

                let plan = match plan_error {
                    Some(ScenarioPlanLoadError::Missing) => {
                        missing.push("scenarios plan missing".to_string());
                        None
                    }
                    Some(ScenarioPlanLoadError::Invalid(message)) => {
                        let blocker = enrich::Blocker {
                            code: "scenario_plan_invalid".to_string(),
                            message,
                            evidence: vec![scenarios_evidence.clone()],
                            next_action: Some("fix scenarios/plan.json".to_string()),
                        };
                        local_blockers.push(blocker);
                        None
                    }
                    None => plan,
                };

                if !template_path.is_file() {
                    missing_artifacts.push(template_evidence.path.clone());
                    missing.push(format!(
                        "verification lens missing ({})",
                        enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL
                    ));
                }
                if !semantics_path.is_file() {
                    missing_artifacts.push(semantics_evidence.path.clone());
                    missing
                        .push("verification semantics missing (enrich/semantics.json)".to_string());
                }

                if let (Some(surface), Some(plan)) = (surface.as_ref(), plan.as_ref()) {
                    let mut surface_ids = BTreeSet::new();
                    let mut surface_evidence_map: BTreeMap<String, Vec<enrich::EvidenceRef>> =
                        BTreeMap::new();
                    for item in surface.items.iter().filter(|item| {
                        matches!(item.kind.as_str(), "option" | "command" | "subcommand")
                    }) {
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

                    let mut queue_ids = BTreeSet::new();
                    let mut triaged_ids = BTreeSet::new();
                    let mut discovered_untriaged_ids = Vec::new();
                    let mut triaged_unverified_ids = Vec::new();
                    let mut excluded = Vec::new();

                    for entry in &plan.verification.queue {
                        let id = entry.surface_id.trim();
                        if id.is_empty() {
                            continue;
                        }
                        queue_ids.insert(id.to_string());
                        if entry.intent == scenarios::VerificationIntent::Exclude {
                            let reason = entry.reason.as_deref().unwrap_or("").trim();
                            excluded.push(enrich::VerificationExclusion {
                                surface_id: id.to_string(),
                                reason: reason.to_string(),
                            });
                            triaged_ids.insert(id.to_string());
                            continue;
                        }
                        if intent_matches_verification_tier(entry.intent, verification_tier) {
                            triaged_ids.insert(id.to_string());
                        }
                    }

                    for id in surface_ids.iter() {
                        if !triaged_ids.contains(id) {
                            discovered_untriaged_ids.push(id.clone());
                            if let Some(item_evidence) = surface_evidence_map.get(id) {
                                evidence.extend(item_evidence.iter().cloned());
                            }
                        }
                    }
                    discovered_untriaged_ids.sort();

                    let mut missing_surface_ids = Vec::new();
                    for id in queue_ids.iter() {
                        if !surface_ids.contains(id) {
                            missing_surface_ids.push(id.clone());
                        }
                    }
                    if !missing_surface_ids.is_empty() {
                        local_blockers.push(enrich::Blocker {
                            code: "verification_surface_missing".to_string(),
                            message: format!(
                                "verification queue surface_id missing from inventory: {}",
                                missing_surface_ids.join(", ")
                            ),
                            evidence: vec![surface_evidence.clone(), scenarios_evidence.clone()],
                            next_action: Some("fix scenarios/plan.json".to_string()),
                        });
                    }

                    if (plan.verification.queue.is_empty() || !discovered_untriaged_ids.is_empty())
                        && verification_next_action.is_none()
                    {
                        let content = serde_json::to_string_pretty(plan)
                            .unwrap_or_else(|_| scenarios::plan_stub(binary_name));
                        let reason = if plan.verification.queue.is_empty() {
                            "add verification triage in scenarios/plan.json".to_string()
                        } else {
                            format!(
                                "add verification triage for {}",
                                discovered_untriaged_ids[0]
                            )
                        };
                        verification_next_action = Some(enrich::NextAction::Edit {
                            path: "scenarios/plan.json".to_string(),
                            content,
                            reason,
                        });
                    }

                    let mut ledger_entries: BTreeMap<String, scenarios::VerificationEntry> =
                        BTreeMap::new();
                    if template_path.is_file() && semantics_path.is_file() {
                        let verification_binary = binary_name
                            .map(|name| name.to_string())
                            .or_else(|| surface.binary_name.clone())
                            .or_else(|| plan.binary.clone())
                            .unwrap_or_else(|| "<binary>".to_string());
                        match scenarios::build_verification_ledger(
                            &verification_binary,
                            surface,
                            paths.root(),
                            &paths.scenarios_plan_path(),
                            &template_path,
                            None,
                            Some(paths.root()),
                        ) {
                            Ok(ledger) => {
                                for entry in ledger.entries {
                                    ledger_entries.insert(entry.surface_id.clone(), entry);
                                }

                                let mut seen = BTreeSet::new();
                                for entry in &plan.verification.queue {
                                    let surface_id = entry.surface_id.trim();
                                    if surface_id.is_empty() {
                                        continue;
                                    }
                                    if entry.intent == scenarios::VerificationIntent::Exclude {
                                        continue;
                                    }
                                    if !intent_matches_verification_tier(
                                        entry.intent,
                                        verification_tier,
                                    ) {
                                        continue;
                                    }
                                    let (status, _, _) = verification_entry_state(
                                        ledger_entries.get(surface_id),
                                        entry.intent,
                                    );
                                    let is_verified = status == "verified";
                                    if !is_verified && seen.insert(surface_id.to_string()) {
                                        triaged_unverified_ids.push(surface_id.to_string());
                                        unverified_ids.push(surface_id.to_string());
                                        if let Some(entry) = ledger_entries.get(surface_id) {
                                            evidence.extend(entry.evidence.iter().cloned());
                                        }
                                    }
                                }

                                if verification_next_action.is_none()
                                    && !plan.verification.queue.is_empty()
                                    && discovered_untriaged_ids.is_empty()
                                    && local_blockers.is_empty()
                                    && missing.is_empty()
                                {
                                    for entry in plan.verification.queue.iter() {
                                        if entry.intent == scenarios::VerificationIntent::Exclude {
                                            continue;
                                        }
                                        if !intent_matches_verification_tier(
                                            entry.intent,
                                            verification_tier,
                                        ) {
                                            continue;
                                        }
                                        let surface_id = entry.surface_id.trim();
                                        if surface_id.is_empty() {
                                            continue;
                                        }
                                        let (status, scenario_ids, scenario_paths) =
                                            verification_entry_state(
                                                ledger_entries.get(surface_id),
                                                entry.intent,
                                            );
                                        if scenario_ids.is_empty() {
                                            if let Some(content) =
                                                verification_stub_from_queue(plan, entry)
                                            {
                                                verification_next_action =
                                                    Some(enrich::NextAction::Edit {
                                                        path: "scenarios/plan.json".to_string(),
                                                        content,
                                                        reason: format!(
                                                            "add a {} scenario for {surface_id}",
                                                            intent_label(entry.intent)
                                                        ),
                                                    });
                                            }
                                            break;
                                        }
                                        if scenario_paths.is_empty() {
                                            let root = paths.root().display();
                                            verification_next_action =
                                                Some(enrich::NextAction::Command {
                                                    command: format!(
                                                        "bman validate --doc-pack {root} && bman plan --doc-pack {root} && bman apply --doc-pack {root}"
                                                    ),
                                                    reason: format!(
                                                        "run {} verification for {surface_id}",
                                                        intent_label(entry.intent)
                                                    ),
                                                });
                                            break;
                                        }
                                        if status != "verified" {
                                            let content = serde_json::to_string_pretty(plan)
                                                .unwrap_or_else(|_| {
                                                    scenarios::plan_stub(binary_name)
                                                });
                                            verification_next_action =
                                                Some(enrich::NextAction::Edit {
                                                    path: "scenarios/plan.json".to_string(),
                                                    content,
                                                    reason: format!(
                                                        "fix {} scenario for {surface_id}",
                                                        intent_label(entry.intent)
                                                    ),
                                                });
                                            break;
                                        }
                                    }
                                }
                            }
                            Err(err) => {
                                let blocker = enrich::Blocker {
                                    code: "verification_query_error".to_string(),
                                    message: err.to_string(),
                                    evidence: vec![template_evidence.clone()],
                                    next_action: Some(format!(
                                        "fix {}",
                                        enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL
                                    )),
                                };
                                local_blockers.push(blocker);
                            }
                        }
                    }

                    let discovered_preview = preview_ids(&discovered_untriaged_ids);
                    let triaged_preview = preview_ids(&triaged_unverified_ids);
                    let summary = enrich::VerificationTriageSummary {
                        discovered_untriaged_count: discovered_untriaged_ids.len(),
                        discovered_untriaged_preview: discovered_preview,
                        triaged_unverified_count: triaged_unverified_ids.len(),
                        triaged_unverified_preview: triaged_preview,
                        excluded_count: if excluded.is_empty() {
                            None
                        } else {
                            Some(excluded.len())
                        },
                        excluded,
                        discovered_untriaged_ids: include_full
                            .then(|| discovered_untriaged_ids.clone()),
                        triaged_unverified_ids: include_full
                            .then(|| triaged_unverified_ids.clone()),
                    };
                    triage_summary = Some(summary);
                }

                enrich::dedupe_evidence_refs(&mut evidence);
                let (status, reason) = if !local_blockers.is_empty() {
                    (
                        enrich::RequirementState::Blocked,
                        "verification inputs blocked".to_string(),
                    )
                } else if !missing.is_empty() {
                    (
                        enrich::RequirementState::Unmet,
                        format!("verification inputs missing: {}", missing.join("; ")),
                    )
                } else if let Some(summary) = triage_summary.as_ref() {
                    if plan
                        .as_ref()
                        .map(|plan| plan.verification.queue.is_empty())
                        .unwrap_or(false)
                    {
                        (
                            enrich::RequirementState::Unmet,
                            "verification triage missing (queue empty)".to_string(),
                        )
                    } else if summary.discovered_untriaged_count > 0 {
                        let preview = format_preview(
                            summary.discovered_untriaged_count,
                            &summary.discovered_untriaged_preview,
                        );
                        (
                            enrich::RequirementState::Unmet,
                            format!("triage missing for ids: {preview}"),
                        )
                    } else if !unverified_ids.is_empty() {
                        let preview = format_preview(
                            summary.triaged_unverified_count,
                            &summary.triaged_unverified_preview,
                        );
                        (
                            enrich::RequirementState::Unmet,
                            format!("triaged ids unverified ({tier_label}): {preview}"),
                        )
                    } else {
                        (
                            enrich::RequirementState::Met,
                            "verification complete".to_string(),
                        )
                    }
                } else {
                    (
                        enrich::RequirementState::Met,
                        "verification complete".to_string(),
                    )
                };

                blockers.extend(local_blockers.clone());
                requirements.push(enrich::RequirementStatus {
                    id: req.clone(),
                    status,
                    reason,
                    unverified_ids: unverified_ids.clone(),
                    unverified_count: if unverified_ids.is_empty() {
                        None
                    } else {
                        Some(unverified_ids.len())
                    },
                    verification: triage_summary,
                    evidence,
                    blockers: local_blockers,
                });
            }
            enrich::RequirementId::CoverageLedger => {
                let SurfaceLoadResult {
                    evidence: surface_evidence,
                    surface,
                    error,
                } = load_surface_inventory_state(paths, &mut missing_artifacts)?;
                let mut evidence = vec![surface_evidence.clone()];
                let mut local_blockers = Vec::new();
                let mut unmet = Vec::new();

                match error {
                    Some(SurfaceLoadError::Missing) => {
                        unmet.push("surface inventory missing".to_string());
                    }
                    Some(SurfaceLoadError::Parse(message)) => {
                        let blocker = enrich::Blocker {
                            code: "surface_parse_error".to_string(),
                            message,
                            evidence: vec![surface_evidence.clone()],
                            next_action: None,
                        };
                        local_blockers.push(blocker);
                    }
                    Some(SurfaceLoadError::Invalid(message)) => {
                        let blocker = enrich::Blocker {
                            code: "surface_schema_invalid".to_string(),
                            message,
                            evidence: vec![surface_evidence.clone()],
                            next_action: Some("fix inventory/surface.json".to_string()),
                        };
                        local_blockers.push(blocker);
                    }
                    None => {
                        let surface = surface.expect("surface inventory present");
                        if surface::meaningful_surface_items(&surface) < 1 {
                            unmet.push("surface inventory missing items".to_string());
                        }
                    }
                }

                let scenarios_path = paths.scenarios_plan_path();
                let scenarios_evidence = paths.evidence_from_path(&scenarios_path)?;
                evidence.push(scenarios_evidence.clone());
                if !scenarios_path.is_file() {
                    missing_artifacts.push(scenarios_evidence.path.clone());
                    unmet.push("scenarios plan missing".to_string());
                }

                let (status, reason) = if !local_blockers.is_empty() {
                    (
                        enrich::RequirementState::Blocked,
                        "coverage inputs blocked".to_string(),
                    )
                } else if !unmet.is_empty() {
                    (
                        enrich::RequirementState::Unmet,
                        format!("coverage inputs missing: {}", unmet.join("; ")),
                    )
                } else {
                    (
                        enrich::RequirementState::Met,
                        "coverage inputs present".to_string(),
                    )
                };
                blockers.extend(local_blockers.clone());
                requirements.push(enrich::RequirementStatus {
                    id: req.clone(),
                    status,
                    reason,
                    unverified_ids: Vec::new(),
                    unverified_count: None,
                    verification: None,
                    evidence,
                    blockers: local_blockers,
                });
            }
            enrich::RequirementId::ExamplesReport => {
                let runs_index_path = paths.pack_root().join("runs").join("index.json");
                let runs_evidence = paths.evidence_from_path(&runs_index_path)?;
                let mut evidence = vec![runs_evidence.clone()];
                let mut local_blockers = Vec::new();
                let mut unmet = Vec::new();

                let scenarios_path = paths.scenarios_plan_path();
                let scenarios_evidence = paths.evidence_from_path(&scenarios_path)?;
                evidence.push(scenarios_evidence.clone());
                if !scenarios_path.is_file() {
                    missing_artifacts.push(scenarios_evidence.path.clone());
                    unmet.push("scenarios plan missing".to_string());
                }

                let runs_index_bytes = scenarios::read_runs_index_bytes(&paths.pack_root())?;
                if let Some(bytes) = runs_index_bytes {
                    let index: RunsIndex = match serde_json::from_slice(&bytes) {
                        Ok(index) => index,
                        Err(err) => {
                            let blocker = enrich::Blocker {
                                code: "runs_index_parse_error".to_string(),
                                message: err.to_string(),
                                evidence: vec![runs_evidence.clone()],
                                next_action: None,
                            };
                            local_blockers.push(blocker);
                            RunsIndex {
                                run_count: Some(0),
                                runs: Vec::new(),
                            }
                        }
                    };
                    let count = index.run_count.unwrap_or(index.runs.len());
                    if count == 0 {
                        unmet.push("no scenario runs recorded".to_string());
                    }
                } else {
                    missing_artifacts.push(runs_evidence.path.clone());
                    unmet.push("scenario runs index missing".to_string());
                }

                let (status, reason) = if !local_blockers.is_empty() {
                    (
                        enrich::RequirementState::Blocked,
                        "scenario runs blocked".to_string(),
                    )
                } else if !unmet.is_empty() {
                    (
                        enrich::RequirementState::Unmet,
                        format!("scenario runs missing: {}", unmet.join("; ")),
                    )
                } else {
                    (
                        enrich::RequirementState::Met,
                        "scenario runs present".to_string(),
                    )
                };
                blockers.extend(local_blockers.clone());
                requirements.push(enrich::RequirementStatus {
                    id: req.clone(),
                    status,
                    reason,
                    unverified_ids: Vec::new(),
                    unverified_count: None,
                    verification: None,
                    evidence,
                    blockers: local_blockers,
                });
            }
            enrich::RequirementId::ManPage => {
                let manifest_path = paths.pack_manifest_path();
                let binary_name = match binary_name {
                    Some(name) => name,
                    None => {
                        let evidence = paths.evidence_from_path(&manifest_path)?;
                        let blocker = enrich::Blocker {
                            code: "missing_manifest".to_string(),
                            message: "binary name unavailable; manifest missing".to_string(),
                            evidence: vec![evidence.clone()],
                            next_action: None,
                        };
                        blockers.push(blocker.clone());
                        requirements.push(enrich::RequirementStatus {
                            id: req.clone(),
                            status: enrich::RequirementState::Blocked,
                            reason: "manifest missing".to_string(),
                            unverified_ids: Vec::new(),
                            unverified_count: None,
                            verification: None,
                            evidence: vec![evidence],
                            blockers: vec![blocker],
                        });
                        continue;
                    }
                };
                let semantics_state = load_semantics_state(paths, &mut missing_artifacts)?;
                let semantics_evidence = semantics_state.evidence.clone();
                match semantics_state.error {
                    Some(SemanticsLoadError::Missing) => {
                        man_semantics_next_action = Some(enrich::NextAction::Edit {
                            path: "enrich/semantics.json".to_string(),
                            content: semantics::semantics_stub(Some(binary_name)),
                            reason: "semantics missing; add render rules".to_string(),
                        });
                        requirements.push(enrich::RequirementStatus {
                            id: req.clone(),
                            status: enrich::RequirementState::Unmet,
                            reason: "semantics missing".to_string(),
                            unverified_ids: Vec::new(),
                            unverified_count: None,
                            verification: None,
                            evidence: vec![semantics_evidence],
                            blockers: Vec::new(),
                        });
                        continue;
                    }
                    Some(SemanticsLoadError::Invalid(message)) => {
                        man_semantics_next_action = Some(enrich::NextAction::Edit {
                            path: "enrich/semantics.json".to_string(),
                            content: semantics::semantics_stub(Some(binary_name)),
                            reason: format!("fix semantics: {message}"),
                        });
                        requirements.push(enrich::RequirementStatus {
                            id: req.clone(),
                            status: enrich::RequirementState::Unmet,
                            reason: "semantics invalid".to_string(),
                            unverified_ids: Vec::new(),
                            unverified_count: None,
                            verification: None,
                            evidence: vec![semantics_evidence],
                            blockers: Vec::new(),
                        });
                        continue;
                    }
                    None => {}
                }
                let surface_path = paths.surface_path();
                let surface_evidence = paths.evidence_from_path(&surface_path)?;
                let multi_command = if surface_path.is_file() {
                    match surface::load_surface_inventory(&surface_path) {
                        Ok(surface) => {
                            if surface::validate_surface_inventory(&surface).is_ok() {
                                surface_is_multi_command(&surface)
                            } else {
                                false
                            }
                        }
                        Err(_) => false,
                    }
                } else {
                    false
                };
                let man_path = paths.man_page_path(binary_name);
                let evidence = paths.evidence_from_path(&man_path)?;
                if !man_path.is_file() {
                    missing_artifacts.push(evidence.path.clone());
                    if man_usage_next_action.is_none()
                        && help_usage_evidence_state(paths) == HelpUsageEvidenceState::NoUsableHelp
                    {
                        let plan_content = scenarios::load_plan_if_exists(
                            &paths.scenarios_plan_path(),
                            paths.root(),
                        )
                        .ok()
                        .flatten()
                        .and_then(|plan| serde_json::to_string_pretty(&plan).ok())
                        .unwrap_or_else(|| scenarios::plan_stub(Some(binary_name)));
                        man_usage_next_action = Some(enrich::NextAction::Edit {
                            path: "scenarios/plan.json".to_string(),
                            content: plan_content,
                            reason: "help scenarios produced no usable usage text; update help scenarios or semantics"
                                .to_string(),
                        });
                    }
                    let mut evidence = vec![evidence];
                    if multi_command {
                        evidence.push(surface_evidence.clone());
                    }
                    requirements.push(enrich::RequirementStatus {
                        id: req.clone(),
                        status: enrich::RequirementState::Unmet,
                        reason: "man page missing".to_string(),
                        unverified_ids: Vec::new(),
                        unverified_count: None,
                        verification: None,
                        evidence,
                        blockers: Vec::new(),
                    });
                } else {
                    let meta_path = paths.man_dir().join("meta.json");
                    let meta_evidence = paths.evidence_from_path(&meta_path)?;
                    let mut requirement_evidence = vec![
                        evidence.clone(),
                        meta_evidence.clone(),
                        semantics_evidence.clone(),
                    ];
                    if multi_command {
                        requirement_evidence.push(surface_evidence.clone());
                    }
                    #[derive(Deserialize, Default)]
                    struct RenderSummaryMeta {
                        #[serde(default)]
                        semantics_unmet: Vec<String>,
                        #[serde(default)]
                        commands_entries: usize,
                    }
                    #[derive(Deserialize, Default)]
                    struct ManMetaInputs {
                        #[serde(default)]
                        inputs_hash: Option<String>,
                        #[serde(default)]
                        render_summary: Option<RenderSummaryMeta>,
                    }

                    let meta = if meta_path.is_file() {
                        let bytes = std::fs::read(&meta_path)
                            .with_context(|| format!("read {}", meta_path.display()))?;
                        match serde_json::from_slice::<ManMetaInputs>(&bytes) {
                            Ok(meta) => Some(meta),
                            Err(err) => {
                                let blocker = enrich::Blocker {
                                    code: "man_meta_parse_error".to_string(),
                                    message: err.to_string(),
                                    evidence: vec![meta_evidence.clone()],
                                    next_action: Some(format!(
                                        "bman apply --doc-pack {}",
                                        paths.root().display()
                                    )),
                                };
                                blockers.push(blocker.clone());
                                requirements.push(enrich::RequirementStatus {
                                    id: req.clone(),
                                    status: enrich::RequirementState::Blocked,
                                    reason: "man metadata parse error".to_string(),
                                    unverified_ids: Vec::new(),
                                    unverified_count: None,
                                    verification: None,
                                    evidence: requirement_evidence,
                                    blockers: vec![blocker],
                                });
                                continue;
                            }
                        }
                    } else {
                        None
                    };

                    let lock_fresh = lock_status.present && !lock_status.stale;
                    let (mut status, mut reason, mut local_blockers) = match meta.as_ref() {
                        None => {
                            missing_artifacts.push(meta_evidence.path.clone());
                            (
                                enrich::RequirementState::Unmet,
                                "man metadata missing".to_string(),
                                Vec::new(),
                            )
                        }
                        Some(meta) if lock_fresh => {
                            let lock_hash = lock_status.inputs_hash.as_deref();
                            let stale = match (meta.inputs_hash.as_deref(), lock_hash) {
                                (Some(meta_hash), Some(lock_hash)) => meta_hash != lock_hash,
                                (None, Some(_)) => true,
                                _ => false,
                            };
                            if stale {
                                (
                                    enrich::RequirementState::Unmet,
                                    "man outputs stale relative to lock".to_string(),
                                    Vec::new(),
                                )
                            } else {
                                (
                                    enrich::RequirementState::Met,
                                    "man page present".to_string(),
                                    Vec::new(),
                                )
                            }
                        }
                        Some(_) => (
                            enrich::RequirementState::Met,
                            "man page present".to_string(),
                            Vec::new(),
                        ),
                    };

                    if status == enrich::RequirementState::Met && lock_fresh {
                        let render_summary =
                            meta.as_ref().and_then(|meta| meta.render_summary.as_ref());
                        if let Some(summary) = render_summary {
                            if !summary.semantics_unmet.is_empty() {
                                let missing = summary.semantics_unmet.join(", ");
                                man_semantics_next_action = Some(enrich::NextAction::Edit {
                                    path: "enrich/semantics.json".to_string(),
                                    content: semantics::semantics_stub(Some(binary_name)),
                                    reason: format!(
                                        "update semantics (missing extractions: {missing})"
                                    ),
                                });
                                status = enrich::RequirementState::Unmet;
                                reason = format!("rendered but semantics insufficient: {missing}");
                            } else if multi_command && summary.commands_entries == 0 {
                                let blocker = enrich::Blocker {
                                    code: "man_commands_missing".to_string(),
                                    message: "man page missing COMMANDS section for subcommands"
                                        .to_string(),
                                    evidence: requirement_evidence.clone(),
                                    next_action: Some(format!(
                                        "bman apply --doc-pack {}",
                                        paths.root().display()
                                    )),
                                };
                                status = enrich::RequirementState::Blocked;
                                reason = "man page missing COMMANDS section".to_string();
                                local_blockers = vec![blocker];
                            }
                        } else {
                            status = enrich::RequirementState::Unmet;
                            reason = "man render summary missing".to_string();
                        }
                    }
                    blockers.extend(local_blockers.clone());
                    requirements.push(enrich::RequirementStatus {
                        id: req.clone(),
                        status,
                        reason,
                        unverified_ids: Vec::new(),
                        unverified_count: None,
                        verification: None,
                        evidence: requirement_evidence,
                        blockers: local_blockers,
                    });
                }
            }
        }
    }

    let unmet: Vec<String> = requirements
        .iter()
        .filter(|req| req.status != enrich::RequirementState::Met)
        .map(|req| req.id.to_string())
        .collect();
    let decision = if !blockers.is_empty() {
        enrich::Decision::Blocked
    } else if unmet.is_empty() {
        enrich::Decision::Complete
    } else {
        enrich::Decision::Incomplete
    };
    let decision_reason = if !blockers.is_empty() {
        let codes: Vec<String> = blockers.iter().map(|b| b.code.clone()).collect();
        Some(format!("blockers present: {}", codes.join(", ")))
    } else if unmet.is_empty() {
        None
    } else {
        Some(format!("unmet requirements: {}", unmet.join(", ")))
    };

    Ok(EvalResult {
        requirements,
        missing_artifacts,
        blockers,
        decision,
        decision_reason,
        coverage_next_action,
        verification_next_action,
        man_semantics_next_action,
        man_usage_next_action,
    })
}

fn load_scenario_failures(
    paths: &enrich::DocPackPaths,
    warnings: &mut Vec<String>,
) -> Result<Vec<enrich::ScenarioFailure>> {
    let plan = match scenarios::load_plan_if_exists(&paths.scenarios_plan_path(), paths.root()) {
        Ok(Some(plan)) => plan,
        Ok(None) => return Ok(Vec::new()),
        Err(err) => {
            warnings.push(format!("scenario plan error: {err}"));
            return Ok(Vec::new());
        }
    };
    let index_path = paths.inventory_scenarios_dir().join("index.json");
    let index = match scenarios::read_scenario_index(&index_path) {
        Ok(Some(index)) => index,
        Ok(None) => return Ok(Vec::new()),
        Err(err) => {
            warnings.push(format!("scenario index error: {err}"));
            return Ok(Vec::new());
        }
    };

    let plan_ids: BTreeSet<String> = plan.scenarios.iter().map(|s| s.id.clone()).collect();
    let mut failures = Vec::new();

    for entry in &index.scenarios {
        if entry.last_pass != Some(false) {
            continue;
        }
        if !plan_ids.contains(&entry.scenario_id) {
            continue;
        }
        let mut evidence = Vec::new();
        for rel in &entry.evidence_paths {
            let path = paths.root().join(rel);
            match paths.evidence_from_path(&path) {
                Ok(evidence_ref) => evidence.push(evidence_ref),
                Err(err) => warnings.push(err.to_string()),
            }
        }
        enrich::dedupe_evidence_refs(&mut evidence);
        failures.push(enrich::ScenarioFailure {
            scenario_id: entry.scenario_id.clone(),
            failures: entry.failures.clone(),
            evidence,
        });
    }

    failures.sort_by(|a, b| a.scenario_id.cmp(&b.scenario_id));
    Ok(failures)
}

fn determine_next_action(
    doc_pack_root: &Path,
    config_exists: bool,
    lock_status: &enrich::LockStatus,
    plan_status: &enrich::PlanStatus,
    decision: &enrich::Decision,
    requirements: &[enrich::RequirementStatus],
) -> enrich::NextAction {
    if !config_exists {
        let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
        if paths.pack_manifest_path().is_file() {
            return enrich::NextAction::Command {
                command: format!("bman init --doc-pack {}", doc_pack_root.display()),
                reason: "enrich/config.json missing".to_string(),
            };
        }
        let bootstrap_ok = enrich::load_bootstrap_optional(doc_pack_root)
            .ok()
            .and_then(|bootstrap| bootstrap)
            .is_some();
        if bootstrap_ok {
            return enrich::NextAction::Command {
                command: format!("bman init --doc-pack {}", doc_pack_root.display()),
                reason: "enrich/config.json missing".to_string(),
            };
        }
        return enrich::NextAction::Edit {
            path: "enrich/bootstrap.json".to_string(),
            content: enrich::bootstrap_stub(),
            reason: "pack missing; init requires binary; set enrich/bootstrap.json".to_string(),
        };
    }
    if !lock_status.present || lock_status.stale {
        return enrich::NextAction::Command {
            command: format!("bman validate --doc-pack {}", doc_pack_root.display()),
            reason: "lock missing or stale".to_string(),
        };
    }
    if !plan_status.present || plan_status.stale {
        return enrich::NextAction::Command {
            command: format!("bman plan --doc-pack {}", doc_pack_root.display()),
            reason: "plan missing or stale".to_string(),
        };
    }
    if *decision != enrich::Decision::Complete {
        let reason = requirements
            .iter()
            .find(|req| req.status != enrich::RequirementState::Met)
            .map(|req| format!("address {}: {}", req.id, req.reason))
            .unwrap_or_else(|| "apply planned actions".to_string());
        return enrich::NextAction::Command {
            command: format!("bman apply --doc-pack {}", doc_pack_root.display()),
            reason,
        };
    }
    enrich::NextAction::Command {
        command: format!("bman status --doc-pack {}", doc_pack_root.display()),
        reason: "requirements met; recheck when needed".to_string(),
    }
}

pub(crate) fn next_action_for_missing_inputs(
    paths: &enrich::DocPackPaths,
    binary_name: Option<&str>,
) -> enrich::NextAction {
    if !paths.scenarios_plan_path().is_file() {
        return enrich::NextAction::Edit {
            path: "scenarios/plan.json".to_string(),
            content: scenarios::plan_stub(binary_name),
            reason: "scenarios/plan.json missing; create a minimal stub".to_string(),
        };
    }
    if !paths.pack_manifest_path().is_file() {
        return enrich::NextAction::Edit {
            path: "enrich/bootstrap.json".to_string(),
            content: enrich::bootstrap_stub(),
            reason: "pack missing; init requires binary; set enrich/bootstrap.json".to_string(),
        };
    }
    enrich::NextAction::Edit {
        path: "enrich/config.json".to_string(),
        content: enrich::config_stub(),
        reason: "config inputs missing; replace with a minimal stub".to_string(),
    }
}

fn coverage_stub_from_plan(
    plan: &scenarios::ScenarioPlan,
    uncovered_ids: &[String],
) -> Option<String> {
    let target_id = uncovered_ids.first()?.trim();
    if target_id.is_empty() {
        return None;
    }
    let mut updated = plan.clone();
    let stub_id = coverage_stub_id(&updated);
    let argv = if target_id.starts_with('-') {
        vec![target_id.to_string()]
    } else {
        vec![target_id.to_string(), "--help".to_string()]
    };
    updated.scenarios.push(scenarios::ScenarioSpec {
        id: stub_id,
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv,
        env: BTreeMap::new(),
        seed_dir: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("acceptance".to_string()),
        covers: vec![target_id.to_string()],
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect::default(),
    });
    serde_json::to_string_pretty(&updated).ok()
}

fn coverage_stub_id(plan: &scenarios::ScenarioPlan) -> String {
    let base = "coverage-todo";
    if plan.scenarios.iter().all(|scenario| scenario.id != base) {
        return base.to_string();
    }
    let mut idx = 1;
    loop {
        let candidate = format!("{base}-{idx}");
        if plan
            .scenarios
            .iter()
            .all(|scenario| scenario.id != candidate)
        {
            return candidate;
        }
        idx += 1;
    }
}

fn intent_matches_verification_tier(intent: scenarios::VerificationIntent, tier: &str) -> bool {
    match tier {
        "behavior" => intent == scenarios::VerificationIntent::VerifyBehavior,
        _ => matches!(
            intent,
            scenarios::VerificationIntent::VerifyAccepted
                | scenarios::VerificationIntent::VerifyBehavior
        ),
    }
}

fn intent_label(intent: scenarios::VerificationIntent) -> &'static str {
    match intent {
        scenarios::VerificationIntent::VerifyBehavior => "behavior",
        scenarios::VerificationIntent::VerifyAccepted => "acceptance",
        scenarios::VerificationIntent::Exclude => "exclude",
    }
}

fn verification_entry_state(
    entry: Option<&scenarios::VerificationEntry>,
    intent: scenarios::VerificationIntent,
) -> (&str, &[String], &[String]) {
    const EMPTY: &[String] = &[];
    match (entry, intent) {
        (Some(entry), scenarios::VerificationIntent::VerifyBehavior) => (
            entry.behavior_status.as_str(),
            entry.behavior_scenario_ids.as_slice(),
            entry.behavior_scenario_paths.as_slice(),
        ),
        (Some(entry), scenarios::VerificationIntent::VerifyAccepted) => (
            entry.status.as_str(),
            entry.scenario_ids.as_slice(),
            entry.scenario_paths.as_slice(),
        ),
        _ => ("unknown", EMPTY, EMPTY),
    }
}

fn verification_stub_from_queue(
    plan: &scenarios::ScenarioPlan,
    entry: &scenarios::VerificationQueueEntry,
) -> Option<String> {
    let target_id = entry.surface_id.trim();
    if target_id.is_empty() {
        return None;
    }
    let coverage_tier = match entry.intent {
        scenarios::VerificationIntent::VerifyBehavior => Some("behavior".to_string()),
        scenarios::VerificationIntent::VerifyAccepted => Some("acceptance".to_string()),
        scenarios::VerificationIntent::Exclude => return None,
    };
    let argv = if target_id.starts_with('-') {
        vec![target_id.to_string()]
    } else {
        vec![target_id.to_string(), "--help".to_string()]
    };
    let mut updated = plan.clone();
    let stub_id = verification_stub_id(&updated, target_id);
    updated.scenarios.push(scenarios::ScenarioSpec {
        id: stub_id,
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv,
        env: BTreeMap::new(),
        seed_dir: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier,
        covers: vec![target_id.to_string()],
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect::default(),
    });
    serde_json::to_string_pretty(&updated).ok()
}

fn verification_stub_id(plan: &scenarios::ScenarioPlan, surface_id: &str) -> String {
    let sanitized = sanitize_scenario_id(surface_id);
    let base = format!("verify_{sanitized}");
    unique_scenario_id(plan, &base)
}

fn unique_scenario_id(plan: &scenarios::ScenarioPlan, base: &str) -> String {
    if plan.scenarios.iter().all(|scenario| scenario.id != base) {
        return base.to_string();
    }
    let mut idx = 1;
    loop {
        let candidate = format!("{base}-{idx}");
        if plan
            .scenarios
            .iter()
            .all(|scenario| scenario.id != candidate)
        {
            return candidate;
        }
        idx += 1;
    }
}

fn sanitize_scenario_id(surface_id: &str) -> String {
    let trimmed = surface_id.trim();
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let cleaned = out.trim_matches('_');
    if cleaned.is_empty() {
        "id".to_string()
    } else {
        cleaned.to_string()
    }
}

pub fn planned_actions_from_requirements(
    requirements: &[enrich::RequirementStatus],
) -> Vec<enrich::PlannedAction> {
    let mut actions = std::collections::BTreeSet::new();
    for req in requirements {
        if req.status == enrich::RequirementState::Met {
            continue;
        }
        actions.insert(req.id.planned_action());
    }
    actions.into_iter().collect()
}

pub fn plan_status(
    lock: Option<&enrich::EnrichLock>,
    plan: Option<&enrich::EnrichPlan>,
) -> enrich::PlanStatus {
    let lock_inputs_hash = lock.map(|lock| lock.inputs_hash.clone());
    let Some(plan) = plan else {
        return enrich::PlanStatus {
            present: false,
            stale: false,
            inputs_hash: None,
            lock_inputs_hash,
        };
    };
    let stale = match lock {
        Some(lock) => plan.lock.inputs_hash != lock.inputs_hash,
        None => true,
    };
    enrich::PlanStatus {
        present: true,
        stale,
        inputs_hash: Some(plan.lock.inputs_hash.clone()),
        lock_inputs_hash,
    }
}

fn surface_is_multi_command(surface: &surface::SurfaceInventory) -> bool {
    surface
        .items
        .iter()
        .any(|item| matches!(item.kind.as_str(), "command" | "subcommand"))
        || surface
            .blockers
            .iter()
            .any(|blocker| blocker.code == "surface_subcommands_missing")
}

pub fn load_plan(doc_pack_root: &Path) -> Result<enrich::EnrichPlan> {
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.plan_path();
    if !path.is_file() {
        return Err(anyhow!(
            "missing plan at {} (run `bman plan --doc-pack {}` first)",
            path.display(),
            doc_pack_root.display()
        ));
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let plan: enrich::EnrichPlan = serde_json::from_slice(&bytes).context("parse plan JSON")?;
    if plan.schema_version != enrich::PLAN_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported plan schema_version {}",
            plan.schema_version
        ));
    }
    Ok(plan)
}

pub fn write_plan(doc_pack_root: &Path, plan: &enrich::EnrichPlan) -> Result<()> {
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.plan_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create enrich dir")?;
    }
    let text = serde_json::to_string_pretty(plan).context("serialize plan")?;
    std::fs::write(&path, text.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn print_status(doc_pack_root: &Path, summary: &enrich::StatusSummary) {
    println!("doc pack: {}", doc_pack_root.display());
    if let Some(binary) = summary.binary_name.as_ref() {
        println!("binary: {binary}");
    }
    let lock_state = if !summary.lock.present {
        "missing"
    } else if summary.lock.stale {
        "stale"
    } else {
        "fresh"
    };
    println!("lock: {lock_state}");
    println!("decision: {}", summary.decision);
    if let Some(reason) = summary.decision_reason.as_ref() {
        println!("decision detail: {reason}");
    }
    println!("requirements:");
    for req in &summary.requirements {
        println!("  - {}: {} ({})", req.id, req.status, req.reason);
    }
    if !summary.blockers.is_empty() {
        println!("blockers:");
        for blocker in &summary.blockers {
            println!("  - {}: {}", blocker.code, blocker.message);
        }
    }
    if !summary.missing_artifacts.is_empty() {
        println!("missing: {}", summary.missing_artifacts.join(", "));
    }
    match &summary.next_action {
        enrich::NextAction::Command { command, reason } => {
            println!("next: {}", command);
            println!("next detail: {reason}");
        }
        enrich::NextAction::Edit { path, reason, .. } => {
            println!("next edit: {}", path);
            println!("next detail: {reason}");
        }
    }
    if !summary.warnings.is_empty() {
        println!("warnings: {}", summary.warnings.join("; "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::{ScenarioEvidence, ScenarioIndex, ScenarioIndexEntry};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        path.push(format!("{prefix}-{nanos}-{}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn status_includes_scenario_failures_and_next_action() {
        let root = temp_dir("bman-status-failure");
        fs::create_dir_all(root.join("enrich")).unwrap();
        fs::create_dir_all(root.join("scenarios")).unwrap();
        fs::create_dir_all(root.join("inventory").join("scenarios")).unwrap();
        fs::create_dir_all(root.join("binary.lens").join("runs")).unwrap();

        let config = enrich::EnrichConfig {
            schema_version: enrich::CONFIG_SCHEMA_VERSION,
            usage_lens_templates: Vec::new(),
            surface_lens_templates: Vec::new(),
            scenario_catalogs: Vec::new(),
            requirements: vec![enrich::RequirementId::ExamplesReport],
            verification_tier: None,
        };
        enrich::write_config(&root, &config).unwrap();

        let scenario = scenarios::ScenarioSpec {
            id: "fail".to_string(),
            kind: scenarios::ScenarioKind::Behavior,
            publish: false,
            argv: vec!["--help".to_string()],
            env: BTreeMap::new(),
            seed_dir: None,
            seed: None,
            cwd: None,
            timeout_seconds: None,
            net_mode: None,
            no_sandbox: None,
            no_strace: None,
            snippet_max_lines: None,
            snippet_max_bytes: None,
            coverage_tier: None,
            covers: Vec::new(),
            coverage_ignore: true,
            expect: scenarios::ScenarioExpect {
                exit_code: Some(0),
                exit_signal: None,
                stdout_contains_all: Vec::new(),
                stdout_contains_any: Vec::new(),
                stdout_regex_all: Vec::new(),
                stdout_regex_any: Vec::new(),
                stderr_contains_all: Vec::new(),
                stderr_contains_any: Vec::new(),
                stderr_regex_all: Vec::new(),
                stderr_regex_any: Vec::new(),
            },
        };
        let plan = scenarios::ScenarioPlan {
            schema_version: 3,
            binary: None,
            default_env: BTreeMap::new(),
            defaults: None,
            coverage: None,
            verification: scenarios::VerificationPlan::default(),
            scenarios: vec![scenario],
        };
        let plan_text = serde_json::to_string_pretty(&plan).unwrap();
        fs::write(
            root.join("scenarios").join("plan.json"),
            plan_text.as_bytes(),
        )
        .unwrap();

        let evidence = ScenarioEvidence {
            schema_version: 3,
            generated_at_epoch_ms: 1,
            scenario_id: "fail".to_string(),
            argv: vec!["bin".to_string(), "--help".to_string()],
            env: BTreeMap::new(),
            seed_dir: None,
            cwd: None,
            timeout_seconds: None,
            net_mode: None,
            no_sandbox: None,
            no_strace: None,
            snippet_max_lines: 1,
            snippet_max_bytes: 1,
            exit_code: Some(1),
            exit_signal: None,
            timed_out: false,
            duration_ms: 1,
            stdout: String::new(),
            stderr: String::new(),
        };
        let evidence_path = root.join("inventory").join("scenarios").join("fail-1.json");
        fs::write(
            &evidence_path,
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();

        let index = ScenarioIndex {
            schema_version: 1,
            scenarios: vec![ScenarioIndexEntry {
                scenario_id: "fail".to_string(),
                scenario_digest: "abc".to_string(),
                last_run_epoch_ms: Some(1),
                last_pass: Some(false),
                failures: vec!["expected exit_code 0".to_string()],
                evidence_paths: vec!["inventory/scenarios/fail-1.json".to_string()],
            }],
        };
        fs::write(
            root.join("inventory").join("scenarios").join("index.json"),
            serde_json::to_vec_pretty(&index).unwrap(),
        )
        .unwrap();

        fs::write(
            root.join("binary.lens").join("runs").join("index.json"),
            br#"{"run_count":1,"runs":[]}"#,
        )
        .unwrap();

        let summary = build_status_summary(
            &root,
            Some("bin"),
            &config,
            true,
            enrich::LockStatus {
                present: true,
                stale: false,
                inputs_hash: Some("hash".to_string()),
            },
            enrich::PlanStatus {
                present: true,
                stale: false,
                inputs_hash: Some("hash".to_string()),
                lock_inputs_hash: Some("hash".to_string()),
            },
            false,
            false,
        )
        .unwrap();

        assert_eq!(summary.scenario_failures.len(), 1);
        assert_eq!(summary.scenario_failures[0].scenario_id, "fail");
        assert_eq!(
            summary.scenario_failures[0].evidence[0].path,
            "inventory/scenarios/fail-1.json"
        );
        match summary.next_action {
            enrich::NextAction::Command { command, .. } => {
                assert!(command.contains("bman status"));
            }
            _ => panic!("expected command next action"),
        }
        assert_eq!(summary.decision, enrich::Decision::Complete);

        let _ = fs::remove_dir_all(&root);
    }
}
