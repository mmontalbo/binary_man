//! Local LM client for behavior verification.
//!
//! Invokes a local LM binary with a prompt on stdin and parses the JSON response
//! from stdout. The LM command can be any tool that accepts text input and
//! produces text output (e.g., `llm`, `ollama run`, custom scripts).
//!
//! # Why a Local LM Client
//!
//! Rather than embedding API keys or requiring specific LM providers, this module
//! delegates to a user-configured command. This enables:
//!
//! - **Provider flexibility**: Works with any LM (local or cloud)
//! - **Cost control**: User chooses their own model/pricing
//! - **Privacy**: No data leaves the machine if using local models
//! - **Customization**: User can wrap with caching, logging, etc.
//!
//! # Prompt Protocol
//!
//! The LM receives a structured prompt on stdin:
//!
//! ```text
//! # Context
//! Binary: ls
//! Man page excerpts for relevant options...
//!
//! # Decision Items
//! [JSON array of items needing scenarios]
//!
//! # Response Format
//! [Instructions for JSON response schema]
//! ```
//!
//! The LM must respond with a JSON `LmResponseBatch` on stdout.
//!
//! # Error Recovery
//!
//! The client implements retry logic for common LM failures:
//!
//! 1. **Parse errors**: Retry with error message included in prompt
//! 2. **Validation errors**: Retry with specific field errors
//! 3. **Timeout/crash**: Propagate error to caller
//!
//! This helps recover from LMs that occasionally produce malformed JSON.
//!
//! # Configuration
//!
//! The LM command is resolved in priority order:
//! 1. `--lm` CLI flag
//! 2. `lm_command` in `enrich/config.json`
//! 3. `BMAN_LM_COMMAND` environment variable

use crate::enrich::{BehaviorNextActionPayload, LearnedHints, StatusSummary};
use crate::workflow::lm_response::LmResponseBatch;
use anyhow::{anyhow, Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

/// Result of an LM invocation with metadata for logging.
#[derive(Debug)]
pub struct LmInvocationResult<T> {
    /// The parsed result.
    pub result: T,
    /// The prompt sent to the LM.
    pub prompt: String,
    /// The raw response text from the LM.
    pub raw_response: String,
}

/// Configuration for the local LM client.
#[derive(Debug, Clone)]
pub struct LmClientConfig {
    /// The command to invoke (parsed via shell-words).
    pub command: String,
}

/// Maximum number of retry attempts for LM invocation.
const MAX_LM_RETRIES: usize = 2;

// Prompt templates loaded at compile time
const BEHAVIOR_BASE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/behavior_base.md"
));
const REASON_UNIFIED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/behavior_reason_unified.md"
));

/// Invoke the LM to generate responses for behavior verification.
///
/// Automatically retries on parse errors, including the error context
/// in the retry prompt so the LM can fix its response.
///
/// Returns the parsed batch along with prompt/response metadata for logging.
pub fn invoke_lm_for_behavior(
    config: &LmClientConfig,
    summary: &StatusSummary,
    payload: &BehaviorNextActionPayload,
    hints: Option<&LearnedHints>,
) -> Result<LmInvocationResult<LmResponseBatch>> {
    let binary_name = summary.binary_name.as_deref().unwrap_or("<binary>");

    let mut last_error: Option<String> = None;
    let mut last_response: Option<String> = None;
    let mut final_prompt;

    for attempt in 0..=MAX_LM_RETRIES {
        // Build prompt - include error context on retry
        let prompt = if attempt == 0 {
            build_behavior_prompt(summary, payload, hints)
        } else {
            eprintln!(
                "  LM retry {}/{} (previous response had error)",
                attempt, MAX_LM_RETRIES
            );
            build_retry_prompt(
                summary,
                payload,
                last_error.as_deref().unwrap_or("unknown error"),
                last_response.as_deref(),
            )
        };
        final_prompt = prompt.clone();

        // Invoke LM
        let response_text = match invoke_lm_command(&config.command, &prompt) {
            Ok(text) => text,
            Err(e) => {
                // Command execution error - don't retry, likely a config issue
                return Err(e);
            }
        };

        // Try to parse the response
        match parse_lm_response(&response_text, binary_name) {
            Ok(batch) => {
                if attempt > 0 {
                    eprintln!("  LM retry succeeded");
                }
                return Ok(LmInvocationResult {
                    result: batch,
                    prompt: final_prompt,
                    raw_response: response_text,
                });
            }
            Err(e) => {
                last_error = Some(e.to_string());
                last_response = Some(response_text);
                // Continue to next attempt (or fall through to final error)
            }
        }
    }

    // All attempts failed
    Err(anyhow!(
        "LM failed after {} attempts. Last error: {}",
        MAX_LM_RETRIES + 1,
        last_error.unwrap_or_else(|| "unknown".to_string())
    ))
}

