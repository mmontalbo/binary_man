//! Mechanical experiment refinement.
//!
//! Given analysis metrics from a previous round, generate a new Script
//! with experiments designed to split remaining identical groups.
//! Returns None when converged (no further refinement possible).

use std::collections::{HashMap, HashSet};

use crate::analyze::AnalysisMetrics;
use crate::discover::FlagInfo;
use crate::parse::{
    Arg, FileContent, NamedContext, Property, Run, Script, SetupCommand,
};

const MAX_INTERACTION_GROUPS: usize = 2;
const MAX_FLAGS_PER_GROUP: usize = 8;
const MAX_UNTESTED_PER_ROUND: usize = 20;

/// Refine an experiment based on analysis metrics.
/// Accumulated state from previous rounds, passed to refine.
pub struct RefineState<'a> {
    pub ever_isolated: &'a HashSet<String>,
    pub unproductive: &'a HashSet<String>,
    pub indist_stems: &'a [String],
    pub round: usize,
    pub max_rounds: usize,
}

/// Returns None if converged (no further refinement possible).
pub fn refine(
    base_script: &Script,
    metrics: &AnalysisMetrics,
    flag_info: Option<&FlagInfo>,
    state: &RefineState,
) -> Option<Script> {
    let ever_isolated = state.ever_isolated;
    let unproductive = state.unproductive;
    let indist_stems = state.indist_stems;
    let round = state.round;
    let max_rounds = state.max_rounds;

    if round >= max_rounds {
        return None;
    }

    // Count unexplained identical groups (excluding known alias pairs)
    let aliases = flag_info.map(|fi| &fi.aliases);
    let unexplained: Vec<_> = metrics.groups.iter()
        .filter(|g| !g.isolated() && !is_alias_pair(g, aliases))
        .collect();

    let untested = flag_info
        .map(|_| &metrics.untested_flags)
        .filter(|u| !u.is_empty());

    if unexplained.is_empty() && untested.is_none() {
        return None;
    }

    let mut contexts: Vec<NamedContext> = Vec::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut strategies: Vec<&str> = Vec::new();

    // Reuse base contexts for perturbation generation
    let base_ctx = base_script.contexts.iter()
        .find(|c| c.name == "numeric_standard")
        .cloned();

    // --- Strategy 1: Interaction testing ---
    // For large identical groups, generate pairwise flag combinations
    let mut interaction_groups: Vec<_> = unexplained.iter()
        .filter(|g| {
            let flag_count = count_unique_flags(g);
            flag_count >= 3
        })
        .collect();
    interaction_groups.sort_by(|a, b| b.run_labels.len().cmp(&a.run_labels.len()));

    for group in interaction_groups.iter().take(MAX_INTERACTION_GROUPS) {
        let flags = extract_flags(group);
        let (prefix, trailing) = extract_positionals(group);

        let flags: Vec<_> = flags.into_iter().take(MAX_FLAGS_PER_GROUP).collect();
        if flags.len() < 2 { continue; }

        // Build args as: [prefix (subcommand)..., flags..., trailing (targets)...]
        let build_interaction_args = |flag_args: &[Arg]| -> Vec<Arg> {
            let mut args = prefix.clone();
            args.extend(flag_args.iter().cloned());
            args.extend(trailing.iter().cloned());
            args
        };

        let base_args = {
            let mut a = prefix.clone();
            a.extend(trailing.iter().cloned());
            a
        };

        for flag in &flags {
            runs.push(Run {
                args: build_interaction_args(std::slice::from_ref(flag)),
                in_contexts: None,
                stdin: None,
                diff_from: if base_args.is_empty() { None } else { Some(base_args.clone()) },
            });
        }
        for i in 0..flags.len() {
            for j in (i + 1)..flags.len() {
                runs.push(Run {
                    args: build_interaction_args(&[flags[i].clone(), flags[j].clone()]),
                    in_contexts: None,
                    stdin: None,
                    diff_from: if base_args.is_empty() { None } else { Some(base_args.clone()) },
                });
            }
        }
        strategies.push("interaction");
    }

    // --- Strategy 1b: Stem-guided cross-group interaction ---
    // Target specific indistinguishable flag stems (from report-level analysis).
    // For each indistinguishable stem, pair it with top isolated flags across
    // all positional arg variants (file AND directory targets).
    // Skip flags whose runs were slow (near timeout) — combining or re-testing
    // them will also be slow or timeout.
    let timeout_threshold_ms = crate::execute::CELL_TIMEOUT_SECS * 1000 - 500;
    let slow_runs: HashSet<&String> = metrics.runs.iter()
        .filter(|r| {
            // Check max wall time across ALL context groups, not just majority
            r.context_groups.iter()
                .any(|(_, obs)| obs.resources.wall_time_ms >= timeout_threshold_ms)
        })
        .map(|r| &r.args_str)
        .collect();
    let isolated_groups: Vec<_> = metrics.groups.iter()
        .filter(|g| g.isolated()
            && !unproductive.contains(&g.run_labels[0])
            && g.majority_obs.resources.wall_time_ms < timeout_threshold_ms)
        .collect();

    if !isolated_groups.is_empty() && !indist_stems.is_empty() {
        // Build isolated candidates with their positionals
        struct IsolatedCandidate {
            flags: Vec<Arg>,
            positionals: Vec<Arg>,
            dimensions: HashSet<String>,
        }
        let isolated_candidates: Vec<IsolatedCandidate> = isolated_groups.iter()
            .filter(|g| !g.sensitivity.is_empty())
            .map(|g| {
                let args = parse_run_label(&g.run_labels[0]);
                let dims: HashSet<String> = g.sensitivity.iter()
                    .map(|s| parse_sensitivity_label(s).0)
                    .collect();
                IsolatedCandidate {
                    flags: args.iter().filter(|a| a.is_flag()).cloned().collect(),
                    positionals: args.iter().filter(|a| !a.is_flag()).cloned().collect(),
                    dimensions: dims,
                }
            })
            .filter(|c| !c.flags.is_empty())
            .collect();

        // Rank isolated candidates by dimension count
        let mut ranked_isolated: Vec<&IsolatedCandidate> = isolated_candidates.iter().collect();
        ranked_isolated.sort_by(|a, b| b.dimensions.len().cmp(&a.dimensions.len()));
        let top_isolated: Vec<&IsolatedCandidate> = ranked_isolated.into_iter().take(5).collect();

        // Collect prefix + trailing for each indistinguishable stem's runs
        // Skip stems whose runs were slow (near timeout)
        struct StemVariant { prefix: Vec<Arg>, trailing: Vec<Arg> }
        let mut stem_variants: HashMap<String, Vec<StemVariant>> = HashMap::new();
        for group in &metrics.groups {
            for label in &group.run_labels {
                if slow_runs.contains(label) { continue; }
                if let Some(stem) = crate::report::flag_stem(label) {
                    if crate::report::is_combination(&stem) { continue; }
                    let canon = crate::report::canonical_flag(&stem, flag_info.map(|fi| &fi.aliases));
                    if indist_stems.contains(&canon) {
                        let args = parse_run_label(label);
                        let (prefix, _, trailing) = split_args(&args);
                        let entry = stem_variants.entry(canon).or_default();
                        if !entry.iter().any(|v| v.prefix == prefix && v.trailing == trailing) {
                            entry.push(StemVariant { prefix, trailing });
                        }
                    }
                }
            }
        }

        let mut cross_count = 0;
        let max_cross_pairs = 40;

        for stem in indist_stems {
            let Some(variants) = stem_variants.get(stem) else { continue };

            for candidate in &top_isolated {
                for variant in variants {
                    // Build: [prefix..., iso_flags..., stem, trailing...]
                    let mut all_flags: Vec<Arg> = candidate.flags.clone();
                    all_flags.push(Arg::Literal(stem.clone()));
                    let args = build_run_args(&variant.prefix, &all_flags, &variant.trailing);
                    let base = build_run_args(&variant.prefix, &[], &variant.trailing);

                    if cross_count >= max_cross_pairs { break; }
                    let already_present = runs.iter().any(|r| r.args == args);
                    if !already_present {
                        runs.push(Run {
                            args,
                            in_contexts: None,
                            stdin: None,
                            diff_from: if base.is_empty() { None } else { Some(base) },
                        });
                        cross_count += 1;
                    }
                }
                // Also try with isolated flag's own trailing targets
                if !candidate.positionals.is_empty() {
                    let (iso_prefix, _, iso_trailing) = split_args(&candidate.positionals);
                    let prefix = if let Some(v) = variants.first() { &v.prefix } else { &iso_prefix };
                    let mut all_flags: Vec<Arg> = candidate.flags.clone();
                    all_flags.push(Arg::Literal(stem.clone()));
                    let args = build_run_args(prefix, &all_flags, &iso_trailing);
                    let base = build_run_args(prefix, &[], &iso_trailing);

                    if cross_count < max_cross_pairs {
                        let already_present = runs.iter().any(|r| r.args == args);
                        if !already_present {
                            runs.push(Run {
                                args,
                                in_contexts: None,
                                stdin: None,
                                diff_from: if base.is_empty() { None } else { Some(base) },
                            });
                            cross_count += 1;
                        }
                    }
                }
                if cross_count >= max_cross_pairs { break; }
            }
            if cross_count >= max_cross_pairs { break; }
        }
        if cross_count > 0 {
            strategies.push("cross-group");
        }
    }

    // --- Strategy 2: Sensitivity refinement ---
    // For dimensions that caused splits, generate graduated variants
    let mut seen_dimensions: HashSet<String> = HashSet::new();
    let mut sensitivity_count = 0;

    for group in &metrics.groups {
        for label in &group.sensitivity {
            if sensitivity_count >= 3 { break; }

            let (dimension, target) = parse_sensitivity_label(label);
            let dim_key = format!("{}:{}", dimension, target);
            if seen_dimensions.contains(&dim_key) { continue; }
            seen_dimensions.insert(dim_key);

            if let Some(ref base) = base_ctx {
                let new_contexts = generate_graduated_variants(base, &dimension, &target);
                contexts.extend(new_contexts);
                sensitivity_count += 1;
            }
        }
    }
    if sensitivity_count > 0 {
        strategies.push("sensitivity");

        // Re-emit runs that showed sensitivity to the refined dimensions
        let sensitive_run_indices: HashSet<usize> = metrics.groups.iter()
            .filter(|g| !g.sensitivity.is_empty())
            .flat_map(|g| g.run_indices.iter().copied())
            .collect();

        for ri in &sensitive_run_indices {
            if let Some(run) = base_script.runs.get(*ri) {
                // Only add if not already in the runs list
                let already_present = runs.iter().any(|r| r.args == run.args);
                if !already_present {
                    runs.push(Run {
                        args: run.args.clone(),
                        in_contexts: None,
                        stdin: None,
                        diff_from: run.diff_from.clone(),
                    });
                }
            }
        }
    }

    // --- Strategy 3: Untested flag pickup ---
    if let Some(untested_flags) = untested {
        let deduplicated = deduplicate_aliases(untested_flags, aliases);
        let (unt_prefix, unt_trailing) = infer_positionals(base_script);

        for flag in deduplicated.iter().take(MAX_UNTESTED_PER_ROUND) {
            if flag == "--help" || flag == "--version" { continue; }
            let args = build_run_args(&unt_prefix, &[Arg::Literal(flag.clone())], &unt_trailing);
            let base = build_run_args(&unt_prefix, &[], &unt_trailing);
            let already_present = runs.iter().any(|r| r.args == args);
            if !already_present {
                runs.push(Run {
                    args,
                    in_contexts: None,
                    stdin: None,
                    diff_from: if base.is_empty() { None } else { Some(base) },
                });
            }
        }
        if !deduplicated.is_empty() {
            strategies.push("untested");
        }
    }

    // --- Re-include all runs from identical groups ---
    // These are the runs we're trying to split. They must be re-tested across
    // the new contexts so grouping can detect if the new contexts differentiate them.
    for group in &metrics.groups {
        if group.isolated() { continue; }
        for label in &group.run_labels {
            if ever_isolated.contains(label) || unproductive.contains(label) { continue; }
            if slow_runs.contains(label) { continue; }
            let args = parse_run_label(label);
            if args.is_empty() { continue; }
            let already_present = runs.iter().any(|r| r.args == args);
            if !already_present {
                // Infer from_ref: if the run has positional args, use them as the base
                let positionals: Vec<Arg> = args.iter()
                    .filter(|a| !a.is_flag())
                    .cloned()
                    .collect();
                let from_ref = if positionals.is_empty() || args.iter().all(|a| !a.is_flag()) {
                    None
                } else {
                    Some(positionals)
                };
                runs.push(Run {
                    args,
                    in_contexts: None,
                    stdin: None,
                    diff_from: from_ref,
                });
            }
        }
    }

    if runs.is_empty() {
        return None;
    }

    // Always include base archetype contexts + any new sensitivity contexts
    if contexts.is_empty() {
        contexts = base_script.contexts.clone();
    } else {
        let mut all = base_script.contexts.clone();
        all.extend(contexts);
        contexts = all;
    }

    let new_runs = runs.len();
    eprintln!("[round {}] strategies: {}, {} runs, {} contexts",
        round + 1, strategies.join("+"), new_runs, contexts.len());

    Some(Script { contexts, runs })
}

