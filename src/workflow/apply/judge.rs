//! Post-execution behavior judgment.
//!
//! Adds a judgment step after scenarios pass `outputs_differ` to verify that
//! the output actually demonstrates the documented behavior, not just that
//! it's different from baseline.
//!
//! # Why Judgment?
//!
//! `outputs_differ` is necessary but not sufficient for verification:
//!
//! ```text
//! --show-stash scenario: git init → run → outputs differ slightly → ✓ verified
//! Reality: No stash existed, so no stash info shown - option behavior not triggered
//! ```
//!
//! The judgment step asks: "Does this output demonstrate the described behavior?"
//!
//! # Workflow Integration
//!
//! ```text
//! 1. LM proposes scenario (existing)
//! 2. bman runs scenario (existing)
//! 3. outputs_differ? NO → retry (no judgment needed)
//! 4. outputs_differ? YES → call judge LM
//! 5. Judge: demonstrates_behavior?
//!    - YES → VERIFIED
//!    - NO  → store feedback, retry (max 3)
//! 6. Max retries exhausted → UNVERIFIABLE
//! ```

use crate::enrich;
use crate::scenarios;
use crate::verification_progress::{load_verification_progress, write_verification_progress};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Result of a behavior judgment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgmentResult {
    /// Whether the output demonstrates the described behavior.
    pub demonstrates_behavior: bool,
    /// Brief explanation of the judgment.
    pub reason: String,
    /// Suggested setup commands if behavior not demonstrated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_setup: Option<Vec<String>>,
}

/// Context for invoking behavior judgment.
#[derive(Debug)]
pub struct JudgmentContext {
    /// The option ID being judged (e.g., "--show-stash")
    pub option_id: String,
    /// Description of what the option should do
    pub description: String,
    /// The command line that was executed
    pub command_line: String,
    /// Exit code of the command
    pub exit_code: i32,
    /// Stdout from the variant (option) run
    pub variant_stdout: String,
    /// Stderr from the variant run (if any)
    pub variant_stderr: Option<String>,
}

/// Prompt template loaded at compile time.
const JUDGE_PROMPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/judge_behavior.md"
));

/// Invoke the LM to judge behavior.
///
/// Returns the judgment result or an error if the LM fails.
pub fn invoke_judge(lm_command: &str, context: &JudgmentContext) -> Result<JudgmentResult> {
    let prompt = build_judge_prompt(context);
    let response = invoke_lm_command(lm_command, &prompt)?;
    parse_judgment_response(&response)
}

/// Build the judge prompt from context.
fn build_judge_prompt(context: &JudgmentContext) -> String {
    let stderr_section = if let Some(stderr) = &context.variant_stderr {
        if !stderr.trim().is_empty() {
            format!("### Stderr\n\n```\n{}\n```\n", truncate_output(stderr, 500))
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    JUDGE_PROMPT
        .replace("{option_id}", &context.option_id)
        .replace("{description}", &context.description)
        .replace("{command_line}", &context.command_line)
        .replace("{exit_code}", &context.exit_code.to_string())
        .replace(
            "{variant_stdout}",
            &truncate_output(&context.variant_stdout, 2000),
        )
        .replace("{stderr_section}", &stderr_section)
}

/// Truncate output to a maximum length with ellipsis.
fn truncate_output(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...(truncated)", &s[..max_len])
    }
}

/// Invoke the LM command with the given prompt.
fn invoke_lm_command(command: &str, prompt: &str) -> Result<String> {
    let args =
        shell_words::split(command).with_context(|| format!("parse LM command: {command}"))?;

    if args.is_empty() {
        return Err(anyhow!("LM command is empty"));
    }

    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn LM command: {}", args[0]))?;

    // Write prompt to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .context("write prompt to LM stdin")?;
    }

    let output = child.wait_with_output().context("wait for LM command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "LM command failed with status {}: {}",
            output.status,
            stderr.trim()
        ));
    }

    String::from_utf8(output.stdout).context("decode LM stdout as UTF-8")
}

