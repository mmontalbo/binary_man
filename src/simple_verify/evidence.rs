//! Scenario execution and evidence capture.
//!
//! This module handles running commands and capturing their outputs in a
//! sandboxed environment using bubblewrap (bwrap). Tests run with:
//! - Network isolation (no external requests)
//! - Read-only root filesystem
//! - Writable work directory only
//! - Process isolation

use super::types::Seed;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Default timeout for scenario execution in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Build a bwrap command with full sandbox isolation.
///
/// The sandbox provides:
/// - Read-only root filesystem
/// - Writable work directory
/// - No network access
/// - Isolated /tmp
/// - Process dies with parent
fn build_sandbox_command(work_dir: &Path) -> Command {
    let mut cmd = Command::new("bwrap");

    // Core filesystem setup
    cmd.args(["--ro-bind", "/", "/"]); // Read-only root
    cmd.args(["--dev", "/dev"]); // Device access
    cmd.args(["--proc", "/proc"]); // Proc filesystem
    cmd.args(["--tmpfs", "/tmp"]); // Isolated /tmp

    // Make work directory writable
    let work_dir_str = work_dir.to_string_lossy();
    cmd.args(["--bind", &work_dir_str, &work_dir_str]);

    // Security isolation
    cmd.arg("--unshare-net"); // No network
    cmd.arg("--die-with-parent"); // Cleanup on parent exit
    cmd.arg("--new-session"); // Signal isolation

    // Set working directory
    cmd.args(["--chdir", &work_dir_str]);

    // Separator before actual command
    cmd.arg("--");

    cmd
}

/// Maximum bytes to capture for stdout/stderr.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Result of a single setup command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupResult {
    /// Index in the setup commands array.
    pub index: usize,
    /// The command that was run.
    pub argv: Vec<String>,
    /// Exit code (None if couldn't be determined).
    pub exit_code: Option<i32>,
    /// Standard error output (truncated to ~200 chars).
    pub stderr: String,
}

/// Evidence captured from a scenario execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    /// Command arguments that were executed.
    pub argv: Vec<String>,
    /// Seed that was used.
    pub seed: Seed,
    /// Standard output (may be truncated).
    pub stdout: String,
    /// Standard error (may be truncated).
    pub stderr: String,
    /// Exit code (None if killed by signal).
    pub exit_code: Option<i32>,
    /// Whether seed setup commands failed.
    pub setup_failed: bool,
    /// Per-command setup results (only populated on failure).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup_results: Vec<SetupResult>,
    /// Execution infrastructure error (not command error).
    pub execution_error: Option<String>,
    /// Timestamp when evidence was captured.
    pub captured_at_ms: u128,
}

