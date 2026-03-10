//! LM invocation and response types for simplified verification.
//!
//! The LM contract is intentionally simple: we send state context, the LM
//! returns a list of actions. No complex protocol negotiation, just JSON in/out.
//!
//! Expected format (nested JSON with arrays):
//! ```json
//! {
//!   "actions": [
//!     { "kind": "SetBaseline", "args": [], "seed": { "setup": [["git", "init"]], "files": [] } },
//!     { "kind": "Test", "surface_id": "--stat", "args": ["--stat"], "seed": { ... } }
//!   ]
//! }
//! ```
//!
//! The parser is intentionally flexible to handle common LM mistakes:
//! - Setup as nested arrays `[["git", "init"]]` (correct)
//! - Setup as flat string array `["git init"]` (auto-split)
//! - Setup as shell string `"git init && touch f"` (auto-split on &&)
//! - Args as array `["--stat"]` (correct) or string `"--stat"` (auto-split)

use super::types::{FileEntry, Seed};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
/// Implements retry logic for parse errors with retry prompts that include context.
/// On complete failure, returns empty actions instead of crashing (graceful degradation).
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
            build_retry_prompt(prompt, last_error.as_deref().unwrap_or("unknown error"))
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

    // Graceful degradation: return empty actions instead of crashing
    eprintln!(
        "  ⚠ LM produced no valid JSON after {} attempts",
        MAX_LM_RETRIES + 1
    );
    eprintln!("  Continuing with empty actions for this cycle");

    // Log what we received for debugging
    if let Some(resp) = &last_response {
        let preview = truncate_safe(resp, 200);
        tracing::warn!(
            response_preview = preview,
            error = last_error.as_deref().unwrap_or("unknown"),
            "LM failed to produce valid JSON, returning empty actions"
        );
    }

    Ok(LmResponse { actions: vec![] })
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
///
/// Tries nested JSON `{"actions": [...]}` format first (primary), then falls
/// back to extracting individual JSON objects from prose.
fn parse_lm_response(text: &str) -> Result<LmResponse> {
    // Extract JSON from markdown code fences if present
    let json_text = extract_json(text);
    let fixed_json = fix_common_typos(&json_text);
    let fixed_json = fix_json_errors(&fixed_json);

    // Try parsing as {"actions": [...]} format (primary path)
    if let Ok(mut response) = serde_json::from_str::<LmResponse>(&fixed_json) {
        // Post-process to handle flexible setup/args formats
        for action in &mut response.actions {
            normalize_action(action)?;
        }
        return Ok(response);
    }

    // Fallback: extract individual JSON objects from prose/JSONL
    if let Ok(response) = parse_extracted_objects(text) {
        if !response.actions.is_empty() {
            return Ok(response);
        }
    }

    // Final error
    Err(anyhow::anyhow!(
        "parse LM response: no valid JSON found. First 500 chars: {}",
        truncate_safe(text, 500)
    ))
}

/// Normalize action fields to handle flexible LM output.
fn normalize_action(_action: &mut LmAction) -> Result<()> {
    // The flexible parsing is already handled in parse_setup_commands and parse_shell_args
    // when parsing from extracted objects. For serde-deserialized actions, the fields
    // are already in the correct format.
    Ok(())
}

/// Parse extracted JSON objects from LM response.
///
/// Extracts JSON objects from text (handles prose wrapping, code fences)
/// and converts them to actions. This is the fallback path for when the
/// LM doesn't produce a clean `{"actions": [...]}` wrapper.
fn parse_extracted_objects(text: &str) -> Result<LmResponse> {
    let mut actions = Vec::new();

    // Extract all {...} objects from text (handles prose wrapping)
    for json_str in extract_json_objects(text) {
        let fixed = fix_json_errors(&json_str);
        if let Ok(obj) = serde_json::from_str::<Value>(&fixed) {
            if let Some(action) = parse_action_object(&obj)? {
                actions.push(action);
            }
        }
    }

    Ok(LmResponse { actions })
}

/// Extract all top-level {...} substrings from text, handling nesting.
fn extract_json_objects(text: &str) -> Vec<String> {
    let mut objects = Vec::new();
    let mut depth = 0;
    let mut start = None;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in text.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match ch {
            '\\' if in_string => {
                escape_next = true;
            }
            '"' => {
                in_string = !in_string;
            }
            '{' if !in_string => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        objects.push(text[s..=i].to_string());
                    }
                    start = None;
                }
            }
            _ => {}
        }
    }

    objects
}