/// Build the prompt for behavior verification.
fn build_behavior_prompt(
    summary: &StatusSummary,
    payload: &BehaviorNextActionPayload,
    hints: Option<&LearnedHints>,
) -> String {
    let binary_name = summary.binary_name.as_deref().unwrap_or("<binary>");
    let reason_code = payload.reason_code.as_deref().unwrap_or("unknown");

    // Build state context from reason code
    let state_context = build_state_context(reason_code, payload);

    // Build hints section from learned patterns
    let hints_section = build_hints_section(hints);

    // Assemble reason section from unified template
    let reason_section = REASON_UNIFIED
        .replace("{reason_code}", reason_code)
        .replace("{state_context}", &state_context)
        .replace("{hints_section}", &hints_section);

    // Build context section (scaffold hints, value requirements)
    let context_section = build_context_section(payload);

    // Build target list with scenario output when available
    let targets = build_targets_section(payload);

    // Assemble prompt from template
    BEHAVIOR_BASE
        .replace("{binary_name}", binary_name)
        .replace("{reason_code}", reason_code)
        .replace("{reason_section}", &reason_section)
        .replace("{context_section}", &context_section)
        .replace("{targets}", &targets)
}

/// Build state-specific context based on reason code.
fn build_state_context(reason_code: &str, payload: &BehaviorNextActionPayload) -> String {
    match reason_code {
        "initial_scenarios" => {
            "Generate scenarios for ALL options. Each needs a scenario OR exclusion.".to_string()
        }
        "no_scenario" => {
            "No scenario exists. Create one based on the option description, or exclude if untestable.".to_string()
        }
        "outputs_equal" => {
            let retry_count = payload.retry_count.unwrap_or(0);
            if retry_count > 0 {
                format!(
                    "Output still matches baseline after {} retries. Try a different approach or exclude with context.",
                    retry_count
                )
            } else {
                "Output matches baseline - no observable difference. Fix by:\n\
                 - Add `seed` files the option needs\n\
                 - Add `stdin` for filter commands\n\
                 - Use assertions (`file_exists`, `exit_code`) instead of stdout\n\
                 - Include action/subcommand in argv\n\
                 - Exclude if truly untestable".to_string()
            }
        }
        "assertion_failed" => {
            "Assertion failed. Fix the assertion, fix the scenario, or exclude if unpredictable.".to_string()
        }
        "setup_failed" => {
            "Setup commands in `seed.setup` failed before the main command ran. Check the scenario and fix or simplify the setup commands.".to_string()
        }
        "scenario_error" => {
            "Scenario configuration is invalid. Check argv, seed, and assertion syntax.".to_string()
        }
        "missing_value_examples" => {
            "Option requires a value but no examples exist. Add value_examples to the overlay.".to_string()
        }
        _ => format!("Handle these items based on the reason code: {reason_code}"),
    }
}

/// Build hints section from learned patterns.
fn build_hints_section(hints: Option<&LearnedHints>) -> String {
    let Some(hints) = hints else {
        return String::new();
    };

    if hints.working_argvs.is_empty() {
        return String::new();
    }

    let mut section = String::from("## Learned Patterns\n\n");
    section.push_str("These argvs successfully verified similar options:\n\n");

    // Show up to 5 examples
    for (surface_id, argv) in hints.working_argvs.iter().take(5) {
        let argv_json = serde_json::to_string(argv).unwrap_or_else(|_| "[]".to_string());
        section.push_str(&format!("- `{}`: `{}`\n", surface_id, argv_json));
    }

    if hints.working_argvs.len() > 5 {
        section.push_str(&format!(
            "\n({} more patterns available)\n",
            hints.working_argvs.len() - 5
        ));
    }

    section.push('\n');
    section
}

