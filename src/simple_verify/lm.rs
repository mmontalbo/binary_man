//! LM invocation and response types for simplified verification.
//!
//! The LM contract is intentionally simple: we send state context, the LM
//! returns a list of actions. No complex protocol negotiation, just JSON in/out.

use super::types::Seed;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Maximum number of retry attempts for malformed LM responses.
const MAX_LM_RETRIES: usize = 2;

/// Response from the LM containing actions to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LmResponse {
    /// Actions to execute in order.
    pub actions: Vec<LmAction>,
}

/// A single action the LM wants to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum LmAction {
    /// Set the baseline scenario (required first, exactly once).
    SetBaseline {
        /// Additional arguments appended to base command (usually empty).
        args: Vec<String>,
        /// Seed setup for the baseline.
        seed: Seed,
    },
    /// Test a specific surface.
    Test {
        /// Which surface to test.
        surface_id: String,
        /// Arguments appended to base command (system prepends context_argv).
        args: Vec<String>,
        /// Seed setup for this test.
        seed: Seed,
    },
    /// Exclude a surface from verification.
    Exclude {
        /// Which surface to exclude.
        surface_id: String,
        /// Why it can't be verified.
        reason: String,
    },
}

/// Invoke the LM with a prompt and parse the response.
///
/// Implements retry logic for parse errors, including the error context
/// in retry prompts so the LM can fix its response.
pub fn invoke_lm(command: &str, prompt: &str) -> Result<LmResponse> {
    let mut last_error: Option<String> = None;
    let mut last_response: Option<String> = None;

    for attempt in 0..=MAX_LM_RETRIES {
        let actual_prompt = if attempt == 0 {
            prompt.to_string()
        } else {
            eprintln!(
                "  LM retry {}/{} (previous response had error)",
                attempt, MAX_LM_RETRIES
            );
            build_retry_prompt(
                prompt,
                last_error.as_deref().unwrap_or("unknown error"),
                last_response.as_deref(),
            )
        };

        let response_text = match invoke_lm_command(command, &actual_prompt) {
            Ok(text) => text,
            Err(e) => {
                // Command execution error - don't retry, likely a config issue
                return Err(e);
            }
        };

        match parse_lm_response(&response_text) {
            Ok(response) => {
                if attempt > 0 {
                    eprintln!("  LM retry succeeded");
                }
                return Ok(response);
            }
            Err(e) => {
                last_error = Some(e.to_string());
                last_response = Some(response_text);
            }
        }
    }

    Err(anyhow!(
        "LM failed after {} attempts. Last error: {}",
        MAX_LM_RETRIES + 1,
        last_error.unwrap_or_else(|| "unknown".to_string())
    ))
}

