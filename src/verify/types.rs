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
/// - v5: Added probe results for evidence gathering before test commitment
/// - v6: Added characterization for reasoning-first seed generation
pub const STATE_SCHEMA_VERSION: u32 = 6;

/// Maximum probe runs per surface.
///
/// Probes are now bilateral (run both control and option) and auto-promote
/// on success, so they're the primary exploration mechanism. Budget is
/// generous since probes don't burn test attempts.
pub const MAX_PROBES_PER_SURFACE: usize = 8;

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
    /// EXAMPLES section from man page, if available.
    #[serde(default)]
    pub examples_section: String,
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
    /// Probe results from evidence-gathering runs (no outcome computed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<ProbeResult>,
    /// History of verification attempts.
    pub attempts: Vec<Attempt>,
    /// Whether this surface has been retried after exclusion.
    #[serde(default)]
    pub retried: bool,
    /// Feedback from critique explaining why a prior verification was rejected.
    /// When present, prompts include this so the LM can adjust its approach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critique_feedback: Option<String>,
    /// Number of times this surface has been demoted by critique.
    /// After 2 demotions, the surface is excluded as critique-irreconcilable.
    #[serde(default)]
    pub critique_demotions: u32,
    /// LM-generated characterization of what input triggers this option's effect.
    /// Populated once before testing begins; updated on repeated failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub characterization: Option<Characterization>,
}

/// LM reasoning about what makes an option produce observable output.
///
/// This separates "understanding the option" from "building a seed",
/// giving the seed-generation step a specification to build against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Characterization {
    /// What kind of input/scenario triggers a visible effect.
    /// e.g., "file with repeated similar lines where hunk boundaries are ambiguous"
    pub trigger: String,
    /// What output difference to expect when the trigger is satisfied.
    /// e.g., "different hunk grouping in diff output"
    pub expected_diff: String,
    /// How many times this characterization has been revised (0 = initial).
    #[serde(default)]
    pub revision: u32,
}

/// Result of a probe run — bilateral comparison without commitment.
///
/// Probes run BOTH control and option in the same sandbox, returning whether
/// outputs differ. This lets the LM explore seeds cheaply without burning
/// test attempts. Probes that show differing outputs can be auto-promoted
/// to formal Tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    /// Cycle number when this probe ran.
    pub cycle: u32,
    /// Full argv used (context_argv + surface_id + extra_args).
    pub argv: Vec<String>,
    /// Seed configuration used.
    pub seed: Seed,
    /// Preview of option stdout (first ~200 chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_preview: Option<String>,
    /// Preview of option stderr (first ~200 chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_preview: Option<String>,
    /// Exit code of the option command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
    /// Preview of control stdout (first ~200 chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_stdout_preview: Option<String>,
    /// Whether control and option outputs differ (bilateral comparison).
    /// When true, this seed is a candidate for auto-promotion to Test.
    #[serde(default)]
    pub outputs_differ: bool,
    /// Whether the seed setup commands failed.
    pub setup_failed: bool,
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
    /// Accepts both array format `[{"path": "f", "content": "c"}]`
    /// and object-map format `{"f": "c"}` (common LM shorthand).
    #[serde(default, deserialize_with = "deserialize_files")]
    pub files: Vec<FileEntry>,
}

/// Deserialize files from either array or object-map format.
fn deserialize_files<'de, D>(deserializer: D) -> Result<Vec<FileEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;
    use serde_json::Value;

    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Array(arr) => {
            // Standard format: [{"path": "f", "content": "c"}, ...]
            arr.into_iter()
                .map(|v| serde_json::from_value(v).map_err(de::Error::custom))
                .collect()
        }
        Value::Object(map) => {
            // Object-map shorthand: {"filename": "content", ...}
            Ok(map
                .into_iter()
                .map(|(path, content)| FileEntry {
                    path,
                    content: content.as_str().unwrap_or_default().to_string(),
                })
                .collect())
        }
        Value::Null => Ok(Vec::new()),
        _ => Err(de::Error::custom("files must be an array or object")),
    }
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
    /// The args used in the successful test (surface_id + extra_args).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
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