/// Check if a 2-run group is a known alias pair.
fn is_alias_pair(
    group: &crate::analyze::BehaviorGroup,
    aliases: Option<&std::collections::HashMap<String, String>>,
) -> bool {
    if group.run_labels.len() != 2 { return false; }
    let aliases = match aliases {
        Some(a) => a,
        None => return false,
    };

    let flags: Vec<Vec<&str>> = group.run_labels.iter()
        .map(|label| {
            crate::output::parse_label(label).into_iter()
                .filter(|s| s.starts_with('-'))
                .collect()
        })
        .collect();

    if flags.len() != 2 { return false; }
    if flags[0].len() != 1 || flags[1].len() != 1 { return false; }

    let f1 = flags[0][0];
    let f2 = flags[1][0];

    aliases.get(f1).map(|a| a == f2).unwrap_or(false)
        || aliases.get(f2).map(|a| a == f1).unwrap_or(false)
}

/// Parse a formatted run label like `"-b" "input.txt"` back to args.
/// Detects `$(...)` expressions and wraps them as Arg::Extract.
fn parse_run_label(label: &str) -> Vec<Arg> {
    label.split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| {
            if s.starts_with("$(") && s.ends_with(')') {
                Arg::Extract(s[2..s.len()-1].to_string())
            } else {
                Arg::Literal(s.to_string())
            }
        })
        .collect()
}

