//! Read-only doc-pack inspector entrypoint.
//!
//! Inspect is a TUI view over status and artifacts without side effects.
//!
//! ## Tab Structure (M21)
//!
//! | Tab | Question | Content |
//! |-----|----------|---------|
//! | Work | "What needs attention?" | Items grouped by status: needs_scenario, needs_fix, excluded |
//! | Log | "What did the LM do?" | Chronological LM invocations with outcomes |
//! | Browse | "What exists?" | File tree of doc pack artifacts |
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

const PREVIEW_LIMIT: usize = 20;
const PREVIEW_MAX_LINES: usize = 2;
const PREVIEW_MAX_CHARS: usize = 160;
const EVENT_POLL_MS: u64 = 200;

/// Main tabs for the inspector TUI.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Tab {
    /// "What needs attention?" - Shows unverified items grouped by status.
    Work,
    /// "What did the LM do?" - Shows LM invocation history.
    Log,
    /// "What exists?" - File tree browser.
    Browse,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Work, Tab::Log, Tab::Browse];

    fn index(self) -> usize {
        match self {
            Tab::Work => 0,
            Tab::Log => 1,
            Tab::Browse => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Work => "Work",
            Tab::Log => "Log",
            Tab::Browse => "Browse",
        }
    }
}

/// Work queue item categories.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum WorkCategory {
    /// No scenario exists for this surface item.
    NeedsScenario,
    /// Scenario exists but verification failed.
    NeedsFix,
    /// Item excluded from verification (interactive, network, etc.).
    Excluded,
    /// Item successfully verified.
    Verified,
}

impl WorkCategory {
    fn label(self) -> &'static str {
        match self {
            WorkCategory::NeedsScenario => "NEEDS SCENARIO",
            WorkCategory::NeedsFix => "NEEDS FIX",
            WorkCategory::Excluded => "EXCLUDED",
            WorkCategory::Verified => "VERIFIED",
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
