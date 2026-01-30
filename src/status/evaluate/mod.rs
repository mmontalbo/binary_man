use crate::enrich;
use anyhow::Result;

mod coverage_ledger_requirement;
mod coverage_requirement;
mod examples_requirement;
mod man_requirement;
mod surface_requirement;
mod verification_requirement;

use coverage_ledger_requirement::eval_coverage_ledger_requirement;
use coverage_requirement::eval_coverage_requirement;
use examples_requirement::eval_examples_report_requirement;
use man_requirement::eval_man_page_requirement;
use surface_requirement::eval_surface_requirement;
use verification_requirement::eval_verification_requirement;

const TRIAGE_PREVIEW_LIMIT: usize = 10;

pub(crate) struct EvalResult {
    pub(crate) requirements: Vec<enrich::RequirementStatus>,
    pub(crate) missing_artifacts: Vec<String>,
    pub(crate) blockers: Vec<enrich::Blocker>,
    pub(crate) decision: enrich::Decision,
    pub(crate) decision_reason: Option<String>,
    pub(crate) coverage_next_action: Option<enrich::NextAction>,
    pub(crate) verification_next_action: Option<enrich::NextAction>,
    pub(crate) man_semantics_next_action: Option<enrich::NextAction>,
    pub(crate) man_usage_next_action: Option<enrich::NextAction>,
}

struct EvalState<'a> {
    paths: &'a enrich::DocPackPaths,
    binary_name: Option<&'a str>,
    config: &'a enrich::EnrichConfig,
    lock_status: &'a enrich::LockStatus,
    include_full: bool,
    missing_artifacts: &'a mut Vec<String>,
    blockers: &'a mut Vec<enrich::Blocker>,
    coverage_next_action: &'a mut Option<enrich::NextAction>,
    verification_next_action: &'a mut Option<enrich::NextAction>,
    man_semantics_next_action: &'a mut Option<enrich::NextAction>,
    man_usage_next_action: &'a mut Option<enrich::NextAction>,
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

pub(crate) fn evaluate_requirements(
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

    {
        let mut state = EvalState {
            paths,
            binary_name,
            config,
            lock_status,
            include_full,
            missing_artifacts: &mut missing_artifacts,
            blockers: &mut blockers,
            coverage_next_action: &mut coverage_next_action,
            verification_next_action: &mut verification_next_action,
            man_semantics_next_action: &mut man_semantics_next_action,
            man_usage_next_action: &mut man_usage_next_action,
        };

        for req in enrich::normalized_requirements(config) {
            let status = match req {
                enrich::RequirementId::Surface => eval_surface_requirement(&mut state, req)?,
                enrich::RequirementId::Coverage => eval_coverage_requirement(&mut state, req)?,
                enrich::RequirementId::Verification => {
                    eval_verification_requirement(&mut state, req)?
                }
                enrich::RequirementId::CoverageLedger => {
                    eval_coverage_ledger_requirement(&mut state, req)?
                }
                enrich::RequirementId::ExamplesReport => {
                    eval_examples_report_requirement(&mut state, req)?
                }
                enrich::RequirementId::ManPage => eval_man_page_requirement(&mut state, req)?,
            };
            requirements.push(status);
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
