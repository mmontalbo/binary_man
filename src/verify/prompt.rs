//! Prompt generation for the LM.
//!
//! Builds structured prompts that give the LM all the context it needs to
//! decide on actions. The prompt format is intentionally simple and human-readable.

use super::types::{
    extract_known_issues, Attempt, KnownIssue, Outcome, State, Status, SurfaceCategory,
};

/// Format the known issues section for the prompt.
fn format_known_issues_section(issues: &[KnownIssue]) -> String {
    if issues.is_empty() {
        return String::new();
    }

    let mut section = String::from("## Known Issues (from all attempts)\n\n");
    for issue in issues {
        section.push_str(&format!(
            "- `{}` failed {}×: {}\n",
            issue.command, issue.count, issue.error
        ));
    }
    section.push('\n');
    section
}

/// Build a constraint section listing the valid pending surface IDs.
///
/// This prevents the LM from hallucinating surface names or targeting
/// already-resolved surfaces, which wastes ~47% of actions.
fn format_valid_surfaces_constraint(state: &State, target_ids: &[String]) -> String {
    let all_pending: Vec<&str> = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Pending))
        .map(|e| e.id.as_str())
        .collect();

    // Only emit constraint if there are surfaces outside the target set
    // (otherwise the "Surfaces Needing Work" section is already sufficient)
    if all_pending.len() <= target_ids.len() {
        return String::new();
    }

    let mut section = String::from("## Valid Surface IDs\n\n");
    section.push_str(
        "You may ONLY use these surface_id values. Any other surface_id will be rejected:\n",
    );
    for id in &all_pending {
        let marker = if target_ids.iter().any(|t| t == id) {
            " ← target"
        } else {
            ""
        };
        section.push_str(&format!("- `{}`{}\n", id, marker));
    }
    section.push('\n');
    section
}

/// Maximum characters for seed summary in attempt history.
const SEED_SUMMARY_MAX_LEN: usize = 200;

/// Format attempt history for retry prompts.
///
/// Shows the last N attempts as a compact summary to help the LM learn from
/// what didn't work. Only includes mechanical data (seed, outcome), no semantic
/// parsing of LM responses.
fn format_attempt_history(attempts: &[Attempt], max: usize) -> String {
    if attempts.is_empty() {
        return String::new();
    }

    let mut history = String::from("Prior attempts:\n");

    // Take the last N attempts
    let start = attempts.len().saturating_sub(max);
    for (i, attempt) in attempts.iter().skip(start).enumerate() {
        let attempt_num = start + i + 1;

        // Format seed summary compactly
        let seed_summary = format_seed_summary(&attempt.seed);

        // Format outcome compactly
        let outcome_summary = format_outcome_compact(&attempt.outcome);

        history.push_str(&format!(
            "- Attempt {}: seed={{{}}}, outcome={}\n",
            attempt_num, seed_summary, outcome_summary
        ));

        // Show prediction failure feedback so the LM can self-correct
        if attempt.prediction_matched == Some(false)
            && matches!(attempt.outcome, Outcome::Verified { .. })
        {
            history.push_str(
                "  ⚠ PREDICTION FAILED: outputs differed but your prediction didn't match. Surface stays Pending.\n",
            );
            if let Some(stdout) = &attempt.stdout_preview {
                history.push_str(&format!("    option_stdout: {:?}\n", stdout));
            }
            if let Some(control) = &attempt.control_stdout_preview {
                history.push_str(&format!("    control_stdout: {:?}\n", control));
            }
        }
    }

    history
}

/// Format seed as a compact summary (file names and setup commands only).
fn format_seed_summary(seed: &super::types::Seed) -> String {
    let mut parts = Vec::new();

    // Include file names (not contents)
    if !seed.files.is_empty() {
        let file_names: Vec<&str> = seed.files.iter().map(|f| f.path.as_str()).collect();
        parts.push(format!("files:{:?}", file_names));
    }

    // Include setup commands
    if !seed.setup.is_empty() {
        let setup_cmds: Vec<String> = seed.setup.iter().map(|cmd| cmd.join(" ")).collect();
        parts.push(format!("setup:{:?}", setup_cmds));
    }

    let summary = parts.join(", ");

    // Truncate if too long
    if summary.len() > SEED_SUMMARY_MAX_LEN {
        let mut end = SEED_SUMMARY_MAX_LEN;
        while end > 0 && !summary.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &summary[..end])
    } else {
        summary
    }
}

/// Format outcome compactly for attempt history.
fn format_outcome_compact(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Verified { diff_kind } => format!("Verified({:?})", diff_kind),
        Outcome::OutputsEqual => "OutputsEqual".to_string(),
        Outcome::SetupFailed { hint } => {
            format!("SetupFailed({})", super::evidence::truncate_str(hint, 50))
        }
        Outcome::Crashed { hint } => {
            format!("Crashed({})", super::evidence::truncate_str(hint, 50))
        }
        Outcome::ExecutionError { error } => format!("ExecutionError({})", error),
        Outcome::OptionError { hint } => {
            format!("OptionError({})", super::evidence::truncate_str(hint, 50))
        }
    }
}

/// Build the LM prompt for a set of target surfaces.
/// Maximum prior attempts to show in prompt history (both build_prompt and retry).
const MAX_PRIOR_ATTEMPTS: usize = 2;

