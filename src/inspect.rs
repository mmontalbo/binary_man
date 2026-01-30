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
