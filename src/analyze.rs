//! Analysis pipeline: Script + GridResult → AnalysisMetrics.
//!
//! Compares observations using structural tree diff: stdout/stderr are tokenized
//! into content-hashed label trees and aligned via two-level Needleman-Wunsch
//! (line-level, then token-level within matched lines). This correctly matches
//! modified lines through shared tokens (e.g., "a.txt" in `ls` output anchors
//! the match with `ls -l` output where 8 tokens are prepended).
//!
//! The delta between a reference (baseline) and observation (flagged run) is an
//! edit script of structural operations (Insert/Delete/Keep/Replace), not raw
//! content. Two flags applying the same transformation produce the same edit
//! script regardless of per-cell nondeterminism.
//!
//! Groups behaviorally equivalent runs and identifies untested flags.

use std::collections::{HashMap, HashSet};

use crate::discover::FlagInfo;
use crate::execute::{self, GridResult, Observation};
use crate::output;
use crate::parse::{Arg, Script};

/// Per-run analysis (used by per-run output modes).
pub struct RunAnalysis {
    pub run_index: usize,
    pub args: Vec<Arg>,
    pub args_str: String,
    /// Representative observation from the majority context group.
    pub majority_obs: Observation,
    pub majority_contexts: Vec<String>,
    /// All distinct context groups: (context_names, observation).
    pub context_groups: Vec<(Vec<String>, Observation)>,
    pub sensitivity: Vec<String>,
    pub universals: Vec<String>,
    pub from_ref: Option<Vec<Arg>>,
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
    pub from_ref: Option<Vec<Arg>>,
    pub vs_diffs: Vec<(String, String)>,
    /// Per-context observations for the first run in this group.
    /// Used for grouping comparisons during refinement.
    obs_list: Vec<(String, ObsKey)>,
}

// --- Structural diff types ---
//
// Stdout/stderr comparison uses a two-level Needleman-Wunsch alignment:
//   1. Tokenize both ref and obs into lines of content-hashed labels
//   2. Align lines (match cost = token edit distance, gap cost = token count)
//   3. Within matched lines, align tokens (unit cost per insert/delete/replace)
//   4. Produce a structural edit script: sequence of LineEdits, each containing TokenEdits
//
// Token values are raw strings — same string matches across ref and obs naturally.
// No hashing, no label pools, no canonicalization. Shared tokens (filenames, keywords)
// are natural alignment anchors. Value-level precision: "root" ≠ "0" → ls -l vs ls -n.

/// Token-level edit operation. Values are the raw token strings.
#[derive(PartialEq, Eq, Clone, Debug)]
enum TokenEdit {
    Keep(String),
    Insert(String),
    Delete(String),
    Replace(String, String), // (old, new)
}

/// Line-level edit operation.
#[derive(PartialEq, Eq, Clone, Debug)]
enum LineEdit {
    Same,
    Modified(Vec<TokenEdit>),
    Inserted,
    Deleted,
}

/// Structural delta for an output channel (stdout or stderr).
#[derive(PartialEq, Eq, Clone, Debug)]
enum OutputDelta {
    Identical,
    Permutation(Vec<usize>),
    Edited(Vec<LineEdit>),
}

/// Tokenize text into lines of whitespace-split tokens.
fn tokenize(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .map(|line| line.split_whitespace().map(|s| s.to_string()).collect())
        .collect()
}

/// Token-level Needleman-Wunsch: compute edit distance and optionally the edit script.
/// Returns (cost, edits). Pass `true` for `need_edits` to get the backtrace.
fn token_nw(a: &[String], b: &[String], need_edits: bool) -> (usize, Vec<TokenEdit>) {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    #[allow(clippy::needless_range_loop)]
    for i in 1..=n { dp[i][0] = i; }
    #[allow(clippy::needless_range_loop)]
    for j in 1..=m { dp[0][j] = j; }
    for i in 1..=n {
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    let cost = dp[n][m];
    if !need_edits {
        return (cost, Vec::new());
    }
    let mut edits = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && dp[i][j] == dp[i - 1][j - 1] + if a[i-1] == b[j-1] { 0 } else { 1 } {
            if a[i - 1] == b[j - 1] {
                edits.push(TokenEdit::Keep(a[i - 1].clone()));
            } else {
                edits.push(TokenEdit::Replace(a[i - 1].clone(), b[j - 1].clone()));
            }
            i -= 1; j -= 1;
        } else if j > 0 && dp[i][j] == dp[i][j - 1] + 1 {
            edits.push(TokenEdit::Insert(b[j - 1].clone()));
            j -= 1;
        } else {
            edits.push(TokenEdit::Delete(a[i - 1].clone()));
            i -= 1;
        }
    }
    edits.reverse();
    (cost, edits)
}

