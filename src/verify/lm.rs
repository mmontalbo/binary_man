//! LM invocation and response types for simplified verification.
//!
//! The LM contract is intentionally simple: we send state context, the LM
//! returns a list of actions. No complex protocol negotiation, just JSON in/out.
//!
//! Expected format (nested JSON with arrays):
//! ```json
//! {
//!   "actions": [
//!     { "kind": "SetBaseline", "seed": { "setup": [["git", "init"]], "files": [] } },
//!     { "kind": "Test", "surface_id": "--stat", "seed": { ... } }
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
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Prediction of expected behavior for a test.
///
/// The LM specifies what it expects to happen when running the test,
/// allowing mechanical verification instead of subjective critique.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prediction {
    /// What type of difference is expected.
    pub diff_type: PredictedDiff,
    /// Brief explanation of why this is expected.
    #[serde(default)]
    pub reason: String,
}

/// Type of difference expected between control and option runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PredictedDiff {
    /// Option output should be empty or much shorter (suppression options like --no-*, --quiet).
    StdoutEmpty,
    /// Option output should contain specific text or pattern.
    StdoutContains(String),
    /// Stderr should contain something different.
    StderrDifferent,
    /// Exit code should differ.
    ExitCodeDifferent,
}

/// Response from the LM containing actions to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LmResponse {
    /// Per-surface reasoning about control equivalence, keyed by surface_id.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub analysis: HashMap<String, String>,
    /// Actions to execute in order.
    pub actions: Vec<LmAction>,
}

/// A single action the LM wants to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum LmAction {
    /// Set the baseline scenario (required first, exactly once).
    SetBaseline {
        /// Seed setup for the baseline.
        seed: Seed,
    },
    /// Test a specific surface.
    Test {
        /// Which surface to test (automatically included in command).
        surface_id: String,
        /// Additional arguments beyond surface_id (optional).
        #[serde(default)]
        extra_args: Vec<String>,
        /// Seed setup for this test.
        seed: Seed,
        /// Prediction of expected outcome (optional for backward compatibility).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prediction: Option<Prediction>,
    },
    /// Probe a surface to gather evidence before committing to a test.
    ///
    /// Runs the command in a sandbox and captures output, but does not
    /// compute a verification outcome or count against the attempt budget.
    Probe {
        /// Which surface this probe is gathering evidence for.
        surface_id: String,
        /// Additional arguments beyond surface_id (optional).
        #[serde(default)]
        extra_args: Vec<String>,
        /// Seed setup for this probe.
        seed: Seed,
    },
}