/// Split args into (prefix, flags, trailing).
/// Prefix = leading non-flag args (subcommand). Trailing = non-flag args after first flag.
fn split_args(args: &[Arg]) -> (Vec<Arg>, Vec<Arg>, Vec<Arg>) {
    let mut prefix = Vec::new();
    let mut flags = Vec::new();
    let mut trailing = Vec::new();
    let mut seen_flag = false;
    for arg in args {
        if arg.is_flag() {
            seen_flag = true;
            flags.push(arg.clone());
        } else if seen_flag {
            trailing.push(arg.clone());
        } else {
            prefix.push(arg.clone());
        }
    }
    (prefix, flags, trailing)
}

/// Build run args preserving subcommand position: [prefix..., flags..., trailing...].
fn build_run_args(prefix: &[Arg], flags: &[Arg], trailing: &[Arg]) -> Vec<Arg> {
    let mut args = prefix.to_vec();
    args.extend(flags.iter().cloned());
    args.extend(trailing.iter().cloned());
    args
}

/// Count unique flag arguments across runs in a group.
fn count_unique_flags(group: &crate::analyze::BehaviorGroup) -> usize {
    let mut flags = HashSet::new();
    for label in &group.run_labels {
        for arg in crate::output::parse_label(label) {
            if arg.starts_with('-') { flags.insert(arg); }
        }
    }
    flags.len()
}

