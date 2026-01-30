use crate::enrich;
use serde::Deserialize;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum HelpUsageEvidenceState {
    MissingRuns,
    NoUsableHelp,
    UsableHelp,
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

pub(super) fn help_usage_evidence_state(paths: &enrich::DocPackPaths) -> HelpUsageEvidenceState {
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
