mod ledger;
mod surface;

use super::super::inputs::{
    load_scenario_plan_state, load_surface_inventory_state, ScenarioPlanLoadError,
    ScenarioPlanLoadResult, SurfaceLoadError, SurfaceLoadResult,
};
use super::{format_preview, preview_ids, EvalState};
use crate::enrich;
use crate::scenarios;
use anyhow::Result;
use ledger::build_verification_ledger_entries;
use surface::collect_surface_ids;

type LedgerEntries = std::collections::BTreeMap<String, scenarios::VerificationEntry>;

#[derive(Default)]
struct VerificationEvalOutput {
    triage_summary: Option<enrich::VerificationTriageSummary>,
    unverified_ids: Vec<String>,
}

struct VerificationInputs {
    surface: Option<crate::surface::SurfaceInventory>,
    plan: Option<scenarios::ScenarioPlan>,
    surface_evidence: enrich::EvidenceRef,
    scenarios_evidence: enrich::EvidenceRef,
    template_path: std::path::PathBuf,
    template_evidence: enrich::EvidenceRef,
    semantics_path: std::path::PathBuf,
    semantics_evidence: enrich::EvidenceRef,
}

struct AutoVerificationContext<'a> {
    ledger_entries: Option<&'a LedgerEntries>,
    include_full: bool,
    evidence: &'a mut Vec<enrich::EvidenceRef>,
    verification_next_action: &'a mut Option<enrich::NextAction>,
    local_blockers: &'a [enrich::Blocker],
    missing: &'a [String],
    paths: &'a enrich::DocPackPaths,
}

struct QueueVerificationContext<'a> {
    plan: &'a scenarios::ScenarioPlan,
    surface: &'a crate::surface::SurfaceInventory,
    include_full: bool,
    ledger_entries: Option<&'a LedgerEntries>,
    evidence: &'a mut Vec<enrich::EvidenceRef>,
    local_blockers: &'a mut Vec<enrich::Blocker>,
    verification_next_action: &'a mut Option<enrich::NextAction>,
    missing: &'a [String],
    paths: &'a enrich::DocPackPaths,
    binary_name: Option<&'a str>,
    surface_evidence: &'a enrich::EvidenceRef,
    scenarios_evidence: &'a enrich::EvidenceRef,
}

fn load_verification_inputs(
    paths: &enrich::DocPackPaths,
    missing_artifacts: &mut Vec<String>,
    missing: &mut Vec<String>,
    local_blockers: &mut Vec<enrich::Blocker>,
) -> Result<VerificationInputs> {
    let SurfaceLoadResult {
        evidence: surface_evidence,
        surface,
        error,
    } = load_surface_inventory_state(paths, missing_artifacts)?;
    let ScenarioPlanLoadResult {
        evidence: scenarios_evidence,
        plan,
        error: plan_error,
    } = load_scenario_plan_state(paths, missing_artifacts)?;
    let template_path = paths
        .root()
        .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);
    let template_evidence = paths.evidence_from_path(&template_path)?;
    let semantics_path = paths.semantics_path();
    let semantics_evidence = paths.evidence_from_path(&semantics_path)?;

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
        missing.push("verification semantics missing (enrich/semantics.json)".to_string());
    }

    Ok(VerificationInputs {
        surface,
        plan,
        surface_evidence,
        scenarios_evidence,
        template_path,
        template_evidence,
        semantics_path,
        semantics_evidence,
    })
}

fn base_evidence(inputs: &VerificationInputs) -> Vec<enrich::EvidenceRef> {
    vec![
        inputs.surface_evidence.clone(),
        inputs.scenarios_evidence.clone(),
        inputs.template_evidence.clone(),
        inputs.semantics_evidence.clone(),
    ]
}

