//! LM invocation logging for enrichment transparency.
//!
//! Provides structured logging of all LM calls during enrichment, enabling
//! users to understand what the LM attempted and why enrichment succeeded
//! or failed.
//!
//! # Log Format
//!
//! Entries are appended to `enrich/lm_log.jsonl` as newline-delimited JSON:
//!
//! ```jsonl
//! {"ts":1707900000,"cycle":1,"kind":"prereq_inference","duration_ms":4200,...}
//! {"ts":1707900060,"cycle":2,"kind":"behavior","duration_ms":3100,...}
//! ```
//!
//! # Optional Full Content
//!
//! When verbose logging is enabled, full prompts and responses are stored in:
//! - `enrich/lm_log/cycle_NNN_<kind>_prompt.txt`
//! - `enrich/lm_log/cycle_NNN_<kind>_response.txt`

use crate::enrich::DocPackPaths;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

/// Current schema version for lm_log.jsonl entries.
pub const LM_LOG_SCHEMA_VERSION: u32 = 1;

/// Kinds of LM invocations during enrichment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LmInvocationKind {
    /// Initial prereq inference from surface item descriptions.
    PrereqInference,
    /// Behavior verification scenario generation/fixing.
    Behavior,
    /// Retry of behavior verification after previous failure.
    BehaviorRetry,
}

impl std::fmt::Display for LmInvocationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PrereqInference => write!(f, "prereq_inference"),
            Self::Behavior => write!(f, "behavior"),
            Self::BehaviorRetry => write!(f, "behavior_retry"),
        }
    }
}

/// Outcome of an LM invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LmOutcome {
    /// All items processed successfully.
    Success,
    /// Some items succeeded, some failed.
    Partial,
    /// All items failed or LM error.
    Failed,
}

impl std::fmt::Display for LmOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Success => write!(f, "success"),
            Self::Partial => write!(f, "partial"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// A single LM invocation log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LmLogEntry {
    /// Schema version for forwards compatibility.
    pub schema_version: u32,

    /// Unix timestamp in milliseconds when the invocation started.
    pub ts: u64,

    /// Cycle number within this enrichment session (1-indexed).
    pub cycle: u32,

    /// Type of LM invocation.
    pub kind: LmInvocationKind,

    /// Duration of the LM call in milliseconds.
    pub duration_ms: u64,

    /// Number of items sent to the LM.
    pub items_count: usize,

    /// Surface IDs of items sent (for quick reference).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub items: Vec<String>,

    /// Outcome of the invocation.
    pub outcome: LmOutcome,

    /// Number of items that succeeded (if partial/success).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub succeeded: Option<usize>,

    /// Number of items that failed (if partial/failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<usize>,

    /// Human-readable summary of what happened.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    /// Error message if failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Prompt preview (first ~500 chars) for quick inspection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_preview: Option<String>,
}

/// Builder for constructing LM log entries with timing.
pub struct LmLogBuilder {
    start: Instant,
    cycle: u32,
    kind: LmInvocationKind,
    items: Vec<String>,
    prompt_preview: Option<String>,
}

impl LmLogBuilder {
    /// Start building a new log entry.
    pub fn new(cycle: u32, kind: LmInvocationKind) -> Self {
        Self {
            start: Instant::now(),
            cycle,
            kind,
            items: Vec::new(),
            prompt_preview: None,
        }
    }

    /// Set the items being processed.
    pub fn with_items(mut self, items: Vec<String>) -> Self {
        self.items = items;
        self
    }

    /// Set a preview of the prompt (will be truncated to 500 chars).
    pub fn with_prompt_preview(mut self, prompt: &str) -> Self {
        let preview = if prompt.len() > 500 {
            format!("{}...", &prompt[..500])
        } else {
            prompt.to_string()
        };
        self.prompt_preview = Some(preview);
        self
    }

    /// Finish the entry with a success outcome.
    pub fn success(self, summary: impl Into<String>) -> LmLogEntry {
        let items_len = self.items.len();
        self.build(
            LmOutcome::Success,
            Some(items_len),
            None,
            Some(summary.into()),
            None,
        )
    }

    /// Finish the entry with a partial success outcome.
    pub fn partial(
        self,
        succeeded: usize,
        failed: usize,
        summary: impl Into<String>,
    ) -> LmLogEntry {
        self.build(
            LmOutcome::Partial,
            Some(succeeded),
            Some(failed),
            Some(summary.into()),
            None,
        )
    }

    /// Finish the entry with a failure outcome.
    pub fn failed(self, error: impl Into<String>) -> LmLogEntry {
        let items_len = self.items.len();
        self.build(
            LmOutcome::Failed,
            None,
            Some(items_len),
            None,
            Some(error.into()),
        )
    }

    fn build(
        self,
        outcome: LmOutcome,
        succeeded: Option<usize>,
        failed: Option<usize>,
        summary: Option<String>,
        error: Option<String>,
    ) -> LmLogEntry {
        let duration = self.start.elapsed();
        LmLogEntry {
            schema_version: LM_LOG_SCHEMA_VERSION,
            ts: now_epoch_ms(),
            cycle: self.cycle,
            kind: self.kind,
            duration_ms: duration.as_millis() as u64,
            items_count: self.items.len(),
            items: self.items,
            outcome,
            succeeded,
            failed,
            summary,
            error,
            prompt_preview: self.prompt_preview,
        }
    }
}

