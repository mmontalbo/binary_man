//! Report formatting for analysis results.
//!
//! Single output format: behavioral groups with sensitivity, diffs, and anomaly notes.

use std::collections::{HashMap, HashSet};

use crate::analyze::AnalysisMetrics;
use crate::discover::FlagInfo;
use crate::output;

/// Extract the canonical flag stem from a run label.
/// `"-b" "input.txt"` → `"-b"`
/// `"--width=10" "."` → `"--width"`
/// `"--full-time" "-b" "."` → `"--full-time -b"` (combination)
/// Returns None for runs with no flags (bare positional args).
pub fn flag_stem(label: &str) -> Option<String> {
    let args: Vec<&str> = label.split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s)
        .collect();
    let flags: Vec<String> = args.iter()
        .filter(|a| a.starts_with('-'))
        .map(|a| {
            // Strip =value suffix: "--width=10" → "--width"
            if let Some(eq) = a.find('=') { a[..eq].to_string() }
            else { a.to_string() }
        })
        .collect();
    if flags.is_empty() { None }
    else { Some(flags.join(" ")) }
}

/// Resolve a flag stem to its canonical form via alias map.
/// "-b" with alias -b = --ignore-leading-blanks → "-b" (keep shorter form)
pub fn canonical_flag(stem: &str, aliases: Option<&HashMap<String, String>>) -> String {
    if let Some(aliases) = aliases {
        // For single flags, check alias
        if !stem.contains(' ') {
            if let Some(alias) = aliases.get(stem) {
                // Return the shorter one as canonical
                if alias.len() < stem.len() { return alias.clone(); }
            }
        }
    }
    stem.to_string()
}

/// Classify a flag stem as solo (one flag) or combination (multiple flags).
pub fn is_combination(stem: &str) -> bool {
    stem.contains(' ')
}

/// Compute indistinguishable flag stems from metrics + ever_isolated.
/// Returns canonical flag stems that are in identical groups and haven't been
/// distinguished by any run (solo or combination) across all rounds.
pub fn indistinguishable_stems(
    metrics: &AnalysisMetrics,
    ever_isolated: &HashSet<String>,
    aliases: Option<&HashMap<String, String>>,
) -> Vec<String> {
    // Build the distinguished set (same logic as format_exploration_report)
    let mut distinguished: HashSet<String> = HashSet::new();
    for label in ever_isolated {
        let Some(stem) = flag_stem(label) else { continue };
        if is_combination(&stem) {
            for part in stem.split(' ') {
                distinguished.insert(canonical_flag(part, aliases));
            }
        } else {
            distinguished.insert(canonical_flag(&stem, aliases));
        }
    }

    // Also include pairwise evidence
    let pairwise = metrics.pairwise_distinguished();
    for flag in &pairwise {
        distinguished.insert(canonical_flag(flag, aliases));
    }

    // Find stems in identical groups that aren't distinguished
    let mut indistinguishable: HashSet<String> = HashSet::new();
    for group in &metrics.groups {
        if group.isolated() { continue; }
        for label in &group.run_labels {
            let Some(stem) = flag_stem(label) else { continue };
            if is_combination(&stem) { continue; }
            let canon = canonical_flag(&stem, aliases);
            if !distinguished.contains(&canon) {
                indistinguishable.insert(canon);
            }
        }
    }

    let mut sorted: Vec<String> = indistinguishable.into_iter().collect();
    sorted.sort();
    sorted
}

