use super::EvalState;
use crate::enrich;
use crate::scenarios;
use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
struct RunsIndex {
    #[serde(default)]
    run_count: Option<usize>,
    #[serde(default)]
    runs: Vec<serde_json::Value>,
}

pub(super) fn eval_examples_report_requirement(
    state: &mut EvalState,
    req: enrich::RequirementId,
) -> Result<enrich::RequirementStatus> {
    let paths = state.paths;
    let missing_artifacts = &mut *state.missing_artifacts;
    let blockers = &mut *state.blockers;

    let runs_index_path = paths.pack_root().join("runs").join("index.json");
    let runs_evidence = paths.evidence_from_path(&runs_index_path)?;
    let mut evidence = vec![runs_evidence.clone()];
    let mut local_blockers = Vec::new();
    let mut unmet = Vec::new();

    let scenarios_path = paths.scenarios_plan_path();
    let scenarios_evidence = paths.evidence_from_path(&scenarios_path)?;
    evidence.push(scenarios_evidence.clone());
    if !scenarios_path.is_file() {
        missing_artifacts.push(scenarios_evidence.path);
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
                    evidence: vec![runs_evidence],
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
        missing_artifacts.push(runs_evidence.path);
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
    Ok(enrich::RequirementStatus {
        id: req,
        status,
        reason,
        verification_tier: None,
        accepted_verified_count: None,
        unverified_ids: Vec::new(),
        accepted_unverified_count: None,
        behavior_verified_count: None,
        behavior_unverified_count: None,
        verification: None,
        evidence,
        blockers: local_blockers,
    })
}
