//! Analysis pipeline: Script + GridResult → AnalysisMetrics.
//!
//! Compares observations using structural tree diff: stdout/stderr are tokenized
//! into lines of whitespace-split tokens and aligned via two-level Needleman-Wunsch
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
    /// Used for grouping comparisons.
    obs_list: Vec<(String, ObsKey)>,
}

// --- Structural diff types ---
//
// Stdout/stderr comparison uses a two-level Needleman-Wunsch alignment:
//   1. Tokenize both ref and obs into lines of whitespace-split tokens
//   2. Align lines (match cost = token edit distance, gap cost = token count)
//   3. Within matched lines, align tokens (unit cost per insert/delete/replace)
//   4. Produce a structural edit script: sequence of LineEdits, each containing TokenEdits
//
// Token values are raw strings — same string matches across ref and obs naturally.
// No hashing, no label pools, no canonicalization. Shared tokens (filenames, keywords)
// are natural alignment anchors. Value-level precision: "root" ≠ "0" → ls -l vs ls -n.

/// Token-level edit operation. Values are the raw token strings.
#[derive(PartialEq, Eq, Clone, Debug, Hash)]
enum TokenEdit {
    Keep(String),
    Insert(String),
    Delete(String),
    Replace(String, String), // (old, new)
}

/// Line-level edit operation.
#[derive(PartialEq, Eq, Clone, Debug, Hash)]
enum LineEdit {
    Same,
    Modified(Vec<TokenEdit>),
    Inserted,
    Deleted,
}

/// Structural delta for an output channel (stdout or stderr).
#[derive(PartialEq, Eq, Clone, Debug, Hash)]
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
#[derive(PartialEq, Eq, Hash)]
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
    /// Leave-one-out robustness: flag stem → (contexts_survived, total_contexts).
    /// A flag with 15/15 is robust; 1/15 is fragile.
    pub robustness: HashMap<String, (usize, usize)>,
}

impl AnalysisMetrics {
    pub fn isolated_count(&self) -> usize {
        self.groups.iter().filter(|g| g.isolated()).count()
    }

    pub fn identical_count(&self) -> usize {
        self.groups.iter().filter(|g| !g.isolated()).count()
    }

    pub fn pairwise_distinguished(&self) -> HashSet<String> {
        pairwise_distinguished_from_groups(&self.groups)
    }
}

/// Find flag pairs proven different by cross-group interaction data.
/// Find flag stems proven distinguishable by cross-group pairwise interaction.
/// Takes groups as slices of label slices — used by both full analysis and leave-one-out.
fn pairwise_distinguished_from_label_groups(groups: &[&[String]]) -> HashSet<String> {
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
    for (gi, labels) in groups.iter().enumerate() {
        for label in *labels {
            let args = output::parse_label(label);
            let flags: Vec<&&str> = args.iter().filter(|a| a.starts_with('-')).collect();
            let positionals: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).map(|s| s.to_string()).collect();
            if flags.len() >= 2 {
                for (fi, modifier) in flags.iter().enumerate() {
                    let base: Vec<String> = flags.iter().enumerate()
                        .filter(|(i, _)| *i != fi)
                        .map(|(_, f)| f.to_string())
                        .collect();
                    let mod_stem = if let Some(eq) = modifier.find('=') {
                        modifier[..eq].to_string()
                    } else {
                        modifier.to_string()
                    };
                    combo_map.entry(ComboKey { base, positionals: positionals.clone() })
                        .or_default().push((mod_stem, gi));
                }
            }
        }
    }

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