/// Format analysis results for a probe execution.
pub fn format_run_report(
    metrics: &AnalysisMetrics,
    flag_info: Option<&FlagInfo>,
    probe_name: &str,
    cell_count: usize,
    setup_failures: &HashMap<String, String>,
) -> String {
    let mut out = String::new();

    // Header
    out.push_str(&format!(
        "# Results for {}\n# {} contexts, {} runs, {} cells\n",
        probe_name, metrics.context_count, metrics.total_runs, cell_count
    ));

    // Alias map
    if let Some(fi) = flag_info {
        let alias_str = format_alias_map(&fi.aliases);
        if !alias_str.is_empty() {
            out.push_str(&format!("# Aliases: {}\n", alias_str));
        }
    }

    // Setup failures
    for (ctx, err) in setup_failures {
        out.push_str(&format!("\n# SETUP FAILED {}: {}\n", ctx, err));
    }

    // Group summary
    let total_runs: usize = metrics.groups.iter().map(|g| g.run_labels.len()).sum();
    out.push_str(&format!("\n# {} runs in {} behavioral groups\n", total_runs, metrics.groups.len()));

    // Describe runs using flag info
    let describe_run = |args_str: &str| -> String {
        if let Some(fi) = flag_info {
            // Parse args from the formatted string
            let args: Vec<String> = args_str.split('"')
                .enumerate()
                .filter(|(i, _)| i % 2 == 1)
                .map(|(_, s)| s.to_string())
                .collect();
            for arg in &args {
                if arg.starts_with('-') {
                    let key = if let Some(eq) = arg.find('=') { &arg[..eq] } else { arg.as_str() };
                    if let Some(desc) = fi.descs.get(key) {
                        return format!("  # {}", desc);
                    }
                }
            }
        }
        String::new()
    };

    // Groups
    for (gi, group) in metrics.groups.iter().enumerate() {
        out.push_str(&format!("\n## group {} ({} runs): {}\n",
            gi + 1, group.run_labels.len(), group.run_labels.join(", ")));

        // Flag descriptions
        for label in &group.run_labels {
            let desc = describe_run(label);
            if !desc.is_empty() {
                out.push_str(&format!("  {}:{}\n", label, desc));
            }
        }

        // Summary: universals + sensitivity
        let mut summary = group.universals.clone();
        if !group.sensitivity.is_empty() {
            summary.push(format!("sensitive to: {}", group.sensitivity.join(", ")));
        }
        if !summary.is_empty() {
            out.push_str(&format!("  {}\n", summary.join(" | ")));
        }

        // Representative observation
        out.push_str(&format!("  {}:\n", output::format_context_group(
            &group.majority_contexts.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            metrics.context_count)));
        output::format_obs(&mut out, &group.majority_obs, "    ");

        // vs-diffs
        if !group.vs_diffs.is_empty() {
            let ref_str = group.from_ref.as_ref().map(|r| output::format_args(r)).unwrap_or_default();
            let all_same = group.vs_diffs.iter().all(|(_, d)| *d == group.vs_diffs[0].1);
            if all_same {
                out.push_str(&format!("  vs {}: {}\n", ref_str, group.vs_diffs[0].1));
            } else {
                for (args, diff) in &group.vs_diffs {
                    out.push_str(&format!("  {} vs {}: {}\n", args, ref_str, diff));
                }
            }
        }

        // Anomaly notes
        if output::has_anomalies(&group.majority_obs, None) {
            let exit = group.majority_obs.exit_code.unwrap_or(-1);
            if exit > 128 {
                out.push_str(&format!("  ANOMALY: {}\n", output::format_exit(exit)));
            }
            let trace = output::format_trace_summary(&group.majority_obs);
            if !trace.is_empty() {
                out.push_str(&format!("  ANOMALY: {}\n", trace));
            }
        }
    }

    // Untested flags
    if !metrics.untested_flags.is_empty() {
        out.push_str(&format!("\n# Not tested ({}/{}): {}\n",
            metrics.untested_flags.len(),
            flag_info.map(|fi| fi.all_flags.len()).unwrap_or(0),
            metrics.untested_flags.join(", ")));
    }

    out
}

/// A round's summary for the exploration report.
pub struct RoundSummary {
    pub round: usize,
    pub total_groups: usize,
    pub isolated: usize,
    pub identical: usize,
    pub strategies: Vec<String>,
}

