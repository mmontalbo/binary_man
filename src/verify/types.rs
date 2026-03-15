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
/// - v3: Added seed_bank for reusing successful seeds
/// - v4: Added surface category for classification-driven scheduling
pub const STATE_SCHEMA_VERSION: u32 = 4;

/// Classification of a surface for scheduling and execution strategy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SurfaceCategory {
    /// Simple output format change (--name-only, --stat).
    FormatChange,
    /// Requires color/TTY to produce observable differences.
    TtyDependent,
    /// Modifier of another option (--no-X modifies --X).
    Modifier { base: String },
    /// Requires a value argument (-U <n>, -G <regex>).
    ValueRequired,
    /// Affects exit code or stderr, not stdout (--exit-code, --quiet).
    MetaEffect,
    /// Default when no heuristic matches.
    General,
}

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
    /// Bank of verified seeds that can be reused for similar surfaces.
    #[serde(default)]
    pub seed_bank: Vec<VerifiedSeed>,
    /// Help text preamble (synopsis, description) for LM context.
    #[serde(default)]
    pub help_preamble: String,
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
    /// Classification for scheduling and execution strategy.
    #[serde(default = "default_category")]
    pub category: SurfaceCategory,
    /// Current verification status.
    pub status: Status,
    /// History of verification attempts.
    pub attempts: Vec<Attempt>,
    /// Whether this surface has been retried after exclusion.
    #[serde(default)]
    pub retried: bool,
    /// Feedback from critique explaining why a prior verification was rejected.
    /// When present, prompts include this so the LM can adjust its approach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critique_feedback: Option<String>,
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
    /// Whether the LM's prediction matched the actual outcome.
    /// None if no prediction was provided, Some(true) if matched, Some(false) if not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prediction_matched: Option<bool>,
}

/// Filesystem changes detected between before/after command execution.
/// Re-exported from evidence module for use in Attempt.
pub use super::evidence::{FsDiff, OutputMetrics};

fn default_category() -> SurfaceCategory {
    SurfaceCategory::General
}

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

/// A verified seed that can be reused for similar surfaces.
///
/// When a surface is successfully verified, we store its seed here
/// so it can be suggested for similar surfaces (e.g., --no-X can
/// reuse the seed from --X).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedSeed {
    /// Surface ID that was verified with this seed.
    pub surface_id: String,
    /// The seed configuration that worked.
    pub seed: Seed,
    /// Cycle when this was verified.
    pub verified_at: u32,
    /// Brief description of why this seed works (from the option behavior).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl VerifiedSeed {
    /// Check if this seed might be relevant for another surface.
    ///
    /// Uses simple heuristics:
    /// - --no-X matches --X (negation pairs)
    /// - --X=value matches --X (value variants)
    /// - Surfaces with common prefixes (--color-moved, --color-moved-ws)
    pub fn is_similar_to(&self, other_surface: &str) -> bool {
        let self_id = &self.surface_id;

        // Exact match (shouldn't happen, but handle it)
        if self_id == other_surface {
            return false;
        }

        // Negation pairs: --no-X <-> --X
        if let Some(stripped) = self_id.strip_prefix("--no-") {
            if other_surface == format!("--{}", stripped) {
                return true;
            }
        }
        if let Some(stripped) = other_surface.strip_prefix("--no-") {
            if *self_id == format!("--{}", stripped) {
                return true;
            }
        }

        // Prefix relationship: one is a prefix of the other (with separator)
        // e.g., --color-moved and --color-moved-ws
        // This avoids false positives like --ignore-space-change matching --ignore-submodules
        if self_id.len() >= 8 && other_surface.len() >= 8 {
            // Check if one is a prefix of the other followed by a separator
            if other_surface.starts_with(self_id)
                && other_surface
                    .chars()
                    .nth(self_id.len())
                    .is_some_and(|c| c == '-' || c == '=')
            {
                return true;
            }
            if self_id.starts_with(other_surface)
                && self_id
                    .chars()
                    .nth(other_surface.len())
                    .is_some_and(|c| c == '-' || c == '=')
            {
                return true;
            }
        }

        false
    }
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
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
            }],
            cycle: 0,
            seed_bank: vec![],
            help_preamble: String::new(),
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

    #[test]
    fn test_verified_seed_similarity() {
        let seed = VerifiedSeed {
            surface_id: "--indent-heuristic".to_string(),
            seed: Seed::default(),
            verified_at: 1,
            hint: None,
        };

        // Negation pair
        assert!(seed.is_similar_to("--no-indent-heuristic"));

        // Not similar to unrelated
        assert!(!seed.is_similar_to("--stat"));
        assert!(!seed.is_similar_to("--color"));

        // Self is not similar (should return false for same surface)
        assert!(!seed.is_similar_to("--indent-heuristic"));

        // Test the reverse direction (--no-X to --X)
        let no_seed = VerifiedSeed {
            surface_id: "--no-color-moved".to_string(),
            seed: Seed::default(),
            verified_at: 1,
            hint: None,
        };
        assert!(no_seed.is_similar_to("--color-moved"));

        // Common prefix matching
        let color_seed = VerifiedSeed {
            surface_id: "--color-moved".to_string(),
            seed: Seed::default(),
            verified_at: 1,
            hint: None,
        };
        assert!(color_seed.is_similar_to("--color-moved-ws"));
    }
}
