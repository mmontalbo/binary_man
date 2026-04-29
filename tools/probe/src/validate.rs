//! Validate predictions against observations.

use crate::delta;
use crate::execute::Observation;
use crate::execute::CheckResult;
use crate::parse::{Expectation, OutputDimension, Predicate, Test};
use std::collections::HashMap;

/// Check all expectations for a test against its observation and cross-references.
pub fn check_expectations(
    test: &Test,
    obs: &Observation,
    all_observations: &HashMap<Vec<String>, Observation>,
) -> Vec<CheckResult> {
    test.expectations
        .iter()
        .map(|exp| check_one(exp, obs, all_observations))
        .collect()
}

fn check_one(
    exp: &Expectation,
    obs: &Observation,
    all_obs: &HashMap<Vec<String>, Observation>,
) -> CheckResult {
    match (&exp.dimension, &exp.predicate) {
        // === STDOUT ===
        (OutputDimension::Stdout, Predicate::Empty) => {
            check(obs.stdout.trim().is_empty(), "expected stdout empty", &obs.stdout)
        }
        (OutputDimension::Stdout, Predicate::NotEmpty) => {
            check(!obs.stdout.trim().is_empty(), "expected stdout not-empty", &obs.stdout)
        }
        (OutputDimension::Stdout, Predicate::Contains(text)) => {
            check(
                obs.stdout.contains(text.as_str()),
                &format!("expected stdout contains {:?}", text),
                &obs.stdout,
            )
        }
        (OutputDimension::Stdout, Predicate::NotContains(text)) => {
            check(
                !obs.stdout.contains(text.as_str()),
                &format!("expected stdout not-contains {:?}", text),
                &obs.stdout,
            )
        }
        (OutputDimension::Stdout, Predicate::LinesExactly(n)) => {
            let actual = obs.stdout.lines().filter(|l| !l.is_empty()).count();
            check(
                actual == *n,
                &format!("expected {} lines, got {}", n, actual),
                "",
            )
        }
        (OutputDimension::Stdout, Predicate::EveryLineMatches(pattern)) => {
            match regex::Regex::new(pattern) {
                Ok(re) => {
                    let lines: Vec<&str> = obs.stdout.lines().filter(|l| !l.is_empty()).collect();
                    let all_match = lines.iter().all(|l| re.is_match(l));
                    let failing = lines.iter().find(|l| !re.is_match(l));
                    if all_match {
                        CheckResult {
                            passed: true,
                            detail: format!("all {} lines match /{}/", lines.len(), pattern),
                            context: vec![],
                        }
                    } else {
                        let mut ctx = vec![
                            format!("  failing: {:?}", failing.unwrap_or(&"")),
                        ];
                        // Show first few lines for context
                        for (i, l) in lines.iter().take(3).enumerate() {
                            let mark = if re.is_match(l) { "✓" } else { "✗" };
                            ctx.push(format!("  line {}: {} {:?}", i + 1, mark, l));
                        }
                        CheckResult {
                            passed: false,
                            detail: format!("expected every line matches /{}/", pattern),
                            context: ctx,
                        }
                    }
                }
                Err(e) => CheckResult {
                    passed: false,
                    detail: format!("invalid regex /{}/:  {}", pattern, e),
                    context: vec![],
                },
            }
        }

        // Relational stdout predicates (vs another invocation)
        (OutputDimension::Stdout, pred) => check_relational_stdout(pred, obs, all_obs),

        // === STDERR ===
        (OutputDimension::Stderr, Predicate::StderrEmpty) => {
            check(obs.stderr.trim().is_empty(), "expected stderr empty", &obs.stderr)
        }
        (OutputDimension::Stderr, Predicate::StderrNotEmpty) => {
            check(!obs.stderr.trim().is_empty(), "expected stderr not-empty", &obs.stderr)
        }
        (OutputDimension::Stderr, Predicate::StderrContains(text)) => {
            check(
                obs.stderr.contains(text.as_str()),
                &format!("expected stderr contains {:?}", text),
                &obs.stderr,
            )
        }
        (OutputDimension::Stderr, Predicate::Unchanged { vs_args }) => {
            if let Some(other) = all_obs.get(vs_args) {
                check(
                    obs.stderr == other.stderr,
                    &format!("expected stderr unchanged vs {:?}", vs_args),
                    &format!("got {:?} vs {:?}", &obs.stderr[..obs.stderr.len().min(80)], &other.stderr[..other.stderr.len().min(80)]),
                )
            } else {
                CheckResult {
                    passed: false,
                    detail: format!("reference invocation {:?} not found", vs_args),
                    context: vec![],
                }
            }
        }

        // === EXIT CODE ===
        (OutputDimension::Exit, Predicate::ExitCode(expected)) => {
            let actual = obs.exit_code.unwrap_or(-1);
            check(
                actual == *expected,
                &format!("expected exit {}, got {}", expected, actual),
                "",
            )
        }
        (OutputDimension::Exit, Predicate::ExitUnchanged { vs_args }) => {
            if let Some(other) = all_obs.get(vs_args) {
                check(
                    obs.exit_code == other.exit_code,
                    &format!(
                        "expected exit unchanged vs {:?}, got {:?} vs {:?}",
                        vs_args, obs.exit_code, other.exit_code
                    ),
                    "",
                )
            } else {
                CheckResult {
                    passed: false,
                    detail: format!("reference invocation {:?} not found", vs_args),
                    context: vec![],
                }
            }
        }
        (OutputDimension::Exit, Predicate::ExitChanged { vs_args }) => {
            if let Some(other) = all_obs.get(vs_args) {
                check(
                    obs.exit_code != other.exit_code,
                    &format!(
                        "expected exit changed vs {:?}, both are {:?}",
                        vs_args, obs.exit_code
                    ),
                    "",
                )
            } else {
                CheckResult {
                    passed: false,
                    detail: format!("reference invocation {:?} not found", vs_args),
                    context: vec![],
                }
            }
        }

        // === FILESYSTEM ===
        (OutputDimension::Fs, Predicate::FsUnchanged) => {
            // TODO: implement fs snapshot comparison
            CheckResult {
                passed: true,
                detail: "fs unchanged (not yet implemented)".to_string(),
                    context: vec![],
            }
        }
        (OutputDimension::Fs, _) => CheckResult {
            passed: false,
            detail: "fs predicates not yet implemented".to_string(),
                    context: vec![],
        },

        // Catch-all
        _ => CheckResult {
            passed: false,
            detail: format!("unsupported predicate combination: {:?}", exp),
                    context: vec![],
        },
    }
}

