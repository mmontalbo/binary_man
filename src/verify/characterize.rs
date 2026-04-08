//! Surface characterization: upfront analysis and post-failure revision.
//!
//! Upfront characterization (`characterize_pending_surfaces`) runs once after
//! bootstrap, before the verification pipeline starts. It asks the LM to
//! analyze each pending surface and produce a trigger condition and expected
//! observable diff. This frontloads the semantic reasoning that would otherwise
//! happen ad-hoc during verification cycles.
//!
//! Re-characterization (`recharacterize_surface`) revises a characterization
//! after accumulated evidence shows it was wrong.

use super::config::CHARACTERIZE_CHUNK_SIZE;
use super::types::{Characterization, State, Status};
use crate::lm::{create_plugin, LmConfig};
use anyhow::Result;
use std::path::Path;
use std::thread;

/// Characterize all pending surfaces that lack a characterization.
///
/// Runs after bootstrap/batch-probe, before the verification pipeline starts.
/// Asks the LM to produce trigger + expected_diff for each surface in bulk,
/// giving the verification phase a specification to build seeds against.
pub(super) fn characterize_pending_surfaces(
    state: &mut State,
    pack_path: &Path,
    lm_config: &LmConfig,
    verbose: bool,
) -> Result<()> {
    // Collect pending surfaces without characterizations
    let pending_ids: Vec<String> = state
        .entries
        .iter()
        .filter(|e| {
            matches!(e.status, Status::Pending)
                && e.characterization.is_none()
                && e.attempts.is_empty()
        })
        .map(|e| e.id.clone())
        .collect();

    if pending_ids.is_empty() {
        return Ok(());
    }

    if verbose {
        eprintln!(
            "Characterizing {} pending surfaces...",
            pending_ids.len()
        );
    }

    // Build all prompts upfront (needs &state, which we can't send across threads)
    let chunks: Vec<Vec<String>> = pending_ids
        .chunks(CHARACTERIZE_CHUNK_SIZE)
        .map(|c| c.to_vec())
        .collect();

    let prompts: Vec<String> = chunks
        .iter()
        .map(|chunk| build_bulk_characterize_prompt(state, chunk))
        .collect();

    // Run LM calls in parallel across chunks
    let results_collected: Vec<Vec<(String, Characterization)>> = thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .zip(prompts.iter())
            .enumerate()
            .map(|(i, (chunk_ids, prompt))| {
                s.spawn(move || {
                    let mut plugin = create_plugin(lm_config);
                    if plugin.init().is_err() {
                        return vec![];
                    }
                    let response_text =
                        match super::run::invoke_lm_with_retry(&mut *plugin, prompt, verbose) {
                            Ok(text) => text,
                            Err(e) => {
                                if verbose {
                                    eprintln!("  Characterization chunk {} failed: {}", i, e);
                                }
                                plugin.shutdown().ok();
                                return vec![];
                            }
                        };
                    plugin.shutdown().ok();
                    parse_characterize_response(&response_text, chunk_ids)
                })
            })
            .collect();

        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });

    // Apply results to state
    let mut total_characterized = 0;
    for results in results_collected {
        for (surface_id, char) in results {
            if let Some(entry) = state.find_entry_mut(&surface_id) {
                entry.characterization = Some(char);
                total_characterized += 1;
            }
        }
    }

    if verbose {
        eprintln!(
            "Characterized {}/{} surfaces",
            total_characterized,
            pending_ids.len()
        );
    }

    state.save(pack_path)?;
    Ok(())
}

