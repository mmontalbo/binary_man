//! Characterization pass — reasoning about options before testing.
//!
//! Asks the LM a pure reading-comprehension question: "Given this option's
//! description, what input would trigger a visible output difference?"
//!
//! This separates understanding from construction. The LM reasons about
//! activation conditions first, then the seed-generation step builds against
//! that specification. Works well with weak models because each call is
//! a narrow, structured task.

use super::types::{Characterization, State, Status};
use crate::lm::{create_plugin, LmConfig};
use anyhow::Result;
use std::path::Path;

/// Maximum surfaces per characterization batch.
const BATCH_SIZE: usize = 20;

/// Characterize all pending surfaces that lack a characterization.
///
/// Runs once before the main verification loop. Each batch is a single
/// LM call that returns trigger/expected_diff pairs — no sandbox execution.
pub(super) fn characterize_surfaces(
    state: &mut State,
    pack_path: &Path,
    lm_config: &LmConfig,
    verbose: bool,
) -> Result<()> {
    let needs_characterization: Vec<String> = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Pending) && e.characterization.is_none())
        .map(|e| e.id.clone())
        .collect();

    if needs_characterization.is_empty() {
        return Ok(());
    }

    if verbose {
        eprintln!(
            "Characterizing {} surface(s)...",
            needs_characterization.len()
        );
    }

    let batches: Vec<(Vec<String>, String)> = needs_characterization
        .chunks(BATCH_SIZE)
        .map(|batch| {
            let batch_ids: Vec<String> = batch.to_vec();
            let prompt = build_characterize_prompt(state, &batch_ids);
            (batch_ids, prompt)
        })
        .collect();

    let all_results = super::run::run_parallel_lm_batches(
        batches,
        lm_config,
        verbose,
        "Characterize",
        parse_characterize_response,
    );

    let mut count = 0;
    for results in all_results {
        for (surface_id, characterization) in results {
            if let Some(entry) = state.entries.iter_mut().find(|e| e.id == surface_id) {
                if entry.characterization.is_none() {
                    entry.characterization = Some(characterization);
                    count += 1;
                }
            }
        }
    }

    if verbose {
        eprintln!(
            "Characterized {}/{} surfaces",
            count,
            needs_characterization.len()
        );
    }

    state.save(pack_path)?;
    Ok(())
}

/// Re-characterize surfaces that have failed repeatedly.
///
/// Called when a surface has N OutputsEqual outcomes and a characterization
/// that hasn't helped. Asks the LM to revise its understanding given the
/// failure evidence.
pub(super) fn recharacterize_surface(
    state: &mut State,
    pack_path: &Path,
    lm_config: &LmConfig,
    verbose: bool,
    surface_id: &str,
) -> Result<()> {
    let entry = match state.entries.iter().find(|e| e.id == surface_id) {
        Some(e) => e,
        None => return Ok(()),
    };

    let old_char = match &entry.characterization {
        Some(c) => c.clone(),
        None => return Ok(()),
    };

    let oe_count = entry
        .attempts
        .iter()
        .filter(|a| matches!(a.outcome, super::types::Outcome::OutputsEqual))
        .count();

    // Only recharacterize after 2+ OutputsEqual with an existing characterization
    if oe_count < 2 {
        return Ok(());
    }

    if verbose {
        eprintln!(
            "  Re-characterizing {} (rev {}, {} OutputsEqual)...",
            surface_id, old_char.revision, oe_count
        );
    }

    let prompt = build_recharacterize_prompt(state, surface_id, &old_char);

    let mut plugin = create_plugin(lm_config);
    plugin.init()?;

    let response_text = match super::run::invoke_lm_with_retry(&mut *plugin, &prompt, verbose) {
        Ok(text) => text,
        Err(e) => {
            if verbose {
                eprintln!("  Re-characterize failed for {}: {}", surface_id, e);
            }
            plugin.shutdown().ok();
            return Ok(());
        }
    };
    plugin.shutdown().ok();

    // Parse single-surface response
    let results = parse_characterize_response(&response_text, &[surface_id.to_string()]);
    if let Some((_, mut new_char)) = results.into_iter().next() {
        new_char.revision = old_char.revision + 1;
        if let Some(entry) = state.entries.iter_mut().find(|e| e.id == surface_id) {
            if verbose {
                eprintln!(
                    "  {} re-characterized: \"{}\" → \"{}\"",
                    surface_id, old_char.trigger, new_char.trigger
                );
            }
            entry.characterization = Some(new_char);
        }
    }

    state.save(pack_path)?;
    Ok(())
}

