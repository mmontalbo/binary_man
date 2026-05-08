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
fn flag_stem(label: &str) -> Option<String> {
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
fn canonical_flag(stem: &str, aliases: Option<&HashMap<String, String>>) -> String {
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
fn is_combination(stem: &str) -> bool {
    stem.contains(' ')
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
    let total_flags = flag_info.map(|fi| fi.all_flags.len()).unwrap_or(0);

    // Classify all ever_isolated run labels into flag stems
    let mut solo_characterized: HashSet<String> = HashSet::new();
    let mut combo_characterized: HashSet<String> = HashSet::new();
    let mut combo_evidence: HashMap<String, Vec<String>> = HashMap::new();

    for label in ever_isolated {
        let Some(stem) = flag_stem(label) else { continue };
        if is_combination(&stem) {
            for part in stem.split(' ') {
                let canon = canonical_flag(part, aliases);
                if !solo_characterized.contains(&canon) {
                    combo_characterized.insert(canon.clone());
                    combo_evidence.entry(canon).or_default().push(label.clone());
                }
            }
        } else {
            let canon = canonical_flag(&stem, aliases);
            solo_characterized.insert(canon.clone());
            combo_characterized.remove(&canon);
        }
    }

    // Pairwise distinguishability: flags proven different by cross-group evidence
    // (X+A in group 1, X+B in group 2 → A ≠ B)
    let pairwise = final_metrics.pairwise_distinguished();
    for flag in &pairwise {
        let canon = canonical_flag(flag, aliases);
        if !solo_characterized.contains(&canon) && !combo_characterized.contains(&canon) {
            combo_characterized.insert(canon.clone());
            combo_evidence.entry(canon).or_default().push("(pairwise evidence)".into());
        }
    }

    let all_characterized: HashSet<&String> = solo_characterized.iter()
        .chain(combo_characterized.iter())
        .collect();

    // Identify uncharacterized flags (in identical groups, not characterized by any means)
    let mut uncharacterized_groups: Vec<Vec<String>> = Vec::new();
    for group in &final_metrics.groups {
        if group.isolated() { continue; }
        let stems: Vec<String> = group.run_labels.iter()
            .filter_map(|l| flag_stem(l))
            .filter(|s| !is_combination(s))
            .map(|s| canonical_flag(&s, aliases))
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|s| !all_characterized.contains(s))
            .collect();
        if stems.len() >= 2 {
            uncharacterized_groups.push(stems);
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

    // Flag characterization summary
    let untested = final_metrics.untested_flags.len();
    out.push_str(&format!("## Characterization: {}/{} flags\n", all_characterized.len(), total_flags));
    out.push_str(&format!("  {} solo (unique behavior)\n", solo_characterized.len()));
    if !combo_characterized.is_empty() {
        out.push_str(&format!("  {} via combination only\n", combo_characterized.len()));
    }
    if !uncharacterized_groups.is_empty() {
        let unchar_count: usize = uncharacterized_groups.iter()
            .flat_map(|g| g.iter())
            .collect::<HashSet<_>>()
            .len();
        out.push_str(&format!("  {} uncharacterized (in identical groups)\n", unchar_count));
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

    // Solo-characterized flags
    out.push_str("## Solo (unique behavior)\n");
    let mut sorted_solo: Vec<&String> = solo_characterized.iter().collect();
    sorted_solo.sort();
    for flag in &sorted_solo {
        // Find a representative isolated run for this flag
        let desc = flag_info.and_then(|fi| fi.descs.get(flag.as_str()))
            .map(|d| format!("  # {}", d))
            .unwrap_or_default();
        out.push_str(&format!("  {}{}\n", flag, desc));
    }
    out.push('\n');

    // Combo-characterized flags
    if !combo_characterized.is_empty() {
        out.push_str("## Via combination (distinguishable when paired)\n");
        let mut sorted_combo: Vec<&String> = combo_characterized.iter().collect();
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
    if !uncharacterized_groups.is_empty() {
        out.push_str("## Uncharacterized (equivalent or underexplored)\n");
        // Deduplicate groups that share the same flags
        for group in &uncharacterized_groups {
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
            untested, total_flags,
            final_metrics.untested_flags.join(", ")));
    }

    out
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