/// Parse the judgment response from LM output.
fn parse_judgment_response(text: &str) -> Result<JudgmentResult> {
    let json_text = extract_json(text);

    serde_json::from_str(json_text)
        .with_context(|| format!("parse judgment response: {}", &text[..text.len().min(200)]))
}

/// Extract JSON from text that might have markdown code fences.
fn extract_json(text: &str) -> &str {
    let text = text.trim();

    if let Some(start) = text.find("```json") {
        let start = start + 7;
        if let Some(end) = text[start..].find("```") {
            return text[start..start + end].trim();
        }
    } else if let Some(start) = text.find("```") {
        let start = start + 3;
        let start = text[start..]
            .find('\n')
            .map(|i| start + i + 1)
            .unwrap_or(start);
        if let Some(end) = text[start..].find("```") {
            return text[start..start + end].trim();
        }
    }

    text
}

// JudgmentFeedback is now imported from crate::verification_progress

/// Arguments for running post-apply judgment.
pub struct JudgmentArgs<'a> {
    pub paths: &'a enrich::DocPackPaths,
    pub lm_command: &'a str,
    pub verbose: bool,
}

/// Run post-apply judgment on delta_seen entries.
///
/// For each entry that passed `outputs_differ`, check if judgment is needed
/// and invoke the judge LM. Returns the number of entries that failed judgment.
pub fn run_post_apply_judgment(args: &JudgmentArgs<'_>) -> Result<usize> {
    // Load verification cache
    let cache_path = args.paths.root().join("inventory/verification_cache.json");
    if !cache_path.exists() {
        return Ok(0);
    }

    #[derive(Deserialize)]
    struct Cache {
        ledger: scenarios::VerificationLedger,
    }

    let content = std::fs::read_to_string(&cache_path)?;
    let cache: Cache = serde_json::from_str(&content)?;

    // Load surface inventory for descriptions
    let surface_path = args.paths.root().join("inventory/surface.json");
    let surface_map = load_surface_descriptions(&surface_path)?;

    // Load existing progress (consolidated file)
    let mut progress = load_verification_progress(args.paths);

    // Find delta_seen entries that need judgment
    let mut failed_count = 0;
    for entry in &cache.ledger.entries {
        // Skip if not delta_seen
        if entry.delta_outcome.as_deref() != Some("delta_seen") {
            continue;
        }

        // Skip if already passed judgment
        if progress.has_judgment_passed(&entry.surface_id) {
            continue;
        }

        // Skip if already unverifiable
        if progress
            .judgment_unverifiable
            .contains_key(&entry.surface_id)
        {
            continue;
        }

        // Get description for this surface
        let description = surface_map
            .get(&entry.surface_id)
            .cloned()
            .unwrap_or_default();

        // Load scenario evidence to get output
        let scenario_output = load_scenario_output(args.paths, entry)?;
        let Some(output) = scenario_output else {
            if args.verbose {
                eprintln!(
                    "  judgment: skipped {} (no scenario output)",
                    entry.surface_id
                );
            }
            continue;
        };

        // Build judgment context
        let context = JudgmentContext {
            option_id: entry.surface_id.clone(),
            description: description.clone(),
            command_line: output.command_line,
            exit_code: output.exit_code,
            variant_stdout: output.stdout,
            variant_stderr: output.stderr,
        };

        // Invoke judgment
        if args.verbose {
            eprintln!("  judgment: evaluating {}", entry.surface_id);
        }

        match invoke_judge(args.lm_command, &context) {
            Ok(result) => {
                if result.demonstrates_behavior {
                    progress.record_judgment_pass(&entry.surface_id);
                    if args.verbose {
                        eprintln!("    ✓ passed: {}", result.reason);
                    }
                } else {
                    progress.record_judgment_failure(
                        &entry.surface_id,
                        &result.reason,
                        result.suggested_setup,
                    );
                    failed_count += 1;
                    if args.verbose {
                        eprintln!("    ✗ failed: {}", result.reason);
                    }
                }
            }
            Err(e) => {
                if args.verbose {
                    eprintln!("    ✗ error: {}", e);
                }
                // Treat LM errors as judgment failures
                progress.record_judgment_failure(
                    &entry.surface_id,
                    &format!("judgment error: {}", e),
                    None,
                );
                failed_count += 1;
            }
        }
    }

    // Save progress (consolidated file)
    write_verification_progress(args.paths, &progress)?;

    Ok(failed_count)
}