/// Parse the LM response text into an LmResponse.
///
/// Tries multiple extraction strategies to handle LM responses that include
/// prose before/after the JSON. Strategies in order:
/// 1. Extract from markdown code fences
/// 2. Find `{"actions":` pattern anywhere in text
/// 3. Extract individual JSON objects from prose
pub(super) fn parse_lm_response(text: &str) -> Result<LmResponse> {
    // Strategy 1: Extract JSON from markdown code fences if present
    let json_text = extract_json(text);
    let fixed_json = fix_common_typos(&json_text);
    let fixed_json = fix_json_errors(&fixed_json);

    // Try parsing as {"actions": [...]} format (primary path)
    if let Ok(response) = serde_json::from_str::<LmResponse>(&fixed_json) {
        return Ok(response);
    }

    // Strategy 2: Look for {"actions": pattern anywhere in text
    if let Some(response) = extract_actions_json(text) {
        return Ok(response);
    }

    // Strategy 3: Extract individual JSON objects from prose/JSONL
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

/// Try to find and extract {"actions": [...]} from anywhere in text.
/// Handles cases where LM outputs prose before/after the JSON.
fn extract_actions_json(text: &str) -> Option<LmResponse> {
    // Look for the start of an actions JSON object
    let patterns = [r#"{"actions":"#, r#"{ "actions":"#, r#"{"actions" :"#];

    for pattern in patterns {
        if let Some(start) = text.find(pattern) {
            // Find the matching closing brace
            let json_start = &text[start..];
            if let Some(json_str) = extract_balanced_json(json_start) {
                let fixed = fix_common_typos(&json_str);
                let fixed = fix_json_errors(&fixed);
                if let Ok(response) = serde_json::from_str::<LmResponse>(&fixed) {
                    return Some(response);
                }
            }
        }
    }
    None
}

/// Extract a balanced JSON object starting with '{'.
fn extract_balanced_json(text: &str) -> Option<String> {
    if !text.starts_with('{') {
        return None;
    }

    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in text.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match ch {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
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

    Ok(LmResponse {
        analysis: HashMap::new(),
        actions,
    })
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

    // Fix \xNN hex escapes (Python/C style) to \u00NN (valid JSON)
    // LMs commonly produce these when trying to write binary content
    let hex_escape_re = regex::Regex::new(r"\\x([0-9a-fA-F]{2})").unwrap();
    result = hex_escape_re.replace_all(&result, r"\u00$1").to_string();

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
/// Recognizes two object types by their key:
/// - `{"baseline": true, ...}` -> SetBaseline
/// - `{"test": "surface_id", ...}` -> Test
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
        let seed = parse_seed(obj)?;
        return Ok(Some(LmAction::SetBaseline { seed }));
    }

    // Check for probe action (probe/Probe) — must check before test since both have surface_id
    if let Some(surface) = get_key(obj, &["probe", "Probe"]).and_then(|v| v.as_str()) {
        let extra_args = parse_shell_args(get_key(obj, &["extra_args", "Extra_args"]))?;
        let seed = parse_seed(obj)?;
        return Ok(Some(LmAction::Probe {
            surface_id: surface.to_string(),
            extra_args,
            seed,
        }));
    }

    // Check for test action (test/Test)
    if let Some(surface) = get_key(obj, &["test", "Test"]).and_then(|v| v.as_str()) {
        let extra_args = parse_shell_args(get_key(obj, &["extra_args", "Extra_args"]))?;
        let seed = parse_seed(obj)?;
        let prediction = parse_prediction(get_key(obj, &["prediction", "Prediction"]));
        return Ok(Some(LmAction::Test {
            surface_id: surface.to_string(),
            extra_args,
            seed,
            prediction,
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
            Ok(arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect())
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

    let mut seed = Seed { setup, files };
    repair_seed(&mut seed);
    Ok(seed)
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

/// Parse prediction from JSON object.
///
/// Expected format:
/// ```json
/// {
///   "diff_type": "StdoutEmpty" | "StderrDifferent" | "ExitCodeDifferent" | {"StdoutContains": "text"},
///   "reason": "explanation"
/// }
/// ```
fn parse_prediction(value: Option<&Value>) -> Option<Prediction> {
    let obj = value?;

    // Get reason (required)
    let reason = obj
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();

    // Parse diff_type - can be string or object with StdoutContains
    let diff_type_value = obj.get("diff_type")?;

    let diff_type = match diff_type_value {
        Value::String(s) => match s.as_str() {
            "StdoutEmpty" => PredictedDiff::StdoutEmpty,
            "StdoutContains" => {
                // Flat format: diff_type is "StdoutContains" with separate "content" field
                let content = obj
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                PredictedDiff::StdoutContains(content)
            }
            "StderrDifferent" => PredictedDiff::StderrDifferent,
            "ExitCodeDifferent" => PredictedDiff::ExitCodeDifferent,
            _ => return None,
        },
        Value::Object(map) => {
            // Handle {"StdoutContains": "text"} format
            if let Some(Value::String(text)) = map.get("StdoutContains") {
                PredictedDiff::StdoutContains(text.clone())
            } else {
                return None;
            }
        }
        _ => return None,
    };

    Some(Prediction { diff_type, reason })
}

/// Extract JSON from text that might have markdown code fences.
fn extract_json(text: &str) -> String {
    let text = text.trim();

    // Try to extract content from markdown code fences
    let extracted = if let Some(fence_start) = text.find("```json") {
        let content_start = fence_start + 7;
        // Skip any whitespace/newline after ```json
        let content_start = text[content_start..]
            .find(|c: char| !c.is_whitespace() || c == '\n')
            .map(|i| content_start + i)
            .unwrap_or(content_start);
        let content_start = if text[content_start..].starts_with('\n') {
            content_start + 1
        } else {
            content_start
        };

        if let Some(end) = text[content_start..].find("```") {
            text[content_start..content_start + end].trim()
        } else {
            // No closing fence - still strip the opening ```json
            text[content_start..].trim()
        }
    } else if let Some(fence_start) = text.find("```") {
        let after_fence = fence_start + 3;
        // Skip language identifier and newline
        let content_start = text[after_fence..]
            .find('\n')
            .map(|i| after_fence + i + 1)
            .unwrap_or(after_fence);

        if let Some(end) = text[content_start..].find("```") {
            text[content_start..content_start + end].trim()
        } else {
            // No closing fence - still strip the opening ```
            text[content_start..].trim()
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
pub(super) fn log_prompt(pack_path: &Path, cycle: u32, prompt: &str) -> Result<()> {
    let log_dir = pack_path.join("lm_log");
    std::fs::create_dir_all(&log_dir).context("create lm_log directory")?;

    let path = log_dir.join(format!("c{cycle}_prompt.md"));
    std::fs::write(&path, prompt).with_context(|| format!("write prompt to {}", path.display()))
}

/// Log an LM response to the LM log directory.
pub(super) fn log_response(pack_path: &Path, cycle: u32, response: &LmResponse) -> Result<()> {
    let log_dir = pack_path.join("lm_log");
    std::fs::create_dir_all(&log_dir).context("create lm_log directory")?;

    let path = log_dir.join(format!("c{cycle}_response.json"));
    let content = serde_json::to_string_pretty(response).context("serialize response")?;
    std::fs::write(&path, content).with_context(|| format!("write response to {}", path.display()))
}

/// Log a raw LM response that failed to parse.
pub(super) fn log_raw_response(pack_path: &Path, cycle: u32, raw: &str) -> Result<()> {
    let log_dir = pack_path.join("lm_log");
    std::fs::create_dir_all(&log_dir).context("create lm_log directory")?;

    let path = log_dir.join(format!("c{cycle}_response_raw.txt"));
    std::fs::write(&path, raw).with_context(|| format!("write raw response to {}", path.display()))
}

/// Repair shell-redirect patterns in seed setup by converting them to files entries.
///
/// Local LMs frequently produce `["echo", "content", ">", "file.txt"]` despite instructions
/// saying not to. Since setup runs via execvp (no shell), the `>` becomes a literal argument.
/// This function detects those patterns and mechanically converts them to files entries.
fn repair_seed(seed: &mut Seed) {
    let mut remaining = Vec::new();

    for cmd in seed.setup.drain(..) {
        if let Some(repairs) = try_repair_sh_c(&cmd) {
            for (path, content) in repairs {
                if !seed.files.iter().any(|f| f.path == path) {
                    seed.files.push(FileEntry { path, content });
                }
            }
        } else if let Some((path, content)) = try_extract_redirect(&cmd) {
            if !seed.files.iter().any(|f| f.path == path) {
                seed.files.push(FileEntry { path, content });
            }
        } else {
            remaining.push(cmd);
        }
    }

    seed.setup = remaining;
}

/// Try to extract a redirect pattern from a single setup command.
///
/// Matches:
/// - `["echo", content..., ">"|">>", path]`
/// - `["echo", "-e", content..., ">"|">>", path]` (interprets \n, \t)
/// - `["printf", fmt, ">"|">>", path]`
fn try_extract_redirect(cmd: &[String]) -> Option<(String, String)> {
    if cmd.len() < 3 {
        return None;
    }

    // Find the redirect operator position (last occurrence)
    let redir_pos = cmd.iter().rposition(|s| s == ">" || s == ">>")?;

    // Must have exactly one arg after the redirect (the path)
    if redir_pos + 1 != cmd.len() - 1 {
        return None;
    }

    let path = cmd[redir_pos + 1].clone();
    let program = cmd[0].as_str();

    match program {
        "echo" => {
            let (content_parts, echo_e) = if cmd.get(1).map(|s| s.as_str()) == Some("-e") {
                (&cmd[2..redir_pos], true)
            } else {
                (&cmd[1..redir_pos], false)
            };
            let joined = content_parts.join(" ");
            let content = if echo_e {
                interpret_echo_e(&joined)
            } else {
                format!("{joined}\n")
            };
            Some((path, content))
        }
        "printf" => {
            // printf fmt [args...] > path — just use the format string
            if redir_pos < 2 {
                return None;
            }
            let content = interpret_echo_e(&cmd[1..redir_pos].join(" "));
            Some((path, content))
        }
        _ => None,
    }
}

/// Try to repair `["sh", "-c", cmd_str]` or `["bash", "-c", cmd_str]`.
///
/// Parses the inner command string for simple redirect patterns.
/// Returns None for complex cases (pipes, chains, multiple commands).
fn try_repair_sh_c(cmd: &[String]) -> Option<Vec<(String, String)>> {
    if cmd.len() != 3 {
        return None;
    }
    let shell = cmd[0].as_str();
    if shell != "sh" && shell != "bash" {
        return None;
    }
    if cmd[1] != "-c" {
        return None;
    }

    let inner = &cmd[2];

    // Bail on complex shell constructs
    if inner.contains('|') || inner.contains("&&") || inner.contains(';') {
        return None;
    }

    // Parse the inner command as shell words
    let words = shell_words::split(inner).ok()?;
    try_extract_redirect(&words).map(|r| vec![r])
}

/// Interpret echo -e / printf escape sequences: \n, \t, \\
fn interpret_echo_e(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('\\') => result.push('\\'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }
    result
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
            LmAction::SetBaseline { seed } => {
                assert_eq!(seed.setup.len(), 2);
                assert_eq!(seed.setup[0], vec!["git", "init"]);
                assert_eq!(seed.setup[1], vec!["touch", "file.txt"]);
            }
            _ => panic!("Expected SetBaseline"),
        }
    }

    #[test]
    fn test_parse_simplified_test() {
        let text = r#"{"test": "--stat", "setup": "git init"}"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 1);

        match &response.actions[0] {
            LmAction::Test {
                surface_id,
                extra_args,
                seed,
                prediction,
                ..
            } => {
                assert_eq!(surface_id, "--stat");
                assert!(extra_args.is_empty()); // No extra_args needed
                assert_eq!(seed.setup.len(), 1);
                assert_eq!(seed.setup[0], vec!["git", "init"]);
                assert!(prediction.is_none()); // No prediction in this test case
            }
            _ => panic!("Expected Test"),
        }
    }

    #[test]
    fn test_parse_simplified_test_with_extra_args() {
        let text = r#"{"test": "--width", "extra_args": "20", "setup": "touch file.txt"}"#;
        let response = parse_lm_response(text).unwrap();

        match &response.actions[0] {
            LmAction::Test {
                surface_id,
                extra_args,
                ..
            } => {
                assert_eq!(surface_id, "--width");
                assert_eq!(extra_args, &["20"]);
            }
            _ => panic!("Expected Test"),
        }
    }

    #[test]
    fn test_parse_simplified_multiple_objects() {
        let text = r#"
{"baseline": true, "setup": "touch file.txt"}
{"test": "--all", "setup": "touch .hidden"}
{"test": "--verbose", "setup": "touch test.txt"}
"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 3);

        assert!(matches!(&response.actions[0], LmAction::SetBaseline { .. }));
        assert!(matches!(&response.actions[1], LmAction::Test { .. }));
        assert!(matches!(&response.actions[2], LmAction::Test { .. }));
    }

    #[test]
    fn test_parse_simplified_with_prose() {
        let text = r#"Here's my response:

First, set up the baseline:
{"baseline": true, "setup": "touch file.txt"}

Then test the option:
{"test": "--all", "setup": "touch .hidden"}

That should work!
"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 2);
    }

    #[test]
    fn test_parse_simplified_with_code_fence() {
        let text = r#"```json
{"baseline": true, "setup": "touch file.txt"}
{"test": "--all", "setup": "touch .hidden"}
```"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 2);
    }

    #[test]
    fn test_parse_simplified_empty_setup() {
        let text = r#"{"baseline": true, "setup": ""}"#;
        let response = parse_lm_response(text).unwrap();

        match &response.actions[0] {
            LmAction::SetBaseline { seed } => {
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
            LmAction::SetBaseline { seed } => {
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
                    "seed": {"setup": [["git", "init"]], "files": []}
                },
                {
                    "kind": "Test",
                    "surface_id": "--stat",
                    "seed": {"setup": [["git", "init"]], "files": []}
                }
            ]
        }"#;

        let response = parse_lm_response(json).unwrap();
        assert_eq!(response.actions.len(), 2);

        match &response.actions[0] {
            LmAction::SetBaseline { .. } => {}
            _ => panic!("Expected SetBaseline"),
        }

        match &response.actions[1] {
            LmAction::Test {
                surface_id,
                extra_args,
                ..
            } => {
                assert_eq!(surface_id, "--stat");
                assert!(extra_args.is_empty()); // No extra_args in this case
            }
            _ => panic!("Expected Test"),
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
        assert_eq!(
            commands,
            vec![vec!["git", "init"], vec!["touch", "file.txt"]]
        );
    }

    #[test]
    fn test_parse_setup_string_array() {
        // LM mistake: flat array of shell strings
        let value = serde_json::json!(["git init", "touch file.txt"]);
        let commands = parse_setup_commands(Some(&value)).unwrap();
        assert_eq!(
            commands,
            vec![vec!["git", "init"], vec!["touch", "file.txt"]]
        );
    }

    #[test]
    fn test_parse_setup_shell_string() {
        // LM mistake: single shell string with &&
        let value = serde_json::json!("git init && touch file.txt");
        let commands = parse_setup_commands(Some(&value)).unwrap();
        assert_eq!(
            commands,
            vec![vec!["git", "init"], vec!["touch", "file.txt"]]
        );
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
{"test": "--stat", "setup": "touch test.txt"}
"#;

        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 2);

        // Verify flexible setup parsing worked
        match &response.actions[0] {
            LmAction::SetBaseline { seed } => {
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
                    "seed": {"setup": [["git", "init"], ["touch", "file.txt"]], "files": []}
                },
                {
                    "kind": "Test",
                    "surface_id": "--stat",
                    "seed": {"setup": [["git", "init"]], "files": []}
                }
            ]
        }"#;

        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 2);

        match &response.actions[0] {
            LmAction::SetBaseline { seed } => {
                assert_eq!(seed.setup.len(), 2);
                assert_eq!(seed.setup[0], vec!["git", "init"]);
            }
            _ => panic!("Expected SetBaseline"),
        }
    }

    // ==================== Prediction Parsing Tests ====================

    #[test]
    fn test_parse_prediction_stdout_empty() {
        let text = r#"{"test": "--no-patch", "prediction": {"diff_type": "StdoutEmpty", "reason": "suppresses diff output"}, "setup": "git init"}"#;
        let response = parse_lm_response(text).unwrap();

        match &response.actions[0] {
            LmAction::Test { prediction, .. } => {
                let pred = prediction.as_ref().expect("Expected prediction");
                assert_eq!(pred.diff_type, PredictedDiff::StdoutEmpty);
                assert_eq!(pred.reason, "suppresses diff output");
            }
            _ => panic!("Expected Test"),
        }
    }

    #[test]
    fn test_parse_prediction_stdout_contains() {
        let text = r#"{"test": "--stat", "prediction": {"diff_type": {"StdoutContains": "insertions"}, "reason": "shows statistics"}, "setup": "git init"}"#;
        let response = parse_lm_response(text).unwrap();

        match &response.actions[0] {
            LmAction::Test { prediction, .. } => {
                let pred = prediction.as_ref().expect("Expected prediction");
                assert_eq!(
                    pred.diff_type,
                    PredictedDiff::StdoutContains("insertions".to_string())
                );
                assert_eq!(pred.reason, "shows statistics");
            }
            _ => panic!("Expected Test"),
        }
    }

    #[test]
    fn test_parse_prediction_none_when_missing() {
        let text = r#"{"test": "--verbose", "setup": "touch file.txt"}"#;
        let response = parse_lm_response(text).unwrap();

        match &response.actions[0] {
            LmAction::Test { prediction, .. } => {
                assert!(prediction.is_none());
            }
            _ => panic!("Expected Test"),
        }
    }

    #[test]
    fn test_parse_prediction_in_actions_format() {
        let text = r#"{
            "actions": [
                {
                    "kind": "Test",
                    "surface_id": "--quiet",
                    "prediction": {"diff_type": "StdoutEmpty", "reason": "suppresses output"},
                    "seed": {"setup": [], "files": []}
                }
            ]
        }"#;
        let response = parse_lm_response(text).unwrap();

        match &response.actions[0] {
            LmAction::Test { prediction, .. } => {
                let pred = prediction.as_ref().expect("Expected prediction");
                assert_eq!(pred.diff_type, PredictedDiff::StdoutEmpty);
            }
            _ => panic!("Expected Test"),
        }
    }

    // ==================== Probe Parsing Tests ====================

    #[test]
    fn test_parse_probe_simplified() {
        let text = r#"{"probe": "--verbose", "setup": "touch file.txt"}"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 1);

        match &response.actions[0] {
            LmAction::Probe {
                surface_id,
                extra_args,
                seed,
                ..
            } => {
                assert_eq!(surface_id, "--verbose");
                assert!(extra_args.is_empty());
                assert_eq!(seed.setup.len(), 1);
            }
            _ => panic!("Expected Probe"),
        }
    }

    #[test]
    fn test_parse_probe_with_extra_args() {
        let text = r#"{"probe": "-N", "extra_args": "10", "setup": "touch file.txt"}"#;
        let response = parse_lm_response(text).unwrap();

        match &response.actions[0] {
            LmAction::Probe {
                surface_id,
                extra_args,
                ..
            } => {
                assert_eq!(surface_id, "-N");
                assert_eq!(extra_args, &["10"]);
            }
            _ => panic!("Expected Probe"),
        }
    }

    #[test]
    fn test_parse_probe_in_actions_format() {
        let text = r#"{
            "actions": [
                {
                    "kind": "Probe",
                    "surface_id": "--verbose",
                    "seed": {"setup": [["touch", "file.txt"]], "files": []}
                }
            ]
        }"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 1);

        match &response.actions[0] {
            LmAction::Probe { surface_id, .. } => {
                assert_eq!(surface_id, "--verbose");
            }
            _ => panic!("Expected Probe"),
        }
    }

    #[test]
    fn test_parse_mixed_probe_and_test() {
        let text = r#"
{"probe": "--unknown-opt", "setup": "touch file.txt"}
{"test": "--verbose", "setup": "touch file.txt"}
"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 2);
        assert!(matches!(&response.actions[0], LmAction::Probe { .. }));
        assert!(matches!(&response.actions[1], LmAction::Test { .. }));
    }

    #[test]
    fn test_parse_hex_escapes_in_file_content() {
        // LMs produce \xNN hex escapes (Python/C style) which are invalid JSON.
        // The parser should fix these to \u00NN.
        let text = r#"{"actions": [{"kind": "Test", "surface_id": "--binary", "seed": {"setup": [], "files": [{"path": "data.bin", "content": "binary\x00content\x01here"}]}}]}"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 1);
        assert!(matches!(&response.actions[0], LmAction::Test { .. }));
    }

    #[test]
    fn test_parse_files_as_object_map() {
        // LMs sometimes produce files as {"filename": "content"} instead of
        // [{"path": "filename", "content": "content"}]
        let text = r#"{"actions": [{"kind": "Test", "surface_id": "--stat", "seed": {"setup": [["git", "init"]], "files": {"file.txt": "hello\nworld\n"}}}]}"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 1);
        if let LmAction::Test { seed, .. } = &response.actions[0] {
            assert_eq!(seed.files.len(), 1);
            assert_eq!(seed.files[0].path, "file.txt");
            assert_eq!(seed.files[0].content, "hello\nworld\n");
        } else {
            panic!("Expected Test action");
        }
    }

    #[test]
    fn test_parse_prediction_flat_format() {
        // Flat prediction format with separate "content" field works via
        // the simplified (non-actions) parsing path
        let text = r#"{"test": "-u", "setup": "touch file.txt", "prediction": {"diff_type": "StdoutContains", "content": "@@", "reason": "unified diff has @@ markers"}}"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 1);
        if let LmAction::Test { prediction, .. } = &response.actions[0] {
            let pred = prediction.as_ref().unwrap();
            assert_eq!(
                pred.diff_type,
                PredictedDiff::StdoutContains("@@".to_string())
            );
        } else {
            panic!("Expected Test action");
        }
    }

    #[test]
    fn test_parse_prediction_nested_format() {
        // Standard nested format should still work
        let text = r#"{"actions": [{"kind": "Test", "surface_id": "-u", "seed": {"setup": [], "files": []}, "prediction": {"diff_type": {"StdoutContains": "@@"}, "reason": "has markers"}}]}"#;
        let response = parse_lm_response(text).unwrap();
        if let LmAction::Test { prediction, .. } = &response.actions[0] {
            let pred = prediction.as_ref().unwrap();
            assert_eq!(
                pred.diff_type,
                PredictedDiff::StdoutContains("@@".to_string())
            );
        }
    }

    #[test]
    fn test_parse_prediction_extra_fields_ignored() {
        // Extra fields in prediction shouldn't break parsing
        let text = r#"{"actions": [{"kind": "Test", "surface_id": "--stat", "seed": {"setup": [], "files": []}, "prediction": {"diff_type": "StderrDifferent", "reason": "stat format", "confidence": 0.9, "notes": "test"}}]}"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.actions.len(), 1);
    }

    // ==================== Analysis Field Tests ====================

    #[test]
    fn test_parse_analysis_present() {
        let text = r#"{
            "analysis": {
                "--stat": "Control shows raw diff. Adding --stat appends diffstat summary.",
                "-true": "No-op unless combined with expression that suppresses default action."
            },
            "actions": [
                {"kind": "Test", "surface_id": "--stat", "seed": {"setup": [["git", "init"]], "files": []}}
            ]
        }"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.analysis.len(), 2);
        assert!(response.analysis.contains_key("--stat"));
        assert!(response.analysis.contains_key("-true"));
        assert_eq!(response.actions.len(), 1);
    }

    #[test]
    fn test_parse_analysis_absent_defaults_empty() {
        let text = r#"{
            "actions": [
                {"kind": "Test", "surface_id": "--stat", "seed": {"setup": [], "files": []}}
            ]
        }"#;
        let response = parse_lm_response(text).unwrap();
        assert!(response.analysis.is_empty());
        assert_eq!(response.actions.len(), 1);
    }

    #[test]
    fn test_parse_analysis_extra_keys_ignored() {
        // Analysis with extra unknown fields alongside it doesn't break parsing
        let text = r#"{
            "analysis": {"--stat": "control shows raw diff"},
            "thinking": "this extra field should be ignored",
            "actions": [
                {"kind": "Test", "surface_id": "--stat", "seed": {"setup": [], "files": []}}
            ]
        }"#;
        let response = parse_lm_response(text).unwrap();
        assert_eq!(response.analysis.len(), 1);
        assert_eq!(response.actions.len(), 1);
    }

    #[test]
    fn test_analysis_not_serialized_when_empty() {
        let response = LmResponse {
            analysis: HashMap::new(),
            actions: vec![],
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("analysis"));
    }

    // ==================== Seed Repair Tests ====================

    #[test]
    fn test_repair_echo_redirect() {
        let mut seed = Seed {
            setup: vec![
                vec!["git".into(), "init".into()],
                vec!["echo".into(), "hello".into(), ">".into(), "file.txt".into()],
            ],
            files: vec![],
        };
        repair_seed(&mut seed);
        assert_eq!(seed.setup, vec![vec!["git", "init"]]);
        assert_eq!(seed.files.len(), 1);
        assert_eq!(seed.files[0].path, "file.txt");
        assert_eq!(seed.files[0].content, "hello\n");
    }

    #[test]
    fn test_repair_echo_multiword_redirect() {
        let mut seed = Seed {
            setup: vec![vec![
                "echo".into(), "hello".into(), "world".into(), ">".into(), "f.txt".into(),
            ]],
            files: vec![],
        };
        repair_seed(&mut seed);
        assert_eq!(seed.files[0].content, "hello world\n");
        assert_eq!(seed.files[0].path, "f.txt");
        assert!(seed.setup.is_empty());
    }

    #[test]
    fn test_repair_echo_append_redirect() {
        let mut seed = Seed {
            setup: vec![vec![
                "echo".into(), "line".into(), ">>".into(), "out.txt".into(),
            ]],
            files: vec![],
        };
        repair_seed(&mut seed);
        assert_eq!(seed.files[0].content, "line\n");
        assert_eq!(seed.files[0].path, "out.txt");
    }

    #[test]
    fn test_repair_echo_e_redirect() {
        let mut seed = Seed {
            setup: vec![vec![
                "echo".into(), "-e".into(), "a\\nb".into(), ">".into(), "f.txt".into(),
            ]],
            files: vec![],
        };
        repair_seed(&mut seed);
        assert_eq!(seed.files[0].content, "a\nb");
        assert_eq!(seed.files[0].path, "f.txt");
    }

    #[test]
    fn test_repair_printf_redirect() {
        let mut seed = Seed {
            setup: vec![vec![
                "printf".into(), "no newline".into(), ">".into(), "f.txt".into(),
            ]],
            files: vec![],
        };
        repair_seed(&mut seed);
        assert_eq!(seed.files[0].content, "no newline");
        assert_eq!(seed.files[0].path, "f.txt");
    }

    #[test]
    fn test_repair_sh_c_redirect() {
        let mut seed = Seed {
            setup: vec![vec![
                "sh".into(), "-c".into(), "echo hello > file.txt".into(),
            ]],
            files: vec![],
        };
        repair_seed(&mut seed);
        assert!(seed.setup.is_empty());
        assert_eq!(seed.files[0].path, "file.txt");
        assert_eq!(seed.files[0].content, "hello\n");
    }

    #[test]
    fn test_repair_bash_c_redirect() {
        let mut seed = Seed {
            setup: vec![vec![
                "bash".into(), "-c".into(), "echo foo > bar.txt".into(),
            ]],
            files: vec![],
        };
        repair_seed(&mut seed);
        assert_eq!(seed.files[0].path, "bar.txt");
        assert_eq!(seed.files[0].content, "foo\n");
    }

    #[test]
    fn test_repair_sh_c_complex_skipped() {
        // Pipes and chains should NOT be repaired
        let mut seed = Seed {
            setup: vec![vec![
                "sh".into(), "-c".into(), "echo a | tee file.txt".into(),
            ]],
            files: vec![],
        };
        repair_seed(&mut seed);
        assert_eq!(seed.setup.len(), 1); // left alone
        assert!(seed.files.is_empty());
    }

    #[test]
    fn test_repair_skips_existing_file() {
        let mut seed = Seed {
            setup: vec![vec![
                "echo".into(), "new".into(), ">".into(), "f.txt".into(),
            ]],
            files: vec![FileEntry { path: "f.txt".into(), content: "original\n".into() }],
        };
        repair_seed(&mut seed);
        // Should not overwrite existing file entry
        assert_eq!(seed.files.len(), 1);
        assert_eq!(seed.files[0].content, "original\n");
    }

    #[test]
    fn test_repair_non_redirect_untouched() {
        let mut seed = Seed {
            setup: vec![
                vec!["git".into(), "init".into()],
                vec!["git".into(), "add".into(), ".".into()],
            ],
            files: vec![],
        };
        repair_seed(&mut seed);
        assert_eq!(seed.setup.len(), 2);
        assert!(seed.files.is_empty());
    }

    #[test]
    fn test_repair_mixed_commands() {
        let mut seed = Seed {
            setup: vec![
                vec!["git".into(), "init".into()],
                vec!["echo".into(), "content".into(), ">".into(), "a.txt".into()],
                vec!["git".into(), "add".into(), ".".into()],
                vec!["echo".into(), "other".into(), ">".into(), "b.txt".into()],
            ],
            files: vec![],
        };
        repair_seed(&mut seed);
        assert_eq!(seed.setup, vec![vec!["git", "init"], vec!["git", "add", "."]]);
        assert_eq!(seed.files.len(), 2);
        assert_eq!(seed.files[0].path, "a.txt");
        assert_eq!(seed.files[1].path, "b.txt");
    }

    #[test]
    fn test_repair_integration_via_parse() {
        // End-to-end: LM produces echo redirect in setup, gets repaired during parsing
        let text = r#"{"test": "--stat", "setup": [["git", "init"], ["echo", "hello", ">", "file.txt"]], "files": []}"#;
        let response = parse_lm_response(text).unwrap();
        if let LmAction::Test { seed, .. } = &response.actions[0] {
            assert_eq!(seed.setup, vec![vec!["git", "init"]]);
            assert_eq!(seed.files.len(), 1);
            assert_eq!(seed.files[0].path, "file.txt");
            assert_eq!(seed.files[0].content, "hello\n");
        } else {
            panic!("Expected Test action");
        }
    }
}