fn eval_auto_verification(
    auto_state: crate::status::verification::AutoVerificationState,
    ctx: &mut AutoVerificationContext<'_>,
) -> VerificationEvalOutput {
    let crate::status::verification::AutoVerificationState {
        targets,
        remaining_ids,
        remaining_by_kind,
        excluded,
        excluded_count,
        ..
    } = auto_state;
    if let Some(ledger_entries) = ctx.ledger_entries {
        for surface_id in &remaining_ids {
            if let Some(entry) = ledger_entries.get(surface_id) {
                ctx.evidence.extend(entry.evidence.iter().cloned());
            }
        }
    }
    let remaining_preview = preview_ids(&remaining_ids);
    let remaining_by_kind_summary = remaining_by_kind
        .iter()
        .map(|group| enrich::VerificationKindSummary {
            kind: group.kind.as_str().to_string(),
            target_count: group.target_count,
            remaining_count: group.remaining_ids.len(),
            remaining_preview: preview_ids(&group.remaining_ids),
            remaining_ids: ctx.include_full.then(|| group.remaining_ids.clone()),
        })
        .collect();
    let summary = enrich::VerificationTriageSummary {
        discovered_untriaged_count: 0,
        discovered_untriaged_preview: Vec::new(),
        triaged_unverified_count: remaining_ids.len(),
        triaged_unverified_preview: remaining_preview,
        remaining_by_kind: remaining_by_kind_summary,
        excluded_count: if excluded_count == 0 {
            None
        } else {
            Some(excluded_count)
        },
        excluded,
        behavior_unverified_reasons: Vec::new(),
        discovered_untriaged_ids: ctx.include_full.then(Vec::new),
        triaged_unverified_ids: ctx.include_full.then(|| remaining_ids.clone()),
    };
    let summary_preview = format!(
        "auto verification: {} remaining ({})",
        summary.triaged_unverified_count,
        format_preview(
            summary.triaged_unverified_count,
            &summary.triaged_unverified_preview
        )
    );

    let mut output = VerificationEvalOutput {
        triage_summary: Some(summary),
        unverified_ids: Vec::new(),
    };
    if output
        .triage_summary
        .as_ref()
        .is_some_and(|summary| summary.triaged_unverified_count > 0)
    {
        output.unverified_ids.push(summary_preview);
    }
    if ctx.verification_next_action.is_none()
        && !remaining_ids.is_empty()
        && ctx.local_blockers.is_empty()
        && ctx.missing.is_empty()
    {
        let remaining_total = remaining_ids.len();
        let by_kind = remaining_by_kind
            .iter()
            .map(|group| format!("{} {}", group.kind.as_str(), group.remaining_ids.len()))
            .collect::<Vec<_>>()
            .join(", ");
        let mut reason = format!("auto verification remaining: {remaining_total}");
        if !by_kind.is_empty() {
            reason.push_str(&format!(" ({by_kind})"));
        }
        reason.push_str(&format!(
            "; max_new_runs_per_apply={}",
            targets.max_new_runs_per_apply
        ));
        reason.push_str(&format!(
            "; set scenarios/plan.json.verification.policy.max_new_runs_per_apply >= {remaining_total} to finish in one apply"
        ));
        let root = ctx.paths.root().display();
        *ctx.verification_next_action = Some(enrich::NextAction::Command {
            command: format!("bman apply --doc-pack {root}"),
            reason,
        });
    }

    output
}