/// Scenario output for judgment.
struct ScenarioOutput {
    command_line: String,
    exit_code: i32,
    stdout: String,
    stderr: Option<String>,
}

/// Load scenario output for a verification entry.
fn load_scenario_output(
    paths: &enrich::DocPackPaths,
    entry: &scenarios::VerificationEntry,
) -> Result<Option<ScenarioOutput>> {
    // Find the most recent behavior scenario evidence
    let Some(scenario_path) = entry.behavior_scenario_paths.first() else {
        return Ok(None);
    };

    let evidence_path = paths.root().join(scenario_path);
    if !evidence_path.exists() {
        return Ok(None);
    }

    #[derive(Deserialize)]
    struct Evidence {
        argv: Vec<String>,
        exit_code: i32,
        stdout: String,
        #[serde(default)]
        stderr: Option<String>,
    }

    let content = std::fs::read_to_string(&evidence_path)?;
    let evidence: Evidence = serde_json::from_str(&content)?;

    Ok(Some(ScenarioOutput {
        command_line: evidence.argv.join(" "),
        exit_code: evidence.exit_code,
        stdout: evidence.stdout,
        stderr: evidence.stderr,
    }))
}

/// Load surface descriptions from inventory.
fn load_surface_descriptions(path: &Path) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    #[derive(Deserialize)]
    struct Surface {
        items: Vec<SurfaceItem>,
    }

    #[derive(Deserialize)]
    struct SurfaceItem {
        id: String,
        #[serde(default)]
        description: Option<String>,
    }

    let content = std::fs::read_to_string(path)?;
    let surface: Surface = serde_json::from_str(&content)?;

    Ok(surface
        .items
        .into_iter()
        .filter_map(|item| item.description.map(|d| (item.id, d)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_judgment_positive() {
        let text = r#"{"demonstrates_behavior": true, "reason": "Output shows expected data"}"#;
        let result = parse_judgment_response(text).unwrap();
        assert!(result.demonstrates_behavior);
        assert_eq!(result.reason, "Output shows expected data");
        assert!(result.suggested_setup.is_none());
    }

    #[test]
    fn test_parse_judgment_negative_with_setup() {
        let text = r#"{
            "demonstrates_behavior": false,
            "reason": "No stash exists to display",
            "suggested_setup": ["git stash push -m test"]
        }"#;
        let result = parse_judgment_response(text).unwrap();
        assert!(!result.demonstrates_behavior);
        assert!(result.suggested_setup.is_some());
        assert_eq!(result.suggested_setup.unwrap().len(), 1);
    }

    #[test]
    fn test_parse_judgment_with_code_fence() {
        let text = r#"Here's my judgment:
```json
{"demonstrates_behavior": true, "reason": "Looks good"}
```
"#;
        let result = parse_judgment_response(text).unwrap();
        assert!(result.demonstrates_behavior);
    }

    #[test]
    fn test_truncate_output_short() {
        let s = "short text";
        assert_eq!(truncate_output(s, 100), "short text");
    }

    #[test]
    fn test_truncate_output_long() {
        let s = "a".repeat(200);
        let result = truncate_output(&s, 50);
        assert!(result.ends_with("...(truncated)"));
        assert!(result.len() < 100);
    }
}