/// Append an LM log entry to the log file.
pub fn append_lm_log(paths: &DocPackPaths, entry: &LmLogEntry) -> Result<()> {
    let log_path = paths.lm_log_path();

    // Ensure parent directory exists
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).context("create enrich directory for lm_log")?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open lm_log for append: {}", log_path.display()))?;

    let line = serde_json::to_string(entry).context("serialize lm_log entry")?;
    writeln!(file, "{}", line).context("write lm_log entry")?;

    Ok(())
}

/// Store full prompt/response content for a cycle.
pub fn store_lm_content(
    paths: &DocPackPaths,
    cycle: u32,
    kind: LmInvocationKind,
    prompt: &str,
    response: &str,
) -> Result<()> {
    let log_dir = paths.lm_log_dir();
    fs::create_dir_all(&log_dir).context("create lm_log directory")?;

    let prompt_path = log_dir.join(format!("cycle_{:03}_{}_prompt.txt", cycle, kind));
    let response_path = log_dir.join(format!("cycle_{:03}_{}_response.txt", cycle, kind));

    fs::write(&prompt_path, prompt)
        .with_context(|| format!("write prompt: {}", prompt_path.display()))?;
    fs::write(&response_path, response)
        .with_context(|| format!("write response: {}", response_path.display()))?;

    Ok(())
}

/// Load all LM log entries from the log file.
pub fn load_lm_log(paths: &DocPackPaths) -> Result<Vec<LmLogEntry>> {
    let log_path = paths.lm_log_path();

    if !log_path.exists() {
        return Ok(Vec::new());
    }

    let file =
        File::open(&log_path).with_context(|| format!("open lm_log: {}", log_path.display()))?;

    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read line {} of lm_log", line_num + 1))?;
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<LmLogEntry>(&line) {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                // Log warning but continue - don't fail on corrupt entries
                eprintln!(
                    "warning: skip corrupt lm_log entry at line {}: {}",
                    line_num + 1,
                    e
                );
            }
        }
    }

    Ok(entries)
}

/// Get the next cycle number based on existing log entries.
pub fn next_cycle_number(paths: &DocPackPaths) -> Result<u32> {
    let entries = load_lm_log(paths)?;
    let max_cycle = entries.iter().map(|e| e.cycle).max().unwrap_or(0);
    Ok(max_cycle + 1)
}

/// Get current epoch time in milliseconds.
fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_paths(name: &str) -> PathBuf {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let root = std::env::temp_dir().join(format!("{name}-{}-{now}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create temp root");
        root
    }

    #[test]
    fn test_log_entry_roundtrip() {
        let entry = LmLogEntry {
            schema_version: LM_LOG_SCHEMA_VERSION,
            ts: 1707900000000,
            cycle: 1,
            kind: LmInvocationKind::PrereqInference,
            duration_ms: 4200,
            items_count: 34,
            items: vec!["--verbose".to_string(), "--quiet".to_string()],
            outcome: LmOutcome::Success,
            succeeded: Some(34),
            failed: None,
            summary: Some("10 definitions, 34 mappings".to_string()),
            error: None,
            prompt_preview: None,
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: LmLogEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.cycle, 1);
        assert_eq!(parsed.kind, LmInvocationKind::PrereqInference);
        assert_eq!(parsed.items_count, 34);
    }

    #[test]
    fn test_append_and_load() {
        let root = test_paths("bman-lm-log-append");
        let paths = DocPackPaths::new(root);

        let entry1 = LmLogBuilder::new(1, LmInvocationKind::PrereqInference)
            .with_items(vec!["--edit".to_string()])
            .success("1 item processed");

        let entry2 = LmLogBuilder::new(2, LmInvocationKind::Behavior)
            .with_items(vec!["--local".to_string(), "--global".to_string()])
            .partial(1, 1, "partial success");

        append_lm_log(&paths, &entry1).unwrap();
        append_lm_log(&paths, &entry2).unwrap();

        let entries = load_lm_log(&paths).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].cycle, 1);
        assert_eq!(entries[1].cycle, 2);
    }

    #[test]
    fn test_next_cycle_number() {
        let root = test_paths("bman-lm-log-next-cycle");
        let paths = DocPackPaths::new(root);

        assert_eq!(next_cycle_number(&paths).unwrap(), 1);

        let entry = LmLogBuilder::new(5, LmInvocationKind::Behavior)
            .with_items(vec![])
            .success("done");
        append_lm_log(&paths, &entry).unwrap();

        assert_eq!(next_cycle_number(&paths).unwrap(), 6);
    }

    #[test]
    fn test_store_content_writes_files() {
        let root = test_paths("bman-lm-log-content");
        let paths = DocPackPaths::new(root);

        let prompt = "Test prompt content";
        let response = "Test response content";

        store_lm_content(&paths, 1, LmInvocationKind::Behavior, prompt, response).unwrap();

        // Verify files were written
        let log_dir = paths.lm_log_dir();
        let prompt_path = log_dir.join("cycle_001_behavior_prompt.txt");
        let response_path = log_dir.join("cycle_001_behavior_response.txt");

        assert!(prompt_path.exists());
        assert!(response_path.exists());
        assert_eq!(std::fs::read_to_string(&prompt_path).unwrap(), prompt);
        assert_eq!(std::fs::read_to_string(&response_path).unwrap(), response);
    }
}
