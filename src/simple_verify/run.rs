//! Main verification loop.
//!
//! This module implements the core verification loop:
//! bootstrap → [gather pending → lm_call → apply actions → save]* → done

use super::apply::apply_action;
use super::bootstrap::bootstrap;
use super::lm::{invoke_lm, log_prompt, log_response};
use super::prompt::build_prompt;
use super::types::{State, Status};
use super::validate::validate_action;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Maximum verification attempts per surface before auto-exhausting.
const MAX_ATTEMPTS: usize = 5;

/// Maximum surfaces to include in each LM batch.
const BATCH_SIZE: usize = 5;

/// Result of a verification run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResult {
    /// All surfaces verified or excluded.
    Complete,
    /// Reached max_cycles limit.
    HitMaxCycles,
    /// LM returned no actions for pending surfaces.
    LmGaveUp,
}

/// Run the verification loop.
///
/// This is the main entry point for the simplified verification workflow.
pub fn run(
    binary: &str,
    context_argv: &[String],
    pack_path: &Path,
    max_cycles: u32,
    lm_command: &str,
    verbose: bool,
) -> Result<RunResult> {
    // Create pack directory structure
    fs::create_dir_all(pack_path.join("evidence")).context("create evidence directory")?;
    fs::create_dir_all(pack_path.join("lm_log")).context("create lm_log directory")?;

    // Load or bootstrap
    let mut state = if pack_path.join("state.json").exists() {
        if verbose {
            eprintln!("Loading existing state from {}", pack_path.display());
        }
        State::load(pack_path)?
    } else {
        if verbose {
            eprintln!("Bootstrapping new state for {}", binary);
        }
        let state = bootstrap(binary, context_argv)?;
        if verbose {
            eprintln!("Discovered {} surfaces", state.entries.len());
        }
        state
    };

    // Save initial state
    state.save(pack_path)?;

    loop {
        // Check cycle limit
        if state.cycle >= max_cycles {
            if verbose {
                eprintln!("Hit max cycles limit ({})", max_cycles);
            }
            return Ok(RunResult::HitMaxCycles);
        }

        // Auto-exhaust surfaces over attempt limit
        for entry in &mut state.entries {
            if matches!(entry.status, Status::Pending) && entry.attempts.len() >= MAX_ATTEMPTS {
                entry.status = Status::Excluded {
                    reason: format!("Exhausted after {} attempts", MAX_ATTEMPTS),
                };
                if verbose {
                    eprintln!("Auto-excluded {} (exhausted attempts)", entry.id);
                }
            }
        }

        // Find pending targets
        let pending_ids: Vec<String> = state
            .entries
            .iter()
            .filter(|e| matches!(e.status, Status::Pending))
            .take(BATCH_SIZE)
            .map(|e| e.id.clone())
            .collect();

        if pending_ids.is_empty() {
            if verbose {
                eprintln!("All surfaces processed - complete!");
            }
            return Ok(RunResult::Complete);
        }

        // Increment cycle
        state.cycle += 1;

        if verbose {
            eprintln!(
                "Cycle {}: processing {} surface(s): {}",
                state.cycle,
                pending_ids.len(),
                pending_ids.join(", ")
            );
        }

        // Build LM context and call
        let prompt = build_prompt(&state, &pending_ids);
        log_prompt(pack_path, state.cycle, &prompt)?;

        if verbose {
            eprintln!("  Invoking LM...");
        }
        let response = invoke_lm(lm_command, &prompt)?;
        log_response(pack_path, state.cycle, &response)?;

        // Handle empty response
        if response.actions.is_empty() {
            if verbose {
                eprintln!("  LM returned no actions - giving up on remaining surfaces");
            }
            for entry in &mut state.entries {
                if matches!(entry.status, Status::Pending) {
                    entry.status = Status::Excluded {
                        reason: "LM provided no actions".into(),
                    };
                }
            }
            state.save(pack_path)?;
            return Ok(RunResult::LmGaveUp);
        }

        if verbose {
            eprintln!("  LM returned {} action(s)", response.actions.len());
        }

        // Apply actions (save after each for crash recovery)
        for action in response.actions {
            // Validate first
            if let Err(e) = validate_action(&action, &state) {
                eprintln!("  Skipping invalid action: {}", e);
                continue;
            }

            // Apply
            let action_desc = format_action_desc(&action);
            if verbose {
                eprintln!("  Applying: {}", action_desc);
            }

            if let Err(e) = apply_action(&mut state, pack_path, action) {
                eprintln!("  Action failed: {}", e);
            }

            // Save after each action
            state.save(pack_path)?;
        }

        // Report progress
        if verbose {
            let verified = state
                .entries
                .iter()
                .filter(|e| matches!(e.status, Status::Verified))
                .count();
            let excluded = state
                .entries
                .iter()
                .filter(|e| matches!(e.status, Status::Excluded { .. }))
                .count();
            let pending = state
                .entries
                .iter()
                .filter(|e| matches!(e.status, Status::Pending))
                .count();
            eprintln!(
                "  Progress: {} verified, {} excluded, {} pending",
                verified, excluded, pending
            );
        }
    }
}

