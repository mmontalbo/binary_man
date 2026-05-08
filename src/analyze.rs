//! Analysis pipeline: Script + GridResult → AnalysisMetrics.
//!
//! Collapses observations across contexts, computes sensitivity and universals,
//! groups behaviorally equivalent runs, and identifies untested flags.

use std::collections::{HashMap, HashSet};

use crate::discover::FlagInfo;
use crate::execute::{self, GridResult, Observation};
use crate::output;
use crate::parse::Script;

/// Per-run analysis (used by per-run output modes).
pub struct RunAnalysis {
    pub run_index: usize,
    pub args: Vec<String>,
    pub args_str: String,
    /// Representative observation from the majority context group.
    pub majority_obs: Observation,
    pub majority_contexts: Vec<String>,
    /// All distinct context groups: (context_names, observation).
    pub context_groups: Vec<(Vec<String>, Observation)>,
    pub sensitivity: Vec<String>,
    pub universals: Vec<String>,
    pub from_ref: Option<Vec<String>>,
    pub vs_diff: Option<String>,
    pub has_anomaly: bool,
    pub obs_count: usize,
}

/// A group of runs with identical per-context observations.
pub struct BehaviorGroup {
    pub run_indices: Vec<usize>,
    pub run_labels: Vec<String>,
    pub majority_obs: Observation,
    pub majority_contexts: Vec<String>,
    pub sensitivity: Vec<String>,
    pub universals: Vec<String>,
    pub from_ref: Option<Vec<String>>,
    pub vs_diffs: Vec<(String, String)>,
    /// Per-context observations for the first run in this group.
    /// Used for grouping comparisons during refinement.
    obs_list: Vec<(String, ObsKey)>,
}

/// Lightweight observation key for grouping comparisons (avoids cloning full observations).
#[derive(PartialEq, Eq)]
struct ObsKey {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    fs_changes: Vec<execute::FsChange>,
}

impl ObsKey {
    fn from_obs(obs: &Observation) -> Self {
        ObsKey {
            stdout: obs.stdout.clone(),
            stderr: obs.stderr.clone(),
            exit_code: obs.exit_code,
            fs_changes: obs.fs_changes.clone(),
        }
    }
}

impl BehaviorGroup {
    pub fn isolated(&self) -> bool { self.run_indices.len() == 1 }
}

/// Full analysis result.
pub struct AnalysisMetrics {
    pub groups: Vec<BehaviorGroup>,
    pub runs: Vec<RunAnalysis>,
    pub untested_flags: Vec<String>,
    pub context_count: usize,
    pub total_runs: usize,
}

impl AnalysisMetrics {
    pub fn isolated_count(&self) -> usize {
        self.groups.iter().filter(|g| g.isolated()).count()
    }

    pub fn identical_count(&self) -> usize {
        self.groups.iter().filter(|g| !g.isolated()).count()
    }

    /// Identify run labels that produced no useful signal:
    /// runs in a large identical group (≥5 runs) with the same positional args,
    /// meaning the target arg isn't exercising the tool's behavior.
    ///
    /// Note: error-exit runs are NOT pruned — error behavior is still behavior.
    /// Two flags that both exit 2 may produce different errors and belong in
    /// different groups.
    pub fn unproductive_runs(&self) -> HashSet<String> {
        let mut unproductive = HashSet::new();

        // Runs in large identical groups where all runs share the same
        // non-flag args (same target, same pattern) — the target isn't helping
        for group in &self.groups {
            if group.run_labels.len() < 5 { continue; }
            // Extract positional args from each run in the group
            let positionals: Vec<Vec<String>> = group.run_labels.iter()
                .map(|label| {
                    label.split('"')
                        .enumerate()
                        .filter(|(i, _)| i % 2 == 1)
                        .map(|(_, s)| s.to_string())
                        .filter(|s| !s.starts_with('-'))
                        .collect()
                })
                .collect();
            // If all runs have the same positionals, this target isn't differentiating
            if !positionals.is_empty() && positionals.iter().all(|p| *p == positionals[0]) {
                for label in &group.run_labels {
                    unproductive.insert(label.clone());
                }
            }
        }

        unproductive
    }