/// Line-level NW alignment. Match cost = token edit distance (unit leaf costs).
/// Delete/insert cost = token count of the line.
fn align_lines(ref_lines: &[Vec<String>], obs_lines: &[Vec<String>]) -> Vec<LineEdit> {
    let n = ref_lines.len();
    let m = obs_lines.len();

    // Precompute token-level match costs for all line pairs
    let match_costs: Vec<Vec<usize>> = (0..n).map(|i|
        (0..m).map(|j| token_nw(&ref_lines[i], &obs_lines[j], false).0).collect()
    ).collect();

    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in 1..=n { dp[i][0] = dp[i - 1][0] + ref_lines[i - 1].len().max(1); }
    for j in 1..=m { dp[0][j] = dp[0][j - 1] + obs_lines[j - 1].len().max(1); }
    for i in 1..=n {
        for j in 1..=m {
            dp[i][j] = (dp[i - 1][j] + ref_lines[i - 1].len().max(1))
                .min(dp[i][j - 1] + obs_lines[j - 1].len().max(1))
                .min(dp[i - 1][j - 1] + match_costs[i - 1][j - 1]);
        }
    }

    // Backtrace — use cached match_costs, only compute edits for matched pairs
    let mut edits = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && dp[i][j] == dp[i - 1][j - 1] + match_costs[i - 1][j - 1] {
            if match_costs[i - 1][j - 1] == 0 {
                edits.push(LineEdit::Same);
            } else {
                let (_, tok_edits) = token_nw(&ref_lines[i - 1], &obs_lines[j - 1], true);
                edits.push(LineEdit::Modified(tok_edits));
            }
            i -= 1; j -= 1;
        } else if j > 0 && dp[i][j] == dp[i][j - 1] + obs_lines[j - 1].len().max(1) {
            edits.push(LineEdit::Inserted);
            j -= 1;
        } else {
            edits.push(LineEdit::Deleted);
            i -= 1;
        }
    }
    edits.reverse();
    edits
}

/// Compute structural delta for an output channel (stdout or stderr).
fn compute_output_delta(ref_out: &str, obs_out: &str) -> OutputDelta {
    if ref_out == obs_out {
        return OutputDelta::Identical;
    }

    let ref_labels = tokenize(ref_out);
    let obs_labels = tokenize(obs_out);

    // Check for pure reorder: same line multiset by canonical label pattern
    if ref_labels.len() == obs_labels.len() {
        let mut ref_sorted = ref_labels.clone();
        let mut obs_sorted = obs_labels.clone();
        ref_sorted.sort();
        obs_sorted.sort();
        if ref_sorted == obs_sorted {
            let perm: Vec<usize> = obs_labels.iter().map(|obs_line| {
                ref_labels.iter().position(|r| r == obs_line).unwrap_or(usize::MAX)
            }).collect();
            return OutputDelta::Permutation(perm);
        }
    }

    OutputDelta::Edited(align_lines(&ref_labels, &obs_labels))
}

// --- Observation key for grouping ---

/// Observation key for grouping comparisons.
///
/// For runs with a from-reference (flagged runs), this is the structural edit
/// script produced by two-level NW alignment of ref vs obs stdout/stderr.
/// For runs without a from-reference (baselines), this is the content-hashed
/// token pattern of the raw observation.
///
/// Two runs group together when their ObsKeys match across all contexts.
#[derive(PartialEq, Eq)]
struct ObsKey {
    stdout: OutputDelta,
    stderr: OutputDelta,
    exit_code: Option<i32>,
    fs_changes: Vec<execute::FsChange>,
}

