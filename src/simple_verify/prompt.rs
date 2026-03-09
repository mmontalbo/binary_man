//! Prompt generation for the LM.
//!
//! Builds structured prompts that give the LM all the context it needs to
//! decide on actions. The prompt format is intentionally simple and human-readable.

use super::types::{Outcome, State};

/// Build the LM prompt for a set of target surfaces.
pub fn build_prompt(state: &State, target_ids: &[String]) -> String {
    let mut prompt = String::new();

    // Header
    let context_str = if state.context_argv.is_empty() {
        String::new()
    } else {
        format!(" {}", state.context_argv.join(" "))
    };
    prompt.push_str(&format!(
        "# Behavior Verification: {}{}\n\n",
        state.binary, context_str
    ));

    // Baseline info
    if let Some(baseline) = &state.baseline {
        prompt.push_str("## Baseline\n\n");
        prompt.push_str(&format!("argv: {:?}\n", baseline.argv));
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
            if let Some(hint) = &entry.value_hint {
                prompt.push_str(&format!("Value hint: {}\n", hint));
            }
            prompt.push_str(&format!("Attempts: {}\n", entry.attempts.len()));

            // Show last attempt if any
            if let Some(last) = entry.attempts.last() {
                prompt.push_str(&format!("Last argv: {:?}\n", last.argv));
                prompt.push_str(&format!(
                    "Last outcome: {}\n",
                    format_outcome(&last.outcome)
                ));
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
            "OutputsEqual (output matches baseline - try different approach)".to_string()
        }
        Outcome::SetupFailed { hint } => format!("SetupFailed: {}", hint),
        Outcome::Crashed { hint } => format!("Crashed: {}", hint),
        Outcome::ExecutionError { error } => format!("ExecutionError: {}", error),
    }
}

const INSTRUCTIONS: &str = r#"## Instructions

For each surface, provide ONE action:

1. **SetBaseline** (required first, once only): Define the baseline scenario
   - argv: Command arguments WITHOUT the surface being tested
   - seed: Setup commands and files needed

2. **Test**: Test a surface
   - surface_id: Which surface to test
   - argv: Full command with the surface option
   - seed: Setup commands (copy baseline's seed if same setup works)

3. **Exclude**: Give up on a surface
   - surface_id: Which surface
   - reason: Why it can't be verified

## Execution Model

Each scenario runs in a fresh empty temp directory. ALL commands run in this SAME directory:
- seed.files are written first
- seed.setup commands run in sequence
- The main command runs last

Do NOT use `cd`, `sh -c`, or create subdirectories. Work in the current directory.

Respond with JSON:
```json
{
  "actions": [
    { "kind": "SetBaseline", "argv": [], "seed": { "setup": [["touch", "file.txt"]], "files": [{"path": "input.txt", "content": "hello"}] } },
    { "kind": "Test", "surface_id": "--example", "argv": ["--example"], "seed": { "setup": [["touch", "file.txt"]], "files": [{"path": "input.txt", "content": "hello"}] } }
  ]
}
```

CRITICAL: Each setup command is an ARRAY of strings: `["cmd", "arg1", "arg2"]`, NOT a single string.

Key principles:
- Output must DIFFER from baseline to verify a surface
- If OutputsEqual, try: different argv, different seed files, different setup commands
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
        assert!(prompt.contains("SetBaseline"));
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
                value_hint: None,
                status: Status::Pending,
                attempts: vec![],
            }],
            cycle: 1,
        };

        let prompt = build_prompt(&state, &["--stat".to_string()]);

        assert!(prompt.contains("argv: [\"diff\"]"));
        assert!(prompt.contains("git"));
        assert!(prompt.contains("init"));
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
                value_hint: None,
                status: Status::Pending,
                attempts: vec![Attempt {
                    cycle: 1,
                    argv: vec!["--verbose".to_string()],
                    seed: Seed::default(),
                    evidence_path: "evidence/verbose_c1.json".to_string(),
                    outcome: Outcome::OutputsEqual,
                }],
            }],
            cycle: 2,
        };

        let prompt = build_prompt(&state, &["--verbose".to_string()]);

        assert!(prompt.contains("Attempts: 1"));
        assert!(prompt.contains("Last argv: [\"--verbose\"]"));
        assert!(prompt.contains("OutputsEqual"));
    }

    #[test]
    fn test_format_outcome() {
        assert!(format_outcome(&Outcome::Verified {
            diff_kind: DiffKind::Stdout
        })
        .contains("Verified"));
        assert!(format_outcome(&Outcome::OutputsEqual).contains("matches baseline"));
        assert!(format_outcome(&Outcome::SetupFailed {
            hint: "error".to_string()
        })
        .contains("SetupFailed"));
    }
}