    /// Find flag pairs proven different by cross-group interaction data.
    ///
    /// For combination runs like `X A target` and `X B target` that are in different
    /// behavioral groups, flags A and B are proven distinguishable (they modify X's
    /// behavior differently). Returns the set of flag stems proven different.
    pub fn pairwise_distinguished(&self) -> HashSet<String> {
        // Map: (base_flags, positionals) → Vec<(modifier_flag, group_index)>
        struct ComboKey { base: Vec<String>, positionals: Vec<String> }
        impl std::hash::Hash for ComboKey {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.base.hash(state);
                self.positionals.hash(state);
            }
        }
        impl PartialEq for ComboKey { fn eq(&self, other: &Self) -> bool { self.base == other.base && self.positionals == other.positionals } }
        impl Eq for ComboKey {}

        let mut combo_map: HashMap<ComboKey, Vec<(String, usize)>> = HashMap::new();

        for (gi, group) in self.groups.iter().enumerate() {
            for label in &group.run_labels {
                let args: Vec<String> = label.split('"')
                    .enumerate()
                    .filter(|(i, _)| i % 2 == 1)
                    .map(|(_, s)| s.to_string())
                    .collect();

                let flags: Vec<&String> = args.iter().filter(|a| a.starts_with('-')).collect();
                let positionals: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();

                // For runs with 2+ flags, try each flag as the "modifier"
                if flags.len() >= 2 {
                    for (fi, modifier) in flags.iter().enumerate() {
                        let base: Vec<String> = flags.iter().enumerate()
                            .filter(|(i, _)| *i != fi)
                            .map(|(_, f)| f.to_string())
                            .collect();
                        // Strip =value from modifier for canonical comparison
                        let mod_stem = if let Some(eq) = modifier.find('=') {
                            modifier[..eq].to_string()
                        } else {
                            modifier.to_string()
                        };
                        let key = ComboKey { base, positionals: positionals.clone() };
                        combo_map.entry(key).or_default().push((mod_stem, gi));
                    }
                }
            }
        }

        // For each (base, positionals) group, if two modifiers are in different
        // behavioral groups, they're proven different
        let mut distinguished = HashSet::new();
        for entries in combo_map.values() {
            if entries.len() < 2 { continue; }
            for i in 0..entries.len() {
                for j in (i + 1)..entries.len() {
                    let (flag_a, group_a) = &entries[i];
                    let (flag_b, group_b) = &entries[j];
                    if group_a != group_b && flag_a != flag_b {
                        distinguished.insert(flag_a.clone());
                        distinguished.insert(flag_b.clone());
                    }
                }
            }
        }

        distinguished
    }
}