/// Build the targets section, including scenario output for error feedback.
fn build_targets_section(payload: &BehaviorNextActionPayload) -> String {
    use std::collections::HashMap;

    // Index scenario output by surface_id
    let output_map: HashMap<&str, &crate::enrich::TargetScenarioOutput> = payload
        .target_scenario_output
        .iter()
        .map(|o| (o.surface_id.as_str(), o))
        .collect();

    // Index judgment feedback by surface_id
    let judgment_map: HashMap<&str, &crate::enrich::TargetJudgmentFeedback> = payload
        .target_judgment_feedback
        .iter()
        .map(|f| (f.surface_id.as_str(), f))
        .collect();

    payload
        .target_ids
        .iter()
        .map(|id| {
            let mut line = format!("- `{id}`");

            // Add scenario output if available
            if let Some(output) = output_map.get(id.as_str()) {
                if output.setup_failed {
                    // Setup commands failed before main command ran
                    line.push_str(" (**setup_failed**: seed.setup commands failed - check scenario in plan.json)");
                } else if let Some(exit_code) = output.exit_code {
                    line.push_str(&format!(" (exit_code: {exit_code}"));
                    if let Some(stderr) = &output.stderr_preview {
                        // Truncate and escape for display
                        let stderr_short = if stderr.len() > 100 {
                            format!("{}...", &stderr[..100])
                        } else {
                            stderr.clone()
                        };
                        let stderr_escaped = stderr_short.replace('\n', " ");
                        line.push_str(&format!(", stderr: \"{stderr_escaped}\""));
                    }
                    line.push(')');
                }
            }

            // Add judgment feedback if this target failed judgment
            if let Some(feedback) = judgment_map.get(id.as_str()) {
                line.push_str(&format!(
                    "\n  **Previous Attempt Failed**: \"{}\"\n",
                    feedback.reason
                ));
                if let Some(setup) = &feedback.suggested_setup {
                    if !setup.is_empty() {
                        line.push_str(&format!("  Suggested setup: {}\n", setup.join("; ")));
                    }
                }
                line.push_str(&format!(
                    "  (attempt {}/3 - please propose an improved scenario)\n",
                    feedback.failure_count
                ));
            }

            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build the context section with scaffold hints and value requirements.
fn build_context_section(payload: &BehaviorNextActionPayload) -> String {
    let mut context = String::new();

    if let Some(ctx) = &payload.scaffold_context {
        if let Some(guidance) = &ctx.guidance {
            context.push_str(&format!("## Guidance\n{guidance}\n\n"));
        }

        // For initial_scenarios, include all surface items with descriptions
        if !ctx.all_surface_items.is_empty() {
            context.push_str("## Surface Items\n\n");
            for item in &ctx.all_surface_items {
                context.push_str(&format!("### `{}`\n", item.id));
                if let Some(desc) = &item.description {
                    context.push_str(&format!("- Description: {}\n", desc));
                }
                if let Some(placeholder) = &item.value_placeholder {
                    context.push_str(&format!(
                        "- Value required (placeholder: {})\n",
                        placeholder
                    ));
                }
                context.push('\n');
            }
        }

        if !ctx.value_required.is_empty() {
            context.push_str("## Options Requiring Values\n");
            for hint in &ctx.value_required {
                context.push_str(&format!(
                    "- `{}` (placeholder: {}): {}\n",
                    hint.option_id, hint.placeholder, hint.description
                ));
            }
            context.push('\n');
        }
    }

    context
}

/// Build a retry prompt that includes the error from the previous attempt.
fn build_retry_prompt(
    summary: &StatusSummary,
    payload: &BehaviorNextActionPayload,
    error: &str,
    previous_response: Option<&str>,
) -> String {
    let binary_name = summary.binary_name.as_deref().unwrap_or("<binary>");

    let mut prompt = String::new();

    // Error context
    prompt.push_str(&format!(
        r#"You are helping verify behavior documentation for the `{binary_name}` command.

## Previous Response Error

Your previous response could not be parsed. Please fix the error and try again.

**Error:** {error}

"#
    ));

    // Show snippet of previous response if available
    if let Some(resp) = previous_response {
        let snippet = if resp.len() > 1000 {
            format!("{}...(truncated)", &resp[..1000])
        } else {
            resp.to_string()
        };
        prompt.push_str(&format!(
            "**Your previous response (may be truncated):**\n```\n{}\n```\n\n",
            snippet
        ));
    }

    // Add the original task context
    prompt.push_str("## Original Task\n\n");

    // Add target IDs
    prompt.push_str("Generate responses for these options:\n");
    for id in &payload.target_ids {
        prompt.push_str(&format!("- `{id}`\n"));
    }

    // Reminder about format
    prompt.push_str(
        r#"
## Response Format Reminder

Use the simplified `add_behavior_scenario` format:
```json
{
  "responses": [
    {
      "surface_id": "--option",
      "action": {
        "kind": "add_behavior_scenario",
        "argv": ["--option"],
        "seed": {
          "files": {"name": "content"},
          "dirs": ["dirname"],
          "symlinks": {"link": "target"},
          "executables": {"script.sh": "content"}
        }
      }
    }
  ]
}
```

Common issues to avoid:
- Missing `surface_id` field
- Using wrong action kind - use `add_behavior_scenario` not `add_scenario`
- JavaScript expressions - use literal strings only
- Invalid JSON syntax

Respond ONLY with the corrected JSON object, no other text.
"#,
    );

    prompt
}

/// Invoke the LM command with the given prompt.
fn invoke_lm_command(command: &str, prompt: &str) -> Result<String> {
    let args =
        shell_words::split(command).with_context(|| format!("parse LM command: {command}"))?;

    if args.is_empty() {
        return Err(anyhow!("LM command is empty"));
    }

    let start = Instant::now();
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
    let elapsed_ms = start.elapsed().as_millis();

    tracing::info!(
        elapsed_ms,
        prompt_bytes = prompt.len(),
        response_bytes = output.stdout.len(),
        "lm invoke complete"
    );

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

/// Truncate a string to at most `max_bytes` without splitting multi-byte UTF-8 characters.
fn truncate_to_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Find the last character boundary at or before max_bytes
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Parse the LM response text into an LmResponseBatch.
fn parse_lm_response(text: &str, binary_name: &str) -> Result<LmResponseBatch> {
    // Try to extract JSON from the response (LM might include markdown fences)
    let json_text = extract_json(text);

    // Fix common LM typos before parsing
    let fixed_json = fix_common_typos(&json_text);

    let mut batch: LmResponseBatch = match serde_json::from_str(&fixed_json) {
        Ok(b) => b,
        Err(e) => {
            // Try to show where parsing failed
            let line = e.line();
            let col = e.column();
            let context = if !fixed_json.is_empty() {
                // Find the line in the JSON
                let lines: Vec<&str> = fixed_json.lines().collect();
                if line > 0 && line <= lines.len() {
                    let line_content = lines[line - 1];
                    format!("line {}: {}", line, line_content)
                } else {
                    format!("(line {} not found)", line)
                }
            } else {
                "(empty response)".to_string()
            };

            // Safely truncate to ~500 chars without splitting multi-byte characters
            let truncated = truncate_to_char_boundary(text, 500);

            return Err(anyhow::anyhow!(
                "parse LM response as JSON: {} at line {}, column {}\n\nContext: {}\n\nFirst 500 chars: {}",
                e,
                line,
                col,
                context,
                truncated
            ));
        }
    };

    // Sanitize to fix common LM mistakes
    sanitize_lm_response(&mut batch, binary_name);

    Ok(batch)
}

/// Fix common LM typos in JSON responses.
fn fix_common_typos(json: &str) -> String {
    // Fix typos in assertion kinds
    let mut result = json
        .replace("outputs_differs", "outputs_differ")
        .replace("\"stdout_contain\"", "\"stdout_contains\"")
        .replace("\"stdout_lack\"", "\"stdout_lacks\"")
        .replace("add_scenarios", "add_scenario")
        .replace("add_exclusions", "add_exclusion");

    // Fix JavaScript-style .repeat() expressions that LMs sometimes generate
    // e.g., "A".repeat(2048) -> "AAAA..." (up to 100 chars to keep files small)
    // Also handles expressions like .repeat(1024 * 1024) by capturing the whole expression
    let repeat_regex = regex::Regex::new(r#""([^"]+)"\.repeat\(\s*([^)]+)\s*\)"#).unwrap();
    result = repeat_regex
        .replace_all(&result, |caps: &regex::Captures| {
            let pattern = &caps[1];
            // Try to evaluate simple expressions like "1024 * 1024" or just "100"
            let expr = caps[2].trim();
            let count: usize = if let Ok(num) = expr.parse::<usize>() {
                num
            } else if let Some((a, b)) = expr.split_once('*') {
                // Handle "N * M" expressions
                let a: usize = a.trim().parse().unwrap_or(1);
                let b: usize = b.trim().parse().unwrap_or(1);
                a.saturating_mul(b)
            } else {
                100 // fallback
            };
            // Limit to 100 repetitions to avoid huge strings
            let limited_count = count.min(100);
            let repeated = pattern.repeat(limited_count);
            format!("\"{}\"", repeated)
        })
        .to_string();

    result
}

/// Sanitize an LM response batch to fix common issues.
fn sanitize_lm_response(batch: &mut LmResponseBatch, binary_name: &str) {
    use crate::workflow::lm_response::LmAction;

    for response in &mut batch.responses {
        if let LmAction::AddScenario { scenario } = &mut response.action {
            // Strip binary name from argv if LM included it (common mistake)
            if !scenario.argv.is_empty() && scenario.argv[0] == binary_name {
                scenario.argv.remove(0);
            }

            // Sanitize seed entries
            if let Some(ref mut seed) = scenario.seed {
                // Filter out invalid seed entries
                seed.entries.retain(|entry| {
                    let path = entry.path.trim();
                    // Remove empty paths, ".", "..", or paths starting with ".."
                    !path.is_empty() && path != "." && path != ".." && !path.starts_with("../")
                });

                // Remove duplicate paths (keep first occurrence)
                let mut seen_paths = std::collections::HashSet::new();
                seed.entries
                    .retain(|entry| seen_paths.insert(entry.path.clone()));

                // Fix mode values - LMs often use "644" meaning octal 0o644
                // but JSON parses it as decimal 644. Convert common patterns.
                for entry in &mut seed.entries {
                    if let Some(mode) = entry.mode {
                        let fixed_mode = match mode {
                            // Common "octal-looking" modes that are actually decimal
                            644 => 0o644,               // rw-r--r--
                            755 => 0o755,               // rwxr-xr-x
                            777 => 0o777,               // rwxrwxrwx
                            666 => 0o666,               // rw-rw-rw-
                            600 => 0o600,               // rw-------
                            700 => 0o700,               // rwx------
                            444 => 0o444,               // r--r--r--
                            555 => 0o555,               // r-xr-xr-x
                            _ if mode > 0o777 => 0o755, // Fallback
                            _ => mode,
                        };
                        entry.mode = Some(fixed_mode);
                    }
                }
            }
        }
    }
}

/// Extract JSON from text that might have markdown code fences.
/// Returns a Cow<str> because we may need to fix malformed JSON.
fn extract_json(text: &str) -> std::borrow::Cow<'_, str> {
    let text = text.trim();

    let extracted = if let Some(start) = text.find("```json") {
        // Try to find JSON in code fences
        let start = start + 7;
        if let Some(end) = text[start..].find("```") {
            text[start..start + end].trim()
        } else {
            text
        }
    } else if let Some(start) = text.find("```") {
        // Try plain code fences
        let start = start + 3;
        // Skip language identifier if present
        let start = text[start..]
            .find('\n')
            .map(|i| start + i + 1)
            .unwrap_or(start);
        if let Some(end) = text[start..].find("```") {
            text[start..start + end].trim()
        } else {
            text
        }
    } else {
        text
    };

    // Fix common LM issue: JSON missing opening brace
    // e.g., LM outputs `"definitions": {...}` instead of `{"definitions": {...}}`
    let trimmed = extracted.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('}') {
        // Likely missing opening brace
        return std::borrow::Cow::Owned(format!("{{{}", trimmed));
    }

    std::borrow::Cow::Borrowed(extracted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_plain() {
        let text = r#"{"responses": []}"#;
        assert_eq!(extract_json(text), r#"{"responses": []}"#);
    }

    #[test]
    fn test_extract_json_with_fences() {
        let text = r#"Here is the response:
```json
{"responses": []}
```
"#;
        assert_eq!(extract_json(text), r#"{"responses": []}"#);
    }

    #[test]
    fn test_extract_json_plain_fences() {
        let text = r#"```
{"responses": []}
```"#;
        assert_eq!(extract_json(text), r#"{"responses": []}"#);
    }

    #[test]
    fn test_extract_json_missing_brace() {
        // LM sometimes outputs JSON without opening brace
        let text = r#"```json
  "definitions": {},
  "surface_map": {}
}
```"#;
        // Should fix by adding opening brace
        let extracted = extract_json(text);
        assert!(
            extracted.starts_with('{'),
            "Should add missing brace: {}",
            extracted
        );
        // Should be parseable
        let _: serde_json::Value = serde_json::from_str(&extracted).expect("Should be valid JSON");
    }

    #[test]
    fn test_parse_lm_response() {
        let text = r#"{
            "schema_version": 1,
            "responses": [
                {
                    "surface_id": "--color",
                    "action": {
                        "kind": "add_value_examples",
                        "value_examples": ["always", "never"]
                    }
                }
            ]
        }"#;

        let batch = parse_lm_response(text, "test-binary").unwrap();
        assert_eq!(batch.responses.len(), 1);
        assert_eq!(batch.responses[0].surface_id, "--color");
    }
}