/// Extract unique flag args from a group's run labels.
fn extract_flags(group: &crate::analyze::BehaviorGroup) -> Vec<Arg> {
    let mut flags = Vec::new();
    let mut seen = HashSet::new();
    for label in &group.run_labels {
        for arg in crate::output::parse_label(label) {
            if arg.starts_with('-') && seen.insert(arg) {
                flags.push(Arg::Literal(arg.to_string()));
            }
        }
    }
    flags
}

/// Extract common positional args from a group's first run label.
/// Extract positional args from a run label, split into (prefix, trailing).
/// Prefix = leading non-flag args before the first flag (subcommand args).
/// Trailing = non-flag args after the first flag (file targets).
fn extract_positionals(group: &crate::analyze::BehaviorGroup) -> (Vec<Arg>, Vec<Arg>) {
    if group.run_labels.is_empty() { return (Vec::new(), Vec::new()); }
    let label = &group.run_labels[0];
    let args = parse_run_label(label);

    let mut prefix = Vec::new();
    let mut trailing = Vec::new();
    let mut seen_flag = false;
    for arg in &args {
        if arg.is_flag() {
            seen_flag = true;
        } else if seen_flag {
            trailing.push(arg.clone());
        } else {
            prefix.push(arg.clone());
        }
    }
    (prefix, trailing)
}

