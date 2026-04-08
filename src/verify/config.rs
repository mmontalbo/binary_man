//! Centralized tuning constants for the verification pipeline.
//!
//! All behavioral thresholds and limits live here so experiments
//! can be configured by editing a single file.

// ── Verification lifecycle ───────────────────────────────────────

/// Maximum verification attempts per surface before auto-exhausting.
pub const MAX_ATTEMPTS: usize = 5;

/// Consecutive OutputsEqual outcomes that trigger early stagnation exclusion.
pub const STAGNATION_THRESHOLD: usize = 3;

/// Total failures across all sessions before a surface is globally excluded.
pub const GLOBAL_FAILURE_THRESHOLD: usize = 5;

/// Maximum probes per surface before the probe budget is exhausted.
pub const MAX_PROBES_PER_SURFACE: usize = 6;

/// Critique demotions before a surface is excluded as irreconcilable.
pub const CRITIQUE_EXCLUSION_THRESHOLD: u32 = 2;

// ── Batching ─────────────────────────────────────────────────────

/// Maximum surfaces to include in each LM verification batch.
pub const BATCH_SIZE: usize = 5;

/// Maximum surfaces per critique batch.
pub const CRITIQUE_BATCH_SIZE: usize = 10;

/// Surfaces per bulk characterization call.
pub const CHARACTERIZE_CHUNK_SIZE: usize = 20;

// ── LM invocation ────────────────────────────────────────────────

/// Default LM timeout in seconds.
pub const LM_TIMEOUT_SECS: u64 = 120;

/// Maximum retry attempts for LM calls.
pub const MAX_LM_RETRIES: usize = 3;

// ── Pipeline ─────────────────────────────────────────────────────

/// How often (in cycles) to checkpoint parallel session progress to disk.
pub const CHECKPOINT_INTERVAL: u32 = 10;

/// Maximum concurrent probes during batch probe phase.
pub const MAX_CONCURRENT_PROBES: usize = 16;

// ── Companions ───────────────────────────────────────────────────

/// Include generic companion dependency hints in verification prompts.
/// When true, prompts remind the LM that some options need companion flags
/// for observable output (e.g., a mode flag like -c or -x).
pub const COMPANION_HINTS: bool = true;

// ── Extraction ───────────────────────────────────────────────────

/// Target size (in characters) for each extraction chunk sent to LM.
pub const EXTRACT_CHUNK_TARGET_SIZE: usize = 2000;

/// Lines of surrounding context to include with each option in extraction prompts.
pub const CONTEXT_WINDOW_SIZE: usize = 2;

// ── Evidence ─────────────────────────────────────────────────────

/// Default sandbox command timeout in seconds.
pub const SANDBOX_TIMEOUT_SECS: u64 = 30;

/// Maximum captured output per channel (stdout/stderr) in bytes.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

// ── Prompt ────────────────────────────────────────────────────────

/// Maximum prior attempts to show per surface in full prompts.
pub const MAX_PRIOR_ATTEMPTS: usize = 2;

/// Maximum length for seed summary strings in prompts.
pub const SEED_SUMMARY_MAX_LEN: usize = 200;

/// Maximum length for output previews in critique prompts.
pub const CRITIQUE_OUTPUT_MAX_LEN: usize = 1500;

/// Maximum length for surface descriptions during extraction.
pub const DESC_MAX_LEN: usize = 600;

/// Snapshot all tunable constants for experiment traceability.
pub fn experiment_params() -> serde_json::Value {
    serde_json::json!({
        "max_attempts": MAX_ATTEMPTS,
        "stagnation_threshold": STAGNATION_THRESHOLD,
        "batch_size": BATCH_SIZE,
        "checkpoint_interval": CHECKPOINT_INTERVAL,
        "global_failure_threshold": GLOBAL_FAILURE_THRESHOLD,
        "max_probes_per_surface": MAX_PROBES_PER_SURFACE,
        "critique_exclusion_threshold": CRITIQUE_EXCLUSION_THRESHOLD,
        "prediction_gate": true,
        "e1_empty_stdout_bypass": true,
        "option_error_patterns": ["error:", "fatal:"],
        "prompt_hints": [
            "no_shell_escaping",
            "sandbox_writable_tmp",
        ],
        "companion_hints": COMPANION_HINTS,
    })
}