impl ObsKey {
    fn from_obs(obs: &Observation) -> Self {
        fn output_key(text: &str) -> OutputDelta {
            if text.is_empty() {
                return OutputDelta::Identical;
            }
            let labels = tokenize(text);
            OutputDelta::Edited(labels.iter().map(|line| {
                if line.is_empty() { LineEdit::Same }
                else { LineEdit::Modified(line.iter().map(|l| TokenEdit::Keep(l.clone())).collect()) }
            }).collect())
        }
        ObsKey {
            stdout: output_key(&obs.stdout),
            stderr: output_key(&obs.stderr),
            exit_code: obs.exit_code,
            fs_changes: obs.fs_changes.iter()
                .filter(|c| !matches!(c, execute::FsChange::Modified { detail, .. } if detail == "mtime changed"))
                .cloned().collect(),
        }
    }

    /// Compute a structural delta: what transformation did this flag apply to the base?
    ///
    /// Uses two-level NW alignment on content-hashed tokens. The resulting edit
    /// script captures the shape of the transformation (which lines were inserted,
    /// deleted, or modified, and within modified lines, which tokens changed).
    /// Shared tokens (filenames, keywords) serve as natural anchors for alignment.
    fn from_delta(reference: &Observation, obs: &Observation) -> Self {
        let stdout = compute_output_delta(&reference.stdout, &obs.stdout);
        let stderr = compute_output_delta(&reference.stderr, &obs.stderr);

        let exit_code = if reference.exit_code == obs.exit_code {
            reference.exit_code
        } else {
            Some(reference.exit_code.unwrap_or(0) * 1000 + obs.exit_code.unwrap_or(0))
        };

        // FS changes: set difference, filtering mtime-only modifications.
        // Mtime changes from directory entry creation are timing-dependent
        // (second-level granularity makes detection a coin flip), not behavioral signal.
        let ref_fs: HashSet<&execute::FsChange> = reference.fs_changes.iter().collect();
        let obs_fs: HashSet<&execute::FsChange> = obs.fs_changes.iter().collect();
        let mut fs_changes: Vec<execute::FsChange> = obs_fs.difference(&ref_fs)
            .filter(|c| !matches!(c, execute::FsChange::Modified { detail, .. } if detail == "mtime changed"))
            .map(|c| (*c).clone())
            .collect();
        fs_changes.sort_by(|a, b| format!("{:?}", a).cmp(&format!("{:?}", b)));

        ObsKey { stdout, stderr, exit_code, fs_changes }
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
            let positionals: Vec<Vec<&str>> = group.run_labels.iter()
                .map(|label| {
                    output::parse_label(label).into_iter()
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
                let args = output::parse_label(label);
                let flags: Vec<&&str> = args.iter().filter(|a| a.starts_with('-')).collect();
                let positionals: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).map(|s| s.to_string()).collect();

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
    let obs_by_args: HashMap<(&[Arg], &str), &Observation> = grid.cells.iter()
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

        // Save obs keys for grouping.
        // For runs with a from-reference, use delta keys (what changed vs base).
        // This groups by "what the flag does" rather than "what the output looks like."
        let obs_keys: Vec<(String, ObsKey)> = if let Some(ref ref_args) = run.diff_from {
            obs_list.iter().map(|(name, obs)| {
                let ref_obs = obs_by_args.get(&(ref_args.as_slice(), *name));
                let key = match ref_obs {
                    Some(ref_obs) => ObsKey::from_delta(ref_obs, obs),
                    None => ObsKey::from_obs(obs),
                };
                (name.to_string(), key)
            }).collect()
        } else {
            obs_list.iter()
                .map(|(name, obs)| (name.to_string(), ObsKey::from_obs(obs)))
                .collect()
        };
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
            g.from_ref.as_ref() == analysis.from_ref.as_ref()
            && g.obs_list.len() == keys.len()
            && g.obs_list.iter().zip(keys.iter()).all(|((_, a), (_, b))| a == b)
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
                if let Some(key) = arg.flag_key() {
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
