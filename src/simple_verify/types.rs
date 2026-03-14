//! Core types for simplified behavior verification.
//!
//! This module defines the state model for the LM-driven verification loop.
//! The design prioritizes simplicity: a single `State` struct captures all
//! verification progress, eliminating the need for SQL queries or scattered
//! state files.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Current schema version for state serialization.
///
/// Version history:
/// - v1: Initial schema
/// - v2: Added context to SurfaceEntry, output previews to Attempt
pub const STATE_SCHEMA_VERSION: u32 = 2;

/// Complete verification state for a binary.
///
/// This is the only state file we write (besides evidence). The LM sees this
/// state and decides all actions - no decision tree needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    /// Schema version for forward compatibility.
    pub schema_version: u32,
    /// Binary being verified (e.g., "git").
    pub binary: String,
    /// Context arguments that scope verification (e.g., ["diff"] for "git diff").
    pub context_argv: Vec<String>,
    /// Baseline scenario record, if established.
    pub baseline: Option<BaselineRecord>,
    /// All discovered surface entries.
    pub entries: Vec<SurfaceEntry>,
    /// Current cycle number (incremented each LM call).
    pub cycle: u32,
}

impl State {
    /// Load state from a pack directory.
    pub fn load(pack_path: &Path) -> Result<Self> {
        let state_path = pack_path.join("state.json");
        let content = fs::read_to_string(&state_path)
            .with_context(|| format!("read state from {}", state_path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("parse state from {}", state_path.display()))
    }

    /// Save state to a pack directory.
    pub fn save(&self, pack_path: &Path) -> Result<()> {
        let state_path = pack_path.join("state.json");
        let content = serde_json::to_string_pretty(self).context("serialize state")?;
        fs::write(&state_path, content)
            .with_context(|| format!("write state to {}", state_path.display()))
    }
}

/// Record of the baseline scenario execution.
///
/// The baseline represents the command without any surface option enabled,
/// providing a reference point to detect behavioral differences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineRecord {
    /// Command arguments used for baseline (without surface options).
    pub argv: Vec<String>,
    /// Seed configuration used for baseline.
    pub seed: Seed,
    /// Relative path to evidence file within the pack.
    pub evidence_path: String,
}

/// A surface item discovered from help output.
///
/// Each entry tracks verification progress for a single option/flag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceEntry {
    /// Unique identifier (typically the option name, e.g., "--stat").
    pub id: String,
    /// Human-readable description from help output (multi-line descriptions joined).
    pub description: String,
    /// Surrounding context (nearby options) for additional hints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Hint about expected value type (e.g., "<n>", "<file>").
    pub value_hint: Option<String>,
    /// Current verification status.
    pub status: Status,
    /// History of verification attempts.
    pub attempts: Vec<Attempt>,
    /// Whether this surface has been retried after exclusion.
    #[serde(default)]
    pub retried: bool,
}

/// A single verification attempt for a surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    /// Cycle number when this attempt was made.
    pub cycle: u32,
    /// Arguments provided by the LM (appended to base command).
    pub args: Vec<String>,
    /// Full argv used for execution (context_argv + args).
    pub full_argv: Vec<String>,
    /// Seed configuration used.
    pub seed: Seed,
    /// Relative path to evidence file within the pack.
    pub evidence_path: String,
    /// Result of the attempt.
    pub outcome: Outcome,
    /// Preview of stdout from the option run (first ~200 chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_preview: Option<String>,
    /// Preview of stderr from the option run (first ~200 chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_preview: Option<String>,
    /// Preview of stdout from the control run (for comparison).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_stdout_preview: Option<String>,
    /// Filesystem changes from the option run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs_diff: Option<FsDiff>,
    /// Output metrics for stdout (line count, byte count).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_metrics: Option<OutputMetrics>,
    /// Output metrics for stderr (line count, byte count).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_metrics: Option<OutputMetrics>,
}

/// Filesystem changes detected between before/after command execution.
/// Re-exported from evidence module for use in Attempt.
pub use super::evidence::{FsDiff, OutputMetrics};

/// Environment setup before scenario execution.
///
/// Seeds create the filesystem state needed for the command to demonstrate
/// interesting behavior (e.g., git init, create test files).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Seed {
    /// Setup commands to run before the main command.
    /// Each inner Vec is a command with arguments.
    #[serde(default)]
    pub setup: Vec<Vec<String>>,
    /// Files to create before execution.
    #[serde(default)]
    pub files: Vec<FileEntry>,
}

/// A file to create as part of seed setup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative path for the file.
    pub path: String,
    /// File content.
    pub content: String,
}

/// Verification status for a surface entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum Status {
    /// Not yet verified.
    Pending,
    /// Successfully verified (output differs from baseline).
    Verified,
    /// Explicitly excluded from verification.
    Excluded {
        /// Reason for exclusion.
        reason: String,
    },
}

/// Result of a verification attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum Outcome {
    /// Output differs from baseline - verification succeeded.
    Verified {
        /// What aspect of the output differed.
        diff_kind: DiffKind,
    },
    /// Output matches baseline exactly - need different approach.
    OutputsEqual,
    /// Seed setup commands failed before main command ran.
    SetupFailed {
        /// Diagnostic hint from stderr.
        hint: String,
    },
    /// Command crashed (non-zero exit with no stdout).
    Crashed {
        /// Diagnostic hint including exit code and stderr.
        hint: String,
    },
    /// Execution infrastructure error (not the command's fault).
    ExecutionError {
        /// Error description.
        error: String,
    },
    /// Option caused an error while control succeeded.
    /// This indicates the test scenario was invalid, not a successful verification.
    OptionError {
        /// Diagnostic hint including exit code and stderr.
        hint: String,
    },
}

/// Which aspect of output differed from baseline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DiffKind {
    /// Stdout content differs.
    Stdout,
    /// Stderr content differs.
    Stderr,
    /// Exit code differs.
    ExitCode,
    /// Multiple aspects differ.
    Multiple,
    /// Filesystem side effects (files created/modified).
    SideEffect,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_roundtrip() {
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "test".to_string(),
            context_argv: vec!["sub".to_string()],
            baseline: Some(BaselineRecord {
                argv: vec!["sub".to_string()],
                seed: Seed::default(),
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "--verbose".to_string(),
                description: "Enable verbose output".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                attempts: vec![],
                retried: false,
            }],
            cycle: 0,
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: State = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.binary, "test");
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].id, "--verbose");
    }

    #[test]
    fn test_status_serialization() {
        let pending = Status::Pending;
        let verified = Status::Verified;
        let excluded = Status::Excluded {
            reason: "Requires hardware".to_string(),
        };

        let json = serde_json::to_string(&pending).unwrap();
        assert!(json.contains("Pending"));

        let json = serde_json::to_string(&verified).unwrap();
        assert!(json.contains("Verified"));

        let json = serde_json::to_string(&excluded).unwrap();
        assert!(json.contains("Excluded"));
        assert!(json.contains("Requires hardware"));
    }

    #[test]
    fn test_outcome_serialization() {
        let verified = Outcome::Verified {
            diff_kind: DiffKind::Stdout,
        };
        let equal = Outcome::OutputsEqual;
        let failed = Outcome::SetupFailed {
            hint: "git init failed".to_string(),
        };

        let json = serde_json::to_string(&verified).unwrap();
        assert!(json.contains("Verified"));
        assert!(json.contains("Stdout"));

        let json = serde_json::to_string(&equal).unwrap();
        assert!(json.contains("OutputsEqual"));

        let json = serde_json::to_string(&failed).unwrap();
        assert!(json.contains("SetupFailed"));
    }
}
