//! Critique pass for validating verified surfaces.
//!
//! Reviews all Verified surfaces to catch false positives where outputs
//! differed but didn't actually demonstrate the documented behavior.

use super::config::{CRITIQUE_BATCH_SIZE, CRITIQUE_EXCLUSION_THRESHOLD, CRITIQUE_OUTPUT_MAX_LEN};
use super::types::{State, Status};
use crate::lm::LmConfig;
use anyhow::Result;
use std::fs;
use std::path::Path;

/// Critique action from LM.
#[derive(Debug, Clone)]
enum Action {
    /// Surface correctly demonstrates documented behavior.
    Accept,
    /// Surface should be retried — outputs differed but didn't demonstrate behavior.
    Demote { reason: String },
}

/// Critique a specific set of verified surfaces.
///
/// Can be called inline after each cycle to critique newly-verified surfaces
/// immediately, or in batch at the end.
pub(super) fn critique_surfaces(
    state: &mut State,
    pack_path: &Path,
    lm_config: &LmConfig,
    verbose: bool,
    surface_ids: &[String],
) -> Result<()> {
    let verified_ids: Vec<String> = surface_ids
        .iter()
        .filter(|id| {
            state
                .entries
                .iter()
                .any(|e| &e.id == *id && matches!(e.status, Status::Verified))
        })
        .cloned()
        .collect();

    if verified_ids.is_empty() {
        return Ok(());
    }

    if verbose {
        eprintln!("  Critiquing {} verified surface(s)...", verified_ids.len());
    }

    let batches: Vec<(Vec<String>, String)> = verified_ids
        .chunks(CRITIQUE_BATCH_SIZE)
        .map(|batch| {
            let batch_ids: Vec<String> = batch.to_vec();
            let prompt = build_prompt(state, &batch_ids, pack_path);
            (batch_ids, prompt)
        })
        .collect();

    let all_judgments = super::run::run_parallel_lm_batches(
        batches,
        lm_config,
        verbose,
        "Critique",
        |response_text, _batch_ids| parse_response(response_text),
    );

    // Apply all judgments sequentially
    let mut demoted_count = 0;
    for judgments in all_judgments {
        for (surface_id, action) in judgments {
            if let Some(entry) = state.entries.iter_mut().find(|e| e.id == surface_id) {
                match action {
                    Action::Accept => {
                        if verbose {
                            eprintln!("  {} → ACCEPT", surface_id);
                        }
                    }
                    Action::Demote { reason } => {
                        entry.critique_demotions += 1;
                        if entry.critique_demotions >= CRITIQUE_EXCLUSION_THRESHOLD {
                            entry.status = Status::Excluded {
                                reason: format!(
                                    "Critique-irreconcilable after {} demotions: {}",
                                    entry.critique_demotions, reason
                                ),
                            };
                            if verbose {
                                eprintln!(
                                    "  {} → EXCLUDED (critique-irreconcilable, {} demotions)",
                                    surface_id, entry.critique_demotions
                                );
                            }
                        } else {
                            entry.status = Status::Pending;
                            entry.critique_feedback = Some(reason.clone());
                            if verbose {
                                eprintln!("  {} → DEMOTE ({})", surface_id, reason);
                            }
                        }
                        demoted_count += 1;
                    }
                }
            }
        }
    }

    if verbose {
        eprintln!(
            "Critique complete: {} demoted, {} confirmed",
            demoted_count,
            verified_ids.len() - demoted_count
        );
    }

    state.save(pack_path)?;
    Ok(())
}

/// Evidence outputs read from files.
struct EvidenceOutputs {
    control_stdout: String,
    option_stdout: String,
    option_stderr: String,
    control_exit_code: Option<i64>,
    option_exit_code: Option<i64>,
    /// Files created/modified/deleted by the option run.
    option_fs_diff: Option<(Vec<String>, Vec<String>, Vec<String>)>,
}