/// Run a scenario and capture evidence.
///
/// The scenario execution follows this order:
/// 1. Create a temporary directory
/// 2. Write seed files
/// 3. Run seed setup commands
/// 4. Run the main command
/// 5. Capture outputs
pub fn run_scenario(
    _pack_path: &Path,
    scenario_id: &str,
    binary: &str,
    argv: &[String],
    seed: &Seed,
) -> Result<Evidence> {
    let captured_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    // Create a temporary working directory
    let temp_dir = tempfile::Builder::new()
        .prefix(&format!("sv_{scenario_id}_"))
        .tempdir()
        .context("create temp directory for scenario")?;

    let work_dir = temp_dir.path();

    // Write seed files
    for file in &seed.files {
        let file_path = work_dir.join(&file.path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create parent dirs for {}", file.path))?;
        }
        fs::write(&file_path, &file.content)
            .with_context(|| format!("write seed file {}", file.path))?;
    }

    // Run seed setup commands, capturing per-command results
    let mut setup_results = Vec::new();
    let mut setup_failed = false;
    let mut failed_cmd_summary = String::new();

    for (index, setup_cmd) in seed.setup.iter().enumerate() {
        if setup_cmd.is_empty() {
            continue;
        }

        // Run setup commands in sandbox to prevent malicious actions
        let mut cmd = build_sandbox_command(work_dir);
        cmd.arg(&setup_cmd[0]);
        cmd.args(&setup_cmd[1..]);
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());

        let output = cmd.output();

        match output {
            Ok(out) => {
                let exit_code = out.status.code();
                let stderr = String::from_utf8_lossy(&out.stderr);
                let stderr_truncated = if stderr.len() > 200 {
                    format!("{}...", &stderr[..200])
                } else {
                    stderr.to_string()
                };

                if !out.status.success() {
                    setup_results.push(SetupResult {
                        index,
                        argv: setup_cmd.clone(),
                        exit_code,
                        stderr: stderr_truncated.clone(),
                    });
                    failed_cmd_summary = format!(
                        "Setup command #{} failed: {:?}\nstderr: {}",
                        index,
                        setup_cmd,
                        stderr_truncated.trim()
                    );
                    setup_failed = true;
                    break;
                }
            }
            Err(e) => {
                setup_results.push(SetupResult {
                    index,
                    argv: setup_cmd.clone(),
                    exit_code: None,
                    stderr: e.to_string(),
                });
                failed_cmd_summary = format!(
                    "Setup command #{} failed to execute: {:?}\nerror: {}",
                    index, setup_cmd, e
                );
                setup_failed = true;
                break;
            }
        }
    }

    // If setup failed, capture what we can and return
    if setup_failed {
        return Ok(Evidence {
            argv: argv.to_vec(),
            seed: seed.clone(),
            stdout: String::new(),
            stderr: failed_cmd_summary,
            exit_code: None,
            setup_failed: true,
            setup_results,
            execution_error: None,
            captured_at_ms,
        });
    }

    // Build the main command with full sandbox isolation
    let mut cmd = build_sandbox_command(work_dir);
    cmd.arg(binary);
    cmd.args(argv);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Execute with timeout
    let output = match execute_with_timeout(&mut cmd, Duration::from_secs(DEFAULT_TIMEOUT_SECS)) {
        Ok(output) => output,
        Err(e) => {
            return Ok(Evidence {
                argv: argv.to_vec(),
                seed: seed.clone(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                setup_failed: false,
                setup_results: Vec::new(),
                execution_error: Some(e.to_string()),
                captured_at_ms,
            });
        }
    };

    // Capture and truncate outputs
    let stdout = truncate_output(&output.stdout);
    let stderr = truncate_output(&output.stderr);
    let exit_code = output.status.code();

    Ok(Evidence {
        argv: argv.to_vec(),
        seed: seed.clone(),
        stdout,
        stderr,
        exit_code,
        setup_failed: false,
        setup_results: Vec::new(),
        execution_error: None,
        captured_at_ms,
    })
}

/// Execute a command with a timeout.
fn execute_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<std::process::Output> {
    let mut child = cmd.spawn().context("spawn command")?;

    // Wait with timeout using a simple poll loop
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Process exited
                let stdout = if let Some(mut out) = child.stdout.take() {
                    use std::io::Read;
                    let mut buf = Vec::new();
                    let _ = out.read_to_end(&mut buf);
                    buf
                } else {
                    Vec::new()
                };
                let stderr = if let Some(mut err) = child.stderr.take() {
                    use std::io::Read;
                    let mut buf = Vec::new();
                    let _ = err.read_to_end(&mut buf);
                    buf
                } else {
                    Vec::new()
                };
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                // Still running
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("command timed out after {:?}", timeout);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(e).context("wait for command");
            }
        }
    }
}

/// Truncate output bytes to a reasonable size and convert to String.
fn truncate_output(bytes: &[u8]) -> String {
    let truncated = if bytes.len() > MAX_OUTPUT_BYTES {
        &bytes[..MAX_OUTPUT_BYTES]
    } else {
        bytes
    };
    String::from_utf8_lossy(truncated).to_string()
}