/// Fix common JSON errors in LM output (for simplified format).
fn fix_json_errors(json: &str) -> String {
    let mut result = json.to_string();

    // Fix trailing commas: {"a": 1,} -> {"a": 1}
    // Match comma followed by optional whitespace and closing brace/bracket
    let trailing_comma_re = regex::Regex::new(r",(\s*[}\]])").unwrap();
    result = trailing_comma_re.replace_all(&result, "$1").to_string();

    // Fix single quotes to double quotes (simple case - not inside strings)
    // Only do this if there are no double quotes (pure single-quote JSON)
    if !result.contains('"') && result.contains('\'') {
        result = result.replace('\'', "\"");
    }

    // Fix unquoted keys: {test: "x"} -> {"test": "x"}
    // Match word characters followed by colon at start of object or after comma
    let unquoted_key_re = regex::Regex::new(r"([{,]\s*)(\w+)(\s*:)").unwrap();
    result = unquoted_key_re
        .replace_all(&result, r#"$1"$2"$3"#)
        .to_string();

    // Note: Don't apply fix_common_typos here - those are for legacy format only

    result
}

/// Convert a JSON object to an LmAction.
///
/// Recognizes three object types by their key:
/// - `{"baseline": true, ...}` -> SetBaseline
/// - `{"test": "surface_id", ...}` -> Test
/// - `{"exclude": "surface_id", ...}` -> Exclude
fn parse_action_object(obj: &Value) -> Result<Option<LmAction>> {
    // Helper to get value case-insensitively
    fn get_key<'a>(obj: &'a Value, keys: &[&str]) -> Option<&'a Value> {
        for key in keys {
            if let Some(v) = obj.get(*key) {
                return Some(v);
            }
        }
        None
    }

    // Check for baseline action (baseline/Baseline)
    if get_key(obj, &["baseline", "Baseline"]).is_some() {
        let args = parse_shell_args(get_key(obj, &["args", "Args"]))?;
        let seed = parse_seed(obj)?;
        return Ok(Some(LmAction::SetBaseline { args, seed }));
    }

    // Check for test action (test/Test)
    if let Some(surface) = get_key(obj, &["test", "Test"]).and_then(|v| v.as_str()) {
        let args = parse_shell_args(get_key(obj, &["args", "Args"]))?;
        let seed = parse_seed(obj)?;
        return Ok(Some(LmAction::Test {
            surface_id: surface.to_string(),
            args,
            seed,
        }));
    }

    // Check for exclude action (exclude/Exclude)
    if let Some(surface) = get_key(obj, &["exclude", "Exclude"]).and_then(|v| v.as_str()) {
        let reason = get_key(obj, &["reason", "Reason"])
            .and_then(|v| v.as_str())
            .unwrap_or("No reason provided")
            .to_string();
        return Ok(Some(LmAction::Exclude {
            surface_id: surface.to_string(),
            reason,
        }));
    }

    // Unknown format, skip
    Ok(None)
}