pub(super) fn build_prompt(state: &State, target_ids: &[String]) -> String {
    let mut prompt = String::new();

    // Header with full base command
    let base_command = if state.context_argv.is_empty() {
        state.binary.clone()
    } else {
        format!("{} {}", state.binary, state.context_argv.join(" "))
    };
    prompt.push_str(&format!("# Behavior Verification: {}\n\n", base_command));

    // Show base command clearly
    prompt.push_str(&format!(
        "**Base command:** `{}` (your args will be appended to this)\n\n",
        base_command
    ));

    // Known issues section (aggregated from all SetupFailed attempts)
    let known_issues = extract_known_issues(state);
    prompt.push_str(&format_known_issues_section(&known_issues));

    // Examples from documentation (man page EXAMPLES section)
    if !state.examples_section.is_empty() {
        prompt.push_str("## Examples from Documentation\n\n");
        prompt.push_str(&state.examples_section);
        prompt.push_str("\n\n");
    }

    // Baseline info
    if let Some(baseline) = &state.baseline {
        prompt.push_str("## Baseline\n\n");
        prompt.push_str(&format!(
            "Full command: `{} {}`\n",
            state.binary,
            baseline.argv.join(" ")
        ));
        if !baseline.seed.setup.is_empty() {
            prompt.push_str(&format!("seed.setup: {:?}\n", baseline.seed.setup));
        }
        prompt.push('\n');
    } else {
        prompt.push_str("## Baseline\n\n");
        prompt.push_str("No baseline set yet. You must provide a SetBaseline action first.\n\n");
    }

    // Target surfaces
    prompt.push_str("## Surfaces Needing Work\n\n");
    for id in target_ids {
        if let Some(entry) = state.entries.iter().find(|e| &e.id == id) {
            prompt.push_str(&format!("### {}\n", entry.id));
            if entry.retried {
                prompt.push_str("  **Previously excluded** - try a different/creative approach\n");
            }
            prompt.push_str(&format!("Description: {}\n", entry.description));
            if let Some(context) = &entry.context {
                prompt.push_str(&format!("{}\n", context));
            }
            if let Some(hint) = &entry.value_hint {
                prompt.push_str(&format!("Value hint: {}\n", hint));
            }

            // Show characterization — what input triggers this option's effect
            if let Some(char) = &entry.characterization {
                prompt.push_str(&format!(
                    "\n**Trigger**: {}\n**Expected diff**: {}\n",
                    char.trigger, char.expected_diff
                ));
                if char.revision > 0 {
                    prompt.push_str(&format!(
                        "(revised {}× — previous characterizations didn't lead to verification)\n",
                        char.revision
                    ));
                }
                prompt.push_str("→ Build a seed that creates the trigger condition.\n");
            } else if entry.attempts.is_empty() {
                prompt.push_str(
                    "\n**First attempt** — reason about what input would make this \
                     option produce visibly different output, then build a seed for that.\n",
                );
            }

            // Show critique feedback if a prior verification was rejected
            if let Some(feedback) = &entry.critique_feedback {
                prompt.push_str(&format!(
                    "\n**CRITIQUE FEEDBACK**: A previous verification was rejected: {}\n\
                     Adjust your seed/approach to directly exercise this option's documented behavior.\n",
                    feedback
                ));
            }

            // Prescriptive strategy guidance based on OutputsEqual count.
            // Escalates the approach as repeated attempts fail, forcing the LM
            // to change strategy rather than retry the same approach.
            let oe_count = entry
                .attempts
                .iter()
                .filter(|a| matches!(a.outcome, Outcome::OutputsEqual))
                .count();
            if oe_count >= 4 {
                prompt.push_str(
                    "\n**STRATEGY (final attempt):** Invert your approach entirely. \
                     If prior seeds were complex, try the simplest possible seed. \
                     If they were simple, construct a more elaborate scenario. \
                     If all used the same file type, try a different one.\n",
                );
            } else if oe_count >= 3 {
                prompt.push_str(&format!(
                    "\n**STRATEGY (re-evaluate):** Your trigger assumption may be wrong. \
                     Ignore the characterization and reason directly from the help text: \
                     what does \"{}\" actually do? What input would make that visible?\n",
                    entry.description,
                ));
            } else if oe_count >= 2 {
                prompt.push_str(
                    "\n**STRATEGY (change approach):** Previous seeds produced identical output. \
                     Your next seed MUST differ structurally from all prior attempts — \
                     different files, different setup commands, different content strategy. \
                     Minor variations of the same approach will not work.\n",
                );
            }

            // Default-on hint: detect options that are likely enabled by default
            // (positive form of a negation pair) and have stagnated in probes.
            if let Some(neg_form) = entry.find_negation_form(&state.entries) {
                let identical_probes = entry
                    .probes
                    .iter()
                    .filter(|p| !p.outputs_differ && !p.setup_failed)
                    .count();
                if identical_probes >= 3 {
                    prompt.push_str(&format!(
                        "\n**DEFAULT-ON:** This option appears to be enabled by default ({} probes \
                         returned identical). To verify it, disable the behavior first (e.g., \
                         via configuration or by including {} in your seed setup), then test \
                         whether this option re-enables it.\n",
                        identical_probes, neg_form
                    ));
                }
            }

            // Suggest similar verified seeds from the seed bank
            let similar_seeds: Vec<_> = state
                .seed_bank
                .iter()
                .filter(|s| s.is_similar_to(&entry.id) || (s.surface_id == entry.id && s.is_starter_seed()))
                .collect();
            if !similar_seeds.is_empty() {
                prompt.push_str("\n**Suggested seeds** (from similar verified surfaces):\n");
                for seed in similar_seeds.iter().take(2) {
                    prompt.push_str(&format!("  From `{}`", seed.surface_id));
                    // Include the source surface's characterization trigger so the LM
                    // understands *why* this seed worked, not just its structure.
                    if let Some(source_entry) =
                        state.entries.iter().find(|e| e.id == seed.surface_id)
                    {
                        if let Some(char) = &source_entry.characterization {
                            prompt.push_str(&format!(" (trigger: \"{}\")", char.trigger));
                        }
                    }
                    prompt.push_str(":\n");
                    if !seed.args.is_empty() {
                        prompt.push_str(&format!("    args: {:?}\n", seed.args));
                    }
                    if !seed.seed.setup.is_empty() {
                        prompt.push_str(&format!("    setup: {:?}\n", seed.seed.setup));
                    }
                    if !seed.seed.files.is_empty() {
                        let file_names: Vec<&str> =
                            seed.seed.files.iter().map(|f| f.path.as_str()).collect();
                        prompt.push_str(&format!("    files: {:?}\n", file_names));
                    }
                }
            }

            // Show probe results (bilateral comparison evidence)
            // Always show outputs_differ probes; cap identical/failed to most recent 2.
            if !entry.probes.is_empty() {
                let differ_probes: Vec<_> = entry
                    .probes
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.outputs_differ)
                    .collect();
                let other_probes: Vec<_> = entry
                    .probes
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| !p.outputs_differ)
                    .collect();
                let omitted = other_probes.len().saturating_sub(2);
                let shown_others: Vec<_> = other_probes
                    .into_iter()
                    .rev()
                    .take(2)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();

                prompt.push_str(&format!(
                    "\n**Probes:** {} (budget: {}/{})\n\n",
                    entry.probes.len(),
                    entry.probes.len(),
                    super::types::MAX_PROBES_PER_SURFACE
                ));

                if omitted > 0 {
                    prompt.push_str(&format!(
                        "({} earlier identical/failed probes omitted)\n",
                        omitted
                    ));
                }

                for (i, probe) in differ_probes.iter().chain(shown_others.iter()) {
                    let diff_status = if probe.setup_failed {
                        "SetupFailed"
                    } else if probe.outputs_differ {
                        "OUTPUTS DIFFER ✓"
                    } else {
                        "identical"
                    };
                    prompt.push_str(&format!(
                        "  **Probe {}** (cycle {}) — {}:\n",
                        i + 1,
                        probe.cycle,
                        diff_status
                    ));
                    prompt.push_str(&format!("    argv: {:?}\n", probe.argv));
                    if !probe.seed.setup.is_empty() {
                        prompt.push_str(&format!("    seed.setup: {:?}\n", probe.seed.setup));
                    }
                    if probe.setup_failed {
                        prompt.push_str("    result: SetupFailed\n");
                    } else {
                        prompt.push_str(&format!("    exit_code: {:?}\n", probe.exit_code));
                        if let Some(stdout) = &probe.stdout_preview {
                            prompt.push_str(&format!("    option_stdout: {:?}\n", stdout));
                        }
                        if let Some(control) = &probe.control_stdout_preview {
                            prompt.push_str(&format!("    control_stdout: {:?}\n", control));
                        }
                        if let Some(stderr) = &probe.stderr_preview {
                            prompt.push_str(&format!("    stderr: {:?}\n", stderr));
                        }
                    }
                    prompt.push('\n');
                }
            }

            // Show recent attempts with detailed output information (capped)
            if !entry.attempts.is_empty() {
                prompt.push_str(&format!(
                    "\n**Attempts:** {} total\n\n",
                    entry.attempts.len()
                ));

                let show_start = entry.attempts.len().saturating_sub(MAX_PRIOR_ATTEMPTS);
                if show_start > 0 {
                    prompt.push_str(&format!(
                        "({} earlier attempt(s) omitted)\n\n",
                        show_start
                    ));
                }
                for (i, attempt) in entry.attempts.iter().enumerate().skip(show_start) {
                    prompt.push_str(&format!(
                        "  **Attempt {}** (cycle {}):\n",
                        i + 1,
                        attempt.cycle
                    ));
                    prompt.push_str(&format!("    args: {:?}\n", attempt.args));
                    if !attempt.seed.setup.is_empty() {
                        prompt.push_str(&format!("    seed.setup: {:?}\n", attempt.seed.setup));
                    }
                    if !attempt.seed.files.is_empty() {
                        let file_names: Vec<&str> =
                            attempt.seed.files.iter().map(|f| f.path.as_str()).collect();
                        prompt.push_str(&format!("    seed.files: {:?}\n", file_names));
                    }
                    prompt.push_str(&format!(
                        "    outcome: {}\n",
                        format_outcome(&attempt.outcome)
                    ));

                    // Show outputs for OutputsEqual failures - this is key diagnostic info
                    if matches!(attempt.outcome, Outcome::OutputsEqual) {
                        if let Some(stdout) = &attempt.stdout_preview {
                            prompt.push_str(&format!("    option_stdout: {:?}\n", stdout));
                        }
                        if let Some(control) = &attempt.control_stdout_preview {
                            prompt.push_str(&format!("    control_stdout: {:?}\n", control));
                        }

                        // Show output metrics to help LM understand output characteristics
                        if let Some(metrics) = &attempt.stdout_metrics {
                            prompt.push_str(&format!(
                                "    (Outputs identical: {} lines, {} bytes)\n",
                                metrics.line_count, metrics.byte_count
                            ));
                        }

                        // Show fs_diff if any files were created/modified
                        if let Some(fs_diff) = &attempt.fs_diff {
                            prompt.push_str(&format!(
                                "    fs_diff: created={:?}, modified={:?}\n",
                                fs_diff.created, fs_diff.modified
                            ));
                        }

                        // Diagnosis: compare characterization against reality
                        if let Some(char) = &entry.characterization {
                            prompt.push_str(&format!(
                                "    → DIAGNOSIS: Your trigger was \"{}\", expected \"{}\".\n\
                                 \x20   The seed didn't satisfy the trigger — outputs were identical.\n\
                                 \x20   Does the seed actually create the trigger condition?\n",
                                char.trigger, char.expected_diff
                            ));
                        } else {
                            prompt.push_str(
                                "    → Outputs matched! Try a different seed that exercises the option's effect.\n",
                            );
                        }
                    }

                    // Show stderr if present (useful for debugging)
                    if let Some(stderr) = &attempt.stderr_preview {
                        prompt.push_str(&format!("    stderr: {:?}\n", stderr));
                    }
                    prompt.push('\n');
                }
            } else {
                prompt.push_str("Attempts: 0\n");
            }
            prompt.push('\n');
        }
    }

    // Valid surface constraint — prevents hallucinated surface IDs
    prompt.push_str(&format_valid_surfaces_constraint(state, target_ids));

    // Instructions
    prompt.push_str(INSTRUCTIONS);

    prompt
}

