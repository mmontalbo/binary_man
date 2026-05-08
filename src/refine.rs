//! Mechanical experiment refinement.
//!
//! Given analysis metrics from a previous round, generate a new Script
//! with experiments designed to split remaining identical groups.
//! Returns None when converged (no further refinement possible).

use std::collections::HashSet;

use crate::analyze::AnalysisMetrics;
use crate::discover::FlagInfo;
use crate::parse::{
    FileContent, NamedContext, Property, Run, Script, SetupCommand,
};

const MAX_INTERACTION_GROUPS: usize = 2;
const MAX_FLAGS_PER_GROUP: usize = 8;
const MAX_UNTESTED_PER_ROUND: usize = 20;

/// Refine an experiment based on analysis metrics.
/// Returns None if converged (no further refinement possible).
///
/// `ever_isolated` contains run labels that were isolated in any previous round.
/// `unproductive` contains run labels that showed no signal (all errors, or in large
/// identical groups with the same target). Both are excluded from refinement.
pub fn refine(
    base_script: &Script,
    metrics: &AnalysisMetrics,
    flag_info: Option<&FlagInfo>,
    ever_isolated: &HashSet<String>,
    unproductive: &HashSet<String>,
    round: usize,
    max_rounds: usize,
) -> Option<Script> {
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
        let positionals = extract_positionals(group);

        let flags: Vec<_> = flags.into_iter().take(MAX_FLAGS_PER_GROUP).collect();
        if flags.len() < 2 { continue; }

        // Generate singles + pairs
        for flag in &flags {
            let mut args = vec![flag.clone()];
            args.extend(positionals.iter().cloned());
            runs.push(Run {
                args,
                in_contexts: None,
                stdin: None,
                diff_from: if positionals.is_empty() { None } else {
                    Some(positionals.clone())
                },
            });
        }
        for i in 0..flags.len() {
            for j in (i + 1)..flags.len() {
                let mut args = vec![flags[i].clone(), flags[j].clone()];
                args.extend(positionals.iter().cloned());
                runs.push(Run {
                    args,
                    in_contexts: None,
                    stdin: None,
                    diff_from: if positionals.is_empty() { None } else {
                        Some(positionals.clone())
                    },
                });
            }
        }
        strategies.push("interaction");
    }

    // --- Strategy 1b: Cross-group interaction ---
    // Pair identical-group flags with top isolated flags (modifier + mode).
    // E.g., ls: pair `-h` (identical) with `-l` (isolated) to reveal `-h`'s effect.
    let isolated_groups: Vec<_> = metrics.groups.iter()
        .filter(|g| g.isolated() && !unproductive.contains(&g.run_labels[0]))
        .collect();

    if !isolated_groups.is_empty() && !unexplained.is_empty() {
        // Pick top 3 isolated flags by sensitivity count (most behavioral signal)
        let mut ranked_isolated: Vec<_> = isolated_groups.iter()
            .map(|g| (&g.run_labels[0], g.sensitivity.len()))
            .collect();
        ranked_isolated.sort_by(|a, b| b.1.cmp(&a.1));

        let top_isolated: Vec<Vec<String>> = ranked_isolated.iter()
            .take(3)
            .map(|(label, _)| parse_run_label(label))
            .filter(|args| !args.is_empty())
            .collect();

        // For each unexplained group, pair each flag with each top isolated flag
        let mut cross_count = 0;
        for group in unexplained.iter().take(2) {
            let group_flags = extract_flags(group);
            let group_positionals = extract_positionals(group);

            for iso_args in &top_isolated {
                // Extract just the flags from the isolated run
                let iso_flags: Vec<&String> = iso_args.iter()
                    .filter(|a| a.starts_with('-'))
                    .collect();
                if iso_flags.is_empty() { continue; }

                for gflag in group_flags.iter().take(MAX_FLAGS_PER_GROUP) {
                    let mut args: Vec<String> = iso_flags.iter().map(|s| s.to_string()).collect();
                    args.push(gflag.clone());
                    // Use the positionals from whichever side has them
                    let positionals = if group_positionals.is_empty() {
                        iso_args.iter().filter(|a| !a.starts_with('-')).cloned().collect()
                    } else {
                        group_positionals.clone()
                    };
                    args.extend(positionals.iter().cloned());

                    let already_present = runs.iter().any(|r| r.args == args);
                    if !already_present {
                        runs.push(Run {
                            args,
                            in_contexts: None,
                            stdin: None,
                            diff_from: if positionals.is_empty() { None } else {
                                Some(positionals)
                            },
                        });
                        cross_count += 1;
                    }
                }
            }
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
        let positionals = infer_positionals(base_script);

        for flag in deduplicated.iter().take(MAX_UNTESTED_PER_ROUND) {
            if flag == "--help" || flag == "--version" { continue; }
            let mut args = vec![flag.clone()];
            args.extend(positionals.iter().cloned());
            let already_present = runs.iter().any(|r| r.args == args);
            if !already_present {
                runs.push(Run {
                    args,
                    in_contexts: None,
                    stdin: None,
                    diff_from: if positionals.is_empty() { None } else {
                        Some(positionals.clone())
                    },
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
            let args = parse_run_label(label);
            if args.is_empty() { continue; }
            let already_present = runs.iter().any(|r| r.args == args);
            if !already_present {
                // Infer from_ref: if the run has positional args, use them as the base
                let positionals: Vec<String> = args.iter()
                    .filter(|a| !a.starts_with('-'))
                    .cloned()
                    .collect();
                let from_ref = if positionals.is_empty() || args.iter().all(|a| !a.starts_with('-')) {
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

    let flags: Vec<Vec<String>> = group.run_labels.iter()
        .map(|label| {
            label.split('"')
                .enumerate()
                .filter(|(i, _)| i % 2 == 1)
                .map(|(_, s)| s.to_string())
                .filter(|s| s.starts_with('-'))
                .collect()
        })
        .collect();

    if flags.len() != 2 { return false; }
    if flags[0].len() != 1 || flags[1].len() != 1 { return false; }

    let f1 = &flags[0][0];
    let f2 = &flags[1][0];

    aliases.get(f1).map(|a| a == f2).unwrap_or(false)
        || aliases.get(f2).map(|a| a == f1).unwrap_or(false)
}

/// Parse a formatted run label like `"-b" "input.txt"` back to args.
fn parse_run_label(label: &str) -> Vec<String> {
    label.split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_string())
        .collect()
}

/// Count unique flag arguments across runs in a group.
fn count_unique_flags(group: &crate::analyze::BehaviorGroup) -> usize {
    let mut flags = HashSet::new();
    for label in &group.run_labels {
        for arg in label.split('"').enumerate().filter(|(i, _)| i % 2 == 1).map(|(_, s)| s) {
            if arg.starts_with('-') {
                flags.insert(arg.to_string());
            }
        }
    }
    flags.len()
}

/// Extract unique flag args from a group's run labels.
fn extract_flags(group: &crate::analyze::BehaviorGroup) -> Vec<String> {
    let mut flags = Vec::new();
    let mut seen = HashSet::new();
    for label in &group.run_labels {
        for arg in label.split('"').enumerate().filter(|(i, _)| i % 2 == 1).map(|(_, s)| s) {
            if arg.starts_with('-') && seen.insert(arg.to_string()) {
                flags.push(arg.to_string());
            }
        }
    }
    flags
}

/// Extract common positional args from a group's first run label.
fn extract_positionals(group: &crate::analyze::BehaviorGroup) -> Vec<String> {
    if group.run_labels.is_empty() { return Vec::new(); }
    let label = &group.run_labels[0];
    label.split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_string())
        .filter(|s| !s.starts_with('-'))
        .collect()
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
            for size in [1, 100, 1000, 10000, 100000] {
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
fn infer_positionals(script: &Script) -> Vec<String> {
    // Find the most common non-flag trailing args
    for run in &script.runs {
        let positionals: Vec<String> = run.args.iter()
            .filter(|a| !a.starts_with('-'))
            .cloned()
            .collect();
        if !positionals.is_empty() {
            return positionals;
        }
    }
    Vec::new()
}
