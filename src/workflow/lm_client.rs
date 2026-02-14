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

use crate::enrich::{
    BehaviorNextActionPayload, FlatSeed, PrereqInferenceDefinition, PrereqsFile, StatusSummary,
    PREREQS_SCHEMA_VERSION,
};
use crate::workflow::lm_response::LmResponseBatch;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

/// Configuration for the local LM client.
#[derive(Debug, Clone)]
pub struct LmClientConfig {
    /// The command to invoke (parsed via shell-words).
    pub command: String,
}

/// Maximum number of retry attempts for LM invocation.
const MAX_LM_RETRIES: usize = 2;

/// Invoke the LM to generate responses for behavior verification.
///
/// Automatically retries on parse errors, including the error context
/// in the retry prompt so the LM can fix its response.
pub fn invoke_lm_for_behavior(
    config: &LmClientConfig,
    summary: &StatusSummary,
    payload: &BehaviorNextActionPayload,
) -> Result<LmResponseBatch> {
    let binary_name = summary.binary_name.as_deref().unwrap_or("<binary>");

    let mut last_error: Option<String> = None;
    let mut last_response: Option<String> = None;

    for attempt in 0..=MAX_LM_RETRIES {
        // Build prompt - include error context on retry
        let prompt = if attempt == 0 {
            build_behavior_prompt(summary, payload)
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
                return Ok(batch);
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
fn build_behavior_prompt(summary: &StatusSummary, payload: &BehaviorNextActionPayload) -> String {
    let binary_name = summary.binary_name.as_deref().unwrap_or("<binary>");

    let reason_code = payload.reason_code.as_deref().unwrap_or("unknown");

    let mut prompt = String::new();

    // System context
    prompt.push_str(&format!(
        r#"You are helping verify behavior documentation for the `{binary_name}` command.

## Task
Generate test scenarios or exclusions for the following unverified options.

## Reason Code: {reason_code}
"#
    ));

    // Add reason-specific guidance
    match reason_code {
        "no_scenario" => {
            prompt.push_str(r#"
These options have no test scenario. For each option, either:
1. Create a scenario that demonstrates the option's behavior, OR
2. Add an exclusion if the option cannot be tested (e.g., requires interactive TTY, has unsafe side effects)

When creating scenarios:
- Use realistic argument values based on the option's description
- Include assertions that verify the option changes the output
- The scenario should have a `covers` array including the option id
"#);
        }
        "outputs_equal" => {
            // Check if this is a retry (workarounds already attempted)
            let retry_count = payload.retry_count.unwrap_or(0);
            if retry_count > 0 {
                prompt.push_str(&format!(
                    r#"
These options still produce identical output after {} retry attempts.
Analyze the option description to determine what fixtures are needed. Common fixes:

1. **Update scenario with seed fixtures** - create files/dirs/symlinks that demonstrate the behavior
2. **Add exclusion** if the option cannot be tested (requires interactive TTY, system changes, etc.)

Use seed fixtures when the option's behavior depends on specific file types or contents.
"#,
                    retry_count
                ));
            } else {
                prompt.push_str(
                    r#"
These options produce identical output to the baseline. Analyze the option description to determine why:

1. **Option needs specific fixture files**: Update scenario with a `seed` field containing
   files, directories, or symlinks that demonstrate the behavioral difference.

2. **Option needs a specific value**: Add value_examples based on the option description.

3. **Option needs other flags**: Add requires_argv if the option only works with other options.

4. **Option effect not visible in text output**: Add exclusion with appropriate reason_code.

Prefer updating the scenario with seed fixtures before excluding.
"#,
                );
            }
        }
        "assertion_failed" => {
            prompt.push_str(
                r#"
These scenarios have failing assertions. Either:
1. Fix the assertions to match actual behavior
2. Add an exclusion if the behavior is not reliably testable
"#,
            );
        }
        _ => {
            prompt.push_str(&format!(
                "\nHandle these items based on the reason code: {reason_code}\n"
            ));
        }
    }

    // Add scaffold context if available
    if let Some(ctx) = &payload.scaffold_context {
        if let Some(guidance) = &ctx.guidance {
            prompt.push_str(&format!("\n## Guidance\n{guidance}\n"));
        }

        if !ctx.value_required.is_empty() {
            prompt.push_str("\n## Options Requiring Values\n");
            for hint in &ctx.value_required {
                prompt.push_str(&format!(
                    "- `{}` (placeholder: {}): {}\n",
                    hint.option_id, hint.placeholder, hint.description
                ));
            }
        }
    }

    // Add target IDs
    prompt.push_str("\n## Target Options\n");
    for id in &payload.target_ids {
        prompt.push_str(&format!("- `{id}`\n"));
    }

    // Note: assertion_starters are available but currently omitted from prompt
    // as they can confuse the LM into using invalid assertion kinds.
    // The outputs_differ assertion is sufficient for most cases.

    // Response format
    prompt.push_str(
        r#"
## Response Format
Respond with a JSON object containing a `responses` array. Each response must have:
- `surface_id`: The option id (e.g., "--verbose")
- `action`: One of the action types below

### Action Types

1. **add_behavior_scenario**: Create a test scenario (PREFERRED - simplified format)

Only specify `argv` and optionally `seed`. Everything else is auto-generated.

**Basic scenario (no fixtures):**
```json
{
  "kind": "add_behavior_scenario",
  "argv": ["--option-name"]
}
```

**Scenario with seed fixtures:**
```json
{
  "kind": "add_behavior_scenario",
  "argv": ["--all"],
  "seed": {
    "files": {".hidden": "secret", "visible.txt": "hello"},
    "dirs": ["subdir"],
    "symlinks": {"link": "visible.txt"}
  }
}
```

**IMPORTANT seed format:**
- `files`: Object mapping filename to content, e.g. `{"file.txt": "content"}`
- `dirs`: Array of directory names, e.g. `["dir1", "dir2"]`
- `symlinks`: Object mapping link name to target, e.g. `{"link": "file.txt"}`
- `executables`: Object mapping filename to content, e.g. `{"run.sh": "echo hi"}`

NOTE: `files` and `symlinks` are OBJECTS (key-value maps), NOT arrays!

2. **add_value_examples**: Specify valid values for an option
```json
{
  "kind": "add_value_examples",
  "value_examples": ["value1", "value2", "value3"]
}
```

3. **add_requires_argv**: Specify prerequisite flags
```json
{
  "kind": "add_requires_argv",
  "requires_argv": ["-l"]
}
```

4. **add_exclusion**: Mark as untestable with reason
```json
{
  "kind": "add_exclusion",
  "reason_code": "fixture_gap",
  "note": "Brief explanation why this cannot be tested"
}
```
**IMPORTANT**: The `note` field must be <= 200 characters. Keep it concise.
Valid reason codes: fixture_gap, assertion_gap, nondeterministic, requires_interactive_tty, unsafe_side_effects

5. **skip**: Skip for now (will retry later)
```json
{
  "kind": "skip",
  "reason": "Need more context"
}
```

## Example Response
```json
{
  "schema_version": 1,
  "responses": [
    {
      "surface_id": "--color",
      "action": {
        "kind": "add_value_examples",
        "value_examples": ["always", "never", "auto"]
      }
    },
    {
      "surface_id": "--interactive",
      "action": {
        "kind": "add_exclusion",
        "reason_code": "requires_interactive_tty",
        "note": "Requires TTY for prompts"
      }
    }
  ]
}
```

Respond ONLY with the JSON object, no other text.
"#,
    );

    prompt
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

/// Parse the LM response text into an LmResponseBatch.
fn parse_lm_response(text: &str, binary_name: &str) -> Result<LmResponseBatch> {
    // Try to extract JSON from the response (LM might include markdown fences)
    let json_text = extract_json(text);

    // Fix common LM typos before parsing
    let fixed_json = fix_common_typos(json_text);

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

            return Err(anyhow::anyhow!(
                "parse LM response as JSON: {} at line {}, column {}\n\nContext: {}\n\nFirst 500 chars: {}",
                e,
                line,
                col,
                context,
                &text[..text.len().min(500)]
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

// ============================================================================
// Prereq Inference
// ============================================================================

/// Surface item info passed to prereq inference.
#[derive(Debug, Clone)]
pub struct SurfaceItemInfo {
    pub id: String,
    pub description: Option<String>,
    pub forms: Vec<String>,
}

/// LM response format for prereq inference.
#[derive(Debug, Deserialize)]
struct LmPrereqResponse {
    #[serde(default)]
    definitions: BTreeMap<String, LmPrereqDefinition>,
    #[serde(default)]
    surface_map: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct LmPrereqDefinition {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    seed: Option<FlatSeed>,
    #[serde(default)]
    exclude: bool,
}

/// Invoke the LM to infer prereqs for surface items.
pub fn invoke_lm_for_prereqs(
    config: &LmClientConfig,
    binary_name: &str,
    existing_definitions: &BTreeMap<String, PrereqInferenceDefinition>,
    items: &[SurfaceItemInfo],
) -> Result<PrereqsFile> {
    let prompt = build_prereq_prompt(binary_name, existing_definitions, items);

    let response_text = invoke_lm_command(&config.command, &prompt)?;
    parse_prereq_response(&response_text)
}

/// Build prompt for prereq inference.
fn build_prereq_prompt(
    binary_name: &str,
    existing_definitions: &BTreeMap<String, PrereqInferenceDefinition>,
    items: &[SurfaceItemInfo],
) -> String {
    let mut prompt = String::new();

    prompt.push_str(&format!(
        r#"You are helping determine prerequisites for testing `{binary_name}` command options.

## Task
Analyze each option's documentation and determine what prerequisites it needs for auto-verification.

## Categories
- **filesystem**: needs specific directory/file structure (provide seed)
- **config**: needs config files present (provide seed)
- **state**: needs existing state like commits, staged files (provide seed)
- **interactive**: requires TTY/editor (exclude from auto-verify)
- **network**: requires network access (exclude from auto-verify)
- **privilege**: requires elevated permissions (exclude from auto-verify)
- **null**: no special requirements

"#
    ));

    // Add existing definitions as context
    if !existing_definitions.is_empty() {
        prompt.push_str("## Existing Prereq Definitions (reference these when applicable)\n");
        for (key, def) in existing_definitions {
            let desc = def.description.as_deref().unwrap_or("no description");
            prompt.push_str(&format!("- `{key}`: {desc}\n"));
        }
        prompt.push('\n');
    }

    // Add surface items to analyze
    prompt.push_str("## Surface Items to Analyze\n");
    for item in items {
        let desc = item.description.as_deref().unwrap_or("no description");
        let forms = if item.forms.is_empty() {
            String::new()
        } else {
            format!(" (forms: {})", item.forms.join(", "))
        };
        prompt.push_str(&format!("`{}`{}: {}\n", item.id, forms, desc));
    }

    prompt.push_str(
        r#"
## Output Format
Return a JSON object with:
1. `definitions`: New prereq definitions (only if no existing one fits)
2. `surface_map`: Mapping from option id to prereq keys (or empty array for no prereqs)

```json
{
  "definitions": {
    "git_repo": {
      "description": "git repository with .git directory",
      "seed": {"dirs": [".git"]},
      "exclude": false
    },
    "interactive": {
      "description": "requires interactive TTY",
      "seed": null,
      "exclude": true
    }
  },
  "surface_map": {
    "--edit": ["interactive"],
    "--local": ["git_repo"],
    "--global": []
  }
}
```

**Seed format:**
- `dirs`: Array of directory paths, e.g. `["dir1", "dir2"]`
- `files`: Object mapping path to content, e.g. `{"file.txt": "content"}`
- `symlinks`: Object mapping path to target, e.g. `{"link": "file.txt"}`
- `executables`: Object mapping path to content (mode 755), e.g. `{"run.sh": "echo hi"}`

**Rules:**
- Reference existing definitions when they apply
- Define new prereqs only when no existing one fits
- Use `exclude: true` for interactive, network, and privilege categories
- Empty array `[]` means no prereqs needed
- Keep descriptions concise

Respond ONLY with the JSON object, no other text.
"#,
    );

    prompt
}

/// Parse LM prereq response into PrereqsFile.
fn parse_prereq_response(text: &str) -> Result<PrereqsFile> {
    let json_text = extract_json(text);
    let response: LmPrereqResponse = serde_json::from_str(json_text)
        .with_context(|| format!("parse prereq response: {}", &text[..text.len().min(500)]))?;

    let mut prereqs = PrereqsFile {
        schema_version: PREREQS_SCHEMA_VERSION,
        definitions: BTreeMap::new(),
        surface_map: response.surface_map,
    };

    // Convert LM definitions to PrereqInferenceDefinition
    for (key, def) in response.definitions {
        prereqs.definitions.insert(
            key,
            PrereqInferenceDefinition {
                description: def.description,
                seed: def.seed.map(|flat| flat.to_seed_spec()),
                exclude: def.exclude,
            },
        );
    }

    Ok(prereqs)
}

/// Extract JSON from text that might have markdown code fences.
fn extract_json(text: &str) -> &str {
    let text = text.trim();

    // Try to find JSON in code fences
    if let Some(start) = text.find("```json") {
        let start = start + 7;
        if let Some(end) = text[start..].find("```") {
            return text[start..start + end].trim();
        }
    }

    // Try plain code fences
    if let Some(start) = text.find("```") {
        let start = start + 3;
        // Skip language identifier if present
        let start = text[start..]
            .find('\n')
            .map(|i| start + i + 1)
            .unwrap_or(start);
        if let Some(end) = text[start..].find("```") {
            return text[start..start + end].trim();
        }
    }

    // Return as-is, trimmed
    text
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