/// Invoke the LM command with stdin/stdout.
fn invoke_lm_command(command: &str, prompt: &str) -> Result<String> {
    let args =
        shell_words::split(command).with_context(|| format!("parse LM command: {command}"))?;

    if args.is_empty() {
        return Err(anyhow!("LM command is empty"));
    }

    let start = std::time::Instant::now();
    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn LM command: {}", args[0]))?;

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

/// Parse the LM response text into an LmResponse.
fn parse_lm_response(text: &str) -> Result<LmResponse> {
    let json_text = extract_json(text);
    let fixed_json = fix_common_typos(&json_text);

    serde_json::from_str(&fixed_json).with_context(|| {
        let truncated = truncate_safe(text, 500);
        format!("parse LM response as JSON. First 500 chars: {truncated}")
    })
}

/// Extract JSON from text that might have markdown code fences.
fn extract_json(text: &str) -> String {
    let text = text.trim();

    let extracted = if let Some(start) = text.find("```json") {
        let start = start + 7;
        if let Some(end) = text[start..].find("```") {
            text[start..start + end].trim()
        } else {
            text
        }
    } else if let Some(start) = text.find("```") {
        let start = start + 3;
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

    // Fix missing opening brace
    let trimmed = extracted.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('}') {
        return format!("{{{}", trimmed);
    }

    extracted.to_string()
}

/// Fix common LM typos in JSON responses.
fn fix_common_typos(json: &str) -> String {
    json.replace("outputs_differs", "outputs_differ")
        .replace("\"SetBase\"", "\"SetBaseline\"")
        .replace("\"set_baseline\"", "\"SetBaseline\"")
        .replace("\"test\"", "\"Test\"")
        .replace("\"exclude\"", "\"Exclude\"")
}

/// Build a retry prompt that includes the error context.
fn build_retry_prompt(
    original_prompt: &str,
    error: &str,
    previous_response: Option<&str>,
) -> String {
    let mut prompt = String::new();

    prompt.push_str("## Previous Response Error\n\n");
    prompt.push_str("Your previous response could not be parsed. Please fix the error.\n\n");
    prompt.push_str(&format!("**Error:** {error}\n\n"));

    if let Some(resp) = previous_response {
        let snippet = truncate_safe(resp, 1000);
        prompt.push_str(&format!(
            "**Your previous response (may be truncated):**\n```\n{snippet}\n```\n\n"
        ));
    }

    prompt.push_str("## Original Task\n\n");
    prompt.push_str(original_prompt);

    prompt
}

/// Truncate a string safely without splitting UTF-8 characters.
fn truncate_safe(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Log a prompt to the LM log directory.
pub fn log_prompt(pack_path: &Path, cycle: u32, prompt: &str) -> Result<()> {
    let log_dir = pack_path.join("lm_log");
    std::fs::create_dir_all(&log_dir).context("create lm_log directory")?;

    let path = log_dir.join(format!("c{cycle}_prompt.md"));
    std::fs::write(&path, prompt).with_context(|| format!("write prompt to {}", path.display()))
}

/// Log an LM response to the LM log directory.
pub fn log_response(pack_path: &Path, cycle: u32, response: &LmResponse) -> Result<()> {
    let log_dir = pack_path.join("lm_log");
    std::fs::create_dir_all(&log_dir).context("create lm_log directory")?;

    let path = log_dir.join(format!("c{cycle}_response.json"));
    let content = serde_json::to_string_pretty(response).context("serialize response")?;
    std::fs::write(&path, content).with_context(|| format!("write response to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_response() {
        let json = r#"{
            "actions": [
                {
                    "kind": "SetBaseline",
                    "args": [],
                    "seed": {"setup": [["git", "init"]], "files": []}
                },
                {
                    "kind": "Test",
                    "surface_id": "--stat",
                    "args": ["--stat"],
                    "seed": {"setup": [["git", "init"]], "files": []}
                }
            ]
        }"#;

        let response = parse_lm_response(json).unwrap();
        assert_eq!(response.actions.len(), 2);

        match &response.actions[0] {
            LmAction::SetBaseline { args, .. } => {
                assert!(args.is_empty());
            }
            _ => panic!("Expected SetBaseline"),
        }

        match &response.actions[1] {
            LmAction::Test {
                surface_id, args, ..
            } => {
                assert_eq!(surface_id, "--stat");
                assert_eq!(args, &["--stat"]);
            }
            _ => panic!("Expected Test"),
        }
    }

    #[test]
    fn test_parse_exclude_action() {
        let json = r#"{
            "actions": [
                {
                    "kind": "Exclude",
                    "surface_id": "--gpg-sign",
                    "reason": "Requires GPG key setup"
                }
            ]
        }"#;

        let response = parse_lm_response(json).unwrap();
        assert_eq!(response.actions.len(), 1);

        match &response.actions[0] {
            LmAction::Exclude { surface_id, reason } => {
                assert_eq!(surface_id, "--gpg-sign");
                assert!(reason.contains("GPG"));
            }
            _ => panic!("Expected Exclude"),
        }
    }

    #[test]
    fn test_extract_json_with_fences() {
        let text = r#"Here's my response:
```json
{"actions": []}
```
"#;
        let extracted = extract_json(text);
        assert_eq!(extracted, r#"{"actions": []}"#);
    }

    #[test]
    fn test_extract_json_plain() {
        let text = r#"{"actions": []}"#;
        let extracted = extract_json(text);
        assert_eq!(extracted, r#"{"actions": []}"#);
    }
}
