use crate::enrich;
use crate::scenarios;
use anyhow::Result;
use std::collections::BTreeSet;

pub(crate) fn load_scenario_failures(
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
