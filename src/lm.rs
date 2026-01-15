//! LM prompt assembly and Claude CLI invocation.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::runner::run_direct;
use crate::scenario::ScenarioLimits;

const HELP_LIMITS: ScenarioLimits = ScenarioLimits {
    wall_time_ms: 2000,
    cpu_time_ms: 1000,
    memory_kb: 65536,
    file_size_kb: 1024,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LmCommandConfig {
    command: Vec<String>,
}

pub(crate) struct LmCommand {
    pub(crate) argv: Vec<String>,
}

pub(crate) struct HelpCapture {
    pub(crate) bytes: Vec<u8>,
    pub(crate) source: &'static str,
    pub(crate) flag: &'static str,
}

pub(crate) struct ExtractedOutput {
    pub(crate) value: Value,
    pub(crate) bytes: Vec<u8>,
}

/// Capture help text for a binary using `--help`, falling back to `-h`.
pub(crate) fn capture_help(binary: &Path) -> Result<HelpCapture> {
    let cwd = std::env::current_dir().context("resolve cwd for help")?;
    let output = capture_help_with_arg(binary, "--help", &cwd)?;
    if !output.bytes.is_empty() {
        return Ok(output);
    }
    capture_help_with_arg(binary, "-h", &cwd)
}

/// Load the LM command configuration, falling back to Claude defaults.
pub(crate) fn load_lm_command() -> Result<LmCommand> {
    if let Ok(raw) = env::var("BMAN_LM_COMMAND") {
        let argv = parse_command_config(&raw)
            .context("parse BMAN_LM_COMMAND")?;
        return Ok(LmCommand { argv });
    }
    Ok(default_lm_command())
}

fn parse_command_config(raw: &str) -> Result<Vec<String>> {
    let config: LmCommandConfig =
        serde_json::from_str(raw).context("parse LM command JSON")?;
    if config.command.is_empty() {
        return Err(anyhow!("LM command is empty"));
    }
    Ok(config.command)
}

fn default_lm_command() -> LmCommand {
    LmCommand {
        argv: vec![
            "claude".to_string(),
            "--print".to_string(),
            "--output-format".to_string(),
            "json".to_string(),
            "--json-schema".to_string(),
            "{schema}".to_string(),
            "--no-session-persistence".to_string(),
            "--system-prompt".to_string(),
            "Return a single JSON object only. No prose or code fences.".to_string(),
            "--tools".to_string(),
            "".to_string(),
        ],
    }
}

fn capture_help_with_arg(binary: &Path, flag: &'static str, cwd: &Path) -> Result<HelpCapture> {
    let args = vec![flag.to_string()];
    let result = run_direct(binary, &args, cwd, HELP_LIMITS).context("run help command")?;
    if result.timed_out {
        return Err(anyhow!("help command timed out"));
    }
    if !result.stdout.is_empty() {
        return Ok(HelpCapture {
            bytes: result.stdout,
            source: "stdout",
            flag,
        });
    }
    Ok(HelpCapture {
        bytes: result.stderr,
        source: "stderr",
        flag,
    })
}

/// Load a UTF-8 file into a string for prompt assembly.
pub(crate) fn load_text(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Invoke the configured LM CLI to obtain a JSON response.
pub(crate) fn run_lm(prompt: &str, schema: &str, command: &LmCommand) -> Result<Vec<u8>> {
    if command.argv.is_empty() {
        return Err(anyhow!("LM command is empty"));
    }
    let mut argv = command.argv.clone();
    let mut has_placeholder = false;
    for arg in &mut argv {
        if arg == "{prompt}" {
            *arg = prompt.to_string();
            has_placeholder = true;
        }
        if arg == "{schema}" {
            *arg = schema.to_string();
        }
    }
    let program = argv.remove(0);
    let mut command = Command::new(program);
    command.args(argv);
    if has_placeholder {
        command.stdin(Stdio::null());
    } else {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let output = if has_placeholder {
        command.output().context("run LM command")?
    } else {
        let mut child = command.spawn().context("spawn LM command")?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .context("write LM prompt")?;
        }
        child.wait_with_output().context("wait LM output")?
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("LM command failed: {}", stderr.trim()));
    }
    Ok(output.stdout)
}

/// Extract structured JSON from an LM response, including Claude envelopes.
pub(crate) fn extract_structured_output(response: &[u8]) -> Result<ExtractedOutput, Vec<String>> {
    let mut details = Vec::new();
    let value: Value = match serde_json::from_slice(response) {
        Ok(value) => value,
        Err(err) => {
            details.push(format!("response JSON failed to parse: {err}"));
            return Err(details);
        }
    };

    if let Some(structured) = value.get("structured_output") {
        return value_to_extracted(structured.clone(), "structured_output", &mut details);
    }

    if let Some(result) = value.get("result").and_then(|value| value.as_str()) {
        let cleaned = strip_code_fences(result);
        let parsed: Value = match serde_json::from_str(&cleaned) {
            Ok(parsed) => parsed,
            Err(err) => {
                if let Some(parsed) = extract_json_from_text(&cleaned) {
                    return value_to_extracted(parsed, "result", &mut details);
                }
                details.push(format!("result JSON failed to parse: {err}"));
                return Err(details);
            }
        };
        return value_to_extracted(parsed, "result", &mut details);
    }

    details.push("response JSON missing structured_output or result".to_string());
    Err(details)
}

fn value_to_extracted(
    value: Value,
    label: &str,
    details: &mut Vec<String>,
) -> Result<ExtractedOutput, Vec<String>> {
    let bytes = match serde_json::to_vec_pretty(&value) {
        Ok(bytes) => bytes,
        Err(err) => {
            details.push(format!("{label} serialization failed: {err}"));
            return Err(details.clone());
        }
    };
    Ok(ExtractedOutput { value, bytes })
}

fn strip_code_fences(raw: &str) -> String {
    let trimmed = raw.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    let mut lines: Vec<&str> = trimmed.lines().collect();
    if let Some(first) = lines.first() {
        if first.trim_start().starts_with("```") {
            lines.remove(0);
        }
    }
    if let Some(last) = lines.last() {
        if last.trim_start().starts_with("```") {
            lines.pop();
        }
    }
    lines.join("\n").trim().to_string()
}

fn extract_json_from_text(raw: &str) -> Option<Value> {
    for (idx, ch) in raw.char_indices() {
        if ch != '{' {
            continue;
        }
        let slice = &raw[idx..];
        let mut deserializer = serde_json::Deserializer::from_str(slice);
        if let Ok(value) = Value::deserialize(&mut deserializer) {
            return Some(value);
        }
    }
    None
}