#[allow(clippy::cognitive_complexity)]
fn eval_behavior_verification(ctx: &mut QueueVerificationContext<'_>) -> VerificationEvalOutput {
    let (surface_ids, _surface_evidence_map) = collect_surface_ids(ctx.surface);
    let (excluded_entries, excluded_ids) = ctx.plan.collect_queue_exclusions();
    let excluded: Vec<enrich::VerificationExclusion> = excluded_entries
        .into_iter()
        .map(|entry| enrich::VerificationExclusion {
            surface_id: entry.surface_id,
            prereqs: entry
                .prereqs
                .iter()
                .map(|prereq| prereq.as_str().to_string())
                .collect(),
            reason: entry.reason.unwrap_or_default(),
        })
        .collect();

    let mut required_ids = Vec::new();
    let mut required_seen = std::collections::BTreeSet::new();
    for entry in &ctx.plan.verification.queue {
        if entry.intent != scenarios::VerificationIntent::VerifyBehavior {
            continue;
        }
        let id = entry.surface_id.trim();
        if id.is_empty() {
            continue;
        }
        if excluded_ids.contains(id) {
            continue;
        }
        if required_seen.insert(id.to_string()) {
            required_ids.push(id.to_string());
        }
    }

    if required_ids.is_empty() {
        if ctx.verification_next_action.is_none() {
            let content = serde_json::to_string_pretty(ctx.plan)
                .unwrap_or_else(|_| scenarios::plan_stub(ctx.binary_name));
            *ctx.verification_next_action = Some(enrich::NextAction::Edit {
                path: "scenarios/plan.json".to_string(),
                content,
                reason: "add behavior verification triage in scenarios/plan.json".to_string(),
            });
        }
        let summary = enrich::VerificationTriageSummary {
            discovered_untriaged_count: 0,
            discovered_untriaged_preview: Vec::new(),
            triaged_unverified_count: 0,
            triaged_unverified_preview: Vec::new(),
            remaining_by_kind: Vec::new(),
            excluded_count: if excluded.is_empty() {
                None
            } else {
                Some(excluded.len())
            },
            excluded,
            behavior_unverified_reasons: Vec::new(),
            discovered_untriaged_ids: ctx.include_full.then(Vec::new),
            triaged_unverified_ids: ctx.include_full.then(Vec::new),
        };
        return VerificationEvalOutput {
            triage_summary: Some(summary),
            unverified_ids: vec!["behavior verification queue empty".to_string()],
        };
    }

    let mut missing_surface_ids = Vec::new();
    for id in &required_ids {
        if !surface_ids.contains(id) {
            missing_surface_ids.push(id.clone());
        }
    }
    if !missing_surface_ids.is_empty() {
        ctx.local_blockers.push(enrich::Blocker {
            code: "verification_surface_missing".to_string(),
            message: format!(
                "behavior verification queue surface_id missing from inventory: {}",
                missing_surface_ids.join(", ")
            ),
            evidence: vec![ctx.surface_evidence.clone(), ctx.scenarios_evidence.clone()],
            next_action: Some(
                "fix inventory/surface.json or inventory/surface.seed.json".to_string(),
            ),
        });
        if ctx.verification_next_action.is_none() {
            *ctx.verification_next_action = Some(enrich::NextAction::Edit {
                path: "inventory/surface.seed.json".to_string(),
                content: surface_seed_stub(),
                reason: format!(
                    "add surface items for behavior queue: {}",
                    missing_surface_ids.join(", ")
                ),
            });
        }
    }

    let Some(ledger_entries) = ctx.ledger_entries else {
        return VerificationEvalOutput::default();
    };
    let baseline_id = crate::status::verification::find_behavior_baseline_id(ctx.plan);

    let mut remaining_ids = Vec::new();
    for surface_id in &required_ids {
        let status = ledger_entries
            .get(surface_id)
            .map(|entry| entry.behavior_status.as_str())
            .unwrap_or("unknown");
        if status != "verified" {
            remaining_ids.push(surface_id.clone());
            if let Some(entry) = ledger_entries.get(surface_id) {
                ctx.evidence.extend(entry.evidence.iter().cloned());
            }
        }
    }
    remaining_ids.sort();
    remaining_ids.dedup();

    let remaining_preview = preview_ids(&remaining_ids);
    let missing_surface_set: std::collections::BTreeSet<String> =
        missing_surface_ids.iter().cloned().collect();
    let behavior_unverified_reasons =
        build_behavior_reason_summary(&remaining_ids, &missing_surface_set, ledger_entries);
    let remaining_by_kind_summary = if required_ids.is_empty() {
        Vec::new()
    } else {
        vec![enrich::VerificationKindSummary {
            kind: scenarios::VerificationTargetKind::Option
                .as_str()
                .to_string(),
            target_count: required_ids.len(),
            remaining_count: remaining_ids.len(),
            remaining_preview: preview_ids(&remaining_ids),
            remaining_ids: ctx.include_full.then(|| remaining_ids.clone()),
        }]
    };

    let summary = enrich::VerificationTriageSummary {
        discovered_untriaged_count: 0,
        discovered_untriaged_preview: Vec::new(),
        triaged_unverified_count: remaining_ids.len(),
        triaged_unverified_preview: remaining_preview,
        remaining_by_kind: remaining_by_kind_summary,
        excluded_count: if excluded.is_empty() {
            None
        } else {
            Some(excluded.len())
        },
        excluded,
        behavior_unverified_reasons,
        discovered_untriaged_ids: ctx.include_full.then(Vec::new),
        triaged_unverified_ids: ctx.include_full.then(|| remaining_ids.clone()),
    };

    if ctx.verification_next_action.is_none()
        && baseline_id.is_none()
        && !remaining_ids.is_empty()
        && ctx.local_blockers.is_empty()
        && ctx.missing.is_empty()
        && missing_surface_ids.is_empty()
    {
        if let Some(content) = crate::status::verification::behavior_baseline_stub(ctx.plan) {
            *ctx.verification_next_action = Some(enrich::NextAction::Edit {
                path: "scenarios/plan.json".to_string(),
                content,
                reason: "add a baseline behavior scenario".to_string(),
            });
        }
    }

    if ctx.verification_next_action.is_none()
        && baseline_id.is_some()
        && !remaining_ids.is_empty()
        && ctx.local_blockers.is_empty()
        && ctx.missing.is_empty()
        && missing_surface_ids.is_empty()
    {
        let mut prioritized_id = None;
        for entry in &ctx.plan.verification.queue {
            if entry.intent != scenarios::VerificationIntent::VerifyBehavior {
                continue;
            }
            let id = entry.surface_id.trim();
            if id.is_empty() {
                continue;
            }
            if remaining_ids.iter().any(|remaining| remaining == id) {
                prioritized_id = Some(id.to_string());
                break;
            }
        }
        let next_id = prioritized_id.unwrap_or_else(|| remaining_ids[0].clone());
        if let Some(entry) = ledger_entries.get(&next_id) {
            let scenario_id = entry
                .behavior_scenario_ids
                .first()
                .cloned()
                .unwrap_or_else(|| next_id.clone());
            let reason_code = entry.behavior_unverified_reason_code.as_deref();
            if entry.behavior_scenario_ids.is_empty() {
                let content = crate::status::verification::verification_stub_from_queue(
                    ctx.plan,
                    &scenarios::VerificationQueueEntry {
                        surface_id: next_id.clone(),
                        intent: scenarios::VerificationIntent::VerifyBehavior,
                        prereqs: Vec::new(),
                        reason: None,
                    },
                )
                .unwrap_or_else(|| scenarios::plan_stub(ctx.binary_name));
                *ctx.verification_next_action = Some(enrich::NextAction::Edit {
                    path: "scenarios/plan.json".to_string(),
                    content,
                    reason: format!("add a behavior scenario for {next_id}"),
                });
            } else if entry.behavior_assertion_scenario_ids.is_empty() {
                let content = serde_json::to_string_pretty(ctx.plan)
                    .unwrap_or_else(|_| scenarios::plan_stub(ctx.binary_name));
                *ctx.verification_next_action = Some(enrich::NextAction::Edit {
                    path: "scenarios/plan.json".to_string(),
                    content,
                    reason: behavior_unverified_reason(reason_code, &scenario_id, &next_id),
                });
            } else if entry.behavior_scenario_paths.is_empty() {
                let root = ctx.paths.root().display();
                *ctx.verification_next_action = Some(enrich::NextAction::Command {
                    command: format!(
                        "bman validate --doc-pack {root} && bman plan --doc-pack {root} && bman apply --doc-pack {root}"
                    ),
                    reason: format!("run behavior verification for {next_id}"),
                });
            } else if entry.behavior_status != "verified" {
                let content = serde_json::to_string_pretty(ctx.plan)
                    .unwrap_or_else(|_| scenarios::plan_stub(ctx.binary_name));
                *ctx.verification_next_action = Some(enrich::NextAction::Edit {
                    path: "scenarios/plan.json".to_string(),
                    content,
                    reason: behavior_unverified_reason(reason_code, &scenario_id, &next_id),
                });
            }
        } else {
            let content = crate::status::verification::verification_stub_from_queue(
                ctx.plan,
                &scenarios::VerificationQueueEntry {
                    surface_id: next_id.clone(),
                    intent: scenarios::VerificationIntent::VerifyBehavior,
                    prereqs: Vec::new(),
                    reason: None,
                },
            )
            .unwrap_or_else(|| scenarios::plan_stub(ctx.binary_name));
            *ctx.verification_next_action = Some(enrich::NextAction::Edit {
                path: "scenarios/plan.json".to_string(),
                content,
                reason: format!("add a behavior scenario for {next_id}"),
            });
        }
    }

    let summary_preview = format!(
        "behavior verification: {} remaining ({})",
        summary.triaged_unverified_count,
        format_preview(
            summary.triaged_unverified_count,
            &summary.triaged_unverified_preview
        )
    );

    let mut output = VerificationEvalOutput {
        triage_summary: Some(summary),
        unverified_ids: Vec::new(),
    };
    if output
        .triage_summary
        .as_ref()
        .is_some_and(|summary| summary.triaged_unverified_count > 0)
    {
        output.unverified_ids.push(summary_preview);
    }

    output
}