fn pairwise_distinguished_from_groups(groups: &[BehaviorGroup]) -> HashSet<String> {
    let label_slices: Vec<&[String]> = groups.iter().map(|g| g.run_labels.as_slice()).collect();
    pairwise_distinguished_from_label_groups(&label_slices)
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

    // Cache NW delta results by (ref_stdout_hash, obs_stdout_hash) to avoid
    // recomputing for combo runs that produce identical output.
    let mut delta_cache: HashMap<(u64, u64), OutputDelta> = HashMap::new();
    let str_hash = |s: &str| -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    };

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
                    Some(ref_obs) => {
                        // Cache stdout/stderr deltas by content hash
                        let stdout_key = (str_hash(&ref_obs.stdout), str_hash(&obs.stdout));
                        let stdout = delta_cache.entry(stdout_key)
                            .or_insert_with(|| compute_output_delta(&ref_obs.stdout, &obs.stdout))
                            .clone();
                        let stderr_key = (str_hash(&ref_obs.stderr), str_hash(&obs.stderr));
                        let stderr = delta_cache.entry(stderr_key)
                            .or_insert_with(|| compute_output_delta(&ref_obs.stderr, &obs.stderr))
                            .clone();

                        let exit_code = if ref_obs.exit_code == obs.exit_code {
                            ref_obs.exit_code
                        } else {
                            Some(ref_obs.exit_code.unwrap_or(0) * 1000 + obs.exit_code.unwrap_or(0))
                        };

                        let ref_fs: HashSet<&execute::FsChange> = ref_obs.fs_changes.iter().collect();
                        let obs_fs: HashSet<&execute::FsChange> = obs.fs_changes.iter().collect();
                        let mut fs_changes: Vec<execute::FsChange> = obs_fs.difference(&ref_fs)
                            .filter(|c| !matches!(c, execute::FsChange::Modified { detail, .. } if detail == "mtime changed"))
                            .map(|c| (*c).clone()).collect();
                        fs_changes.sort_by(|a, b| format!("{:?}", a).cmp(&format!("{:?}", b)));

                        ObsKey { stdout, stderr, exit_code, fs_changes }
                    }
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
    // Hash-based grouping: O(runs × contexts) instead of O(runs × groups × contexts).
    // Each run's per-context obs keys are hashed to a u64 for fast lookup.
    let mut behavior_groups: Vec<BehaviorGroup> = Vec::new();
    let mut group_index: HashMap<u64, Vec<usize>> = HashMap::new();

    for analysis in &run_analyses {
        let ri = analysis.run_index;

        let obs_entry = run_obs_keys.iter()
            .find(|e| e.run_index == ri);

        let Some(entry) = obs_entry else { continue };
        let keys = &entry.keys;

        // Hash the grouping key: from_ref + per-context obs keys
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        analysis.from_ref.hash(&mut hasher);
        keys.hash(&mut hasher);
        let h = hasher.finish();

        // Look up candidate groups by hash, verify with equality
        let found = group_index.get(&h).and_then(|indices| {
            indices.iter().find(|&&gi| {
                let g = &behavior_groups[gi];
                g.from_ref.as_ref() == analysis.from_ref.as_ref()
                && g.obs_list.len() == keys.len()
                && g.obs_list.iter().zip(keys.iter()).all(|((_, a), (_, b))| a == b)
            }).copied()
        });

        if let Some(gi) = found {
            let group = &mut behavior_groups[gi];
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
            let gi = behavior_groups.len();
            group_index.entry(h).or_default().push(gi);
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

    // --- Leave-one-out robustness ---
    let robustness_start = std::time::Instant::now();
    // For each context, mask it and re-group to check if each flag is still distinguished.
    // Uses lightweight grouping: just (from_ref_hash, masked_obs_keys) → group_index,
    // then pairwise_distinguished_from_labels on the resulting label groups.
    let context_names: Vec<String> = script.contexts.iter().map(|c| c.name.clone()).collect();
    let all_distinguished = pairwise_distinguished_from_groups(&behavior_groups);

    // Sample contexts for leave-one-out when the grid is large.
    // Full LOO is O(contexts × runs²) — for ls (35 × 4380²) this takes >100s.
    // Sampling 10 contexts gives equivalent confidence at bounded cost.
    let max_loo = 10;
    let loo_contexts: Vec<&String> = if context_names.len() <= max_loo {
        context_names.iter().collect()
    } else {
        // Deterministic sample: evenly spaced
        let step = context_names.len() / max_loo;
        context_names.iter().step_by(step).take(max_loo).collect()
    };

    let mut robustness: HashMap<String, (usize, usize)> = HashMap::new();
    let n_contexts = loo_contexts.len();
    for flag in &all_distinguished {
        robustness.insert(flag.clone(), (0, n_contexts));
    }

    for &drop_ctx in &loo_contexts {
        // Re-group: hash each run's (from_ref, masked obs_keys) → group labels
        let mut loo_label_groups: Vec<Vec<String>> = Vec::new();
        let mut loo_index: HashMap<u64, Vec<usize>> = HashMap::new();

        for entry in &run_obs_keys {
            let ri = entry.run_index;
            let Some(analysis) = run_analyses.iter().find(|a| a.run_index == ri) else { continue };
            let masked_keys: Vec<&(String, ObsKey)> = entry.keys.iter()
                .filter(|(ctx, _)| ctx != drop_ctx)
                .collect();

            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            analysis.from_ref.hash(&mut hasher);
            for (_, k) in &masked_keys { k.hash(&mut hasher); }
            let h = hasher.finish();

            let found = loo_index.get(&h).and_then(|indices| {
                indices.iter().find(|&&gi| {
                    // Verify: same from_ref and same masked obs_keys
                    let other_entry = run_obs_keys.iter().find(|e| {
                        run_analyses.iter().any(|a| a.run_index == e.run_index
                            && a.args_str == loo_label_groups[gi][0])
                    });
                    if let Some(other) = other_entry {
                        let other_analysis = run_analyses.iter().find(|a| a.run_index == other.run_index).unwrap();
                        if other_analysis.from_ref != analysis.from_ref { return false; }
                        let other_masked: Vec<&(String, ObsKey)> = other.keys.iter()
                            .filter(|(ctx, _)| ctx != drop_ctx).collect();
                        masked_keys.len() == other_masked.len()
                            && masked_keys.iter().zip(other_masked.iter()).all(|((_, a), (_, b))| a == b)
                    } else { false }
                }).copied()
            });

            if let Some(gi) = found {
                loo_label_groups[gi].push(analysis.args_str.clone());
            } else {
                let gi = loo_label_groups.len();
                loo_index.entry(h).or_default().push(gi);
                loo_label_groups.push(vec![analysis.args_str.clone()]);
            }
        }

        let loo_slices: Vec<&[String]> = loo_label_groups.iter().map(|g| g.as_slice()).collect();
        let loo_distinguished = pairwise_distinguished_from_label_groups(&loo_slices);
        for flag in &all_distinguished {
            if loo_distinguished.contains(flag) {
                robustness.get_mut(flag).unwrap().0 += 1;
            }
        }
    }

    let robustness_ms = robustness_start.elapsed().as_millis();
    if robustness_ms > 1000 {
        eprintln!("  robustness: {}ms ({} contexts × {} runs)", robustness_ms, context_names.len(), run_obs_keys.len());
    }

    AnalysisMetrics {
        groups: behavior_groups,
        runs: run_analyses,
        untested_flags,
        context_count: grid.context_count,
        total_runs,
        robustness,
    }
}