/// Parse a sensitivity label into (dimension, target).
fn parse_sensitivity_label(label: &str) -> (String, String) {
    // Strip effect annotation: "input.txt=size:1 (-4 lines)" → "input.txt=size:1"
    let label = if let Some(idx) = label.find(" (") {
        &label[..idx]
    } else {
        label
    };

    if let Some(rest) = label.strip_prefix("remove ") {
        return ("remove".into(), rest.to_string());
    }
    if let Some(idx) = label.find("=size:") {
        return ("size".into(), label[..idx].to_string());
    }
    if let Some(idx) = label.find("=empty") {
        return ("empty".into(), label[..idx].to_string());
    }
    if label.contains(" mtime") {
        let target = label.split_whitespace().next().unwrap_or(label);
        return ("mtime".into(), target.to_string());
    }
    if label.contains(" readonly") {
        let target = label.split_whitespace().next().unwrap_or(label);
        return ("perms".into(), target.to_string());
    }
    if label.starts_with("env ") {
        let parts: Vec<&str> = label.splitn(2, ' ').collect();
        let var = parts.get(1).unwrap_or(&"");
        let var = var.split('=').next().unwrap_or(var);
        return ("env".into(), var.to_string());
    }

    ("content".into(), label.to_string())
}

/// Generate graduated context variants for a dimension.
fn generate_graduated_variants(
    base: &NamedContext,
    dimension: &str,
    target: &str,
) -> Vec<NamedContext> {
    let mut variants = Vec::new();

    match dimension {
        "size" => {
            for size in [1, 100, 1000] {
                let mut cmds = base.commands.clone();
                cmds.push(SetupCommand::CreateFile {
                    path: target.to_string(),
                    content: FileContent::Size(size),
                });
                variants.push(NamedContext {
                    name: format!("many_files /{}=size:{}", target, size),
                    extends: None,
                    commands: cmds,
                });
            }
        }
        "mtime" => {
            for (prop, label) in [(Property::MtimeOld, "old"), (Property::MtimeRecent, "recent")] {
                let mut cmds = base.commands.clone();
                cmds.push(SetupCommand::SetProps {
                    path: target.to_string(),
                    props: vec![prop],
                });
                variants.push(NamedContext {
                    name: format!("many_files /{} mtime={}", target, label),
                    extends: None,
                    commands: cmds,
                });
            }
        }
        "remove" => {
            let mut cmds_remove = base.commands.clone();
            cmds_remove.push(SetupCommand::Remove { path: target.to_string() });
            variants.push(NamedContext {
                name: format!("many_files /remove {}", target),
                extends: None,
                commands: cmds_remove,
            });

            let mut cmds_empty = base.commands.clone();
            cmds_empty.push(SetupCommand::CreateFile {
                path: target.to_string(),
                content: FileContent::Empty,
            });
            variants.push(NamedContext {
                name: format!("many_files /{}=empty", target),
                extends: None,
                commands: cmds_empty,
            });

            let mut cmds_broken = base.commands.clone();
            cmds_broken.push(SetupCommand::CreateLink {
                path: target.to_string(),
                target: "nonexistent".to_string(),
            });
            variants.push(NamedContext {
                name: format!("many_files /{} -> nonexistent", target),
                extends: None,
                commands: cmds_broken,
            });
        }
        "perms" => {
            for (prop, label) in [(Property::ReadOnly, "readonly"), (Property::Executable, "executable")] {
                let mut cmds = base.commands.clone();
                cmds.push(SetupCommand::SetProps {
                    path: target.to_string(),
                    props: vec![prop],
                });
                variants.push(NamedContext {
                    name: format!("many_files /{} {}", target, label),
                    extends: None,
                    commands: cmds,
                });
            }
        }
        "env" => {
            let mut cmds_alt = base.commands.clone();
            cmds_alt.push(SetupCommand::SetEnv {
                var: target.to_string(),
                value: "alternate".to_string(),
            });
            variants.push(NamedContext {
                name: format!("many_files /env {}=alternate", target),
                extends: None,
                commands: cmds_alt,
            });

            let mut cmds_empty = base.commands.clone();
            cmds_empty.push(SetupCommand::SetEnv {
                var: target.to_string(),
                value: String::new(),
            });
            variants.push(NamedContext {
                name: format!("many_files /env {}=empty", target),
                extends: None,
                commands: cmds_empty,
            });

            let mut cmds_unset = base.commands.clone();
            cmds_unset.push(SetupCommand::RemoveEnv { var: target.to_string() });
            variants.push(NamedContext {
                name: format!("many_files /remove env {}", target),
                extends: None,
                commands: cmds_unset,
            });
        }
        _ => {}
    }

    variants
}