/// Write evidence to a file in the pack.
pub fn write_evidence(pack_path: &Path, relative_path: &str, evidence: &Evidence) -> Result<()> {
    let full_path = pack_path.join(relative_path);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).context("create evidence directory")?;
    }
    let content = serde_json::to_string_pretty(evidence).context("serialize evidence")?;
    fs::write(&full_path, content)
        .with_context(|| format!("write evidence to {}", full_path.display()))
}

/// Truncate a string to a maximum number of characters.
pub fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        let mut end = max_chars;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

/// Maximum length for output previews stored in Attempt records.
pub const OUTPUT_PREVIEW_MAX_LEN: usize = 200;

/// Create an output preview, returning None if empty.
pub fn make_output_preview(output: &str, max_len: usize) -> Option<String> {
    if output.is_empty() {
        None
    } else {
        Some(truncate_str(output, max_len))
    }
}

/// Sanitize a surface ID for use in filenames.
pub fn sanitize_id(id: &str) -> String {
    // Leading dashes are common in option names but problematic in filenames
    let trimmed = id.trim_start_matches('-');
    trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

use super::types::{DiffKind, Outcome};

/// Compute the outcome by comparing option evidence to control evidence.
///
/// The control evidence is from running the same seed with just context_argv (no option).
/// The option evidence is from running with the full argv including the option.
/// This isolates the effect of the option by keeping everything else constant.
pub fn compute_outcome(option_evidence: &Evidence, control_evidence: &Evidence) -> Outcome {
    // Handle execution errors in the option run
    if let Some(error) = &option_evidence.execution_error {
        return Outcome::ExecutionError {
            error: error.clone(),
        };
    }

    // Handle setup failures in the option run
    if option_evidence.setup_failed {
        return Outcome::SetupFailed {
            hint: truncate_str(&option_evidence.stderr, 200),
        };
    }

    // Compare option evidence to control evidence FIRST
    // This ensures options that intentionally change exit code (like --quiet)
    // are recognized as verified rather than crashed
    let stdout_differs = option_evidence.stdout != control_evidence.stdout;
    let stderr_differs = option_evidence.stderr != control_evidence.stderr;
    let exit_differs = option_evidence.exit_code != control_evidence.exit_code;

    if stdout_differs || stderr_differs || exit_differs {
        let diff_kind = match (stdout_differs, stderr_differs, exit_differs) {
            (true, false, false) => DiffKind::Stdout,
            (false, true, false) => DiffKind::Stderr,
            (false, false, true) => DiffKind::ExitCode,
            _ => DiffKind::Multiple,
        };
        return Outcome::Verified { diff_kind };
    }

    // No difference from control - check if both crashed the same way
    if let Some(exit_code) = option_evidence.exit_code {
        if exit_code != 0 && option_evidence.stdout.is_empty() {
            return Outcome::Crashed {
                hint: format!(
                    "exit={}, stderr: {}",
                    exit_code,
                    truncate_str(&option_evidence.stderr, 150)
                ),
            };
        }
    }

    Outcome::OutputsEqual
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simple_verify::types::FileEntry;

    #[test]
    fn test_run_simple_scenario() {
        let temp_pack = tempfile::tempdir().unwrap();

        let seed = Seed::default();
        let evidence = run_scenario(
            temp_pack.path(),
            "test",
            "echo",
            &["hello".to_string()],
            &seed,
        )
        .unwrap();

        assert_eq!(evidence.stdout.trim(), "hello");
        assert_eq!(evidence.exit_code, Some(0));
        assert!(!evidence.setup_failed);
        assert!(evidence.execution_error.is_none());
    }

    #[test]
    fn test_run_with_seed_files() {
        let temp_pack = tempfile::tempdir().unwrap();

        let seed = Seed {
            setup: vec![],
            files: vec![FileEntry {
                path: "input.txt".to_string(),
                content: "test content".to_string(),
            }],
        };
        let evidence = run_scenario(
            temp_pack.path(),
            "test",
            "cat",
            &["input.txt".to_string()],
            &seed,
        )
        .unwrap();

        assert_eq!(evidence.stdout.trim(), "test content");
        assert_eq!(evidence.exit_code, Some(0));
    }

    #[test]
    fn test_run_with_setup_commands() {
        let temp_pack = tempfile::tempdir().unwrap();

        let seed = Seed {
            setup: vec![vec![
                "mkdir".to_string(),
                "-p".to_string(),
                "subdir".to_string(),
            ]],
            files: vec![FileEntry {
                path: "subdir/file.txt".to_string(),
                content: "nested content".to_string(),
            }],
        };
        let evidence = run_scenario(
            temp_pack.path(),
            "test",
            "cat",
            &["subdir/file.txt".to_string()],
            &seed,
        )
        .unwrap();

        assert_eq!(evidence.stdout.trim(), "nested content");
    }

    #[test]
    fn test_evidence_roundtrip() {
        let temp_pack = tempfile::tempdir().unwrap();

        let evidence = Evidence {
            argv: vec!["--test".to_string()],
            seed: Seed::default(),
            stdout: "output".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            setup_results: Vec::new(),
            execution_error: None,
            captured_at_ms: 12345,
        };

        write_evidence(temp_pack.path(), "evidence/test.json", &evidence).unwrap();

        // Load and verify
        let full_path = temp_pack.path().join("evidence/test.json");
        let content = std::fs::read_to_string(&full_path).unwrap();
        let loaded: Evidence = serde_json::from_str(&content).unwrap();

        assert_eq!(loaded.argv, evidence.argv);
        assert_eq!(loaded.stdout, evidence.stdout);
        assert_eq!(loaded.exit_code, evidence.exit_code);
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 5), "hello...");
    }

    #[test]
    fn test_sanitize_id() {
        assert_eq!(sanitize_id("--verbose"), "verbose");
        assert_eq!(sanitize_id("-v"), "v");
        assert_eq!(sanitize_id("--color=always"), "color_always");
        assert_eq!(sanitize_id("normal-id"), "normal-id");
    }

    #[test]
    fn test_make_output_preview() {
        assert_eq!(make_output_preview("", 100), None);
        assert_eq!(make_output_preview("hello", 100), Some("hello".to_string()));
        assert_eq!(
            make_output_preview("hello world", 5),
            Some("hello...".to_string())
        );
    }

    #[test]
    fn test_compute_outcome_outputs_equal() {
        let control = Evidence {
            argv: vec!["test".to_string()],
            seed: Seed::default(),
            stdout: "output".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            setup_results: Vec::new(),
            execution_error: None,
            captured_at_ms: 0,
        };

        let option = Evidence {
            argv: vec!["--opt".to_string(), "test".to_string()],
            seed: Seed::default(),
            stdout: "output".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            setup_results: Vec::new(),
            execution_error: None,
            captured_at_ms: 0,
        };

        let outcome = compute_outcome(&option, &control);
        assert!(matches!(outcome, Outcome::OutputsEqual));
    }

    #[test]
    fn test_compute_outcome_stdout_differs() {
        let control = Evidence {
            argv: vec!["test".to_string()],
            seed: Seed::default(),
            stdout: "original".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            setup_results: Vec::new(),
            execution_error: None,
            captured_at_ms: 0,
        };

        let option = Evidence {
            argv: vec!["--opt".to_string(), "test".to_string()],
            seed: Seed::default(),
            stdout: "different".to_string(),
            stderr: "".to_string(),
            exit_code: Some(0),
            setup_failed: false,
            setup_results: Vec::new(),
            execution_error: None,
            captured_at_ms: 0,
        };

        let outcome = compute_outcome(&option, &control);
        match outcome {
            Outcome::Verified { diff_kind } => {
                assert!(matches!(diff_kind, DiffKind::Stdout));
            }
            _ => panic!("Expected Verified with Stdout diff"),
        }
    }
}