/// Build the characterization prompt for a batch of surfaces.
fn build_characterize_prompt(state: &State, surface_ids: &[String]) -> String {
    let mut prompt = String::new();

    let base_command = if state.context_argv.is_empty() {
        state.binary.clone()
    } else {
        format!("{} {}", state.binary, state.context_argv.join(" "))
    };

    prompt.push_str("# Characterize Options\n\n");
    prompt.push_str(&format!("Command: `{}`\n\n", base_command));

    if !state.help_preamble.is_empty() {
        prompt.push_str(&format!(
            "## Command Description\n\n{}\n\n",
            state.help_preamble
        ));
    }

    if !state.examples_section.is_empty() {
        prompt.push_str(&format!(
            "## Examples from Documentation\n\n{}\n\n",
            state.examples_section
        ));
    }

    prompt.push_str(
        "For each option below, answer two questions:\n\
         1. **trigger**: What kind of input/scenario would make this option produce \
            visibly different output compared to running without it? Be specific about \
            what properties the input needs.\n\
         2. **expected_diff**: What output difference would you see? \
            (e.g., \"different format\", \"suppressed output\", \"additional lines\")\n\n\
         Think about what the option DOES, then reason backwards to what input would \
         make that effect VISIBLE.\n\n",
    );

    prompt.push_str("## Options\n\n");

    for surface_id in surface_ids {
        if let Some(entry) = state.entries.iter().find(|e| e.id == *surface_id) {
            prompt.push_str(&format!("### {}\n", surface_id));
            prompt.push_str(&format!("Description: {}\n", entry.description));
            if let Some(hint) = &entry.value_hint {
                prompt.push_str(&format!("Value: {}\n", hint));
            }
            if let Some(context) = &entry.context {
                prompt.push_str(&format!("{}\n", context));
            }
            prompt.push('\n');
        }
    }

    prompt.push_str("## Response Format\n\n");
    prompt.push_str("IMPORTANT: Respond with ONLY a JSON object. No prose.\n\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"characterizations\": [\n");
    prompt.push_str("    {\"surface_id\": \"--example\", \"trigger\": \"input with X property\", \"expected_diff\": \"output changes in Y way\"}\n");
    prompt.push_str("  ]\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n");

    prompt
}

