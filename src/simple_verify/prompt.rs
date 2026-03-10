//! Prompt generation for the LM.
//!
//! Builds structured prompts that give the LM all the context it needs to
//! decide on actions. The prompt format is intentionally simple and human-readable.

use super::types::{Outcome, State};
use std::collections::HashMap;

/// A known issue extracted from SetupFailed outcomes.
struct KnownIssue {
    /// The command that failed (e.g., "git checkout main").
    command: String,
    /// The error message (truncated).
    error: String,
    /// How many times this combination occurred.
    count: usize,
}

/// Extract aggregated known issues from all SetupFailed outcomes across the state.
///
/// Returns issues sorted by count descending, filtered to those with count >= 2.
fn extract_known_issues(state: &State) -> Vec<KnownIssue> {
    // Map from (command, error_prefix) -> count
    let mut counts: HashMap<(String, String), usize> = HashMap::new();

    for entry in &state.entries {
        for attempt in &entry.attempts {
            if let Outcome::SetupFailed { hint } = &attempt.outcome {
                if let Some((cmd, err)) = parse_setup_failed_hint(hint) {
                    *counts.entry((cmd, err)).or_insert(0) += 1;
                }
            }
        }
    }

    // Convert to Vec and filter to count >= 2
    let mut issues: Vec<KnownIssue> = counts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|((command, error), count)| KnownIssue {
            command,
            error,
            count,
        })
        .collect();

    // Sort by count descending, then by command for stability
    issues.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.command.cmp(&b.command)));

    // Return top 5
    issues.truncate(5);
    issues
}

/// Parse a SetupFailed hint to extract the command and error.
///
/// The hint format is:
/// ```text
/// Setup command #N failed: ["cmd", "arg1", "arg2"]
/// stderr: error message here
/// ```
/// or:
/// ```text
/// Setup command #N failed to execute: ["cmd", "arg1"]
/// error: message
/// ```
///
/// Returns (command_string, error_prefix) or None if parsing fails.
fn parse_setup_failed_hint(hint: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = hint.lines().collect();
    if lines.is_empty() {
        return None;
    }

    // Parse the first line to extract the command array
    let first_line = lines[0];

    // Find the array part: [...] at the end of the first line
    let array_start = first_line.find('[')?;
    let array_end = first_line.rfind(']')?;
    if array_start >= array_end {
        return None;
    }

    let array_str = &first_line[array_start..=array_end];
    let command = parse_debug_string_array(array_str)?;

    // Extract error from the second line (if present)
    let error = if lines.len() > 1 {
        let second_line = lines[1];
        // Remove "stderr: " or "error: " prefix
        let error_text = second_line
            .strip_prefix("stderr: ")
            .or_else(|| second_line.strip_prefix("error: "))
            .unwrap_or(second_line);
        // Truncate to ~60 chars for grouping
        truncate_error(error_text, 60)
    } else {
        "(no details)".to_string()
    };

    Some((command, error))
}

