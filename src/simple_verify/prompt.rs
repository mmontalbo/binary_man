//! Prompt generation for the LM.
//!
//! Builds structured prompts that give the LM all the context it needs to
//! decide on actions. The prompt format is intentionally simple and human-readable.

use super::types::{Outcome, State};

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
}