/// Parse args flexibly - handles both array and string formats.
///
/// Format 1 (correct): Array `["--stat", "--name-only"]`
/// Format 2 (mistake): Shell string `"--stat --name-only"` -> auto-split
fn parse_shell_args(value: Option<&Value>) -> Result<Vec<String>> {
    match value {
        Some(Value::String(s)) if s.is_empty() => Ok(vec![]),
        Some(Value::String(s)) => {
            shell_words::split(s).with_context(|| format!("parse args shell string: {s}"))
        }
        Some(Value::Array(arr)) => {
            // Legacy array format - extract strings
            Ok(arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        }
        _ => Ok(vec![]),
    }
}

/// Parse seed from object (setup as shell string, files embedded).
fn parse_seed(obj: &Value) -> Result<Seed> {
    // Get setup/Setup key
    let setup_value = obj.get("setup").or_else(|| obj.get("Setup"));
    let setup = parse_setup_commands(setup_value)?;

    // Get files/Files key
    let files_value = obj.get("files").or_else(|| obj.get("Files"));
    let files = parse_files(files_value)?;

    Ok(Seed { setup, files })
}

/// Parse setup commands flexibly - handles multiple formats LMs produce.
///
/// Format 1 (correct): Nested arrays `[["git", "init"], ["touch", "file.txt"]]`
/// Format 2 (mistake): Flat string array `["git init", "touch file.txt"]` -> auto-split each
/// Format 3 (mistake): Shell string `"git init && touch file.txt"` -> split on &&
fn parse_setup_commands(value: Option<&Value>) -> Result<Vec<Vec<String>>> {
    match value {
        Some(Value::String(s)) if s.is_empty() => Ok(vec![]),
        Some(Value::String(s)) => {
            // Split on && and parse each command
            let commands: Result<Vec<Vec<String>>> = s
                .split("&&")
                .map(|part| {
                    let trimmed = part.trim();
                    if trimmed.is_empty() {
                        Ok(vec![])
                    } else {
                        shell_words::split(trimmed)
                            .with_context(|| format!("parse setup command: {trimmed}"))
                    }
                })
                .filter(|r| r.as_ref().map(|v| !v.is_empty()).unwrap_or(true))
                .collect();
            commands
        }
        Some(Value::Array(arr)) => {
            // Legacy array format - each element is a command array
            let mut commands = Vec::new();
            for item in arr {
                match item {
                    Value::Array(cmd_arr) => {
                        let cmd: Vec<String> = cmd_arr
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();
                        if !cmd.is_empty() {
                            commands.push(cmd);
                        }
                    }
                    Value::String(s) => {
                        // Single string command - parse it
                        if let Ok(cmd) = shell_words::split(s) {
                            if !cmd.is_empty() {
                                commands.push(cmd);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(commands)
        }
        _ => Ok(vec![]),
    }
}

/// Parse files from array format.
fn parse_files(value: Option<&Value>) -> Result<Vec<FileEntry>> {
    match value {
        Some(Value::Array(arr)) => {
            let mut files = Vec::new();
            for item in arr {
                if let (Some(path), Some(content)) = (
                    item.get("path").and_then(|v| v.as_str()),
                    item.get("content").and_then(|v| v.as_str()),
                ) {
                    files.push(FileEntry {
                        path: path.to_string(),
                        content: content.to_string(),
                    });
                }
            }
            Ok(files)
        }
        _ => Ok(vec![]),
    }
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

/// Build a retry prompt that includes the original context plus correction instructions.
///
/// We include the full original prompt because without context, the LM doesn't know
/// what surfaces to work on and produces meaningless responses.
fn build_retry_prompt(original_prompt: &str, error: &str) -> String {
    format!(
        r#"{original_prompt}

---

⚠️ RETRY: Your previous response could not be parsed as JSON.

Error: {error}

Please respond with ONLY a valid JSON object. No explanations before or after."#
    )
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

    // ==================== Simplified JSONL Format Tests ====================

    #[test]
    fn test_parse_simplified_baseline() {
        let text = r#"{"baseline": true, "setup": "git init && touch file.txt"}"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 1);

        match &response.actions[0] {
            LmAction::SetBaseline { args, seed } => {
                assert!(args.is_empty());
                assert_eq!(seed.setup.len(), 2);
                assert_eq!(seed.setup[0], vec!["git", "init"]);
                assert_eq!(seed.setup[1], vec!["touch", "file.txt"]);
            }
            _ => panic!("Expected SetBaseline"),
        }
    }

    #[test]
    fn test_parse_simplified_test() {
        let text = r#"{"test": "--stat", "args": "--stat", "setup": "git init"}"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 1);

        match &response.actions[0] {
            LmAction::Test {
                surface_id,
                args,
                seed,
            } => {
                assert_eq!(surface_id, "--stat");
                assert_eq!(args, &["--stat"]);
                assert_eq!(seed.setup.len(), 1);
                assert_eq!(seed.setup[0], vec!["git", "init"]);
            }
            _ => panic!("Expected Test"),
        }
    }

    #[test]
    fn test_parse_simplified_test_with_multi_args() {
        let text = r#"{"test": "--width", "args": "--width 20", "setup": "touch file.txt"}"#;
        let response = parse_lm_response(text).unwrap();

        match &response.actions[0] {
            LmAction::Test { args, .. } => {
                assert_eq!(args, &["--width", "20"]);
            }
            _ => panic!("Expected Test"),
        }
    }

    #[test]
    fn test_parse_simplified_exclude() {
        let text = r#"{"exclude": "--gpg-sign", "reason": "requires GPG key"}"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 1);

        match &response.actions[0] {
            LmAction::Exclude { surface_id, reason } => {
                assert_eq!(surface_id, "--gpg-sign");
                assert_eq!(reason, "requires GPG key");
            }
            _ => panic!("Expected Exclude"),
        }
    }

    #[test]
    fn test_parse_simplified_multiple_objects() {
        let text = r#"
{"baseline": true, "setup": "touch file.txt"}
{"test": "--all", "args": "--all", "setup": "touch .hidden"}
{"exclude": "--context", "reason": "requires SELinux"}
"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 3);

        assert!(matches!(&response.actions[0], LmAction::SetBaseline { .. }));
        assert!(matches!(&response.actions[1], LmAction::Test { .. }));
        assert!(matches!(&response.actions[2], LmAction::Exclude { .. }));
    }

    #[test]
    fn test_parse_simplified_with_prose() {
        let text = r#"Here's my response:

First, set up the baseline:
{"baseline": true, "setup": "touch file.txt"}

Then test the option:
{"test": "--all", "args": "--all", "setup": "touch .hidden"}

That should work!
"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 2);
    }

    #[test]
    fn test_parse_simplified_with_code_fence() {
        let text = r#"```json
{"baseline": true, "setup": "touch file.txt"}
{"test": "--all", "args": "--all", "setup": "touch .hidden"}
```"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 2);
    }

    #[test]
    fn test_parse_simplified_empty_setup() {
        let text = r#"{"baseline": true, "args": "", "setup": ""}"#;
        let response = parse_lm_response(text).unwrap();

        match &response.actions[0] {
            LmAction::SetBaseline { args, seed } => {
                assert!(args.is_empty());
                assert!(seed.setup.is_empty());
            }
            _ => panic!("Expected SetBaseline"),
        }
    }

    #[test]
    fn test_parse_simplified_no_setup() {
        let text = r#"{"baseline": true}"#;
        let response = parse_lm_response(text).unwrap();

        match &response.actions[0] {
            LmAction::SetBaseline { seed, .. } => {
                assert!(seed.setup.is_empty());
            }
            _ => panic!("Expected SetBaseline"),
        }
    }

    // ==================== JSON Error Fixing Tests ====================

    #[test]
    fn test_fix_trailing_comma() {
        let json = r#"{"baseline": true, "setup": "touch file.txt",}"#;
        let fixed = fix_json_errors(json);
        assert!(!fixed.contains(",}"));
        // Should be parseable now
        let _: Value = serde_json::from_str(&fixed).unwrap();
    }

    #[test]
    fn test_fix_single_quotes() {
        let json = "{'baseline': true, 'setup': 'touch file.txt'}";
        let fixed = fix_json_errors(json);
        assert!(fixed.contains('"'));
        // Should be parseable now
        let _: Value = serde_json::from_str(&fixed).unwrap();
    }

    #[test]
    fn test_fix_unquoted_keys() {
        let json = r#"{baseline: true, setup: "touch file.txt"}"#;
        let fixed = fix_json_errors(json);
        assert!(fixed.contains("\"baseline\""));
        assert!(fixed.contains("\"setup\""));
    }

    // ==================== Extract JSON Objects Tests ====================

    #[test]
    fn test_extract_json_objects_single() {
        let text = r#"{"test": "value"}"#;
        let objects = extract_json_objects(text);
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0], r#"{"test": "value"}"#);
    }

    #[test]
    fn test_extract_json_objects_multiple() {
        let text = r#"{"a": 1} some text {"b": 2}"#;
        let objects = extract_json_objects(text);
        assert_eq!(objects.len(), 2);
    }

    #[test]
    fn test_extract_json_objects_nested() {
        let text = r#"{"outer": {"inner": "value"}}"#;
        let objects = extract_json_objects(text);
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0], text);
    }

    #[test]
    fn test_extract_json_objects_with_braces_in_strings() {
        let text = r#"{"setup": "echo '{hello}'"}"#;
        let objects = extract_json_objects(text);
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0], text);
    }

    // ==================== Legacy Format Tests (backward compatibility) ====================

    #[test]
    fn test_parse_legacy_response() {
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
    fn test_parse_legacy_exclude_action() {
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

    // ==================== Shell String Parsing Tests ====================

    #[test]
    fn test_parse_setup_multi_command() {
        let value: Value = serde_json::json!("git init && touch file.txt && echo hello > test.txt");
        let commands = parse_setup_commands(Some(&value)).unwrap();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0], vec!["git", "init"]);
        assert_eq!(commands[1], vec!["touch", "file.txt"]);
        // Note: shell redirection is parsed as separate tokens by shell_words
        assert_eq!(commands[2][0], "echo");
    }

    #[test]
    fn test_parse_setup_legacy_array() {
        let value: Value = serde_json::json!([["git", "init"], ["touch", "file.txt"]]);
        let commands = parse_setup_commands(Some(&value)).unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0], vec!["git", "init"]);
        assert_eq!(commands[1], vec!["touch", "file.txt"]);
    }

    #[test]
    fn test_parse_shell_args_string() {
        let value: Value = serde_json::json!("--stat --name-only");
        let args = parse_shell_args(Some(&value)).unwrap();
        assert_eq!(args, vec!["--stat", "--name-only"]);
    }

    #[test]
    fn test_parse_shell_args_array() {
        let value: Value = serde_json::json!(["--stat", "--name-only"]);
        let args = parse_shell_args(Some(&value)).unwrap();
        assert_eq!(args, vec!["--stat", "--name-only"]);
    }

    #[test]
    fn test_parse_shell_args_with_equals() {
        let value: Value = serde_json::json!("--format=short");
        let args = parse_shell_args(Some(&value)).unwrap();
        assert_eq!(args, vec!["--format=short"]);
    }

    #[test]
    fn test_parse_shell_args_quoted() {
        let value: Value = serde_json::json!(r#"--message "hello world""#);
        let args = parse_shell_args(Some(&value)).unwrap();
        assert_eq!(args, vec!["--message", "hello world"]);
    }

    // ==================== Flexible Parsing Tests ====================

    #[test]
    fn test_parse_setup_nested_arrays() {
        // Correct format: nested arrays
        let value = serde_json::json!([["git", "init"], ["touch", "file.txt"]]);
        let commands = parse_setup_commands(Some(&value)).unwrap();
        assert_eq!(commands, vec![
            vec!["git", "init"],
            vec!["touch", "file.txt"]
        ]);
    }

    #[test]
    fn test_parse_setup_string_array() {
        // LM mistake: flat array of shell strings
        let value = serde_json::json!(["git init", "touch file.txt"]);
        let commands = parse_setup_commands(Some(&value)).unwrap();
        assert_eq!(commands, vec![
            vec!["git", "init"],
            vec!["touch", "file.txt"]
        ]);
    }

    #[test]
    fn test_parse_setup_shell_string() {
        // LM mistake: single shell string with &&
        let value = serde_json::json!("git init && touch file.txt");
        let commands = parse_setup_commands(Some(&value)).unwrap();
        assert_eq!(commands, vec![
            vec!["git", "init"],
            vec!["touch", "file.txt"]
        ]);
    }

    #[test]
    fn test_parse_args_array() {
        // Correct format: array of strings
        let value = serde_json::json!(["--width", "20"]);
        let args = parse_shell_args(Some(&value)).unwrap();
        assert_eq!(args, vec!["--width", "20"]);
    }

    #[test]
    fn test_parse_args_string() {
        // LM mistake: shell string
        let value = serde_json::json!("--width 20");
        let args = parse_shell_args(Some(&value)).unwrap();
        assert_eq!(args, vec!["--width", "20"]);
    }

    #[test]
    fn test_parse_individual_objects_with_flexible_setup() {
        // When LM produces individual objects (not wrapped in {"actions": [...]}),
        // the fallback path extracts and parses them with flexible handling
        let text = r#"
Here's my response:

{"baseline": true, "setup": "git init && touch file.txt"}
{"test": "--stat", "args": "--stat", "setup": "touch test.txt"}
"#;

        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 2);

        // Verify flexible setup parsing worked
        match &response.actions[0] {
            LmAction::SetBaseline { seed, .. } => {
                assert_eq!(seed.setup.len(), 2);
                assert_eq!(seed.setup[0], vec!["git", "init"]);
                assert_eq!(seed.setup[1], vec!["touch", "file.txt"]);
            }
            _ => panic!("Expected SetBaseline"),
        }
    }

    #[test]
    fn test_parse_correct_nested_format() {
        // When LM produces the correct nested format, serde parses it directly
        let text = r#"{
            "actions": [
                {
                    "kind": "SetBaseline",
                    "args": [],
                    "seed": {"setup": [["git", "init"], ["touch", "file.txt"]], "files": []}
                },
                {
                    "kind": "Test",
                    "surface_id": "--stat",
                    "args": ["--stat"],
                    "seed": {"setup": [["git", "init"]], "files": []}
                }
            ]
        }"#;

        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 2);

        match &response.actions[0] {
            LmAction::SetBaseline { seed, .. } => {
                assert_eq!(seed.setup.len(), 2);
                assert_eq!(seed.setup[0], vec!["git", "init"]);
            }
            _ => panic!("Expected SetBaseline"),
        }
    }
}