/// Build re-characterization prompt with failure evidence.
fn build_recharacterize_prompt(
    state: &State,
    surface_id: &str,
    old_char: &Characterization,
) -> String {
    let mut prompt = String::new();

    let base_command = if state.context_argv.is_empty() {
        state.binary.clone()
    } else {
        format!("{} {}", state.binary, state.context_argv.join(" "))
    };

    prompt.push_str("# Re-characterize Option\n\n");
    prompt.push_str(&format!("Command: `{}`\n\n", base_command));

    if let Some(entry) = state.entries.iter().find(|e| e.id == surface_id) {
        prompt.push_str(&format!("## {} \n", surface_id));
        prompt.push_str(&format!("Description: {}\n\n", entry.description));

        prompt.push_str(&format!(
            "**Previous characterization** (revision {}):\n\
             - trigger: {}\n\
             - expected_diff: {}\n\n",
            old_char.revision, old_char.trigger, old_char.expected_diff
        ));

        prompt.push_str("**This characterization hasn't worked.** Seeds built to match it produced identical output.\n\n");

        // Include attempt evidence
        let oe_attempts: Vec<_> = entry
            .attempts
            .iter()
            .filter(|a| matches!(a.outcome, super::types::Outcome::OutputsEqual))
            .collect();

        if !oe_attempts.is_empty() {
            prompt.push_str("Attempts that failed (OutputsEqual):\n");
            for (i, attempt) in oe_attempts.iter().take(3).enumerate() {
                prompt.push_str(&format!("  {}. args={:?}", i + 1, attempt.args));
                if !attempt.seed.setup.is_empty() {
                    prompt.push_str(&format!(", setup={:?}", attempt.seed.setup));
                }
                if let Some(stdout) = &attempt.stdout_preview {
                    prompt.push_str(&format!(", stdout={:?}", stdout));
                }
                prompt.push('\n');
            }
            prompt.push('\n');
        }

        // Include probe evidence
        if !entry.probes.is_empty() {
            prompt.push_str("Probe results:\n");
            for (i, probe) in entry.probes.iter().take(2).enumerate() {
                prompt.push_str(&format!("  {}. argv={:?}", i + 1, probe.argv));
                if let Some(stdout) = &probe.stdout_preview {
                    prompt.push_str(&format!(", stdout={:?}", stdout));
                }
                prompt.push('\n');
            }
            prompt.push('\n');
        }
    }

    prompt.push_str(
        "Revise your characterization. What did the previous one get wrong? \
         What input property is actually needed?\n\n",
    );
    prompt.push_str("IMPORTANT: Respond with ONLY a JSON object. No prose.\n\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"characterizations\": [\n");
    prompt.push_str(&format!(
        "    {{\"surface_id\": \"{}\", \"trigger\": \"revised trigger\", \"expected_diff\": \"revised diff\"}}\n",
        surface_id
    ));
    prompt.push_str("  ]\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n");

    prompt
}

/// Parse characterization response from LM.
fn parse_characterize_response(
    response: &str,
    expected_ids: &[String],
) -> Vec<(String, Characterization)> {
    let mut results = Vec::new();

    // Extract JSON from response
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

    if let Some(chars) = parsed.get("characterizations").and_then(|c| c.as_array()) {
        for item in chars {
            let surface_id = item
                .get("surface_id")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());

            let trigger = item
                .get("trigger")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());

            let expected_diff = item
                .get("expected_diff")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());

            if let (Some(id), Some(trigger), Some(expected_diff)) =
                (surface_id, trigger, expected_diff)
            {
                // Only accept characterizations for surfaces we asked about
                if expected_ids.iter().any(|eid| eid == &id) {
                    results.push((
                        id,
                        Characterization {
                            trigger,
                            expected_diff,
                            revision: 0,
                        },
                    ));
                }
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_characterize_response() {
        let response = r#"```json
{
  "characterizations": [
    {"surface_id": "--stat", "trigger": "any diff between two versions", "expected_diff": "summary table instead of patch"},
    {"surface_id": "--patience", "trigger": "file with repeated similar lines", "expected_diff": "different hunk boundaries"}
  ]
}
```"#;

        let expected = vec!["--stat".to_string(), "--patience".to_string()];
        let results = parse_characterize_response(response, &expected);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "--stat");
        assert!(results[0].1.trigger.contains("diff"));
        assert_eq!(results[1].0, "--patience");
        assert!(results[1].1.trigger.contains("repeated"));
    }

    #[test]
    fn test_parse_characterize_response_ignores_unknown() {
        let response = r#"{
  "characterizations": [
    {"surface_id": "--stat", "trigger": "any diff", "expected_diff": "summary"},
    {"surface_id": "--unknown", "trigger": "x", "expected_diff": "y"}
  ]
}"#;

        let expected = vec!["--stat".to_string()];
        let results = parse_characterize_response(response, &expected);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "--stat");
    }

    #[test]
    fn test_parse_characterize_response_partial() {
        // Missing expected_diff field
        let response = r#"{
  "characterizations": [
    {"surface_id": "--stat", "trigger": "any diff"},
    {"surface_id": "--name-only", "trigger": "any diff", "expected_diff": "filenames only"}
  ]
}"#;

        let expected = vec!["--stat".to_string(), "--name-only".to_string()];
        let results = parse_characterize_response(response, &expected);

        // --stat skipped (missing expected_diff), --name-only included
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "--name-only");
    }
}