/// Format the final exploration report after iterative refinement.
///
/// `ever_isolated` is the cumulative set of run labels isolated across all rounds.
/// The report deduplicates to unique flag stems and separates solo-flag isolation
/// from combination-based evidence.
pub fn format_exploration_report(
    rounds: &[RoundSummary],
    final_metrics: &AnalysisMetrics,
    flag_info: Option<&FlagInfo>,
    ever_isolated: &HashSet<String>,
    binary_label: &str,
) -> String {
    let mut out = String::new();
    let aliases = flag_info.map(|fi| &fi.aliases);
    // total_flags from --help (includes both short and long forms of aliases)

    // Classify all ever_isolated run labels into flag stems
    let mut solo_distinguished: HashSet<String> = HashSet::new();
    let mut combo_distinguished: HashSet<String> = HashSet::new();
    let mut combo_evidence: HashMap<String, Vec<String>> = HashMap::new();

    for label in ever_isolated {
        let Some(stem) = flag_stem(label) else { continue };
        if is_combination(&stem) {
            for part in stem.split(' ') {
                let canon = canonical_flag(part, aliases);
                if !solo_distinguished.contains(&canon) {
                    combo_distinguished.insert(canon.clone());
                    combo_evidence.entry(canon).or_default().push(label.clone());
                }
            }
        } else {
            let canon = canonical_flag(&stem, aliases);
            solo_distinguished.insert(canon.clone());
            combo_distinguished.remove(&canon);
        }
    }

    // Pairwise distinguishability: flags proven different by cross-group evidence
    // (X+A in group 1, X+B in group 2 → A ≠ B)
    let pairwise = final_metrics.pairwise_distinguished();
    for flag in &pairwise {
        let canon = canonical_flag(flag, aliases);
        if !solo_distinguished.contains(&canon) && !combo_distinguished.contains(&canon) {
            combo_distinguished.insert(canon.clone());
            combo_evidence.entry(canon).or_default().push("(pairwise evidence)".into());
        }
    }

    // Detect behavioral aliases: 2-run groups where both runs are solo flags
    // with different flag names but identical behavior. These are aliases even
    // if the --help alias map doesn't list them.
    let mut behavioral_aliases: HashMap<String, String> = HashMap::new();
    for group in &final_metrics.groups {
        if group.run_labels.len() != 2 { continue; }
        let stems: Vec<Option<String>> = group.run_labels.iter()
            .map(|l| flag_stem(l).filter(|s| !is_combination(s)))
            .collect();
        if let [Some(a), Some(b)] = &stems[..] {
            let ca = canonical_flag(a, aliases);
            let cb = canonical_flag(b, aliases);
            if ca != cb {
                // These two different flag stems have identical behavior — behavioral aliases
                let (primary, secondary) = if ca.len() <= cb.len() { (ca, cb) } else { (cb, ca) };
                behavioral_aliases.insert(secondary, primary);
            }
        }
    }

    let all_distinguished: HashSet<&String> = solo_distinguished.iter()
        .chain(combo_distinguished.iter())
        .collect();

    // Compute unique stem count from --help aliases only (not behavioral aliases).
    // Behavioral aliases are reported separately — they may be genuinely different
    // flags that are indistinguishable under tested conditions.
    let mut all_stems: HashSet<String> = HashSet::new();
    if let Some(fi) = flag_info {
        for flag in &fi.all_flags {
            all_stems.insert(canonical_flag(flag, aliases));
        }
    }
    let untested_count = final_metrics.untested_flags.len();
    let unique_stem_count = all_stems.len();

    // Identify indistinguishable flags (in identical groups, not distinguished by any means)
    let mut indistinguishable_groups: Vec<Vec<String>> = Vec::new();
    for group in &final_metrics.groups {
        if group.isolated() { continue; }
        let stems: Vec<String> = group.run_labels.iter()
            .filter_map(|l| flag_stem(l))
            .filter(|s| !is_combination(s))
            .map(|s| canonical_flag(&s, aliases))
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|s| !all_distinguished.contains(s))
            .collect();
        if stems.len() >= 2 {
            indistinguishable_groups.push(stems);
        }
    }

    out.push_str(&format!("# Exploration: {}\n\n", binary_label));

    // Round history
    out.push_str("## Rounds\n");
    for r in rounds {
        let strat = if r.strategies.is_empty() { "refinement".into() }
            else { r.strategies.join("+") };
        out.push_str(&format!("  round {}: {} isolated cumulative, {} identical in round ({})\n",
            r.round, r.isolated, r.identical, strat));
    }
    out.push('\n');

    // Classify evidence quality: did we actually see the flag work?
    // A flag has "observed behavior" if any of its runs exited 0 with either:
    // - non-empty stdout (the flag produced visible output), OR
    // - filesystem changes (the flag modified files — silent tools like cp, mv, rm)
    let has_observed_behavior = |stem: &str| -> bool {
        for run in &final_metrics.runs {
            let run_stem = flag_stem(&run.args_str);
            if run_stem.as_deref() == Some(stem) || run_stem.as_ref().map(|s| canonical_flag(s, aliases)) == Some(stem.to_string()) {
                for (_, obs) in &run.context_groups {
                    if obs.exit_code == Some(0)
                        && (!obs.stdout.trim().is_empty() || !obs.fs_changes.is_empty())
                    {
                        return true;
                    }
                }
            }
        }
        false
    };

    let solo_observed: Vec<&String> = solo_distinguished.iter()
        .filter(|s| has_observed_behavior(s)).collect();
    let solo_error_only: Vec<&String> = solo_distinguished.iter()
        .filter(|s| !has_observed_behavior(s)).collect();
    let combo_observed: Vec<&String> = combo_distinguished.iter()
        .filter(|s| has_observed_behavior(s)).collect();
    let combo_error_only: Vec<&String> = combo_distinguished.iter()
        .filter(|s| !has_observed_behavior(s)).collect();

    let total_observed = solo_observed.len() + combo_observed.len();
    let total_error_only = solo_error_only.len() + combo_error_only.len();

    // Flag distinguishability summary
    let untested = final_metrics.untested_flags.len();
    let distinguished_count = all_distinguished.len().min(unique_stem_count);
    out.push_str(&format!("## Distinguished: {}/{} flags ({} observed behavior)\n",
        distinguished_count, unique_stem_count, total_observed));
    out.push_str(&format!("  {} observed behavior ({} solo, {} via combination)\n",
        total_observed, solo_observed.len(), combo_observed.len()));
    if total_error_only > 0 {
        out.push_str(&format!("  {} error-differentiated only ({} solo, {} via combination)\n",
            total_error_only, solo_error_only.len(), combo_error_only.len()));
    }
    if !behavioral_aliases.is_empty() {
        out.push_str(&format!("  {} behavioral aliases detected\n", behavioral_aliases.len()));
    }
    if !indistinguishable_groups.is_empty() {
        let indist_count: usize = indistinguishable_groups.iter()
            .flat_map(|g| g.iter())
            .collect::<HashSet<_>>()
            .len();
        out.push_str(&format!("  {} indistinguishable under tested conditions\n", indist_count));
    }
    if untested > 0 {
        out.push_str(&format!("  {} untested\n", untested));
    }
    out.push('\n');

    // Alias map
    if let Some(fi) = flag_info {
        let alias_str = format_alias_map(&fi.aliases);
        if !alias_str.is_empty() {
            out.push_str(&format!("Aliases: {}\n\n", alias_str));
        }
    }

    // Solo-distinguished flags with exemplar observations
    out.push_str("## Solo (unique behavior)\n");
    let mut sorted_solo: Vec<&String> = solo_distinguished.iter().collect();
    sorted_solo.sort();
    for flag in &sorted_solo {
        let desc = flag_info.and_then(|fi| fi.descs.get(flag.as_str()))
            .map(|d| format!("  # {}", d))
            .unwrap_or_default();
        out.push_str(&format!("  {}{}\n", flag, desc));

        // Find the exemplar: the context where this flag's output is most distinctive
        if let Some(ex) = find_exemplar(flag, final_metrics) {
            out.push_str(&format!("    exemplar: {} in {} ({})\n", ex.run_label, ex.context_name, ex.delta_summary));
            out.push_str("    base:\n");
            for line in ex.base_preview.lines() {
                out.push_str(&format!("      {}\n", line));
            }
            out.push_str("    flag:\n");
            for line in ex.flag_preview.lines() {
                out.push_str(&format!("      {}\n", line));
            }
        }
    }
    out.push('\n');

    // Combo-characterized flags
    if !combo_distinguished.is_empty() {
        out.push_str("## Via combination (distinguishable when paired)\n");
        let mut sorted_combo: Vec<&String> = combo_distinguished.iter().collect();
        sorted_combo.sort();
        for flag in &sorted_combo {
            let example = combo_evidence.get(*flag)
                .and_then(|v| v.first())
                .map(|e| format!("  e.g. {}", e))
                .unwrap_or_default();
            out.push_str(&format!("  {}{}\n", flag, example));
        }
        out.push('\n');
    }

    // Uncharacterized groups
    if !indistinguishable_groups.is_empty() {
        out.push_str("## Indistinguishable under tested conditions\n");
        // Deduplicate groups that share the same flags
        for group in &indistinguishable_groups {
            let mut sorted = group.clone();
            sorted.sort();
            // Check if this is an alias pair
            if sorted.len() == 2 {
                if let Some(a) = aliases {
                    if a.get(&sorted[0]).map(|v| v == &sorted[1]).unwrap_or(false) {
                        out.push_str(&format!("  ALIAS: {} = {}\n", sorted[0], sorted[1]));
                        continue;
                    }
                }
            }
            out.push_str(&format!("  UNEXPLAINED ({}): {}\n", sorted.len(), sorted.join(", ")));
        }
        out.push('\n');
    }

    // Untested
    if !final_metrics.untested_flags.is_empty() {
        out.push_str(&format!("## Untested ({}/{})\n  {}\n",
            untested_count, unique_stem_count,
            final_metrics.untested_flags.join(", ")));
    }

    out
}