/// Format an action for display.
fn format_action_desc(action: &super::lm::LmAction) -> String {
    match action {
        super::lm::LmAction::SetBaseline { args, .. } => {
            format!("SetBaseline args={:?}", args)
        }
        super::lm::LmAction::Test {
            surface_id, args, ..
        } => {
            format!("Test {} args={:?}", surface_id, args)
        }
        super::lm::LmAction::Exclude { surface_id, reason } => {
            format!("Exclude {} ({})", surface_id, reason)
        }
    }
}

/// Get a summary of the current state.
pub fn get_summary(state: &State) -> Summary {
    let total = state.entries.len();
    let verified = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Verified))
        .count();
    let excluded = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Excluded { .. }))
        .count();
    let pending = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Pending))
        .count();

    Summary {
        binary: state.binary.clone(),
        context_argv: state.context_argv.clone(),
        cycle: state.cycle,
        total,
        verified,
        excluded,
        pending,
        has_baseline: state.baseline.is_some(),
    }
}

/// Summary of verification state.
#[derive(Debug, Clone)]
pub struct Summary {
    pub binary: String,
    pub context_argv: Vec<String>,
    pub cycle: u32,
    pub total: usize,
    pub verified: usize,
    pub excluded: usize,
    pub pending: usize,
    pub has_baseline: bool,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let context = if self.context_argv.is_empty() {
            String::new()
        } else {
            format!(" {}", self.context_argv.join(" "))
        };
        writeln!(f, "Binary: {}{}", self.binary, context)?;
        writeln!(f, "Cycle: {}", self.cycle)?;
        writeln!(
            f,
            "Baseline: {}",
            if self.has_baseline { "yes" } else { "no" }
        )?;
        writeln!(f, "Surfaces: {} total", self.total)?;
        writeln!(f, "  Verified: {}", self.verified)?;
        writeln!(f, "  Excluded: {}", self.excluded)?;
        writeln!(f, "  Pending: {}", self.pending)?;
        let pct = if self.total > 0 {
            (self.verified * 100) / self.total
        } else {
            0
        };
        write!(f, "Verification rate: {}%", pct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simple_verify::types::{SurfaceEntry, STATE_SCHEMA_VERSION};

    #[test]
    fn test_get_summary() {
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "test".to_string(),
            context_argv: vec!["sub".to_string()],
            baseline: None,
            entries: vec![
                SurfaceEntry {
                    id: "-a".to_string(),
                    description: "A".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Verified,
                    attempts: vec![],
                },
                SurfaceEntry {
                    id: "-b".to_string(),
                    description: "B".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Pending,
                    attempts: vec![],
                },
                SurfaceEntry {
                    id: "-c".to_string(),
                    description: "C".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Excluded {
                        reason: "test".to_string(),
                    },
                    attempts: vec![],
                },
            ],
            cycle: 5,
        };

        let summary = get_summary(&state);

        assert_eq!(summary.binary, "test");
        assert_eq!(summary.context_argv, vec!["sub"]);
        assert_eq!(summary.cycle, 5);
        assert_eq!(summary.total, 3);
        assert_eq!(summary.verified, 1);
        assert_eq!(summary.excluded, 1);
        assert_eq!(summary.pending, 1);
        assert!(!summary.has_baseline);
    }

    #[test]
    fn test_format_action_desc() {
        use crate::simple_verify::lm::LmAction;
        use crate::simple_verify::types::Seed;

        let action = LmAction::SetBaseline {
            args: vec![],
            seed: Seed::default(),
        };
        assert!(format_action_desc(&action).contains("SetBaseline"));

        let action = LmAction::Test {
            surface_id: "--stat".to_string(),
            args: vec!["--stat".to_string()],
            seed: Seed::default(),
        };
        assert!(format_action_desc(&action).contains("Test"));
        assert!(format_action_desc(&action).contains("--stat"));

        let action = LmAction::Exclude {
            surface_id: "--gpg".to_string(),
            reason: "needs key".to_string(),
        };
        assert!(format_action_desc(&action).contains("Exclude"));
    }
}