/// Parse a Rust debug format string array like `["git", "checkout", "main"]`.
fn parse_debug_string_array(s: &str) -> Option<String> {
    // Simple parser for ["a", "b", "c"] format
    let trimmed = s.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }

    let inner = &trimmed[1..trimmed.len() - 1];
    if inner.is_empty() {
        return Some(String::new());
    }

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = inner.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' if !in_quotes => {
                in_quotes = true;
            }
            '"' if in_quotes => {
                parts.push(current.clone());
                current.clear();
                in_quotes = false;
            }
            '\\' if in_quotes => {
                // Handle escaped character
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ',' | ' ' if !in_quotes => {
                // Skip separators outside quotes
            }
            _ if in_quotes => {
                current.push(ch);
            }
            _ => {
                // Skip other chars outside quotes
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Truncate error message for grouping purposes.
fn truncate_error(s: &str, max_len: usize) -> String {
    // Take first line only
    let first_line = s.lines().next().unwrap_or(s);
    let trimmed = first_line.trim();

    if trimmed.len() <= max_len {
        trimmed.to_string()
    } else {
        // Find a safe boundary
        let mut end = max_len;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &trimmed[..end])
    }
}

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

/// Build the LM prompt for a set of target surfaces.
pub fn build_prompt(state: &State, target_ids: &[String]) -> String {
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
            prompt.push_str(&format!("Description: {}\n", entry.description));
            if let Some(context) = &entry.context {
                prompt.push_str(&format!("{}\n", context));
            }
            if let Some(hint) = &entry.value_hint {
                prompt.push_str(&format!("Value hint: {}\n", hint));
            }

            // Show all attempts with detailed output information
            if !entry.attempts.is_empty() {
                prompt.push_str(&format!("\n**Attempts:** {} total\n\n", entry.attempts.len()));

                for (i, attempt) in entry.attempts.iter().enumerate() {
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
                        prompt.push_str(
                            "    → Outputs matched! Try a different seed that exercises the option's effect.\n",
                        );
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
    }
}

const INSTRUCTIONS: &str = r#"## Instructions

For each surface, provide ONE action:

1. **SetBaseline** (required first, once only): Define the baseline scenario
   - args: Arguments to append to base command (usually empty [])
   - seed: Setup commands and files needed

2. **Test**: Test a surface
   - surface_id: Which surface to test
   - args: Arguments to append to base command (include the option being tested)
   - seed: Setup commands (copy baseline's seed if same setup works)

3. **Exclude**: Give up on a surface
   - surface_id: Which surface
   - reason: Why it can't be verified

## Execution Model

Each test runs TWO scenarios with the SAME seed:
1. Control run: base command (no extra args)
2. Option run: base command + your args

The option is **verified if outputs DIFFER**.

Each scenario runs in a fresh empty temp directory. ALL commands run in this SAME directory:
- seed.files are written first
- seed.setup commands run in sequence
- The main command runs last

Do NOT use `cd`, `sh -c`, or create subdirectories. Work in the current directory.

Respond with JSON:
```json
{
  "actions": [
    { "kind": "SetBaseline", "args": [], "seed": { "setup": [["touch", "file.txt"]], "files": [] } },
    { "kind": "Test", "surface_id": "--example", "args": ["--example"], "seed": { "setup": [["touch", "file.txt"]], "files": [] } },
    { "kind": "Exclude", "surface_id": "--other", "reason": "requires root" }
  ]
}
```

CRITICAL: Each setup command is an ARRAY of strings: `["cmd", "arg1", "arg2"]`, NOT a single string.

## Key Principles

- Output must DIFFER from control (same seed, no extra args) to verify a surface
- Craft seeds that EXERCISE the option's behavior:
  - For `ls -B` (ignore backups): seed must include backup files like `file.txt~`
  - For `--color`: seed must include content that triggers colorization
- If OutputsEqual, try: different seed files that better exercise the option
- Learn from stderr errors and adjust seed accordingly
- Exclude if surface genuinely can't be tested (needs root, hardware, network, etc.)
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simple_verify::types::{
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
                attempts: vec![],
            }],
            cycle: 0,
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
                attempts: vec![],
            }],
            cycle: 1,
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
                }],
            }],
            cycle: 2,
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
                attempts: vec![],
            }],
            cycle: 1,
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
                }],
            }],
            cycle: 2,
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
                    },
                ],
            }],
            cycle: 3,
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

        let result = super::parse_setup_failed_hint(hint);
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

        let result = super::parse_setup_failed_hint(hint);
        assert!(result.is_some());
        let (cmd, err) = result.unwrap();
        assert_eq!(cmd, "nonexistent cmd");
        assert!(err.contains("No such file"));
    }

    #[test]
    fn test_parse_debug_string_array() {
        assert_eq!(
            super::parse_debug_string_array(r#"["git", "checkout", "main"]"#),
            Some("git checkout main".to_string())
        );
        assert_eq!(
            super::parse_debug_string_array(r#"["ls", "-la"]"#),
            Some("ls -la".to_string())
        );
        assert_eq!(
            super::parse_debug_string_array(r#"["echo"]"#),
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
                    attempts: vec![
                        make_setup_failed_attempt(1),
                        make_setup_failed_attempt(2),
                        make_setup_failed_attempt(3),
                    ],
                },
                SurfaceEntry {
                    id: "--opt2".to_string(),
                    description: "Option 2".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Pending,
                    attempts: vec![
                        make_setup_failed_attempt(4),
                        make_setup_failed_attempt(5),
                        make_setup_failed_attempt(6),
                    ],
                },
            ],
            cycle: 7,
        };

        let issues = super::extract_known_issues(&state);

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
                }],
            }],
            cycle: 2,
        };

        let issues = super::extract_known_issues(&state);
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
        };

        let issues = super::extract_known_issues(&state);
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
                    attempts: vec![make_setup_failed_attempt(1), make_setup_failed_attempt(2)],
                },
                SurfaceEntry {
                    id: "--oneline".to_string(),
                    description: "One line".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Pending,
                    attempts: vec![make_setup_failed_attempt(3), make_setup_failed_attempt(4)],
                },
            ],
            cycle: 5,
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
                attempts: vec![],
            }],
            cycle: 1,
        };

        let prompt = build_prompt(&state, &["--all".to_string()]);

        // Should NOT contain the known issues section
        assert!(!prompt.contains("Known Issues"));
    }

    #[test]
    fn test_truncate_error() {
        assert_eq!(super::truncate_error("short", 60), "short");
        let long = "a".repeat(100);
        let result = super::truncate_error(&long, 60);
        assert!(result.len() <= 63); // 60 chars + "..."
        assert!(result.ends_with("..."));
    }
}
