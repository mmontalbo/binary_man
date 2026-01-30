use super::ScenarioRunMode;
use crate::scenarios::evidence::{ExamplesReport, ScenarioIndexEntry, ScenarioOutcome};
use std::collections::HashMap;
use std::path::Path;

pub(super) struct PreviousOutcomes {
    pub(super) available: bool,
    pub(super) outcomes: HashMap<String, ScenarioOutcome>,
}

pub(super) fn load_previous_outcomes(doc_pack_root: &Path, verbose: bool) -> PreviousOutcomes {
    let report_path = doc_pack_root.join("man").join("examples_report.json");
    if !report_path.is_file() {
        return PreviousOutcomes {
            available: false,
            outcomes: HashMap::new(),
        };
    }
    let bytes = match std::fs::read(&report_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            if verbose {
                eprintln!("warning: failed to read {}: {err}", report_path.display());
            }
            return PreviousOutcomes {
                available: false,
                outcomes: HashMap::new(),
            };
        }
    };
    let report: ExamplesReport = match serde_json::from_slice(&bytes) {
        Ok(report) => report,
        Err(err) => {
            if verbose {
                eprintln!("warning: failed to parse {}: {err}", report_path.display());
            }
            return PreviousOutcomes {
                available: false,
                outcomes: HashMap::new(),
            };
        }
    };
    let outcomes = report
        .scenarios
        .into_iter()
        .map(|scenario| (scenario.scenario_id.clone(), scenario))
        .collect();
    PreviousOutcomes {
        available: true,
        outcomes,
    }
}

pub(super) fn should_run_scenario(
    run_mode: ScenarioRunMode,
    scenario_digest: &str,
    entry: Option<&ScenarioIndexEntry>,
    has_previous_outcome: bool,
) -> bool {
    if !has_previous_outcome {
        return true;
    }
    match run_mode {
        ScenarioRunMode::RerunAll => true,
        ScenarioRunMode::RerunFailed => match entry {
            Some(entry) => entry.last_pass != Some(true),
            None => true,
        },
        ScenarioRunMode::Default => match entry {
            Some(entry) => {
                entry.last_pass != Some(true) || entry.scenario_digest != scenario_digest
            }
            None => true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_run_scenario_respects_run_mode() {
        let entry = ScenarioIndexEntry {
            scenario_id: "scenario".to_string(),
            scenario_digest: "abc".to_string(),
            last_run_epoch_ms: None,
            last_pass: Some(true),
            failures: Vec::new(),
            evidence_paths: Vec::new(),
        };
        assert!(!should_run_scenario(
            ScenarioRunMode::Default,
            "abc",
            Some(&entry),
            true
        ));
        assert!(should_run_scenario(
            ScenarioRunMode::Default,
            "def",
            Some(&entry),
            true
        ));
        let failed_entry = ScenarioIndexEntry {
            last_pass: Some(false),
            ..entry.clone()
        };
        assert!(should_run_scenario(
            ScenarioRunMode::Default,
            "abc",
            Some(&failed_entry),
            true
        ));
        assert!(should_run_scenario(
            ScenarioRunMode::Default,
            "abc",
            None,
            true
        ));
        assert!(should_run_scenario(
            ScenarioRunMode::RerunAll,
            "abc",
            Some(&entry),
            true
        ));
        assert!(!should_run_scenario(
            ScenarioRunMode::RerunFailed,
            "def",
            Some(&entry),
            true
        ));
        assert!(should_run_scenario(
            ScenarioRunMode::RerunFailed,
            "abc",
            Some(&failed_entry),
            true
        ));
        assert!(should_run_scenario(
            ScenarioRunMode::Default,
            "abc",
            Some(&entry),
            false
        ));
    }
}