/// Check relational stdout predicates that compare against another invocation.
fn check_relational_stdout(
    pred: &Predicate,
    obs: &Observation,
    all_obs: &HashMap<Vec<String>, Observation>,
) -> CheckResult {
    // Extract vs_args from the predicate
    let vs_args = match pred {
        Predicate::Reordered { vs_args }
        | Predicate::Superset { vs_args }
        | Predicate::Subset { vs_args }
        | Predicate::Complement { vs_args }
        | Predicate::Collapsed { vs_args }
        | Predicate::Preserved { vs_args }
        | Predicate::PreservedPrefixAdded { vs_args }
        | Predicate::PreservedFieldsExpanded { vs_args }
        | Predicate::PreservedWrapped { vs_args }
        | Predicate::Identical { vs_args }
        | Predicate::LinesSame { vs_args }
        | Predicate::LinesMore { vs_args }
        | Predicate::LinesFewer { vs_args } => vs_args,
        _ => {
            return CheckResult {
                passed: false,
                detail: format!("not a relational predicate: {:?}", pred),
                    context: vec![],
            }
        }
    };

    let other = match all_obs.get(vs_args) {
        Some(o) => o,
        None => {
            return CheckResult {
                passed: false,
                detail: format!("reference invocation {:?} not found", vs_args),
                    context: vec![],
            }
        }
    };

    // Build a compact summary of both outputs for context on failures
    let stdout_context = format!(
        "control ({} lines):\n{}\noption ({} lines):\n{}",
        other.stdout.lines().count(),
        other.stdout.lines().take(5).collect::<Vec<_>>().join("\n"),
        obs.stdout.lines().count(),
        obs.stdout.lines().take(5).collect::<Vec<_>>().join("\n"),
    );

    match pred {
        Predicate::Reordered { .. } => {
            let rel = delta::classify_stdout(&other.stdout, &obs.stdout);
            check(
                rel == delta::EntryRelation::Reordered,
                &format!("expected reordered, got {:?}", rel),
                &stdout_context,
            )
        }
        Predicate::Superset { .. } => {
            let rel = delta::classify_stdout(&other.stdout, &obs.stdout);
            check(
                rel == delta::EntryRelation::Superset,
                &format!("expected superset, got {:?}", rel),
                &stdout_context,
            )
        }
        Predicate::Subset { .. } => {
            let rel = delta::classify_stdout(&other.stdout, &obs.stdout);
            check(
                rel == delta::EntryRelation::Subset,
                &format!("expected subset, got {:?}", rel),
                &stdout_context,
            )
        }
        Predicate::Complement { .. } => {
            let rel = delta::classify_stdout(&other.stdout, &obs.stdout);
            check(
                rel == delta::EntryRelation::Complement,
                &format!("expected complement, got {:?}", rel),
                &stdout_context,
            )
        }
        Predicate::Collapsed { .. } => {
            let rel = delta::classify_stdout(&other.stdout, &obs.stdout);
            check(
                rel == delta::EntryRelation::Collapsed,
                &format!("expected collapsed, got {:?}", rel),
                &stdout_context,
            )
        }
        Predicate::Preserved { .. } => {
            let rel = delta::classify_stdout(&other.stdout, &obs.stdout);
            check(
                matches!(
                    rel,
                    delta::EntryRelation::Preserved
                        | delta::EntryRelation::PreservedPrefixAdded
                        | delta::EntryRelation::PreservedFieldsExpanded
                        | delta::EntryRelation::PreservedWrapped
                ),
                &format!("expected preserved, got {:?}", rel),
                &stdout_context,
            )
        }
        Predicate::PreservedPrefixAdded { .. } => {
            let rel = delta::classify_stdout(&other.stdout, &obs.stdout);
            check(
                rel == delta::EntryRelation::PreservedPrefixAdded,
                &format!("expected preserved prefix-added, got {:?}", rel),
                &stdout_context,
            )
        }
        Predicate::PreservedFieldsExpanded { .. } => {
            let rel = delta::classify_stdout(&other.stdout, &obs.stdout);
            check(
                rel == delta::EntryRelation::PreservedFieldsExpanded,
                &format!("expected preserved fields-expanded, got {:?}", rel),
                &stdout_context,
            )
        }
        Predicate::PreservedWrapped { .. } => {
            let rel = delta::classify_stdout(&other.stdout, &obs.stdout);
            check(
                rel == delta::EntryRelation::PreservedWrapped,
                &format!("expected preserved wrapped, got {:?}", rel),
                &stdout_context,
            )
        }
        Predicate::Identical { .. } => {
            let rel = delta::classify_stdout(&other.stdout, &obs.stdout);
            check(
                rel == delta::EntryRelation::Identical,
                &format!("expected identical, got {:?}", rel),
                &stdout_context,
            )
        }
        Predicate::LinesSame { .. } => {
            let mine = obs.stdout.lines().filter(|l| !l.is_empty()).count();
            let theirs = other.stdout.lines().filter(|l| !l.is_empty()).count();
            check(
                mine == theirs,
                &format!("expected same line count, got {} vs {}", mine, theirs),
                &stdout_context,
            )
        }
        Predicate::LinesMore { .. } => {
            let mine = obs.stdout.lines().filter(|l| !l.is_empty()).count();
            let theirs = other.stdout.lines().filter(|l| !l.is_empty()).count();
            check(
                mine > theirs,
                &format!("expected more lines, got {} vs {}", mine, theirs),
                &stdout_context,
            )
        }
        Predicate::LinesFewer { .. } => {
            let mine = obs.stdout.lines().filter(|l| !l.is_empty()).count();
            let theirs = other.stdout.lines().filter(|l| !l.is_empty()).count();
            check(
                mine < theirs,
                &format!("expected fewer lines, got {} vs {}", mine, theirs),
                &stdout_context,
            )
        }
        _ => CheckResult {
            passed: false,
            detail: format!("unhandled relational predicate: {:?}", pred),
                    context: vec![],
        },
    }
}

fn check(passed: bool, desc: &str, context_str: &str) -> CheckResult {
    let mut context = Vec::new();
    if !passed && !context_str.is_empty() {
        // Show up to 5 lines of context
        for line in context_str.lines().take(5) {
            context.push(format!("  {}", line));
        }
        let total_lines = context_str.lines().count();
        if total_lines > 5 {
            context.push(format!("  ... ({} more lines)", total_lines - 5));
        }
    }
    CheckResult {
        passed,
        detail: desc.to_string(),
        context,
    }
}

