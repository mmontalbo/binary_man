//! Read-only doc-pack inspector entrypoint.
//!
//! Inspect is a TUI view over status and artifacts without side effects.
mod app;
mod data;
mod external;
mod format;
mod text;
mod ui;

use crate::cli::InspectArgs;
use crate::docpack::doc_pack_root_for_status;
use anyhow::Result;
use std::io::{self, IsTerminal};

const PREVIEW_LIMIT: usize = 10;
const PREVIEW_MAX_LINES: usize = 2;
const PREVIEW_MAX_CHARS: usize = 160;
const EVENT_POLL_MS: u64 = 200;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Tab {
    Intent,
    Evidence,
    Outputs,
    History,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum EvidenceFilter {
    All,
    Help,
    Auto,
    Manual,
}

impl EvidenceFilter {
    const DISPLAY: [EvidenceFilter; 4] = [
        EvidenceFilter::All,
        EvidenceFilter::Help,
        EvidenceFilter::Auto,
        EvidenceFilter::Manual,
    ];

    fn label(self) -> &'static str {
        match self {
            EvidenceFilter::All => "All",
            EvidenceFilter::Help => "Help",
            EvidenceFilter::Auto => "Auto",
            EvidenceFilter::Manual => "Manual",
        }
    }

    fn from_scenario_id(scenario_id: &str) -> EvidenceFilter {
        if scenario_id.starts_with("help--") {
            EvidenceFilter::Help
        } else if scenario_id.starts_with("auto_verify::") {
            EvidenceFilter::Auto
        } else {
            EvidenceFilter::Manual
        }
    }

    fn matches(self, scenario_id: &str) -> bool {
        match self {
            EvidenceFilter::All => true,
            _ => EvidenceFilter::from_scenario_id(scenario_id) == self,
        }
    }

    fn next(self) -> EvidenceFilter {
        let idx = EvidenceFilter::DISPLAY
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        EvidenceFilter::DISPLAY[(idx + 1) % EvidenceFilter::DISPLAY.len()]
    }
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Intent, Tab::Evidence, Tab::Outputs, Tab::History];

    fn index(self) -> usize {
        match self {
            Tab::Intent => 0,
            Tab::Evidence => 1,
            Tab::Outputs => 2,
            Tab::History => 3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Intent => "Intent",
            Tab::Evidence => "Evidence",
            Tab::Outputs => "Outputs",
            Tab::History => "History/Audit",
        }
    }
}

/// Run the inspector, falling back to text output for non-TTY environments.
pub fn run(args: &InspectArgs) -> Result<()> {
    let doc_pack_root = doc_pack_root_for_status(&args.doc_pack)?;
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return text::run_text_summary(&doc_pack_root);
    }
    ui::run_tui(doc_pack_root)
}
