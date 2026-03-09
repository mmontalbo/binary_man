//! Scenario execution and evidence capture.
//!
//! This module handles running commands and capturing their outputs. We use
//! a simple subprocess model rather than the full binary_lens infrastructure,
//! trading some isolation guarantees for simplicity.

use super::types::Seed;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Default timeout for scenario execution in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Maximum bytes to capture for stdout/stderr.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

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

    // Run seed setup commands
    let mut setup_failed = false;
    for setup_cmd in &seed.setup {
        if setup_cmd.is_empty() {
            continue;
        }

        let status = Command::new(&setup_cmd[0])
            .args(&setup_cmd[1..])
            .current_dir(work_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status();

        match status {
            Ok(s) if !s.success() => {
                setup_failed = true;
                break;
            }
            Err(_) => {
                setup_failed = true;
                break;
            }
            _ => {}
        }
    }

    // If setup failed, capture what we can and return
    if setup_failed {
        return Ok(Evidence {
            argv: argv.to_vec(),
            seed: seed.clone(),
            stdout: String::new(),
            stderr: "Setup commands failed".to_string(),
            exit_code: None,
            setup_failed: true,
            execution_error: None,
            captured_at_ms,
        });
    }

    // Build the main command
    let mut cmd = Command::new(binary);
    cmd.args(argv);
    cmd.current_dir(work_dir);
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

/// Load evidence from a file in the pack.
pub fn load_evidence(pack_path: &Path, relative_path: &str) -> Result<Evidence> {
    let full_path = pack_path.join(relative_path);
    let content = fs::read_to_string(&full_path)
        .with_context(|| format!("read evidence from {}", full_path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("parse evidence from {}", full_path.display()))
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
            execution_error: None,
            captured_at_ms: 12345,
        };

        write_evidence(temp_pack.path(), "evidence/test.json", &evidence).unwrap();
        let loaded = load_evidence(temp_pack.path(), "evidence/test.json").unwrap();

        assert_eq!(loaded.argv, evidence.argv);
        assert_eq!(loaded.stdout, evidence.stdout);
        assert_eq!(loaded.exit_code, evidence.exit_code);
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 5), "hello...");
    }
}
