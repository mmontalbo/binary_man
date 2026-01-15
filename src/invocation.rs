//! Iterative invocation model, validation, and prompt helpers.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::lm::extract_structured_output;
use crate::scenario::{
    Scenario, ScenarioArtifacts, ScenarioBinary, ScenarioFixture, ScenarioLimits,
    MAX_ARG_LEN, MAX_RATIONALE_LEN,
};

pub(crate) const MAX_ITERATION_ROUNDS: usize = 8;
pub(crate) const MAX_INVOCATION_ARGS: usize = 32;
pub(crate) const MAX_HISTORY: usize = 32;
pub(crate) const MAX_SNIPPET_LEN: usize = 512;
pub(crate) const INVOCATION_FIXTURE_ID: &str = "fs/empty_dir";

pub(crate) const INVOCATION_LIMITS: ScenarioLimits = ScenarioLimits {
    wall_time_ms: 2000,
    cpu_time_ms: 1000,
    memory_kb: 65536,
    file_size_kb: 1024,
};

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct InvocationRequest {
    pub(crate) args: Vec<String>,
    #[serde(default)]
    pub(crate) rationale: Option<String>,
}

#[derive(Serialize, Debug, Copy, Clone)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InvocationStatus {
    Executed,
    Rejected,
    Skipped,
}

#[derive(Serialize, Debug, Clone)]
pub(crate) struct InvocationFeedback {
    pub(crate) args: Vec<String>,
    pub(crate) status: InvocationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) exit_code: Option<i32>,
    pub(crate) timed_out: bool,
    pub(crate) stdout_bytes: u64,
    pub(crate) stderr_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stdout_snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stderr_snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
}

pub(crate) struct InvocationEnvelope {
    pub(crate) invocation: InvocationRequest,
    pub(crate) invocation_bytes: Vec<u8>,
}

pub(crate) fn invocation_schema_path(root: &Path) -> PathBuf {
    root.join("schema").join("invocation.v0.json")
}

pub(crate) fn build_invocation_prompt(
    binary_path: &Path,
    help_text: &str,
    schema_text: &str,
    history: &[InvocationFeedback],
) -> String {
    let mut prompt = String::new();
    prompt.push_str("Return a single JSON object that conforms to the schema below.\n");
    prompt.push_str("Output JSON only. No prose, no code fences, and no markdown.\n");
    prompt.push_str("Begin with '{' and end with '}'.\n\n");
    prompt.push_str("Target binary path (context only):\n");
    prompt.push_str(&format!("{}\n\n", binary_path.display()));
    prompt.push_str("Schema:\n");
    prompt.push_str(schema_text);
    prompt.push_str("\n\nPrior invocations (most recent last):\n");
    prompt.push_str(&render_history(history));
    prompt.push_str("\n\nRaw help text:\n");
    prompt.push_str(help_text);
    prompt.push_str("\n\nConstraints:\n");
    prompt.push_str(" - Return exactly one invocation via args.\n");
    prompt.push_str(" - args must be an array of strings (no shell parsing).\n");
    prompt.push_str(" - Do not repeat an invocation from history.\n");
    prompt.push_str(&format!(
        " - args length must be <= {}.\n",
        MAX_INVOCATION_ARGS
    ));
    prompt.push_str(" - Use empty args (\"args\": []) to stop.\n");
    prompt
}

pub(crate) fn parse_invocation_response(
    response: &[u8],
) -> Result<InvocationEnvelope, Vec<String>> {
    let mut details = Vec::new();
    match serde_json::from_slice::<InvocationRequest>(response) {
        Ok(invocation) => {
            return Ok(InvocationEnvelope {
                invocation,
                invocation_bytes: response.to_vec(),
            })
        }
        Err(err) => details.push(format!(
            "response JSON failed to parse as invocation: {err}"
        )),
    }
    let extracted = match extract_structured_output(response) {
        Ok(extracted) => extracted,
        Err(mut errors) => {
            details.append(&mut errors);
            return Err(details);
        }
    };
    let invocation: InvocationRequest = match serde_json::from_value(extracted.value) {
        Ok(invocation) => invocation,
        Err(err) => {
            details.push(format!("extracted_output invalid: {err}"));
            return Err(details);
        }
    };
    let invocation_bytes = extracted.bytes;
    Ok(InvocationEnvelope {
        invocation,
        invocation_bytes,
    })
}

pub(crate) fn validate_invocation(invocation: &InvocationRequest) -> Option<Vec<String>> {
    let mut errors = Vec::new();
    if invocation.args.len() > MAX_INVOCATION_ARGS {
        errors.push(format!(
            "args exceeds max count ({MAX_INVOCATION_ARGS})"
        ));
    }
    if let Some(rationale) = &invocation.rationale {
        if rationale.len() > MAX_RATIONALE_LEN {
            errors.push(format!(
                "rationale exceeds max length ({MAX_RATIONALE_LEN})"
            ));
        }
        if rationale.contains('\0') {
            errors.push("rationale contains NUL".to_string());
        }
    }
    for (idx, arg) in invocation.args.iter().enumerate() {
        if arg.is_empty() {
            errors.push(format!("args[{idx}] is empty"));
        }
        if arg.len() > MAX_ARG_LEN {
            errors.push(format!("args[{idx}] exceeds max length ({MAX_ARG_LEN})"));
        }
        if arg.contains('\0') {
            errors.push(format!("args[{idx}] contains NUL"));
        }
    }

    if errors.is_empty() {
        None
    } else {
        Some(errors)
    }
}

pub(crate) fn scenario_for_invocation(
    invocation: &InvocationRequest,
    binary_path: &Path,
    sequence: u64,
) -> Scenario {
    Scenario {
        scenario_id: format!("iter_{sequence}"),
        rationale: invocation
            .rationale
            .clone()
            .unwrap_or_else(|| "Iterative invocation.".to_string()),
        binary: ScenarioBinary {
            path: binary_path.display().to_string(),
        },
        args: invocation.args.clone(),
        fixture: ScenarioFixture {
            id: INVOCATION_FIXTURE_ID.to_string(),
        },
        limits: INVOCATION_LIMITS,
        artifacts: ScenarioArtifacts {
            capture_stdout: true,
            capture_stderr: true,
            capture_exit_code: true,
        },
    }
}

pub(crate) fn summarize_output(bytes: &[u8]) -> (u64, Option<String>) {
    if bytes.is_empty() {
        return (0, None);
    }
    let max = MAX_SNIPPET_LEN.min(bytes.len());
    let snippet = String::from_utf8_lossy(&bytes[..max]).to_string();
    (bytes.len() as u64, Some(snippet))
}

fn render_history(history: &[InvocationFeedback]) -> String {
    let start = history.len().saturating_sub(MAX_HISTORY);
    let slice = &history[start..];
    serde_json::to_string_pretty(slice).unwrap_or_else(|_| "[]".to_string())
}