fn build_behavior_reason_summary(
    remaining_ids: &[String],
    missing_surface_ids: &std::collections::BTreeSet<String>,
    ledger_entries: &LedgerEntries,
) -> Vec<enrich::VerificationReasonSummary> {
    if remaining_ids.is_empty() {
        return Vec::new();
    }
    let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for surface_id in remaining_ids {
        let reason_code = if missing_surface_ids.contains(surface_id) {
            "surface_missing".to_string()
        } else {
            ledger_entries
                .get(surface_id)
                .and_then(|entry| entry.behavior_unverified_reason_code.as_ref())
                .cloned()
                .unwrap_or_else(|| "unknown".to_string())
        };
        grouped
            .entry(reason_code)
            .or_default()
            .push(surface_id.clone());
    }
    grouped
        .into_iter()
        .map(|(reason_code, mut ids)| {
            ids.sort();
            enrich::VerificationReasonSummary {
                reason_code,
                count: ids.len(),
                preview: preview_ids(&ids),
            }
        })
        .collect()
}

fn surface_seed_stub() -> String {
    [
        "{",
        "  \"schema_version\": 2,",
        "  \"items\": [],",
        "  \"overlays\": []",
        "}",
    ]
    .join("\n")
}

fn ensure_verification_policy(
    plan: &scenarios::ScenarioPlan,
    missing: &mut Vec<String>,
    verification_next_action: &mut Option<enrich::NextAction>,
    binary_name: Option<&str>,
) {
    if plan.verification.policy.is_some() {
        return;
    }
    missing.push("verification policy missing (scenarios/plan.json)".to_string());
    let content =
        serde_json::to_string_pretty(plan).unwrap_or_else(|_| scenarios::plan_stub(binary_name));
    *verification_next_action = Some(enrich::NextAction::Edit {
        path: "scenarios/plan.json".to_string(),
        content,
        reason: "add verification policy in scenarios/plan.json".to_string(),
    });
}