/// Core analysis: Script + GridResult → AnalysisMetrics.
///
/// `prior_tested` is the set of flag stems already tested in previous rounds.
/// Combined with this round's flags to compute the untested set cumulatively.
pub fn analyze(
    script: &Script,
    grid: &GridResult,
    flag_info: Option<&FlagInfo>,
    prior_tested: Option<&HashSet<String>>,
) -> AnalysisMetrics {
    // Build obs_by_args for vs-diff lookups
    let obs_by_args: HashMap<(&[String], &str), &Observation> = grid.cells.iter()
        .map(|((ctx, ri), obs)| {
            let args = &script.runs[*ri].args;
            ((args.as_slice(), ctx.as_str()), obs)
        })
        .collect();

    // --- Per-run analysis ---
    let mut run_analyses: Vec<RunAnalysis> = Vec::new();

    // Also collect per-run obs_lists for grouping (lightweight keys)
    struct RunObsEntry {
        run_index: usize,
        keys: Vec<(String, ObsKey)>,
    }
    let mut run_obs_keys: Vec<RunObsEntry> = Vec::new();

    for (ri, run) in script.runs.iter().enumerate() {
        let args_str = output::format_args(&run.args);

        // Collect observations across contexts
        let mut obs_list: Vec<(&str, &Observation)> = Vec::new();
        for ctx in &script.contexts {
            if let Some(obs) = grid.cells.get(&(ctx.name.clone(), ri)) {
                obs_list.push((&ctx.name, obs));
            }
        }

        if obs_list.is_empty() {
            continue;
        }

        // Save obs keys for grouping
        let obs_keys: Vec<(String, ObsKey)> = obs_list.iter()
            .map(|(name, obs)| (name.to_string(), ObsKey::from_obs(obs)))
            .collect();
        run_obs_keys.push(RunObsEntry {
            run_index: ri,
            keys: obs_keys,
        });

        // Collapse identical observations across contexts
        let groups = execute::collapse(&obs_list);
        let largest_idx = groups.iter().enumerate()
            .max_by_key(|(_, (names, _))| names.len())
            .map(|(i, _)| i).unwrap_or(0);
        let (majority_names, majority_obs) = &groups[largest_idx];

        // Compute quantified sensitivity
        let majority_lines: usize = majority_obs.stdout.lines().count();
        let mut sensitive_parts: Vec<String> = Vec::new();
        for (i, (names, obs)) in groups.iter().enumerate() {
            if i == largest_idx { continue; }
            for name in names {
                if !name.contains(" / ") { continue; }
                let label = name.split(" / ").last().unwrap_or(name);
                let obs_lines = obs.stdout.lines().count();
                let mut effects = Vec::new();
                let line_diff = obs_lines as i64 - majority_lines as i64;
                if line_diff != 0 {
                    effects.push(format!("{:+} lines", line_diff));
                } else if obs.stdout != majority_obs.stdout {
                    effects.push("reordered".into());
                }
                if obs.exit_code != majority_obs.exit_code {
                    effects.push(format!("exit {}→{}",
                        majority_obs.exit_code.unwrap_or(-1),
                        obs.exit_code.unwrap_or(-1)));
                }
                if effects.is_empty() {
                    sensitive_parts.push(label.to_string());
                } else {
                    sensitive_parts.push(format!("{} ({})", label, effects.join(", ")));
                }
            }
        }

        // Compute universals
        let exit_codes: Vec<i32> = obs_list.iter()
            .map(|(_, o)| o.exit_code.unwrap_or(-1))
            .collect::<HashSet<_>>().into_iter().collect();
        let all_stdout_nonempty = obs_list.iter().all(|(_, o)| !o.stdout.trim().is_empty());
        let all_stdout_empty = obs_list.iter().all(|(_, o)| o.stdout.trim().is_empty());
        let all_has_fs = obs_list.iter().all(|(_, o)| !o.fs_changes.is_empty());
        let has_signal = exit_codes.iter().any(|c| *c > 128);
        let mut universals = Vec::new();
        if exit_codes.len() == 1 {
            universals.push(format!("exit {}", output::format_exit(exit_codes[0])));
        } else {
            let mut sorted = exit_codes.clone();
            sorted.sort();
            universals.push(format!("exit {{{}}}", sorted.iter().map(|c| output::format_exit(*c)).collect::<Vec<_>>().join(",")));
        }
        if has_signal {
            universals.push("SIGNAL".into());
        }
        if all_stdout_nonempty { universals.push("stdout not empty".into()); }
        if all_stdout_empty { universals.push("stdout empty".into()); }
        if all_has_fs { universals.push("modifies filesystem".into()); }

        // Sort sensitivity: effects first
        if !sensitive_parts.is_empty() {
            sensitive_parts.sort_by(|a, b| {
                let a_has = a.contains('(');
                let b_has = b.contains('(');
                b_has.cmp(&a_has)
            });
        }

        // vs-diff
        let vs_diff = run.diff_from.as_ref().and_then(|ref_args| {
            let majority_ctx = majority_names[0];
            let ref_obs = obs_by_args.get(&(ref_args.as_slice(), majority_ctx))?;
            let diff = execute::compute_diff(ref_obs, majority_obs);
            Some(if diff.is_empty() { "identical".into() } else { diff.join("; ") })
        });

        // Anomaly check
        let majority_exit = majority_obs.exit_code.unwrap_or(-1);
        let has_anomaly = output::has_anomalies(majority_obs, None)
            || obs_list.iter().any(|(_, obs)| output::has_anomalies(obs, Some(majority_exit)));

        // Build owned context groups
        let context_groups: Vec<(Vec<String>, Observation)> = groups.iter()
            .map(|(names, obs)| {
                (names.iter().map(|s| s.to_string()).collect(), (*obs).clone())
            })
            .collect();

        // Stderr feedback
        let exit = obs_list[0].1.exit_code.unwrap_or(-1);
        let sens_label = if sensitive_parts.is_empty() { String::new() } else {
            format!(" [{}]", sensitive_parts.join(", "))
        };
        eprintln!("  run {}: {}/{} distinct, exit {}{}", args_str, groups.len(), obs_list.len(), output::format_exit(exit), sens_label);

        run_analyses.push(RunAnalysis {
            run_index: ri,
            args: run.args.clone(),
            args_str,
            majority_obs: (*majority_obs).clone(),
            majority_contexts: majority_names.iter().map(|s| s.to_string()).collect(),
            context_groups,
            sensitivity: sensitive_parts,
            universals,
            from_ref: run.diff_from.clone(),
            vs_diff,
            has_anomaly,
            obs_count: obs_list.len(),
        });
    }

    // --- Group runs into BehaviorGroups ---
    let mut behavior_groups: Vec<BehaviorGroup> = Vec::new();

    for analysis in &run_analyses {
        let ri = analysis.run_index;

        // Find the obs_keys for this run
        let obs_entry = run_obs_keys.iter()
            .find(|e| e.run_index == ri);

        let Some(entry) = obs_entry else { continue };
        let keys = &entry.keys;

        // Try to find an existing group with identical per-context observations
        let found = behavior_groups.iter_mut().find(|g| {
            if g.from_ref.as_ref() != analysis.from_ref.as_ref() { return false; }
            if g.obs_list.len() != keys.len() { return false; }
            g.obs_list.iter().zip(keys.iter()).all(|((_, a), (_, b))| {
                a.stdout == b.stdout
                && a.stderr == b.stderr
                && a.exit_code == b.exit_code
                && a.fs_changes == b.fs_changes
            })
        });

        if let Some(group) = found {
            group.run_indices.push(ri);
            group.run_labels.push(analysis.args_str.clone());
            if let Some(ref diff) = analysis.vs_diff {
                group.vs_diffs.push((analysis.args_str.clone(), diff.clone()));
            }
            for sp in &analysis.sensitivity {
                if !group.sensitivity.contains(sp) {
                    group.sensitivity.push(sp.clone());
                }
            }
        } else {
            let mut vs_diffs = Vec::new();
            if let Some(ref diff) = analysis.vs_diff {
                vs_diffs.push((analysis.args_str.clone(), diff.clone()));
            }
            behavior_groups.push(BehaviorGroup {
                run_indices: vec![ri],
                run_labels: vec![analysis.args_str.clone()],
                majority_obs: analysis.majority_obs.clone(),
                majority_contexts: analysis.majority_contexts.clone(),
                sensitivity: analysis.sensitivity.clone(),
                universals: analysis.universals.clone(),
                from_ref: analysis.from_ref.clone(),
                vs_diffs,
                obs_list: keys.iter().map(|(name, key)| {
                    (name.clone(), ObsKey {
                        stdout: key.stdout.clone(),
                        stderr: key.stderr.clone(),
                        exit_code: key.exit_code,
                        fs_changes: key.fs_changes.clone(),
                    })
                }).collect(),
            });
        }
    }

    // --- Untested flags ---
    let mut untested_flags = Vec::new();
    if let Some(fi) = flag_info {
        let mut tested: HashSet<String> = prior_tested.cloned().unwrap_or_default();
        for run in &script.runs {
            for arg in &run.args {
                if arg.starts_with('-') {
                    let key = if let Some(eq) = arg.find('=') { &arg[..eq] } else { arg.as_str() };
                    tested.insert(key.to_string());
                    if let Some(alias) = fi.aliases.get(key) {
                        tested.insert(alias.clone());
                    }
                }
            }
        }
        let mut unt: Vec<&String> = fi.all_flags.iter()
            .filter(|f| !tested.contains(f.as_str()))
            .collect();
        unt.sort();
        untested_flags = unt.into_iter().cloned().collect();
    }

    let total_runs = run_analyses.len();
    AnalysisMetrics {
        groups: behavior_groups,
        runs: run_analyses,
        untested_flags,
        context_count: grid.context_count,
        total_runs,
    }
}
