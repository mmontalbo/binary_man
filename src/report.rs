//! Report formatting for analysis results.
//!
//! Single output format: behavioral groups with sensitivity, diffs, and anomaly notes.

use std::collections::{HashMap, HashSet};

use crate::analyze::{AnalysisMetrics, RunAnalysis};
use crate::discover::FlagInfo;
use crate::output;

/// Extract the canonical flag stem from a run label.
/// `"-b" "input.txt"` → `"-b"`
/// `"--width=10" "."` → `"--width"`
/// `"--full-time" "-b" "."` → `"--full-time -b"` (combination)
/// Returns None for runs with no flags (bare positional args).
pub fn flag_stem(label: &str) -> Option<String> {
    let args = output::parse_label(label);
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
            for arg in output::parse_label(args_str) {
                if arg.starts_with('-') {
                    let key = if let Some(eq) = arg.find('=') { &arg[..eq] } else { arg };
                    if let Some(desc) = fi.descs.get(key) {
                        return format!("  # {}", first_sentence(desc, 100));
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

/// Format the exploration report.
///
/// `ever_isolated` is the set of run labels in singleton behavioral groups.
/// The report deduplicates to unique flag stems and separates solo-flag isolation
/// from combination-based evidence.
pub fn format_exploration_report(
    rounds: &[RoundSummary],
    final_metrics: &AnalysisMetrics,
    flag_info: Option<&FlagInfo>,
    ever_isolated: &HashSet<String>,
    binary_label: &str,
    all_runs: &[&RunAnalysis],
    contexts: &[crate::parse::NamedContext],
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

    // Test summary
    out.push_str("## Test scope\n");
    let last = rounds.last();
    let total_isolated = last.map(|r| r.isolated).unwrap_or(0);
    let total_identical = last.map(|r| r.identical).unwrap_or(0);
    out.push_str(&format!("  {} behavioral groups: {} unique, {} shared\n",
        total_isolated + total_identical, total_isolated, total_identical));

    // Collect context names referenced in exemplars, then describe them
    // by listing their actual setup commands (files, properties, env, stdin).
    let mut referenced_contexts: Vec<String> = Vec::new();
    for flag in solo_distinguished.iter().chain(combo_distinguished.iter()) {
        if let Some(ex) = find_exemplar(flag, all_runs, aliases) {
            if !referenced_contexts.contains(&ex.context_name) {
                referenced_contexts.push(ex.context_name);
            }
        }
    }
    referenced_contexts.sort();
    if !referenced_contexts.is_empty() {
        let ctx_map: HashMap<&str, &crate::parse::NamedContext> = contexts.iter()
            .map(|c| (c.name.as_str(), c))
            .collect();
        out.push_str("\n  Contexts referenced below:\n");
        for name in &referenced_contexts {
            out.push_str(&format!("    {}:\n", name));
            if let Some(ctx) = ctx_map.get(name.as_str()) {
                for cmd in &ctx.commands {
                    out.push_str(&format!("      {}\n", output::format_setup_cmd(cmd)));
                }
                if let Some(ref stdin) = ctx.stdin {
                    match stdin {
                        crate::parse::StdinSource::Lines(lines) =>
                            out.push_str(&format!("      stdin: {}\n", lines.join("\\n"))),
                        crate::parse::StdinSource::FromFile(path) =>
                            out.push_str(&format!("      stdin from: {}\n", path)),
                    }
                }
                if ctx.commands.is_empty() && ctx.stdin.is_none() {
                    out.push_str("      (empty directory)\n");
                }
            }
        }
    }
    out.push('\n');

    // Compute operational exit codes from base runs (no flags).
    // If the binary itself exits with code 1 in normal operation (e.g. diff
    // when files differ, grep when no match), then exit 1 is "operational"
    // for this binary, not an error.
    let operational_exit_codes: HashSet<i32> = {
        let mut codes = HashSet::new();
        codes.insert(0); // exit 0 is always operational
        for run in &final_metrics.runs {
            // Base runs: no flag args (all args are positional/extract)
            let has_flag = run.args.iter().any(|a| a.is_flag());
            if !has_flag {
                for (_, obs) in &run.context_groups {
                    if let Some(code) = obs.exit_code {
                        codes.insert(code);
                    }
                }
            }
        }
        codes
    };

    // Classify evidence quality: did we actually see the flag work?
    // A flag has "observed behavior" if any run containing it exited with an
    // operational exit code (matching the base run's behavior) with either:
    // - non-empty stdout (the flag produced visible output), OR
    // - filesystem changes (the flag modified files — silent tools like cp, mv, rm)
    // Checks both exact stem matches and component matches (for prerequisite
    // runs where e.g. "-f -d" contains the target flag "-d").
    let has_observed_behavior = |stem: &str| -> bool {
        for run in all_runs {
            let run_stem = flag_stem(&run.args_str);
            let matches = run_stem.as_ref().is_some_and(|rs| {
                let canon = canonical_flag(rs, aliases);
                canon == stem || rs == stem
                    || rs.split(' ').any(|part| part == stem || canonical_flag(part, aliases) == stem)
            });
            if matches {
                for (_, obs) in &run.context_groups {
                    if obs.exit_code.is_some_and(|c| operational_exit_codes.contains(&c))
                        && (!obs.stdout.trim().is_empty() || !obs.fs_changes.is_empty())
                    {
                        return true;
                    }
                }
            }
        }
        false
    };

    // Only count flags from the --help surface (not expression operators
    // like find's -print or prerequisite companions leaking through).
    let solo_observed: Vec<&String> = solo_distinguished.iter()
        .filter(|s| all_stems.contains(*s) && has_observed_behavior(s)).collect();
    let solo_error_only: Vec<&String> = solo_distinguished.iter()
        .filter(|s| all_stems.contains(*s) && !has_observed_behavior(s)).collect();
    let combo_observed: Vec<&String> = combo_distinguished.iter()
        .filter(|s| all_stems.contains(*s) && has_observed_behavior(s)).collect();
    let combo_error_only: Vec<&String> = combo_distinguished.iter()
        .filter(|s| all_stems.contains(*s) && !has_observed_behavior(s)).collect();

    let total_observed = solo_observed.len() + combo_observed.len();
    let total_error_only = solo_error_only.len() + combo_error_only.len();

    // Flag distinguishability summary
    let untested = final_metrics.untested_flags.len();
    out.push_str(&format!("## Observed: {}/{} flags\n", total_observed, unique_stem_count));
    out.push_str(&format!("  {} uniquely observable, {} distinguishable via flag pairs\n",
        solo_observed.len(), combo_observed.len()));
    if total_error_only > 0 {
        out.push_str(&format!("  {} error-only (flag recognized but no successful output observed)\n", total_error_only));
    }
    if !behavioral_aliases.is_empty() {
        out.push_str(&format!("  {} behavioral aliases (different flags, identical behavior)\n", behavioral_aliases.len()));
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

    // Robustness summary: count flags by confidence tier
    if !final_metrics.robustness.is_empty() {
        let mut robust = 0usize; // survives all contexts
        let mut moderate = 0usize; // survives >50%
        let mut fragile = 0usize; // survives ≤50%
        for (survived, total) in final_metrics.robustness.values() {
            if *total == 0 { continue; }
            if *survived == *total { robust += 1; }
            else if *survived * 2 > *total { moderate += 1; }
            else { fragile += 1; }
        }
        out.push_str(&format!("  robustness: {} verified in all contexts, {} in most, {} context-dependent\n",
            robust, moderate, fragile));
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
    out.push_str("## Unique behavior (observable when tested alone)\n");
    let mut sorted_solo: Vec<&String> = solo_distinguished.iter().collect();
    sorted_solo.sort();
    for flag in &sorted_solo {
        let desc = flag_info.and_then(|fi| fi.descs.get(flag.as_str()))
            .map(|d| format!("  # {}", first_sentence(d, 140)))
            .unwrap_or_default();
        out.push_str(&format!("  {}{}\n", flag, desc));

        // Find the exemplar: the context where this flag's output is most distinctive
        if let Some(ex) = find_exemplar(flag, all_runs, aliases) {
            out.push_str(&format!("    tested: {} in {}\n", ex.run_label, ex.context_name));
            // Show diff components on separate lines for readability
            for part in ex.vs_diff.split("; ") {
                out.push_str(&format!("    | {}\n", part));
            }
            // Show base/flag preview only when there's stdout to show.
            // For side-effect tools (cp, rm) the diff lines above carry the signal.
            let base_has_content = ex.base_preview.lines().any(|l| !l.contains("identical") && !l.trim().is_empty());
            let flag_has_content = ex.flag_preview.lines().any(|l| !l.contains("identical") && !l.trim().is_empty());
            if base_has_content || flag_has_content {
                out.push_str("    without flag:\n");
                for line in ex.base_preview.lines() {
                    out.push_str(&format!("      {}\n", line));
                }
                out.push_str("    with flag:\n");
                for line in ex.flag_preview.lines() {
                    out.push_str(&format!("      {}\n", line));
                }
            }
        }
    }
    out.push('\n');

    // Combo-characterized flags — show vs_diff when available
    if !combo_distinguished.is_empty() {
        out.push_str("## Distinguishable in combination (verified via flag pairs)\n");
        let mut sorted_combo: Vec<&String> = combo_distinguished.iter().collect();
        sorted_combo.sort();
        for flag in &sorted_combo {
            let desc = flag_info.and_then(|fi| fi.descs.get(flag.as_str()))
                .map(|d| format!("  # {}", first_sentence(d, 140)))
                .unwrap_or_default();
            out.push_str(&format!("  {}{}\n", flag, desc));
            // Show the behavioral diff summary from the flag's runs
            if let Some(diff) = find_vs_diff_for_flag(flag, all_runs, aliases) {
                for part in diff.split("; ") {
                    out.push_str(&format!("    | {}\n", part));
                }
            }
            if let Some(evidence) = combo_evidence.get(*flag).and_then(|v| v.first()) {
                out.push_str(&format!("    proven via: {}\n", evidence));
            }
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
    run_label: String,
    context_name: String,
    vs_diff: String,          // computed diff summary (e.g., "3 only in this: .hidden, ., ..")
    base_preview: String,     // first differing region from base
    flag_preview: String,     // first differing region from flag
}

/// Find an observation in a specific context from a run's context groups.
fn obs_in_context<'a>(run: &'a RunAnalysis, ctx: &str) -> Option<&'a crate::execute::Observation> {
    run.context_groups.iter()
        .flat_map(|(names, obs)| names.iter().map(move |n| (n, obs)))
        .find(|(n, _)| *n == ctx)
        .map(|(_, obs)| obs)
}

/// Find the most distinctive observation for a flag.
/// Picks the context where this flag's output is shared by the fewest other flags —
/// the context that most clearly demonstrates what makes this flag unique.
/// Shows the first differing region (not the first N identical lines).
fn find_exemplar(
    target_stem: &str,
    all_runs: &[&RunAnalysis],
    aliases: Option<&HashMap<String, String>>,
) -> Option<Exemplar> {
    // Find all solo runs for this flag (not combinations).
    // Match via alias resolution so "-q" finds "--quiet" runs.
    let target_runs: Vec<&&RunAnalysis> = all_runs.iter()
        .filter(|run| {
            let stem = flag_stem(&run.args_str);
            stem.as_ref().map(|s| !is_combination(s) && canonical_flag(s, aliases) == *target_stem)
                .unwrap_or(false)
        })
        .collect();

    if target_runs.is_empty() { return None; }

    // Pick the context where the flag's effect is most visible.
    // Prefer: contexts where the diff from base is non-identical, then by uniqueness.
    let mut best_context: Option<(&str, &&RunAnalysis, &crate::execute::Observation)> = None;
    let mut best_score: (bool, usize) = (false, usize::MAX); // (has_diff, uniqueness)

    for run in &target_runs {
        // Find the base observation for diff comparison
        let base_map: HashMap<&str, &crate::execute::Observation> = run.from_ref.as_ref()
            .and_then(|from_ref| {
                let base_label = crate::output::format_args(from_ref);
                all_runs.iter().find(|r| r.args_str == base_label)
            })
            .map(|base_run| {
                base_run.context_groups.iter()
                    .flat_map(|(names, obs)| names.iter().map(move |n| (n.as_str(), obs)))
                    .collect()
            })
            .unwrap_or_default();

        for (ctx_names, obs) in &run.context_groups {
            let is_error_only = obs.exit_code.unwrap_or(-1) >= 2 && obs.stdout.trim().is_empty();
            for ctx_name in ctx_names {
                // Check if output actually differs from base in this context
                let has_diff = base_map.get(ctx_name.as_str())
                    .map(|base_obs| {
                        base_obs.stdout != obs.stdout
                            || base_obs.exit_code != obs.exit_code
                            || base_obs.stderr != obs.stderr
                    })
                    .unwrap_or(true); // no base = standalone, always interesting

                let same_output_count = all_runs.iter()
                    .filter(|other| !std::ptr::eq(**other, **run))
                    .filter(|other| {
                        other.context_groups.iter()
                            .any(|(names, other_obs)| {
                                names.contains(ctx_name) && other_obs.stdout == obs.stdout
                            })
                    })
                    .count();

                // Score: prefer non-error contexts with diffs, then unique outputs.
                // Error-only contexts are last resort (penalized by high uniqueness).
                let adj_uniqueness = if is_error_only { same_output_count + 10000 } else { same_output_count };
                let score = (has_diff, adj_uniqueness);
                if score.0 && !best_score.0 || (score.0 == best_score.0 && score.1 < best_score.1) {
                    best_score = score;
                    best_context = Some((ctx_name.as_str(), run, obs));
                }
            }
        }
    }

    let (ctx_name, run, flag_obs) = best_context?;

    // Find the base run's observation in the same context.
    // If the run has an explicit diff_from, use that. Otherwise, find the
    // base run with the same positional args but no flags — the bare invocation
    // that shows what the tool does without this flag.
    let base_obs = if let Some(ref from_ref) = run.from_ref {
        let base_label = crate::output::format_args(from_ref);
        all_runs.iter()
            .find(|r| r.args_str == base_label)
            .and_then(|r| obs_in_context(r, ctx_name))
    } else {
        // Find a base run (no flags) in the same context for comparison.
        // Pick the one with the most args in common with this run.
        let run_args: HashSet<String> = run.args.iter()
            .filter(|a| !a.is_flag())
            .map(|a| a.display())
            .collect();
        all_runs.iter()
            .filter(|r| !r.args.iter().any(|a| a.is_flag()))
            .filter(|r| obs_in_context(r, ctx_name).is_some())
            .max_by_key(|r| {
                r.args.iter()
                    .map(|a| a.display())
                    .filter(|v| run_args.contains(v))
                    .count()
            })
            .and_then(|r| obs_in_context(r, ctx_name))
    };

    // Compute vs_diff using the existing diff function.
    // For standalone runs (no base), describe the observation directly.
    let vs_diff = if let Some(base) = base_obs {
        let diff = crate::execute::compute_diff(base, flag_obs);
        if diff.is_empty() { "identical".into() } else { diff.join("; ") }
    } else {
        describe_observation(flag_obs)
    };

    // Build preview showing the first region where base and flag DIFFER.
    // Skip shared prefix lines so the reader sees the actual change.
    // Strip ANSI escapes so output is readable.
    let (base_preview, flag_preview) = if let Some(base) = base_obs {
        diff_preview(&output::strip_ansi(&base.stdout), &output::strip_ansi(&flag_obs.stdout), 6)
    } else {
        (String::new(), truncate_lines(&output::strip_ansi(&flag_obs.stdout), 6))
    };

    Some(Exemplar {
        run_label: run.args_str.clone(),
        context_name: ctx_name.to_string(),
        vs_diff,
        base_preview,
        flag_preview,
    })
}

/// Describe a standalone observation (no base to diff against).
/// Summarizes what the flag produced: stdout, exit code, stderr, fs changes.
fn describe_observation(obs: &crate::execute::Observation) -> String {
    let mut parts = Vec::new();
    let stdout_lines = obs.stdout.lines().count();
    if stdout_lines > 0 {
        parts.push(format!("stdout: {} lines", stdout_lines));
    }
    parts.push(format!("exit {}", output::format_exit(obs.exit_code.unwrap_or(-1))));
    if !obs.stderr.trim().is_empty() {
        let first = obs.stderr.lines().next().unwrap_or("").trim();
        parts.push(format!("stderr: {}", first));
    }
    for c in &obs.fs_changes {
        match c {
            crate::execute::FsChange::Created { path, size } =>
                parts.push(format!("created {} ({} bytes)", path, size)),
            crate::execute::FsChange::Deleted { path } =>
                parts.push(format!("deleted {}", path)),
            crate::execute::FsChange::Modified { path, detail } =>
                parts.push(format!("modified {} ({})", path, detail)),
        }
    }
    parts.join("; ")
}

/// Build a preview showing the first region where two outputs differ.
/// Skips shared prefix lines. Shows up to `max_lines` from the divergence point.
fn diff_preview(base: &str, flag: &str, max_lines: usize) -> (String, String) {
    let base_lines: Vec<&str> = base.lines().collect();
    let flag_lines: Vec<&str> = flag.lines().collect();

    // Find the first line that differs
    let shared_prefix = base_lines.iter().zip(flag_lines.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let context_before = 1; // show 1 line of context before the diff
    let start = shared_prefix.saturating_sub(context_before);

    let format_region = |lines: &[&str], start: usize, max: usize| -> String {
        let end = (start + max).min(lines.len());
        let region: Vec<&str> = lines[start..end].to_vec();
        let mut result = region.join("\n");
        if end < lines.len() {
            result.push_str(&format!("\n      ... ({} more)", lines.len() - end));
        }
        if start > 0 {
            result = format!("      ... ({} identical)\n{}", start, result);
        }
        result
    };

    (format_region(&base_lines, start, max_lines), format_region(&flag_lines, start, max_lines))
}

/// Truncate to first N lines with "... (N more)" suffix.
fn truncate_lines(s: &str, max: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max {
        lines.join("\n")
    } else {
        format!("{}\n      ... ({} more)", lines[..max].join("\n"), lines.len() - max)
    }
}

/// Find the vs_diff for a flag stem from all runs (used for combo flag summaries).
fn find_vs_diff_for_flag(stem: &str, all_runs: &[&RunAnalysis], aliases: Option<&HashMap<String, String>>) -> Option<String> {
    for run in all_runs {
        let run_stem = flag_stem(&run.args_str);
        let matches = run_stem.as_ref().is_some_and(|rs| {
            let canon = canonical_flag(rs, aliases);
            canon == stem || rs == stem
                || rs.split(' ').any(|part| part == stem || canonical_flag(part, aliases) == stem)
        });
        if matches {
            if let Some(ref diff) = run.vs_diff {
                if diff != "identical" {
                    return Some(diff.clone());
                }
            }
        }
    }
    None
}

/// Extract the first sentence from a --help description.
/// Keeps parentheticals and semicolon clauses that are part of the first
/// sentence. Only truncates at sentence boundaries (". " + uppercase),
/// which is where help text shifts to a new thought or cross-reference.
fn first_sentence(desc: &str, max_len: usize) -> &str {
    let mut end = desc.len();

    // Sentence boundary: ". " followed by uppercase (new sentence)
    // Catches "equivalent to --update[=older].  See below" → keeps first sentence
    for (i, _) in desc.match_indices(". ") {
        let rest = desc[i + 2..].trim_start();
        if rest.starts_with(|c: char| c.is_uppercase()) {
            end = end.min(i);
            break;
        }
    }

    // Cap at max_len with word-boundary truncation
    if end > max_len {
        end = desc[..max_len].rfind(' ').unwrap_or(max_len);
    }

    desc[..end].trim_end()
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