struct Exemplar {
    run_label: String,      // the invocation (e.g., "-R" "." )
    context_name: String,   // which context (e.g., alpha_standard)
    base_preview: String,   // base invocation output (truncated)
    flag_preview: String,   // flag invocation output (truncated)
    delta_summary: String,  // what changed (+N lines, reordered, etc.)
}

/// Find the most distinctive observation for a flag.
/// Picks the context where this flag's output is shared by the fewest other flags —
/// the context that most clearly demonstrates what makes this flag unique.
fn find_exemplar(target_stem: &str, metrics: &AnalysisMetrics) -> Option<Exemplar> {
    // Find all solo runs for this flag (not combinations)
    let target_runs: Vec<&crate::analyze::RunAnalysis> = metrics.runs.iter()
        .filter(|run| {
            let stem = flag_stem(&run.args_str);
            stem.as_ref().map(|s| !is_combination(s) && canonical_flag(s, None) == *target_stem)
                .unwrap_or(false)
        })
        .collect();

    if target_runs.is_empty() { return None; }

    // Collect all solo run outputs per context for comparison
    // For each context name, how many OTHER runs produce the same stdout?
    let mut best_context: Option<(&str, &crate::analyze::RunAnalysis, &crate::execute::Observation)> = None;
    let mut best_uniqueness = usize::MAX; // lower = more unique

    for run in &target_runs {
        for (ctx_names, obs) in &run.context_groups {
            // Skip error-only contexts (exit >= 2 with empty stdout)
            if obs.exit_code.unwrap_or(-1) >= 2 && obs.stdout.trim().is_empty() {
                continue;
            }

            for ctx_name in ctx_names {
                // Count how many other runs produce the same stdout in this context
                let same_output_count = metrics.runs.iter()
                    .filter(|other| !std::ptr::eq(*other, *run))
                    .filter(|other| {
                        other.context_groups.iter()
                            .any(|(names, other_obs)| {
                                names.contains(ctx_name) && other_obs.stdout == obs.stdout
                            })
                    })
                    .count();

                if same_output_count < best_uniqueness {
                    best_uniqueness = same_output_count;
                    best_context = Some((ctx_name.as_str(), run, obs));
                }
            }
        }
    }

    let (ctx_name, run, minority_obs) = best_context?;

    // Find the base run's output in the same context for comparison
    let base_output = if let Some(ref from_ref) = run.from_ref {
        let base_label = crate::output::format_args(from_ref);
        metrics.runs.iter()
            .find(|r| r.args_str == base_label)
            .and_then(|base_run| {
                base_run.context_groups.iter()
                    .flat_map(|(names, obs)| names.iter().map(move |n| (n, obs)))
                    .find(|(n, _)| *n == ctx_name)
                    .map(|(_, obs)| &obs.stdout)
            })
    } else {
        None
    };

    // Build delta summary
    let majority_obs = &run.majority_obs;
    let line_diff = minority_obs.stdout.lines().count() as i64
        - majority_obs.stdout.lines().count() as i64;
    let exit_diff = if minority_obs.exit_code != majority_obs.exit_code {
        format!(", exit {}→{}", majority_obs.exit_code.unwrap_or(-1), minority_obs.exit_code.unwrap_or(-1))
    } else {
        String::new()
    };

    let delta_summary = if line_diff != 0 {
        format!("{:+} lines{}", line_diff, exit_diff)
    } else if minority_obs.stdout != majority_obs.stdout {
        format!("reordered{}", exit_diff)
    } else {
        format!("different{}", exit_diff)
    };

    let truncate = |s: &str, max_lines: usize| -> String {
        let lines: Vec<&str> = s.lines().take(max_lines).collect();
        let result = lines.join("\n");
        if s.lines().count() > max_lines {
            format!("{}\n      ...", result)
        } else {
            result
        }
    };

    let base_preview = base_output
        .map(|s| truncate(s, 5))
        .unwrap_or_else(|| "(no base)".into());

    let flag_preview = truncate(&minority_obs.stdout, 5);

    Some(Exemplar {
        run_label: run.args_str.clone(),
        context_name: ctx_name.to_string(),
        base_preview,
        flag_preview,
        delta_summary,
    })
}

fn format_alias_map(aliases: &HashMap<String, String>) -> String {
    let mut pairs: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (short, long) in aliases {
        if short.len() <= 2 && !seen.contains(short) {
            pairs.push(format!("{} = {}", short, long));
            seen.insert(short.clone());
            seen.insert(long.clone());
        }
    }
    pairs.sort();
    pairs.join(", ")
}