/// Deduplicate flags by alias (keep short form when both are untested).
fn deduplicate_aliases(
    flags: &[String],
    aliases: Option<&std::collections::HashMap<String, String>>,
) -> Vec<String> {
    let aliases = match aliases {
        Some(a) => a,
        None => return flags.to_vec(),
    };

    let flag_set: HashSet<&String> = flags.iter().collect();
    let mut result = Vec::new();
    let mut seen = HashSet::new();

    for flag in flags {
        if seen.contains(flag) { continue; }
        seen.insert(flag.clone());

        if let Some(alias) = aliases.get(flag) {
            seen.insert(alias.clone());
            // Keep the short form if both are untested
            if flag_set.contains(alias) {
                if flag.len() <= alias.len() {
                    result.push(flag.clone());
                } else {
                    result.push(alias.clone());
                }
            } else {
                result.push(flag.clone());
            }
        } else {
            result.push(flag.clone());
        }
    }
    result
}

/// Infer common positional args from the base script's runs.
/// Infer (prefix, trailing) from the script's runs.
fn infer_positionals(script: &Script) -> (Vec<Arg>, Vec<Arg>) {
    for run in &script.runs {
        let (prefix, _, trailing) = split_args(&run.args);
        if !prefix.is_empty() || !trailing.is_empty() {
            return (prefix, trailing);
        }
    }
    (Vec::new(), Vec::new())
}