/// Format an outcome for display in the prompt.
fn format_outcome(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Verified { diff_kind } => format!("Verified ({:?})", diff_kind),
        Outcome::OutputsEqual => {
            "OutputsEqual (output matches control - try different seed)".to_string()
        }
        Outcome::SetupFailed { hint } => format!("SetupFailed: {}", hint),
        Outcome::Crashed { hint } => format!("Crashed: {}", hint),
        Outcome::ExecutionError { error } => format!("ExecutionError: {}", error),
        Outcome::OptionError { hint } => format!("OptionError: {}", hint),
    }
}

/// Build a retry prompt that includes prior attempt history.
///
/// This is used during the retry pass for surfaces that were previously excluded.
/// Each surface only sees its own attempt history (no cross-surface hints).
pub(super) fn build_retry_prompt(
    state: &State,
    target_ids: &[String],
    prior_attempts: &std::collections::HashMap<String, Vec<Attempt>>,
) -> String {
    let mut prompt = String::new();

    // Header with full base command
    let base_command = if state.context_argv.is_empty() {
        state.binary.clone()
    } else {
        format!("{} {}", state.binary, state.context_argv.join(" "))
    };
    prompt.push_str(&format!(
        "# Behavior Verification (Retry): {}\n\n",
        base_command
    ));

    // Show base command clearly
    prompt.push_str(&format!(
        "**Base command:** `{}` (your args will be appended to this)\n\n",
        base_command
    ));

    // Known issues section
    let known_issues = extract_known_issues(state);
    prompt.push_str(&format_known_issues_section(&known_issues));

    // Baseline info
    if let Some(baseline) = &state.baseline {
        prompt.push_str("## Baseline\n\n");
        prompt.push_str(&format!(
            "Full command: `{} {}`\n",
            state.binary,
            baseline.argv.join(" ")
        ));
        if !baseline.seed.setup.is_empty() {
            prompt.push_str(&format!("seed.setup: {:?}\n", baseline.seed.setup));
        }
        prompt.push('\n');
    } else {
        prompt.push_str("## Baseline\n\n");
        prompt.push_str("No baseline set yet. You must provide a SetBaseline action first.\n\n");
    }

    // Target surfaces with prior attempt history
    prompt.push_str("## Surfaces Needing Retry\n\n");
    prompt.push_str(
        "These surfaces were previously excluded. Try a different/creative approach.\n\n",
    );

    for id in target_ids {
        if let Some(entry) = state.entries.iter().find(|e| &e.id == id) {
            prompt.push_str(&format!("### {}\n", entry.id));
            prompt.push_str(&format!("Description: {}\n", entry.description));
            if let Some(context) = &entry.context {
                prompt.push_str(&format!("{}\n", context));
            }
            if let Some(hint) = &entry.value_hint {
                prompt.push_str(&format!("Value hint: {}\n", hint));
            }

            // Show characterization
            if let Some(char) = &entry.characterization {
                prompt.push_str(&format!(
                    "\n**Trigger**: {}\n**Expected diff**: {}\n",
                    char.trigger, char.expected_diff
                ));
                prompt.push_str("→ Build a seed that creates the trigger condition.\n");
            }

            // Include prior attempt history if available (each surface only sees its own)
            if let Some(attempts) = prior_attempts.get(id) {
                if !attempts.is_empty() {
                    prompt.push_str(&format!(
                        "\n{}",
                        format_attempt_history(attempts, MAX_PRIOR_ATTEMPTS)
                    ));
                }

                // Strategy guidance based on OutputsEqual count from prior attempts
                let oe_count = attempts
                    .iter()
                    .filter(|a| matches!(a.outcome, Outcome::OutputsEqual))
                    .count();
                if oe_count >= 3 {
                    prompt.push_str(&format!(
                        "\n**STRATEGY (re-evaluate):** {} prior attempts produced identical output. \
                         Your trigger assumption is likely wrong. \
                         Reason directly from the help text: what does \"{}\" actually do? \
                         Try a fundamentally different approach.\n",
                        oe_count, entry.description,
                    ));
                } else if oe_count >= 2 {
                    prompt.push_str(
                        "\n**STRATEGY (change approach):** Previous seeds produced identical output. \
                         Your next seed MUST differ structurally from all prior attempts.\n",
                    );
                }
            }

            // Include seed suggestions from similar verified surfaces
            let similar_seeds: Vec<_> = state
                .seed_bank
                .iter()
                .filter(|s| s.is_similar_to(id) || (s.surface_id == *id && s.is_starter_seed()))
                .take(2)
                .collect();
            if !similar_seeds.is_empty() {
                prompt.push_str("\n**Suggested seeds** (from similar verified surfaces):\n");
                for seed in similar_seeds {
                    prompt.push_str(&format!("  From `{}`", seed.surface_id));
                    if let Some(source_entry) =
                        state.entries.iter().find(|e| e.id == seed.surface_id)
                    {
                        if let Some(char) = &source_entry.characterization {
                            prompt.push_str(&format!(" (trigger: \"{}\")", char.trigger));
                        }
                    }
                    prompt.push_str(":\n");
                    if !seed.args.is_empty() {
                        prompt.push_str(&format!("    args: {:?}\n", seed.args));
                    }
                    if !seed.seed.setup.is_empty() {
                        prompt.push_str(&format!("    setup: {:?}\n", seed.seed.setup));
                    }
                    if !seed.seed.files.is_empty() {
                        prompt.push_str(&format!(
                            "    files: {:?}\n",
                            seed.seed.files.iter().map(|f| &f.path).collect::<Vec<_>>()
                        ));
                    }
                }
            }

            prompt.push('\n');
        }
    }

    // Valid surface constraint — prevents hallucinated surface IDs
    prompt.push_str(&format_valid_surfaces_constraint(state, target_ids));

    // Instructions
    prompt.push_str(INSTRUCTIONS);

    prompt
}