/// Build a critique prompt for a batch of verified surfaces.
fn build_prompt(state: &State, surface_ids: &[String], pack_path: &Path) -> String {
    let mut prompt = String::new();

    prompt.push_str("# Critique Task\n\n");
    prompt.push_str("Review these verified CLI option tests. Each was marked 'verified' because its output differed from the control run.\n\n");
    prompt.push_str("Your job: Determine if the output difference actually demonstrates the documented behavior.\n\n");
    prompt.push_str("## Actions\n\n");
    prompt.push_str("- **ACCEPT**: The diff clearly shows the option working as documented\n");
    prompt.push_str("- **DEMOTE**: The diff exists but doesn't demonstrate the behavior (e.g., error message, unrelated change)\n\n");

    prompt.push_str("## Surfaces to Review\n\n");

    for surface_id in surface_ids {
        if let Some(entry) = state.entries.iter().find(|e| e.id == *surface_id) {
            prompt.push_str(&format!("### {}\n\n", surface_id));
            prompt.push_str(&format!("**Description**: {}\n\n", entry.description));

            if let Some(attempt) = entry.attempts.last() {
                prompt.push_str(&format!("**Args**: {:?}\n\n", attempt.args));

                let evidence = read_evidence_outputs(pack_path, &attempt.evidence_path);

                if evidence.control_exit_code != evidence.option_exit_code {
                    prompt.push_str(&format!(
                        "**Exit codes**: control={:?}, option={:?}\n\n",
                        evidence.control_exit_code, evidence.option_exit_code
                    ));
                }

                if !evidence.control_stdout.is_empty() && !evidence.option_stdout.is_empty() {
                    let diff =
                        compute_unified_diff(&evidence.control_stdout, &evidence.option_stdout);
                    if !diff.is_empty() {
                        prompt.push_str("**Diff (control vs option)**:\n```diff\n");
                        prompt.push_str(&super::evidence::truncate_str(&diff, CRITIQUE_OUTPUT_MAX_LEN));
                        prompt.push_str("\n```\n\n");
                    }
                }

                if !evidence.control_stdout.is_empty() {
                    prompt.push_str("**Control stdout** (truncated):\n```\n");
                    prompt.push_str(&super::evidence::truncate_str(
                        &evidence.control_stdout,
                        800,
                    ));
                    prompt.push_str("\n```\n\n");
                }
                if !evidence.option_stdout.is_empty() {
                    prompt.push_str("**Option stdout** (truncated):\n```\n");
                    prompt.push_str(&super::evidence::truncate_str(&evidence.option_stdout, 800));
                    prompt.push_str("\n```\n\n");
                }
                if !evidence.option_stderr.is_empty() {
                    prompt.push_str("**Option stderr**:\n```\n");
                    prompt.push_str(&super::evidence::truncate_str(&evidence.option_stderr, 400));
                    prompt.push_str("\n```\n\n");
                }

                if let Some((created, modified, deleted)) = &evidence.option_fs_diff {
                    prompt.push_str("**Filesystem changes** (option run):\n");
                    for f in created {
                        prompt.push_str(&format!("  + created: {}\n", f));
                    }
                    for f in modified {
                        prompt.push_str(&format!("  ~ modified: {}\n", f));
                    }
                    for f in deleted {
                        prompt.push_str(&format!("  - deleted: {}\n", f));
                    }
                    if evidence.option_stdout.is_empty() {
                        prompt.push_str("  (Note: stdout is empty — the option may redirect output to a file)\n");
                    }
                    prompt.push('\n');
                }

                prompt.push_str(&format!("**Outcome**: {:?}\n\n", attempt.outcome));

                match attempt.prediction_matched {
                    Some(true) => {
                        prompt.push_str("**Prediction**: MATCHED (LM predicted this behavior, recommend ACCEPT)\n\n");
                    }
                    Some(false) => {
                        prompt.push_str("**Prediction**: FAILED (LM predicted different behavior, recommend DEMOTE)\n\n");
                    }
                    None => {
                        prompt.push_str("**Prediction**: None (no prediction made)\n\n");
                    }
                }
            }
        }
    }

    prompt.push_str("## Response Format\n\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"judgments\": [\n");
    prompt.push_str("    {\"surface_id\": \"--option\", \"action\": \"ACCEPT\"},\n");
    prompt.push_str("    {\"surface_id\": \"--other\", \"action\": \"DEMOTE\", \"reason\": \"error message, not behavior\"}\n");
    prompt.push_str("  ]\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n");

    prompt
}