/// A known issue extracted from SetupFailed outcomes.
pub(super) struct KnownIssue {
    /// The command that failed (e.g., "git checkout main").
    pub command: String,
    /// The error message (truncated).
    pub error: String,
    /// How many times this combination occurred.
    pub count: usize,
}

/// Extract aggregated known issues from all SetupFailed outcomes across the state.
///
/// Returns issues sorted by count descending, filtered to those with count >= 2.
pub(super) fn extract_known_issues(state: &State) -> Vec<KnownIssue> {
    use std::collections::HashMap;

    let mut counts: HashMap<(String, String), usize> = HashMap::new();

    for entry in &state.entries {
        for attempt in &entry.attempts {
            if let Outcome::SetupFailed { hint } = &attempt.outcome {
                if let Some((cmd, err)) = parse_setup_failed_hint(hint) {
                    *counts.entry((cmd, err)).or_insert(0) += 1;
                }
            }
        }
    }

    let mut issues: Vec<KnownIssue> = counts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|((command, error), count)| KnownIssue {
            command,
            error,
            count,
        })
        .collect();

    issues.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.command.cmp(&b.command))
    });

    issues.truncate(5);
    issues
}

/// Parse a SetupFailed hint to extract the command and error.
///
/// The hint format is:
/// ```text
/// Setup command #N failed: ["cmd", "arg1", "arg2"]
/// stderr: error message here
/// ```
pub(crate) fn parse_setup_failed_hint(hint: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = hint.lines().collect();
    if lines.is_empty() {
        return None;
    }

    let first_line = lines[0];
    let array_start = first_line.find('[')?;
    let array_end = first_line.rfind(']')?;
    if array_start >= array_end {
        return None;
    }

    let array_str = &first_line[array_start..=array_end];
    let command = parse_debug_string_array(array_str)?;

    let error = if lines.len() > 1 {
        let second_line = lines[1];
        let error_text = second_line
            .strip_prefix("stderr: ")
            .or_else(|| second_line.strip_prefix("error: "))
            .unwrap_or(second_line);
        truncate_error(error_text, 60)
    } else {
        "(no details)".to_string()
    };

    Some((command, error))
}

/// Parse a Rust debug format string array like `["git", "checkout", "main"]`.
pub(crate) fn parse_debug_string_array(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }

    let inner = &trimmed[1..trimmed.len() - 1];
    if inner.is_empty() {
        return Some(String::new());
    }

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = inner.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' if !in_quotes => {
                in_quotes = true;
            }
            '"' if in_quotes => {
                parts.push(current.clone());
                current.clear();
                in_quotes = false;
            }
            '\\' if in_quotes => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ',' | ' ' if !in_quotes => {}
            _ if in_quotes => {
                current.push(ch);
            }
            _ => {}
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Truncate error message for grouping purposes.
pub(crate) fn truncate_error(s: &str, max_len: usize) -> String {
    let first_line = s.lines().next().unwrap_or(s);
    let trimmed = first_line.trim();

    if trimmed.len() <= max_len {
        trimmed.to_string()
    } else {
        let mut end = max_len;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &trimmed[..end])
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
                probes: vec![],
                attempts: vec![],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 0,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
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
            args: vec!["--indent-heuristic".to_string()],
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
            args: vec![],
            seed: Seed::default(),
            verified_at: 1,
            hint: None,
        };
        assert!(no_seed.is_similar_to("--color-moved"));

        // Common prefix matching
        let color_seed = VerifiedSeed {
            surface_id: "--color-moved".to_string(),
            args: vec![],
            seed: Seed::default(),
            verified_at: 1,
            hint: None,
        };
        assert!(color_seed.is_similar_to("--color-moved-ws"));
    }
}