/// Build an incremental prompt for stateful LM plugins.
///
/// This is a much shorter prompt that assumes the LM has context from previous cycles.
/// It only sends:
/// - Results from the last cycle
/// - Remaining pending surfaces (brief list)
/// - Request for next actions
pub(super) fn build_incremental_prompt(
    state: &State,
    target_ids: &[String],
    last_response: Option<&super::lm::LmResponse>,
) -> String {
    let mut prompt = String::new();

    prompt.push_str("# Cycle Update\n\n");

    // Show what happened with the last actions
    if let Some(response) = last_response {
        prompt.push_str("## Previous Actions Results\n\n");
        for action in &response.actions {
            match action {
                super::lm::LmAction::SetBaseline { .. } => {
                    if state.baseline.is_some() {
                        prompt.push_str("- SetBaseline: ✓ Baseline established\n");
                    } else {
                        prompt.push_str("- SetBaseline: ✗ Failed\n");
                    }
                }
                super::lm::LmAction::Test { surface_id, .. } => {
                    if let Some(entry) = state.entries.iter().find(|e| &e.id == surface_id) {
                        match &entry.status {
                            super::types::Status::Verified => {
                                prompt.push_str(&format!("- Test {}: ✓ Verified\n", surface_id));
                            }
                            super::types::Status::Pending => {
                                if let Some(attempt) = entry.attempts.last() {
                                    prompt.push_str(&format!(
                                        "- Test {}: {:?} - try different approach\n",
                                        surface_id,
                                        format_outcome(&attempt.outcome)
                                    ));
                                    // Include evidence on OutputsEqual so the LM can diagnose WHY
                                    if matches!(attempt.outcome, Outcome::OutputsEqual) {
                                        if let Some(control) = &attempt.control_stdout_preview {
                                            prompt.push_str(&format!(
                                                "    control_stdout: {:?}\n",
                                                control
                                            ));
                                        }
                                        if let Some(stdout) = &attempt.stdout_preview {
                                            prompt.push_str(&format!(
                                                "    option_stdout: {:?}\n",
                                                stdout
                                            ));
                                        }
                                        if let Some(metrics) = &attempt.stdout_metrics {
                                            prompt.push_str(&format!(
                                                "    (identical: {} lines, {} bytes)\n",
                                                metrics.line_count, metrics.byte_count
                                            ));
                                        }
                                        // Diagnosis against characterization
                                        if let Some(char) = &entry.characterization {
                                            prompt.push_str(&format!(
                                                "    → Trigger was \"{}\". Does your seed create that condition?\n",
                                                char.trigger
                                            ));
                                        }
                                    }
                                    // Include stderr for OptionError
                                    if matches!(attempt.outcome, Outcome::OptionError { .. }) {
                                        if let Some(stderr) = &attempt.stderr_preview {
                                            prompt.push_str(&format!("    stderr: {:?}\n", stderr));
                                        }
                                    }
                                } else {
                                    prompt.push_str(&format!(
                                        "- Test {}: Still pending\n",
                                        surface_id
                                    ));
                                }
                            }
                            super::types::Status::Excluded { reason } => {
                                prompt.push_str(&format!(
                                    "- Test {}: Excluded ({})\n",
                                    surface_id, reason
                                ));
                            }
                        }
                    }
                }
                super::lm::LmAction::Probe { surface_id, .. } => {
                    if let Some(entry) = state.entries.iter().find(|e| &e.id == surface_id) {
                        if let Some(probe) = entry.probes.last() {
                            let status = if probe.setup_failed {
                                "SetupFailed".to_string()
                            } else if probe.outputs_differ {
                                "DIFFER → auto-promoted to Test".to_string()
                            } else {
                                format!("identical (exit={})", probe.exit_code.unwrap_or(0))
                            };
                            prompt.push_str(&format!(
                                "- Probe {}: {} (probes left: {})\n",
                                surface_id,
                                status,
                                super::types::MAX_PROBES_PER_SURFACE
                                    .saturating_sub(entry.probes.len())
                            ));
                            if let Some(stdout) = &probe.stdout_preview {
                                prompt.push_str(&format!("    stdout: {:?}\n", stdout));
                            }
                            if let Some(stderr) = &probe.stderr_preview {
                                prompt.push_str(&format!("    stderr: {:?}\n", stderr));
                            }
                        }
                    }
                }
            }
        }
        prompt.push('\n');
    }

    // Brief state summary
    let verified = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, super::types::Status::Verified))
        .count();
    let excluded = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, super::types::Status::Excluded { .. }))
        .count();
    let pending = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, super::types::Status::Pending))
        .count();

    prompt.push_str(&format!(
        "**Progress:** {} verified, {} excluded, {} pending\n\n",
        verified, excluded, pending
    ));

    // Surfaces needing work (brief version with category hints)
    prompt.push_str("## Next Surfaces to Work On\n\n");
    for id in target_ids {
        if let Some(entry) = state.entries.iter().find(|e| &e.id == id) {
            prompt.push_str(&format!("- **{}**: {}\n", entry.id, entry.description));

            // Category-specific hints to guide the LM
            match &entry.category {
                SurfaceCategory::Modifier { base } => {
                    prompt.push_str(&format!(
                        "  **Modifier of `{}`** — include `{}` in extra_args so the base effect is active.\n",
                        base, base
                    ));
                }
                SurfaceCategory::TtyDependent => {
                    prompt.push_str(
                        "  **TTY-dependent** — color/highlight output may not differ in piped mode.\n",
                    );
                }
                SurfaceCategory::MetaEffect => {
                    prompt.push_str(
                        "  **Meta effect** — may affect exit code or stderr rather than stdout. Use appropriate prediction.\n",
                    );
                }
                _ => {}
            }

            if let Some(feedback) = &entry.critique_feedback {
                prompt.push_str(&format!("  **CRITIQUE**: {}\n", feedback));
            }

            if !entry.attempts.is_empty() {
                let last = entry.attempts.last().unwrap();
                prompt.push_str(&format!(
                    "  Last attempt: {:?} with args {:?}\n",
                    format_outcome(&last.outcome),
                    last.args
                ));
            }
        }
    }
    prompt.push('\n');

    // Only list batch targets in incremental prompt (full prompt retains all pending)
    if !target_ids.is_empty() {
        prompt.push_str("## Valid Surface IDs\n\n");
        prompt.push_str("You may ONLY use these surface_id values:\n");
        for id in target_ids {
            prompt.push_str(&format!("`{}`  ", id));
        }
        prompt.push_str("\n\n");
    }

    // Short instructions reminder
    prompt.push_str(
        r#"Provide your next actions as JSON:
```json
{"actions": [{"kind": "Test", "surface_id": "...", "seed": {...}}, ...]}
```

Note: surface_id is automatically included in the command. Only use "extra_args" if you need additional arguments.

Remember: Output must DIFFER from control run to verify. Try different seeds if OutputsEqual.
"#,
    );

    prompt
}