/// Read stdout/stderr/exit_code from evidence files.
fn read_evidence_outputs(pack_path: &Path, evidence_path: &str) -> EvidenceOutputs {
    let option_path = pack_path.join(evidence_path);
    let control_path_str = evidence_path.replace(".json", "_control.json");
    let control_path = pack_path.join(&control_path_str);

    let mut result = EvidenceOutputs {
        control_stdout: String::new(),
        option_stdout: String::new(),
        option_stderr: String::new(),
        control_exit_code: None,
        option_exit_code: None,
        option_fs_diff: None,
    };

    if let Ok(content) = fs::read_to_string(&option_path) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            result.option_stdout = json
                .get("stdout")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            result.option_stderr = json
                .get("stderr")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            result.option_exit_code = json.get("exit_code").and_then(|v| v.as_i64());

            // Read fs_diff if present
            if let Some(fs_diff) = json.get("fs_diff") {
                let extract = |key| -> Vec<String> {
                    fs_diff
                        .get(key)
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let created = extract("created");
                let modified = extract("modified");
                let deleted = extract("deleted");
                if !created.is_empty() || !modified.is_empty() || !deleted.is_empty() {
                    result.option_fs_diff = Some((created, modified, deleted));
                }
            }
        }
    }

    if let Ok(content) = fs::read_to_string(&control_path) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            result.control_stdout = json
                .get("stdout")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            result.control_exit_code = json.get("exit_code").and_then(|v| v.as_i64());
        }
    }

    result
}

/// Compute a simple unified diff between two strings.
fn compute_unified_diff(control: &str, option: &str) -> String {
    let control_lines: Vec<&str> = control.lines().collect();
    let option_lines: Vec<&str> = option.lines().collect();

    let mut diff = String::new();
    let max_lines = control_lines.len().max(option_lines.len()).min(100);

    for i in 0..max_lines {
        let ctrl = control_lines.get(i).copied().unwrap_or("");
        let opt = option_lines.get(i).copied().unwrap_or("");

        if ctrl != opt {
            if !ctrl.is_empty() {
                diff.push_str(&format!("-{}\n", ctrl));
            }
            if !opt.is_empty() {
                diff.push_str(&format!("+{}\n", opt));
            }
        } else if !ctrl.is_empty() {
            diff.push_str(&format!(" {}\n", ctrl));
        }
    }

    compress_diff_context(&diff, 3)
}

/// Compress diff to show only N lines of context around changes.
fn compress_diff_context(diff: &str, context_lines: usize) -> String {
    let lines: Vec<&str> = diff.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let is_change: Vec<bool> = lines
        .iter()
        .map(|l| l.starts_with('+') || l.starts_with('-'))
        .collect();

    let mut keep = vec![false; lines.len()];
    for (i, &is_ch) in is_change.iter().enumerate() {
        if is_ch {
            let start = i.saturating_sub(context_lines);
            let end = (i + context_lines + 1).min(lines.len());
            for k in &mut keep[start..end] {
                *k = true;
            }
        }
    }

    let mut result = String::new();
    let mut in_skip = false;
    for (i, &line) in lines.iter().enumerate() {
        if keep[i] {
            if in_skip {
                result.push_str("...\n");
                in_skip = false;
            }
            result.push_str(line);
            result.push('\n');
        } else {
            in_skip = true;
        }
    }

    result
}

/// Parse critique response from LM.
fn parse_response(response: &str) -> Vec<(String, Action)> {
    let mut results = Vec::new();

    let json_str = if let Some(start) = response.find('{') {
        if let Some(end) = response.rfind('}') {
            &response[start..=end]
        } else {
            return results;
        }
    } else {
        return results;
    };

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return results,
    };

    if let Some(judgments) = parsed.get("judgments").and_then(|j| j.as_array()) {
        for judgment in judgments {
            let surface_id = judgment
                .get("surface_id")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());

            let action_str = judgment.get("action").and_then(|a| a.as_str());

            if let (Some(id), Some(action)) = (surface_id, action_str) {
                let critique_action = match action.to_uppercase().as_str() {
                    "ACCEPT" => Action::Accept,
                    "DEMOTE" => {
                        let reason = judgment
                            .get("reason")
                            .and_then(|r| r.as_str())
                            .unwrap_or("demoted by critique")
                            .to_string();
                        Action::Demote { reason }
                    }
                    _ => continue,
                };
                results.push((id, critique_action));
            }
        }
    }

    results
}
