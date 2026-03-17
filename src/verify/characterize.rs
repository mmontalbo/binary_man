//! Re-characterization support for options that stagnate during verification.
//!
//! When a surface has accumulated enough evidence that its characterization
//! is wrong (OutputsEqual outcomes + identical probes), this module asks the
//! LM to revise its trigger/expected_diff understanding.
//!
//! Initial characterization is now performed inline during verify cycles
//! (via trigger/expected_diff fields on LmAction::Test and LmAction::Probe).

use super::types::{Characterization, State};
use crate::lm::{create_plugin, LmConfig};
use anyhow::Result;
use std::path::Path;

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
}