const INSTRUCTIONS: &str = concat!(
    include_str!("prompts/response_format.txt"),
    include_str!("prompts/actions.txt"),
    include_str!("prompts/probes.txt"),
    include_str!("prompts/predictions.txt"),
    include_str!("prompts/execution_model.txt"),
    include_str!("prompts/no_shell_escaping.txt"),
    include_str!("prompts/sandbox_writable_tmp.txt"),
    include_str!("prompts/response_json.txt"),
    include_str!("prompts/key_principles.txt"),
    include_str!("prompts/file_creation.txt"),
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::types::{
        Attempt, BaselineRecord, DiffKind, Seed, Status, SurfaceEntry, STATE_SCHEMA_VERSION,
    };

    #[test]
    fn test_build_prompt_no_baseline() {
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "git".to_string(),
            context_argv: vec!["diff".to_string()],
            baseline: None,
            entries: vec![SurfaceEntry {
                id: "--stat".to_string(),
                description: "Show diffstat".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 0,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let prompt = build_prompt(&state, &["--stat".to_string()]);

        assert!(prompt.contains("git diff"));
        assert!(prompt.contains("No baseline set"));
        assert!(prompt.contains("--stat"));
        assert!(prompt.contains("Show diffstat"));
        assert!(prompt.contains("baseline"));
        // Check base command is shown
        assert!(prompt.contains("Base command:"));
        assert!(prompt.contains("`git diff`"));
    }

    #[test]
    fn test_build_prompt_with_baseline() {
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "git".to_string(),
            context_argv: vec!["diff".to_string()],
            baseline: Some(BaselineRecord {
                argv: vec!["diff".to_string()],
                seed: Seed {
                    setup: vec![vec!["git".to_string(), "init".to_string()]],
                    files: vec![],
                },
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "--stat".to_string(),
                description: "Show diffstat".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 1,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let prompt = build_prompt(&state, &["--stat".to_string()]);

        // Check full command is shown
        assert!(prompt.contains("Full command: `git diff`"));
        assert!(prompt.contains("git"));
        assert!(prompt.contains("init"));
        // Check base command reminder
        assert!(prompt.contains("Base command:"));
        assert!(prompt.contains("your args will be appended"));
    }

    #[test]
    fn test_build_prompt_with_attempts() {
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "test".to_string(),
            context_argv: vec![],
            baseline: Some(BaselineRecord {
                argv: vec![],
                seed: Seed::default(),
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "--verbose".to_string(),
                description: "Be verbose".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![Attempt {
                    cycle: 1,
                    args: vec!["--verbose".to_string()],
                    full_argv: vec!["--verbose".to_string()],
                    seed: Seed::default(),
                    evidence_path: "evidence/verbose_c1.json".to_string(),
                    outcome: Outcome::OutputsEqual,
                    stdout_preview: None,
                    stderr_preview: None,
                    control_stdout_preview: None,
                    fs_diff: None,
                    stdout_metrics: None,
                    stderr_metrics: None,
                    prediction: None,
                    prediction_matched: None,
                    prediction_channel_matched: None,
                }],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 2,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let prompt = build_prompt(&state, &["--verbose".to_string()]);

        assert!(prompt.contains("**Attempts:** 1 total"));
        assert!(prompt.contains("args: [\"--verbose\"]"));
        assert!(prompt.contains("OutputsEqual"));
        // Should show the hint for OutputsEqual
        assert!(prompt.contains("Outputs matched!"));
    }

    #[test]
    fn test_format_outcome() {
        assert!(format_outcome(&Outcome::Verified {
            diff_kind: DiffKind::Stdout
        })
        .contains("Verified"));
        assert!(format_outcome(&Outcome::OutputsEqual).contains("matches control"));
        assert!(format_outcome(&Outcome::SetupFailed {
            hint: "error".to_string()
        })
        .contains("SetupFailed"));
    }

    #[test]
    fn test_build_prompt_with_context() {
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "ls".to_string(),
            context_argv: vec![],
            baseline: Some(BaselineRecord {
                argv: vec![],
                seed: Seed::default(),
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "--dereference".to_string(),
                description: "when showing file information for a symbolic link, show information for the file the link references rather than for the link itself".to_string(),
                context: Some("Related options: -H (follow symlinks on command line); -L (dereference all symlinks)".to_string()),
                value_hint: None,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 1,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let prompt = build_prompt(&state, &["--dereference".to_string()]);

        // Should show full description
        assert!(prompt.contains("symbolic link"));
        assert!(prompt.contains("references rather than"));
        // Should show context
        assert!(prompt.contains("Related options:"));
    }

    #[test]
    fn test_build_prompt_with_output_previews() {
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "ls".to_string(),
            context_argv: vec![],
            baseline: Some(BaselineRecord {
                argv: vec![],
                seed: Seed::default(),
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "--all".to_string(),
                description: "do not ignore entries starting with .".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![Attempt {
                    cycle: 1,
                    args: vec!["--all".to_string()],
                    full_argv: vec!["--all".to_string()],
                    seed: Seed::default(),
                    evidence_path: "evidence/all_c1.json".to_string(),
                    outcome: Outcome::OutputsEqual,
                    stdout_preview: Some("file1.txt\nfile2.txt\n".to_string()),
                    stderr_preview: None,
                    control_stdout_preview: Some("file1.txt\nfile2.txt\n".to_string()),
                    fs_diff: None,
                    stdout_metrics: None,
                    stderr_metrics: None,
                    prediction: None,
                    prediction_matched: None,
                    prediction_channel_matched: None,
                }],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 2,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let prompt = build_prompt(&state, &["--all".to_string()]);

        // Should show output previews for OutputsEqual
        assert!(prompt.contains("option_stdout:"));
        assert!(prompt.contains("control_stdout:"));
        assert!(prompt.contains("file1.txt"));
        // Should show the diagnostic hint
        assert!(prompt.contains("Outputs matched!"));
    }

    #[test]
    fn test_build_prompt_shows_all_attempts() {
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "test".to_string(),
            context_argv: vec![],
            baseline: Some(BaselineRecord {
                argv: vec![],
                seed: Seed::default(),
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "--opt".to_string(),
                description: "Test option".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![
                    Attempt {
                        cycle: 1,
                        args: vec!["--opt".to_string()],
                        full_argv: vec!["--opt".to_string()],
                        seed: Seed::default(),
                        evidence_path: "evidence/opt_c1.json".to_string(),
                        outcome: Outcome::OutputsEqual,
                        stdout_preview: Some("output1".to_string()),
                        stderr_preview: None,
                        control_stdout_preview: Some("output1".to_string()),
                        fs_diff: None,
                        stdout_metrics: None,
                        stderr_metrics: None,
                        prediction: None,
                        prediction_matched: None,
                    prediction_channel_matched: None,
                    },
                    Attempt {
                        cycle: 2,
                        args: vec!["--opt".to_string(), "value".to_string()],
                        full_argv: vec!["--opt".to_string(), "value".to_string()],
                        seed: Seed::default(),
                        evidence_path: "evidence/opt_c2.json".to_string(),
                        outcome: Outcome::OutputsEqual,
                        stdout_preview: Some("output2".to_string()),
                        stderr_preview: None,
                        control_stdout_preview: Some("output2".to_string()),
                        fs_diff: None,
                        stdout_metrics: None,
                        stderr_metrics: None,
                        prediction: None,
                        prediction_matched: None,
                    prediction_channel_matched: None,
                    },
                ],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 3,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let prompt = build_prompt(&state, &["--opt".to_string()]);

        // Should show both attempts
        assert!(prompt.contains("**Attempts:** 2 total"));
        assert!(prompt.contains("**Attempt 1** (cycle 1)"));
        assert!(prompt.contains("**Attempt 2** (cycle 2)"));
        assert!(prompt.contains("output1"));
        assert!(prompt.contains("output2"));
    }

    #[test]
    fn test_parse_setup_failed_hint() {
        // Standard format from evidence.rs
        let hint = r#"Setup command #10 failed: ["git", "checkout", "main"]
stderr: error: pathspec 'main' did not match any file(s) known to git"#;

        let result = crate::verify::types::parse_setup_failed_hint(hint);
        assert!(result.is_some());
        let (cmd, err) = result.unwrap();
        assert_eq!(cmd, "git checkout main");
        assert!(err.contains("pathspec"));
    }

    #[test]
    fn test_parse_setup_failed_hint_execute_error() {
        // Format for execution errors
        let hint = r#"Setup command #0 failed to execute: ["nonexistent", "cmd"]
error: No such file or directory"#;

        let result = crate::verify::types::parse_setup_failed_hint(hint);
        assert!(result.is_some());
        let (cmd, err) = result.unwrap();
        assert_eq!(cmd, "nonexistent cmd");
        assert!(err.contains("No such file"));
    }

    #[test]
    fn test_parse_debug_string_array() {
        assert_eq!(
            crate::verify::types::parse_debug_string_array(r#"["git", "checkout", "main"]"#),
            Some("git checkout main".to_string())
        );
        assert_eq!(
            crate::verify::types::parse_debug_string_array(r#"["ls", "-la"]"#),
            Some("ls -la".to_string())
        );
        assert_eq!(
            crate::verify::types::parse_debug_string_array(r#"["echo"]"#),
            Some("echo".to_string())
        );
    }

    #[test]
    fn test_extract_known_issues_with_multiple_failures() {
        // Create a state with multiple SetupFailed attempts with same error
        let make_setup_failed_attempt = |cycle: u32| Attempt {
            cycle,
            args: vec!["--test".to_string()],
            full_argv: vec!["--test".to_string()],
            seed: Seed {
                setup: vec![vec![
                    "git".to_string(),
                    "checkout".to_string(),
                    "main".to_string(),
                ]],
                files: vec![],
            },
            evidence_path: format!("evidence/test_c{}.json", cycle),
            outcome: Outcome::SetupFailed {
                hint: r#"Setup command #0 failed: ["git", "checkout", "main"]
stderr: error: pathspec 'main' did not match"#
                    .to_string(),
            },
            stdout_preview: None,
            stderr_preview: None,
            control_stdout_preview: None,
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            prediction: None,
            prediction_matched: None,
                    prediction_channel_matched: None,
        };

        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "test".to_string(),
            context_argv: vec![],
            baseline: None,
            entries: vec![
                SurfaceEntry {
                    id: "--opt1".to_string(),
                    description: "Option 1".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Pending,
                    probes: vec![],
                    attempts: vec![
                        make_setup_failed_attempt(1),
                        make_setup_failed_attempt(2),
                        make_setup_failed_attempt(3),
                    ],
                    category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                    critique_demotions: 0,
                    characterization: None,
                },
                SurfaceEntry {
                    id: "--opt2".to_string(),
                    description: "Option 2".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Pending,
                    probes: vec![],
                    attempts: vec![
                        make_setup_failed_attempt(4),
                        make_setup_failed_attempt(5),
                        make_setup_failed_attempt(6),
                    ],
                    category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                    critique_demotions: 0,
                    characterization: None,
                },
            ],
            cycle: 7,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let issues = crate::verify::types::extract_known_issues(&state);

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].command, "git checkout main");
        assert_eq!(issues[0].count, 6);
    }

    #[test]
    fn test_extract_known_issues_filters_single_occurrences() {
        // Single occurrence should not appear in known issues
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "test".to_string(),
            context_argv: vec![],
            baseline: None,
            entries: vec![SurfaceEntry {
                id: "--opt".to_string(),
                description: "Option".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![Attempt {
                    cycle: 1,
                    args: vec!["--opt".to_string()],
                    full_argv: vec!["--opt".to_string()],
                    seed: Seed::default(),
                    evidence_path: "evidence/test.json".to_string(),
                    outcome: Outcome::SetupFailed {
                        hint: r#"Setup command #0 failed: ["git", "init"]
stderr: error: already a git repo"#
                            .to_string(),
                    },
                    stdout_preview: None,
                    stderr_preview: None,
                    control_stdout_preview: None,
                    fs_diff: None,
                    stdout_metrics: None,
                    stderr_metrics: None,
                    prediction: None,
                    prediction_matched: None,
                    prediction_channel_matched: None,
                }],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 2,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let issues = crate::verify::types::extract_known_issues(&state);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_extract_known_issues_empty_state() {
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "test".to_string(),
            context_argv: vec![],
            baseline: None,
            entries: vec![],
            cycle: 0,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let issues = crate::verify::types::extract_known_issues(&state);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_build_prompt_includes_known_issues_section() {
        let make_setup_failed_attempt = |cycle: u32| Attempt {
            cycle,
            args: vec!["--test".to_string()],
            full_argv: vec!["--test".to_string()],
            seed: Seed::default(),
            evidence_path: format!("evidence/test_c{}.json", cycle),
            outcome: Outcome::SetupFailed {
                hint: r#"Setup command #0 failed: ["git", "checkout", "main"]
stderr: pathspec 'main' did not match"#
                    .to_string(),
            },
            stdout_preview: None,
            stderr_preview: None,
            control_stdout_preview: None,
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            prediction: None,
            prediction_matched: None,
                    prediction_channel_matched: None,
        };

        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "git".to_string(),
            context_argv: vec!["log".to_string()],
            baseline: None,
            entries: vec![
                SurfaceEntry {
                    id: "--stat".to_string(),
                    description: "Show stats".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Pending,
                    probes: vec![],
                    attempts: vec![make_setup_failed_attempt(1), make_setup_failed_attempt(2)],
                    category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                    critique_demotions: 0,
                    characterization: None,
                },
                SurfaceEntry {
                    id: "--oneline".to_string(),
                    description: "One line".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Pending,
                    probes: vec![],
                    attempts: vec![make_setup_failed_attempt(3), make_setup_failed_attempt(4)],
                    category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                    critique_demotions: 0,
                    characterization: None,
                },
            ],
            cycle: 5,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let prompt = build_prompt(&state, &["--stat".to_string()]);

        // Should contain the known issues section
        assert!(prompt.contains("## Known Issues (from all attempts)"));
        assert!(prompt.contains("`git checkout main` failed 4×"));
        assert!(prompt.contains("pathspec"));
    }

    #[test]
    fn test_build_prompt_no_known_issues_section_when_empty() {
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "ls".to_string(),
            context_argv: vec![],
            baseline: None,
            entries: vec![SurfaceEntry {
                id: "--all".to_string(),
                description: "Show all".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 1,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let prompt = build_prompt(&state, &["--all".to_string()]);

        // Should NOT contain the known issues section
        assert!(!prompt.contains("Known Issues"));
    }

    #[test]
    fn test_truncate_error() {
        assert_eq!(crate::verify::types::truncate_error("short", 60), "short");
        let long = "a".repeat(100);
        let result = crate::verify::types::truncate_error(&long, 60);
        assert!(result.len() <= 63); // 60 chars + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_build_prompt_with_retried_surface() {
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "ls".to_string(),
            context_argv: vec![],
            baseline: Some(BaselineRecord {
                argv: vec![],
                seed: Seed::default(),
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "--all".to_string(),
                description: "Show hidden files".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![],
                category: SurfaceCategory::General,
                retried: true, // This surface was previously excluded and is being retried
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 10,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let prompt = build_prompt(&state, &["--all".to_string()]);

        // Should show the retry hint
        assert!(prompt.contains("**Previously excluded**"));
        assert!(prompt.contains("different/creative approach"));
    }

    #[test]
    fn test_format_attempt_history_empty() {
        let history = super::format_attempt_history(&[], 2);
        assert!(history.is_empty());
    }

    #[test]
    fn test_format_attempt_history_single_attempt() {
        use crate::verify::types::FileEntry;

        let attempts = vec![Attempt {
            cycle: 1,
            args: vec!["--stat".to_string()],
            full_argv: vec!["diff".to_string(), "--stat".to_string()],
            seed: Seed {
                setup: vec![vec!["git".to_string(), "init".to_string()]],
                files: vec![FileEntry {
                    path: "test.txt".to_string(),
                    content: "content".to_string(),
                }],
            },
            evidence_path: "evidence/stat_c1.json".to_string(),
            outcome: Outcome::OutputsEqual,
            stdout_preview: None,
            stderr_preview: None,
            control_stdout_preview: None,
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            prediction: None,
            prediction_matched: None,
                    prediction_channel_matched: None,
        }];

        let history = super::format_attempt_history(&attempts, 2);
        assert!(history.contains("Prior attempts:"));
        assert!(history.contains("Attempt 1:"));
        assert!(history.contains("files:[\"test.txt\"]"));
        assert!(history.contains("setup:[\"git init\"]"));
        assert!(history.contains("outcome=OutputsEqual"));
    }

    #[test]
    fn test_format_attempt_history_limits_to_max() {
        let make_attempt = |cycle: u32, outcome: Outcome| Attempt {
            cycle,
            args: vec!["--opt".to_string()],
            full_argv: vec!["--opt".to_string()],
            seed: Seed::default(),
            evidence_path: format!("evidence/opt_c{}.json", cycle),
            outcome,
            stdout_preview: None,
            stderr_preview: None,
            control_stdout_preview: None,
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            prediction: None,
            prediction_matched: None,
                    prediction_channel_matched: None,
        };

        let attempts = vec![
            make_attempt(1, Outcome::OutputsEqual),
            make_attempt(
                2,
                Outcome::SetupFailed {
                    hint: "error 1".to_string(),
                },
            ),
            make_attempt(3, Outcome::OutputsEqual),
            make_attempt(
                4,
                Outcome::Crashed {
                    hint: "crash".to_string(),
                },
            ),
        ];

        // With max=2, should only show attempts 3 and 4
        let history = super::format_attempt_history(&attempts, 2);
        assert!(!history.contains("Attempt 1:"));
        assert!(!history.contains("Attempt 2:"));
        assert!(history.contains("Attempt 3:"));
        assert!(history.contains("Attempt 4:"));
        assert!(history.contains("Crashed(crash)"));
    }

    #[test]
    fn test_format_seed_summary_truncates() {
        use crate::verify::types::FileEntry;

        let seed = Seed {
            setup: vec![
                vec!["git".to_string(), "init".to_string()],
                vec!["git".to_string(), "add".to_string(), ".".to_string()],
                vec![
                    "git".to_string(),
                    "commit".to_string(),
                    "-m".to_string(),
                    "initial".to_string(),
                ],
            ],
            files: vec![
                FileEntry {
                    path: "file1.txt".to_string(),
                    content: "a".repeat(100),
                },
                FileEntry {
                    path: "file2.txt".to_string(),
                    content: "b".repeat(100),
                },
                FileEntry {
                    path: "very_long_file_name_that_takes_up_space.txt".to_string(),
                    content: "c".to_string(),
                },
            ],
        };

        let summary = super::format_seed_summary(&seed);
        // Should be truncated to <= 200 chars + "..."
        assert!(summary.len() <= 203, "Summary too long: {}", summary.len());
    }

    #[test]
    fn test_build_retry_prompt_includes_prior_history() {
        use std::collections::HashMap;

        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "ls".to_string(),
            context_argv: vec![],
            baseline: Some(BaselineRecord {
                argv: vec![],
                seed: Seed::default(),
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "--all".to_string(),
                description: "Show hidden files".to_string(),
                context: None,
                value_hint: None,
                category: SurfaceCategory::General,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![], // Cleared for retry
                retried: true,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 10,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        // Prior attempts from before the retry
        let mut prior_attempts = HashMap::new();
        prior_attempts.insert(
            "--all".to_string(),
            vec![
                Attempt {
                    cycle: 1,
                    args: vec!["--all".to_string()],
                    full_argv: vec!["--all".to_string()],
                    seed: Seed::default(),
                    evidence_path: "evidence/all_c1.json".to_string(),
                    outcome: Outcome::OutputsEqual,
                    stdout_preview: None,
                    stderr_preview: None,
                    control_stdout_preview: None,
                    fs_diff: None,
                    stdout_metrics: None,
                    stderr_metrics: None,
                    prediction: None,
                    prediction_matched: None,
                    prediction_channel_matched: None,
                },
                Attempt {
                    cycle: 2,
                    args: vec!["--all".to_string()],
                    full_argv: vec!["--all".to_string()],
                    seed: Seed {
                        setup: vec![vec!["touch".to_string(), ".hidden".to_string()]],
                        files: vec![],
                    },
                    evidence_path: "evidence/all_c2.json".to_string(),
                    outcome: Outcome::SetupFailed {
                        hint: "touch failed".to_string(),
                    },
                    stdout_preview: None,
                    stderr_preview: None,
                    control_stdout_preview: None,
                    fs_diff: None,
                    stdout_metrics: None,
                    stderr_metrics: None,
                    prediction: None,
                    prediction_matched: None,
                    prediction_channel_matched: None,
                },
            ],
        );

        let prompt = super::build_retry_prompt(&state, &["--all".to_string()], &prior_attempts);

        // Should contain retry header
        assert!(prompt.contains("Behavior Verification (Retry)"));
        assert!(prompt.contains("Surfaces Needing Retry"));
        // Should contain prior attempt history
        assert!(prompt.contains("Prior attempts:"));
        assert!(prompt.contains("Attempt 1:"));
        assert!(prompt.contains("Attempt 2:"));
        assert!(prompt.contains("OutputsEqual"));
        assert!(prompt.contains("SetupFailed"));
    }

    #[test]
    fn test_build_retry_prompt_no_history_for_surface_without_attempts() {
        use std::collections::HashMap;

        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "ls".to_string(),
            context_argv: vec![],
            baseline: Some(BaselineRecord {
                argv: vec![],
                seed: Seed::default(),
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "--all".to_string(),
                description: "Show hidden files".to_string(),
                context: None,
                value_hint: None,
                category: SurfaceCategory::General,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![],
                retried: true,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: None,
            }],
            cycle: 10,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        // No prior attempts for this surface
        let prior_attempts: HashMap<String, Vec<Attempt>> = HashMap::new();

        let prompt = super::build_retry_prompt(&state, &["--all".to_string()], &prior_attempts);

        // Should NOT contain prior attempts section
        assert!(!prompt.contains("Prior attempts:"));
    }

    #[test]
    fn test_build_prompt_includes_characterization() {
        use crate::verify::types::Characterization;

        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "git".to_string(),
            context_argv: vec!["diff".to_string()],
            baseline: Some(BaselineRecord {
                argv: vec!["diff".to_string()],
                seed: Seed::default(),
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "--patience".to_string(),
                description: "Generate a diff using the patience algorithm".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: Some(Characterization {
                    trigger: "file with repeated similar lines where hunk boundaries are ambiguous"
                        .to_string(),
                    expected_diff: "different hunk grouping in diff output".to_string(),
                    revision: 0,
                }),
            }],
            cycle: 1,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let prompt = build_prompt(&state, &["--patience".to_string()]);

        assert!(prompt.contains("**Trigger**: file with repeated similar lines"));
        assert!(prompt.contains("**Expected diff**: different hunk grouping"));
        assert!(prompt.contains("Build a seed that creates the trigger condition"));
    }

    #[test]
    fn test_build_prompt_outputs_equal_diagnosis_with_characterization() {
        use crate::verify::types::Characterization;

        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "git".to_string(),
            context_argv: vec!["diff".to_string()],
            baseline: Some(BaselineRecord {
                argv: vec!["diff".to_string()],
                seed: Seed::default(),
                evidence_path: "evidence/baseline.json".to_string(),
            }),
            entries: vec![SurfaceEntry {
                id: "--patience".to_string(),
                description: "Generate a diff using the patience algorithm".to_string(),
                context: None,
                value_hint: None,
                status: Status::Pending,
                probes: vec![],
                attempts: vec![Attempt {
                    cycle: 1,
                    args: vec!["--patience".to_string()],
                    full_argv: vec![
                        "git".to_string(),
                        "diff".to_string(),
                        "--patience".to_string(),
                    ],
                    seed: Seed::default(),
                    evidence_path: "evidence/patience_1.json".to_string(),
                    outcome: Outcome::OutputsEqual,
                    stdout_preview: Some("hello world".to_string()),
                    stderr_preview: None,
                    control_stdout_preview: Some("hello world".to_string()),
                    fs_diff: None,
                    stdout_metrics: None,
                    stderr_metrics: None,
                    prediction: None,
                    prediction_matched: None,
                    prediction_channel_matched: None,
                }],
                category: SurfaceCategory::General,
                retried: false,
                critique_feedback: None,
                critique_demotions: 0,
                characterization: Some(Characterization {
                    trigger: "file with repeated similar lines".to_string(),
                    expected_diff: "different hunk boundaries".to_string(),
                    revision: 0,
                }),
            }],
            cycle: 2,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
        };

        let prompt = build_prompt(&state, &["--patience".to_string()]);

        // Should show diagnosis referencing the characterization
        assert!(prompt.contains("DIAGNOSIS"));
        assert!(prompt.contains("file with repeated similar lines"));
        assert!(prompt.contains("Does the seed actually create the trigger condition"));
    }
}