/// Build a bulk characterization prompt for multiple surfaces.
///
/// Unlike the re-characterization prompt which includes failure evidence,
/// this prompt asks for initial analysis based on the help text alone.
fn build_bulk_characterize_prompt(state: &State, surface_ids: &[String]) -> String {
    let mut prompt = String::new();

    let base_command = if state.context_argv.is_empty() {
        state.binary.clone()
    } else {
        format!("{} {}", state.binary, state.context_argv.join(" "))
    };

    prompt.push_str("# Characterize Options\n\n");
    prompt.push_str(&format!("Command: `{}`\n\n", base_command));

    if !state.help_preamble.is_empty() {
        prompt.push_str(&format!("Help preamble:\n{}\n\n", state.help_preamble));
    }

    prompt.push_str(
        "For each option below, analyze what input scenario would make the option \
         produce **visibly different output** compared to running the command without it.\n\n\
         Be specific and concrete:\n\
         - BAD trigger: \"files exist\" (too vague)\n\
         - GOOD trigger: \"directory contains both empty and non-empty files\" (testable)\n\
         - BAD expected_diff: \"output changes\" (says nothing)\n\
         - GOOD expected_diff: \"stdout lists only empty files instead of all files\" (observable)\n\n",
    );

    prompt.push_str("## Options\n\n");

    for id in surface_ids {
        if let Some(entry) = state.find_entry(id) {
            prompt.push_str(&format!("### {}\n", entry.id));
            prompt.push_str(&format!("Description: {}\n", entry.description));
            if let Some(hint) = &entry.value_hint {
                prompt.push_str(&format!("Value hint: {}\n", hint));
            }
            if let Some(context) = &entry.context {
                prompt.push_str(&format!("{}\n", context));
            }
            prompt.push('\n');
        }
    }

    prompt.push_str(
        "IMPORTANT: Respond with ONLY a JSON object. No prose, no markdown outside JSON.\n\n",
    );
    prompt.push_str("```json\n{\n  \"characterizations\": [\n");
    for (i, id) in surface_ids.iter().enumerate() {
        let comma = if i + 1 < surface_ids.len() { "," } else { "" };
        prompt.push_str(&format!(
            "    {{\"surface_id\": \"{}\", \"trigger\": \"...\", \"expected_diff\": \"...\"}}{}\n",
            id, comma
        ));
    }
    prompt.push_str("  ]\n}\n```\n");

    prompt
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
    let entry = match state.find_entry(surface_id) {
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

    let identical_probes = entry
        .probes
        .iter()
        .filter(|p| !p.outputs_differ && !p.setup_failed)
        .count();

    // Only recharacterize when there's enough evidence the characterization is wrong.
    // Count both OE test attempts and identical probes as evidence. Requires
    // 2 evidence per revision to avoid re-running with no new evidence.
    let evidence_count = oe_count + identical_probes;
    let required_evidence = (old_char.revision as usize + 1) * 2;
    if evidence_count < required_evidence {
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
        if let Some(entry) = state.find_entry_mut(surface_id) {
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

    if let Some(entry) = state.find_entry(surface_id) {
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
pub(super) fn parse_characterize_response(
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

    #[test]
    fn test_bulk_characterize_prompt_structure() {
        use super::super::types::*;

        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "find".to_string(),
            context_argv: vec![".".to_string()],
            baseline: None,
            entries: vec![
                SurfaceEntry {
                    id: "-empty".to_string(),
                    description: "match empty files and directories".to_string(),
                    context: None,
                    value_hint: None,
                    category: SurfaceCategory::General,
                    status: Status::Pending,
                    probes: vec![],
                    attempts: vec![],
                    retried: false,
                    critique_feedback: None,
                    critique_demotions: 0,
                    characterization: None,
                },
                SurfaceEntry {
                    id: "-name".to_string(),
                    description: "match filename pattern".to_string(),
                    context: None,
                    value_hint: Some("<pattern>".to_string()),
                    category: SurfaceCategory::ValueRequired,
                    status: Status::Pending,
                    probes: vec![],
                    attempts: vec![],
                    retried: false,
                    critique_feedback: None,
                    critique_demotions: 0,
                    characterization: None,
                },
            ],
            cycle: 0,
            seed_bank: vec![],
            help_preamble: "find - search for files".to_string(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let ids = vec!["-empty".to_string(), "-name".to_string()];
        let prompt = build_bulk_characterize_prompt(&state, &ids);

        assert!(prompt.contains("find ."));
        assert!(prompt.contains("### -empty"));
        assert!(prompt.contains("### -name"));
        assert!(prompt.contains("Value hint: <pattern>"));
        assert!(prompt.contains("find - search for files"));
        assert!(prompt.contains("\"characterizations\""));
        // Template shows both surface IDs
        assert!(prompt.contains("\"-empty\""));
        assert!(prompt.contains("\"-name\""));
    }
}