fn behavior_unverified_reason(
    reason_code: Option<&str>,
    scenario_id: &str,
    surface_id: &str,
) -> String {
    match reason_code {
        Some("missing_assertions") => {
            format!("add assertions to behavior scenario {scenario_id}")
        }
        Some("seed_mismatch") => {
            format!("add seed-grounded assertions and align seed entries for {scenario_id}")
        }
        Some("missing_delta_assertion") => {
            format!("add delta assertions to behavior scenario {scenario_id}")
        }
        Some("missing_semantic_predicate") => {
            format!("add stdout/stderr expectations or a seed-path delta pair for {scenario_id}")
        }
        Some("outputs_equal") => format!(
            "ensure baseline and variant outputs differ for behavior scenario {scenario_id}"
        ),
        Some("assertion_failed") => format!("fix assertions in behavior scenario {scenario_id}"),
        Some("scenario_failed") => format!("fix behavior scenario {scenario_id}"),
        Some("missing_behavior_scenario") => format!("add a behavior scenario for {surface_id}"),
        _ => format!("fix behavior scenario for {surface_id}"),
    }
}

pub(super) fn eval_verification_requirement(
    state: &mut EvalState,
    req: enrich::RequirementId,
) -> Result<enrich::RequirementStatus> {
    let paths = state.paths;
    let binary_name = state.binary_name;
    let config = state.config;
    let include_full = state.include_full;
    let missing_artifacts = &mut *state.missing_artifacts;
    let blockers = &mut *state.blockers;
    let verification_next_action = &mut *state.verification_next_action;

    let mut local_blockers = Vec::new();
    let mut missing = Vec::new();
    let mut unverified_ids = Vec::new();
    let mut triage_summary: Option<enrich::VerificationTriageSummary> = None;
    let mut ledger_snapshot = None;
    let verification_tier = config.verification_tier.as_deref().unwrap_or("accepted");
    let tier_label = if verification_tier == "behavior" {
        "behavior"
    } else {
        "existence"
    };
    let tier_tag = format!("tier={verification_tier}");

    let inputs =
        load_verification_inputs(paths, missing_artifacts, &mut missing, &mut local_blockers)?;
    let mut evidence = base_evidence(&inputs);

    if let (Some(surface), Some(plan)) = (inputs.surface.as_ref(), inputs.plan.as_ref()) {
        if inputs.template_path.is_file() && inputs.semantics_path.is_file() {
            ledger_snapshot = build_verification_ledger_entries(
                binary_name,
                surface,
                plan,
                paths,
                &inputs.template_path,
                &mut local_blockers,
                &inputs.template_evidence,
            );
        }
        let ledger_entries = ledger_snapshot.as_ref().map(|snapshot| &snapshot.entries);

        let output = if verification_tier != "behavior" {
            if let Some(auto_state) = crate::status::verification::auto_verification_state(
                plan,
                surface,
                ledger_entries,
                verification_tier,
            ) {
                let mut ctx = AutoVerificationContext {
                    ledger_entries,
                    include_full,
                    evidence: &mut evidence,
                    verification_next_action,
                    local_blockers: &local_blockers,
                    missing: &missing,
                    paths,
                };
                eval_auto_verification(auto_state, &mut ctx)
            } else {
                VerificationEvalOutput::default()
            }
        } else {
            let mut ctx = QueueVerificationContext {
                plan,
                surface,
                ledger_entries,
                evidence: &mut evidence,
                local_blockers: &mut local_blockers,
                verification_next_action,
                missing: &missing,
                paths,
                binary_name,
                include_full,
                surface_evidence: &inputs.surface_evidence,
                scenarios_evidence: &inputs.scenarios_evidence,
            };
            eval_behavior_verification(&mut ctx)
        };
        unverified_ids = output.unverified_ids;
        triage_summary = output.triage_summary;

        if verification_tier != "behavior" {
            ensure_verification_policy(plan, &mut missing, verification_next_action, binary_name);
        }
    }

    enrich::dedupe_evidence_refs(&mut evidence);
    let (verified_count, unverified_count, behavior_verified_count, behavior_unverified_count) =
        if let Some(snapshot) = ledger_snapshot.as_ref() {
            (
                Some(snapshot.verified_count),
                Some(snapshot.unverified_count),
                Some(snapshot.behavior_verified_count),
                Some(snapshot.behavior_unverified_count),
            )
        } else {
            (
                None,
                triage_summary
                    .as_ref()
                    .map(|summary| summary.triaged_unverified_count),
                None,
                None,
            )
        };
    let behavior_remaining = if verification_tier != "behavior" {
        behavior_unverified_count.filter(|count| *count > 0)
    } else {
        None
    };

    let (status, reason) = if !local_blockers.is_empty() {
        (
            enrich::RequirementState::Blocked,
            format!("verification inputs blocked ({tier_tag})"),
        )
    } else if !missing.is_empty() {
        (
            enrich::RequirementState::Unmet,
            format!(
                "verification inputs missing: {} ({tier_tag})",
                missing.join("; ")
            ),
        )
    } else if !unverified_ids.is_empty() {
        (
            enrich::RequirementState::Unmet,
            format!("verification {tier_label} incomplete ({tier_tag})"),
        )
    } else {
        let mut tag = tier_tag;
        if let Some(remaining) = behavior_remaining {
            tag.push_str(&format!("; behavior_remaining={remaining} not required"));
        }
        (
            enrich::RequirementState::Met,
            format!("verification {tier_label} complete ({tag})"),
        )
    };

    blockers.extend(local_blockers.clone());
    Ok(enrich::RequirementStatus {
        id: req,
        status,
        reason,
        verification_tier: Some(verification_tier.to_string()),
        verified_count,
        unverified_ids,
        unverified_count,
        behavior_verified_count,
        behavior_unverified_count,
        verification: triage_summary,
        evidence,
        blockers: local_blockers,
    })
}
