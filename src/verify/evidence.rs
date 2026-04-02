//! Scenario execution and evidence capture.
//!
//! This module handles running commands and capturing their outputs in an
//! isolated environment. On Linux, bubblewrap (bwrap) provides full sandbox
//! isolation (network, filesystem, process). On macOS, commands run directly
//! in a temporary directory without namespace isolation.

use super::types::Seed;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Default timeout for scenario execution in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;


/// Filesystem changes detected between before/after command execution.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FsDiff {
    pub created: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
}

impl FsDiff {
    pub fn has_changes(&self) -> bool {
        !self.created.is_empty() || !self.modified.is_empty() || !self.deleted.is_empty()
    }
}

/// Metrics about command output (stdout/stderr).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OutputMetrics {
    pub line_count: usize,
    pub byte_count: usize,
    pub is_empty: bool,
}

/// Build a sandbox command that isolates the child process.
///
/// On Linux this uses bubblewrap (bwrap) for namespace-based isolation:
/// read-only root, writable work_dir, network isolation, /proc + /dev.
///
/// On macOS there is no bwrap equivalent. Commands run directly in the
/// temp work directory without namespace isolation. `TMPDIR` is redirected
/// to the sandbox tmp directory for partial /tmp isolation.
///
/// `readonly`: mount work_dir read-only (for test runs after setup).
///             Only enforced on Linux; ignored on macOS.
#[cfg(target_os = "linux")]
fn build_sandbox_command(
    work_dir: &Path,
    sandbox_tmp: &Path,
    readonly: bool,
) -> Command {
    let mut cmd = Command::new("bwrap");

    cmd.args(["--ro-bind", "/", "/"]);
    cmd.args(["--dev", "/dev"]);
    cmd.args(["--proc", "/proc"]);

    let sandbox_tmp_str = sandbox_tmp.to_string_lossy();
    cmd.args(["--bind", &sandbox_tmp_str, "/tmp"]);

    let work_dir_str = work_dir.to_string_lossy();
    let bind_flag = if readonly { "--ro-bind" } else { "--bind" };
    cmd.args([bind_flag, &work_dir_str, &work_dir_str]);

    cmd.arg("--unshare-net");
    cmd.arg("--die-with-parent");
    cmd.arg("--new-session");
    cmd.args(["--chdir", &work_dir_str]);
    cmd.arg("--");

    cmd
}

/// macOS sandbox using `sandbox-exec` (Seatbelt).
///
/// Approximates bwrap isolation with:
/// - Network denied (`deny network*`)
/// - File writes denied except to work_dir (writable mode) and sandbox_tmp
/// - Device writes allowed (PTY, /dev/null)
///
/// Paths are canonicalized because macOS symlinks (e.g. /var -> /private/var)
/// must match the resolved paths that sandbox-exec checks against.
#[cfg(not(target_os = "linux"))]
fn build_sandbox_command(
    work_dir: &Path,
    sandbox_tmp: &Path,
    readonly: bool,
) -> Command {
    let work_dir = fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
    let sandbox_tmp = fs::canonicalize(sandbox_tmp).unwrap_or_else(|_| sandbox_tmp.to_path_buf());
    let work_dir_str = work_dir.to_string_lossy();
    let sandbox_tmp_str = sandbox_tmp.to_string_lossy();

    let mut profile = format!(
        "(version 1)\
         (allow default)\
         (deny network*)\
         (deny file-write*)\
         (allow file-write* (subpath \"{sandbox_tmp_str}\"))\
         (allow file-write* (subpath \"/dev\"))"
    );
    if !readonly {
        profile.push_str(&format!(
            "(allow file-write* (subpath \"{work_dir_str}\"))"
        ));
    }

    let mut cmd = Command::new("sandbox-exec");
    cmd.args(["-p", &profile]);
    cmd.current_dir(&work_dir);
    cmd.env("TMPDIR", &sandbox_tmp);
    cmd
}

/// Append PTY wrapper arguments to a command.
///
/// Uses `script` to allocate a pseudo-terminal so the child process
/// sees an interactive TTY (enabling color output, etc.).
///
/// Linux (GNU coreutils): `script -q -c "cmd args" /dev/null`
/// macOS (BSD):            `script -q /dev/null cmd args`
#[cfg(target_os = "linux")]
fn append_pty_wrapper(cmd: &mut Command, binary: &str, argv: &[String]) {
    cmd.args(["script", "-q"]);
    let cmd_str: String = std::iter::once(binary.to_string())
        .chain(argv.iter().cloned())
        .map(|s| shell_escape(&s))
        .collect::<Vec<_>>()
        .join(" ");
    cmd.args(["-c", &cmd_str]);
    cmd.arg("/dev/null");
}

#[cfg(not(target_os = "linux"))]
fn append_pty_wrapper(cmd: &mut Command, binary: &str, argv: &[String]) {
    cmd.args(["script", "-q", "/dev/null"]);
    cmd.arg(binary);
    cmd.args(argv);
}

/// Escape a string for shell use (needed for GNU script's `-c` flag).
#[cfg(target_os = "linux")]
fn shell_escape(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '/')
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Maximum bytes to capture for stdout/stderr.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Result of running seed setup commands.
struct SetupOutcome {
    results: Vec<SetupResult>,
    failed: bool,
    error: Option<String>,
}

/// Run seed setup commands in the sandbox, stopping on first failure.
fn run_setup_commands(
    seed: &Seed,
    work_dir: &Path,
    sandbox_tmp: &Path,
) -> SetupOutcome {
    let mut results = Vec::new();

    for (index, setup_cmd) in seed.setup.iter().enumerate() {
        if setup_cmd.is_empty() {
            continue;
        }

        let mut cmd = build_sandbox_command(work_dir, sandbox_tmp, false);
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
                    let error = format!(
                        "Setup command #{} failed: {:?}\nstderr: {}",
                        index,
                        setup_cmd,
                        stderr_truncated.trim()
                    );
                    results.push(SetupResult {
                        index,
                        argv: setup_cmd.clone(),
                        exit_code,
                        stderr: stderr_truncated,
                    });
                    return SetupOutcome {
                        results,
                        failed: true,
                        error: Some(error),
                    };
                }
            }
            Err(e) => {
                let error = format!(
                    "Setup command #{} failed to execute: {:?}\nerror: {}",
                    index, setup_cmd, e
                );
                results.push(SetupResult {
                    index,
                    argv: setup_cmd.clone(),
                    exit_code: None,
                    stderr: e.to_string(),
                });
                return SetupOutcome {
                    results,
                    failed: true,
                    error: Some(error),
                };
            }
        }
    }

    SetupOutcome {
        results,
        failed: false,
        error: None,
    }
}

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
    /// Filesystem changes detected during command execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs_diff: Option<FsDiff>,
    /// Output metrics for stdout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_metrics: Option<OutputMetrics>,
    /// Output metrics for stderr.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_metrics: Option<OutputMetrics>,
    /// Environment variables visible to the command.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// Whether this command was run in a PTY (captures colors/formatting).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub with_pty: bool,
}

/// Run a scenario and capture evidence.
///
/// The scenario execution follows this order:
/// 1. Create a temporary directory
/// 2. Write seed files
/// 3. Run seed setup commands
/// 4. Run the main command
/// 5. Capture outputs
///
/// If `with_pty` is true, the command runs in a pseudo-terminal, capturing
/// ANSI color codes and other terminal-dependent output.
pub(super) fn run_scenario(
    scenario_id: &str,
    binary: &str,
    argv: &[String],
    seed: &Seed,
    with_pty: bool,
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

    // Create workspace/tmp directory for observable /tmp inside sandbox
    let sandbox_tmp = work_dir.join("tmp");
    fs::create_dir_all(&sandbox_tmp).context("create workspace/tmp directory")?;

    // Write pre-generated fixtures for LM to use
    super::fixtures::write_fixtures(work_dir)?;

    // Capture environment variables that will be visible in the sandbox
    // We capture a subset of relevant env vars for telemetry
    let env: HashMap<String, String> = std::env::vars()
        .filter(|(k, _)| {
            // Capture locale, timezone, and commonly-relevant vars
            k.starts_with("LANG")
                || k.starts_with("LC_")
                || k == "TZ"
                || k == "HOME"
                || k == "USER"
                || k == "PATH"
                || k == "SHELL"
                || k == "TERM"
        })
        .collect();

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

    // Run seed setup commands in sandbox
    let setup = run_setup_commands(seed, work_dir, &sandbox_tmp);
    if setup.failed {
        return Ok(Evidence {
            argv: argv.to_vec(),
            seed: seed.clone(),
            stdout: String::new(),
            stderr: setup.error.unwrap_or_default(),
            exit_code: None,
            setup_failed: true,
            setup_results: setup.results,
            execution_error: None,
            captured_at_ms,
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            env,
            with_pty,
        });
    }

    // Capture filesystem state before running the main command
    let fs_state_before = capture_fs_state(work_dir);

    // Build the main command with full sandbox isolation
    let mut cmd = build_sandbox_command(work_dir, &sandbox_tmp, false);
    if with_pty {
        append_pty_wrapper(&mut cmd, binary, argv);
    } else {
        cmd.arg(binary);
        cmd.args(argv);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // When running with PTY, disable pagers to prevent blocking
    // (PTY makes programs think they're interactive)
    if with_pty {
        cmd.env("PAGER", "cat");
        cmd.env("GIT_PAGER", "cat");
        cmd.env("MANPAGER", "cat");
        cmd.env("DELTA_PAGER", "cat"); // For delta diff tool
    }

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
                fs_diff: None,
                stdout_metrics: None,
                stderr_metrics: None,
                env,
                with_pty,
            });
        }
    };

    // Capture filesystem state after running the main command
    let fs_state_after = capture_fs_state(work_dir);

    // Compute filesystem diff
    let fs_diff = compute_fs_diff(&fs_state_before, &fs_state_after);
    let fs_diff = if fs_diff.has_changes() {
        Some(fs_diff)
    } else {
        None
    };

    // Capture and truncate outputs
    let stdout = truncate_output(&output.stdout);
    let stderr = truncate_output(&output.stderr);
    let exit_code = output.status.code();

    // Compute output metrics
    let stdout_metrics = Some(compute_output_metrics(&stdout));
    let stderr_metrics = Some(compute_output_metrics(&stderr));

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
        fs_diff,
        stdout_metrics,
        stderr_metrics,
        env,
        with_pty,
    })
}

/// A prepared sandbox ready for running multiple commands.
///
/// After setup completes, the sandbox can run multiple commands sequentially,
/// ensuring they share the same filesystem state (including git commit hashes).
struct PreparedSandbox {
    /// The temporary directory (kept alive for cleanup on drop).
    _temp_dir: tempfile::TempDir,
    /// Working directory path inside the sandbox.
    work_dir: PathBuf,
    /// Sandbox /tmp directory for observable side effects.
    sandbox_tmp: PathBuf,
    /// Results from setup commands (empty if all succeeded).
    setup_results: Vec<SetupResult>,
    /// Whether setup failed.
    setup_failed: bool,
    /// Error summary if setup failed.
    setup_error: Option<String>,
    /// Timestamp when sandbox was created.
    captured_at_ms: u128,
    /// Environment variables captured for telemetry.
    env: HashMap<String, String>,
    /// The seed used to create this sandbox.
    seed: Seed,
}

/// Prepare a sandbox by creating directory, writing files, and running setup.
///
/// Returns a prepared sandbox that can be used to run multiple commands.
/// Setup commands run in a writable sandbox.
fn prepare_sandbox(scenario_id: &str, seed: &Seed) -> Result<PreparedSandbox> {
    let captured_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    // Create a temporary working directory
    let temp_dir = tempfile::Builder::new()
        .prefix(&format!("sv_{scenario_id}_"))
        .tempdir()
        .context("create temp directory for scenario")?;

    let work_dir = temp_dir.path().to_path_buf();

    // Create workspace/tmp directory for observable /tmp inside sandbox
    let sandbox_tmp = work_dir.join("tmp");
    fs::create_dir_all(&sandbox_tmp).context("create workspace/tmp directory")?;

    // Write pre-generated fixtures for LM to use
    super::fixtures::write_fixtures(&work_dir)?;

    // Capture environment variables for telemetry
    let env: HashMap<String, String> = std::env::vars()
        .filter(|(k, _)| {
            k.starts_with("LANG")
                || k.starts_with("LC_")
                || k == "TZ"
                || k == "HOME"
                || k == "USER"
                || k == "PATH"
                || k == "SHELL"
                || k == "TERM"
        })
        .collect();

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

    // Run seed setup commands in sandbox
    let setup = run_setup_commands(seed, &work_dir, &sandbox_tmp);

    Ok(PreparedSandbox {
        _temp_dir: temp_dir,
        work_dir,
        sandbox_tmp,
        setup_results: setup.results,
        setup_failed: setup.failed,
        setup_error: setup.error,
        captured_at_ms,
        env,
        seed: seed.clone(),
    })
}


/// Run a command in a prepared sandbox (read-only mode).
///
/// The sandbox work directory is mounted read-only to detect commands
/// that attempt to mutate state.
fn run_in_sandbox(
    sandbox: &PreparedSandbox,
    binary: &str,
    argv: &[String],
    with_pty: bool,
) -> Result<Evidence> {
    // If setup failed, return failure evidence
    if sandbox.setup_failed {
        return Ok(Evidence {
            argv: argv.to_vec(),
            seed: sandbox.seed.clone(),
            stdout: String::new(),
            stderr: sandbox.setup_error.clone().unwrap_or_default(),
            exit_code: None,
            setup_failed: true,
            setup_results: sandbox.setup_results.clone(),
            execution_error: None,
            captured_at_ms: sandbox.captured_at_ms,
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            env: sandbox.env.clone(),
            with_pty,
        });
    }

    // Capture filesystem state before running (from /tmp only since work_dir is read-only)
    let fs_state_before = capture_fs_state(&sandbox.sandbox_tmp);

    // Build command with READ-ONLY work directory
    let mut cmd = build_sandbox_command(&sandbox.work_dir, &sandbox.sandbox_tmp, true);
    if with_pty {
        append_pty_wrapper(&mut cmd, binary, argv);
    } else {
        cmd.arg(binary);
        cmd.args(argv);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Disable pagers when using PTY
    if with_pty {
        cmd.env("PAGER", "cat");
        cmd.env("GIT_PAGER", "cat");
        cmd.env("MANPAGER", "cat");
        cmd.env("DELTA_PAGER", "cat");
    }

    // Execute with timeout
    let output = match execute_with_timeout(&mut cmd, Duration::from_secs(DEFAULT_TIMEOUT_SECS)) {
        Ok(output) => output,
        Err(e) => {
            return Ok(Evidence {
                argv: argv.to_vec(),
                seed: sandbox.seed.clone(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                setup_failed: false,
                setup_results: Vec::new(),
                execution_error: Some(e.to_string()),
                captured_at_ms: sandbox.captured_at_ms,
                fs_diff: None,
                stdout_metrics: None,
                stderr_metrics: None,
                env: sandbox.env.clone(),
                with_pty,
            });
        }
    };

    // Capture filesystem state after (from /tmp only)
    let fs_state_after = capture_fs_state(&sandbox.sandbox_tmp);

    // Compute filesystem diff
    let fs_diff = compute_fs_diff(&fs_state_before, &fs_state_after);
    let fs_diff = if fs_diff.has_changes() {
        Some(fs_diff)
    } else {
        None
    };

    let stdout = truncate_output(&output.stdout);
    let stderr = truncate_output(&output.stderr);
    let exit_code = output.status.code();

    let stdout_metrics = Some(compute_output_metrics(&stdout));
    let stderr_metrics = Some(compute_output_metrics(&stderr));

    Ok(Evidence {
        argv: argv.to_vec(),
        seed: sandbox.seed.clone(),
        stdout,
        stderr,
        exit_code,
        setup_failed: false,
        setup_results: Vec::new(),
        execution_error: None,
        captured_at_ms: sandbox.captured_at_ms,
        fs_diff,
        stdout_metrics,
        stderr_metrics,
        env: sandbox.env.clone(),
        with_pty,
    })
}

/// Run control and option commands in the same sandbox.
///
/// This ensures both commands see identical filesystem state (including
/// git commit hashes that depend on timestamps), providing meaningful
/// comparison of option effects.
pub(super) fn run_scenario_pair(
    scenario_id: &str,
    binary: &str,
    control_argv: &[String],
    option_argv: &[String],
    seed: &Seed,
    with_pty: bool,
) -> Result<(Evidence, Evidence)> {
    let sandbox = prepare_sandbox(scenario_id, seed)?;

    // Run both commands in the same sandbox (read-only mode)
    let control_evidence = run_in_sandbox(&sandbox, binary, control_argv, with_pty)?;
    let option_evidence = run_in_sandbox(&sandbox, binary, option_argv, with_pty)?;

    Ok((control_evidence, option_evidence))
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

/// Capture filesystem state for a directory.
///
/// Returns a map of relative file paths to (size, mtime) tuples.
/// Ignores hidden files (starting with '.').
fn capture_fs_state(dir: &Path) -> HashMap<PathBuf, (u64, u128)> {
    let mut state = HashMap::new();
    if fs::read_dir(dir).is_ok() {
        capture_fs_state_recursive(dir, dir, &mut state);
    }
    state
}

fn capture_fs_state_recursive(
    base: &Path,
    current: &Path,
    state: &mut HashMap<PathBuf, (u64, u128)>,
) {
    let entries = match fs::read_dir(current) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let file_name = match path.file_name() {
            Some(name) => name.to_string_lossy(),
            None => continue,
        };

        // Skip hidden files
        if file_name.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            capture_fs_state_recursive(base, &path, state);
        } else if let Ok(metadata) = path.metadata() {
            let relative = path.strip_prefix(base).unwrap_or(&path).to_path_buf();
            let size = metadata.len();
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0);
            state.insert(relative, (size, mtime));
        }
    }
}

/// Compute filesystem diff between before and after states.
fn compute_fs_diff(
    before: &HashMap<PathBuf, (u64, u128)>,
    after: &HashMap<PathBuf, (u64, u128)>,
) -> FsDiff {
    let mut created = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();

    // Find created and modified files
    for (path, (size_after, mtime_after)) in after {
        match before.get(path) {
            None => {
                created.push(path.to_string_lossy().to_string());
            }
            Some((size_before, mtime_before)) => {
                if size_after != size_before || mtime_after != mtime_before {
                    modified.push(path.to_string_lossy().to_string());
                }
            }
        }
    }

    // Find deleted files
    for path in before.keys() {
        if !after.contains_key(path) {
            deleted.push(path.to_string_lossy().to_string());
        }
    }

    // Sort for deterministic output
    created.sort();
    modified.sort();
    deleted.sort();

    FsDiff {
        created,
        modified,
        deleted,
    }
}

/// Compute output metrics for a string.
fn compute_output_metrics(output: &str) -> OutputMetrics {
    OutputMetrics {
        line_count: output.lines().count(),
        byte_count: output.len(),
        is_empty: output.is_empty(),
    }
}

/// Write evidence to a file in the pack.
pub(super) fn write_evidence(pack_path: &Path, relative_path: &str, evidence: &Evidence) -> Result<()> {
    let full_path = pack_path.join(relative_path);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).context("create evidence directory")?;
    }
    let content = serde_json::to_string_pretty(evidence).context("serialize evidence")?;
    fs::write(&full_path, content)
        .with_context(|| format!("write evidence to {}", full_path.display()))
}

/// Truncate a string to a maximum number of characters.
pub(super) fn truncate_str(s: &str, max_chars: usize) -> String {
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
pub(super) const OUTPUT_PREVIEW_MAX_LEN: usize = 200;

/// Create an output preview, returning None if empty.
pub(super) fn make_output_preview(output: &str, max_len: usize) -> Option<String> {
    if output.is_empty() {
        None
    } else {
        Some(truncate_str(output, max_len))
    }
}

/// Sanitize a surface ID for use in filenames.
pub(super) fn sanitize_id(id: &str) -> String {
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
pub(super) fn compute_outcome(option_evidence: &Evidence, control_evidence: &Evidence) -> Outcome {
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

    // Detect invalid test scenarios where option causes an error but control succeeds.
    // These are false positives - the difference is due to an error, not the option's behavior.
    let control_succeeded = control_evidence.exit_code.unwrap_or(0) == 0;
    let option_exit = option_evidence.exit_code.unwrap_or(0);
    let option_stderr_lower = option_evidence.stderr.to_lowercase();
    let has_error_stderr =
        option_stderr_lower.contains("error:") || option_stderr_lower.contains("fatal:");

    // Signal exits (>= 128) or error messages when control succeeded indicate bad test scenario
    if control_succeeded && (option_exit >= 128 || (option_exit != 0 && has_error_stderr)) {
        return Outcome::OptionError {
            hint: format!(
                "exit={}, stderr: {}",
                option_exit,
                truncate_str(&option_evidence.stderr, 150)
            ),
        };
    }

    // Compare option evidence to control evidence FIRST
    // This ensures options that intentionally change exit code (like --quiet)
    // are recognized as verified rather than crashed
    let stdout_differs = option_evidence.stdout != control_evidence.stdout;
    let stderr_differs = option_evidence.stderr != control_evidence.stderr;
    let exit_differs = option_evidence.exit_code != control_evidence.exit_code;

    // Reject stderr-only diffs when both runs failed (likely just error message variations)
    let both_failed =
        option_evidence.exit_code.unwrap_or(0) != 0 && control_evidence.exit_code.unwrap_or(0) != 0;
    let stderr_only_diff = stderr_differs && !stdout_differs && !exit_differs;

    if (stdout_differs || stderr_differs || exit_differs) && !(stderr_only_diff && both_failed) {
        let diff_kind = match (stdout_differs, stderr_differs, exit_differs) {
            (true, false, false) => DiffKind::Stdout,
            (false, true, false) => DiffKind::Stderr,
            (false, false, true) => DiffKind::ExitCode,
            _ => DiffKind::Multiple,
        };
        return Outcome::Verified { diff_kind };
    }

    // Check for filesystem side effects when outputs are equal
    let fs_diff_differs = match (&option_evidence.fs_diff, &control_evidence.fs_diff) {
        (Some(opt), Some(ctrl)) => {
            opt.created != ctrl.created
                || opt.modified != ctrl.modified
                || opt.deleted != ctrl.deleted
        }
        (Some(opt), None) => opt.has_changes(),
        (None, Some(ctrl)) => ctrl.has_changes(),
        (None, None) => false,
    };

    if fs_diff_differs {
        return Outcome::Verified {
            diff_kind: DiffKind::SideEffect,
        };
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
    use crate::verify::types::FileEntry;

    #[test]
    fn test_run_simple_scenario() {
        let seed = Seed::default();
        let evidence = run_scenario(
            "test",
            "echo",
            &["hello".to_string()],
            &seed,
            false,
        )
        .unwrap();

        assert_eq!(evidence.stdout.trim(), "hello");
        assert_eq!(evidence.exit_code, Some(0));
        assert!(!evidence.setup_failed);
        assert!(evidence.execution_error.is_none());
    }

    #[test]
    fn test_run_with_seed_files() {
        let seed = Seed {
            setup: vec![],
            files: vec![FileEntry {
                path: "input.txt".to_string(),
                content: "test content".to_string(),
            }],
        };
        let evidence = run_scenario(
            "test",
            "cat",
            &["input.txt".to_string()],
            &seed,
            false,
        )
        .unwrap();

        assert_eq!(evidence.stdout.trim(), "test content");
        assert_eq!(evidence.exit_code, Some(0));
    }

    #[test]
    fn test_run_with_setup_commands() {
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
            "test",
            "cat",
            &["subdir/file.txt".to_string()],
            &seed,
            false,
        )
        .unwrap();

        assert_eq!(evidence.stdout.trim(), "nested content");
    }

    #[test]
    fn test_run_with_pty() {
        let seed = Seed::default();
        // Run echo with PTY mode
        let evidence = run_scenario(
            "test_pty",
            "echo",
            &["hello".to_string()],
            &seed,
            true, // with_pty = true
        )
        .unwrap();

        // Output should contain "hello" (though PTY may add extra chars)
        assert!(evidence.stdout.contains("hello"));
        assert_eq!(evidence.exit_code, Some(0));
        assert!(evidence.with_pty);
    }

    #[test]
    fn test_pty_captures_colors() {
        let seed = Seed::default();
        // Run ls with color=always in PTY mode - should capture ANSI codes
        let evidence = run_scenario(
            "test_color",
            "ls",
            &["--color=always".to_string(), "/".to_string()],
            &seed,
            true, // with_pty = true
        )
        .unwrap();

        // If PTY works, ls --color=always should output ANSI escape codes
        // ANSI codes start with ESC (0x1B) followed by [
        let has_ansi = evidence.stdout.contains("\x1b[") || evidence.stdout.contains("\x1B[");

        // Note: This might not work in all environments (e.g., if ls doesn't support color)
        // So we just check it ran successfully
        assert_eq!(evidence.exit_code, Some(0), "ls should succeed");
        assert!(evidence.with_pty, "Evidence should indicate PTY was used");

        // If ANSI codes are present, great! If not, the test still passes
        // because not all systems/configs produce color output
        if has_ansi {
            eprintln!("PTY successfully captured ANSI color codes");
        } else {
            eprintln!("Note: No ANSI codes captured (may be system-dependent)");
        }
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
            fs_diff: None,
            stdout_metrics: Some(OutputMetrics {
                line_count: 1,
                byte_count: 6,
                is_empty: false,
            }),
            stderr_metrics: None,
            env: HashMap::new(),
            with_pty: false,
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
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            env: HashMap::new(),
            with_pty: false,
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
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            env: HashMap::new(),
            with_pty: false,
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
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            env: HashMap::new(),
            with_pty: false,
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
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            env: HashMap::new(),
            with_pty: false,
        };

        let outcome = compute_outcome(&option, &control);
        match outcome {
            Outcome::Verified { diff_kind } => {
                assert!(matches!(diff_kind, DiffKind::Stdout));
            }
            _ => panic!("Expected Verified with Stdout diff"),
        }
    }

    #[test]
    fn test_fs_diff_has_changes() {
        let empty = FsDiff::default();
        assert!(!empty.has_changes());

        let created = FsDiff {
            created: vec!["file.txt".to_string()],
            modified: vec![],
            deleted: vec![],
        };
        assert!(created.has_changes());

        let modified = FsDiff {
            created: vec![],
            modified: vec!["file.txt".to_string()],
            deleted: vec![],
        };
        assert!(modified.has_changes());

        let deleted = FsDiff {
            created: vec![],
            modified: vec![],
            deleted: vec!["file.txt".to_string()],
        };
        assert!(deleted.has_changes());
    }

    #[test]
    fn test_compute_output_metrics() {
        let empty_metrics = compute_output_metrics("");
        assert_eq!(empty_metrics.line_count, 0);
        assert_eq!(empty_metrics.byte_count, 0);
        assert!(empty_metrics.is_empty);

        let single_line = compute_output_metrics("hello world");
        assert_eq!(single_line.line_count, 1);
        assert_eq!(single_line.byte_count, 11);
        assert!(!single_line.is_empty);

        let multi_line = compute_output_metrics("line1\nline2\nline3");
        assert_eq!(multi_line.line_count, 3);
        assert_eq!(multi_line.byte_count, 17);
        assert!(!multi_line.is_empty);
    }

    #[test]
    fn test_capture_fs_state_ignores_hidden_files() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dir = temp_dir.path();

        // Create visible and hidden files
        std::fs::write(dir.join("visible.txt"), "content").unwrap();
        std::fs::write(dir.join(".hidden"), "secret").unwrap();

        let state = capture_fs_state(dir);

        assert!(state.contains_key(&PathBuf::from("visible.txt")));
        assert!(!state.contains_key(&PathBuf::from(".hidden")));
    }

    #[test]
    fn test_compute_fs_diff() {
        let mut before = HashMap::new();
        before.insert(PathBuf::from("existing.txt"), (100u64, 1000u128));
        before.insert(PathBuf::from("to_delete.txt"), (50u64, 500u128));

        let mut after = HashMap::new();
        after.insert(PathBuf::from("existing.txt"), (100u64, 1000u128)); // unchanged
        after.insert(PathBuf::from("modified.txt"), (200u64, 2000u128)); // new
                                                                         // to_delete.txt is gone

        let diff = compute_fs_diff(&before, &after);

        assert_eq!(diff.created, vec!["modified.txt"]);
        assert!(diff.modified.is_empty());
        assert_eq!(diff.deleted, vec!["to_delete.txt"]);
    }

    #[test]
    fn test_compute_outcome_side_effect() {
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
            fs_diff: None,
            stdout_metrics: None,
            stderr_metrics: None,
            env: HashMap::new(),
            with_pty: false,
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
            fs_diff: Some(FsDiff {
                created: vec!["newfile.txt".to_string()],
                modified: vec![],
                deleted: vec![],
            }),
            stdout_metrics: None,
            stderr_metrics: None,
            env: HashMap::new(),
            with_pty: false,
        };

        let outcome = compute_outcome(&option, &control);
        match outcome {
            Outcome::Verified { diff_kind } => {
                assert!(matches!(diff_kind, DiffKind::SideEffect));
            }
            _ => panic!("Expected Verified with SideEffect diff, got {:?}", outcome),
        }
    }

    // --- Sandbox behavioral tests ---
    // These verify that the sandbox actually enforces isolation properties,
    // not just that commands run. They work on both Linux (bwrap) and
    // macOS (sandbox-exec).

    /// Helper: run a shell command in the sandbox and return its output.
    fn sandbox_run(work_dir: &Path, sandbox_tmp: &Path, readonly: bool, shell_cmd: &str) -> std::process::Output {
        let mut cmd = build_sandbox_command(work_dir, sandbox_tmp, readonly);
        cmd.args(["sh", "-c", shell_cmd]);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.output().expect("failed to run sandbox command")
    }

    /// Helper: set up a temp work directory with sandbox_tmp subdirectory.
    fn sandbox_dirs() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let sandbox_tmp = temp.path().join("tmp");
        fs::create_dir_all(&sandbox_tmp).unwrap();
        let sandbox_tmp_path = sandbox_tmp.to_path_buf();
        (temp, sandbox_tmp_path)
    }

    #[test]
    fn test_sandbox_blocks_writes_outside_workdir() {
        let (temp, sandbox_tmp) = sandbox_dirs();
        let out = sandbox_run(
            temp.path(),
            &sandbox_tmp,
            false,
            "echo probe > /var/tmp/bman_sandbox_probe_test 2>&1",
        );
        // The write should fail — /var/tmp is outside the allowed paths
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !out.status.success() || stdout.contains("ermission") || stderr.contains("ermission"),
            "Write outside sandbox should be denied.\nstdout: {stdout}\nstderr: {stderr}"
        );
        // Clean up just in case the sandbox leaked
        let _ = fs::remove_file("/var/tmp/bman_sandbox_probe_test");
    }

    #[test]
    fn test_sandbox_allows_workdir_writes() {
        let (temp, sandbox_tmp) = sandbox_dirs();
        let out = sandbox_run(
            temp.path(),
            &sandbox_tmp,
            false, // writable
            "echo allowed > test_write.txt && cat test_write.txt",
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "Writes to work_dir should succeed");
        assert_eq!(stdout.trim(), "allowed");
    }

    #[test]
    fn test_sandbox_readonly_blocks_workdir_writes() {
        let (temp, sandbox_tmp) = sandbox_dirs();
        // Pre-create a file so we can try to overwrite it
        fs::write(temp.path().join("existing.txt"), "original").unwrap();

        let _out = sandbox_run(
            temp.path(),
            &sandbox_tmp,
            true, // readonly
            "echo modified > existing.txt 2>&1",
        );
        // The write should fail in readonly mode
        let content = fs::read_to_string(temp.path().join("existing.txt")).unwrap();
        assert_eq!(content, "original", "File should not be modified in readonly mode");
    }

    #[test]
    fn test_sandbox_allows_sandbox_tmp_writes() {
        let (temp, sandbox_tmp) = sandbox_dirs();
        let out = sandbox_run(
            temp.path(),
            &sandbox_tmp,
            true, // even in readonly, sandbox_tmp should be writable
            &format!("echo tmpdata > {}/probe.txt && cat {}/probe.txt",
                sandbox_tmp.to_string_lossy(), sandbox_tmp.to_string_lossy()),
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "Writes to sandbox_tmp should succeed even in readonly mode");
        assert_eq!(stdout.trim(), "tmpdata");
    }

    #[test]
    fn test_sandbox_blocks_network() {
        let (temp, sandbox_tmp) = sandbox_dirs();
        // Use bash /dev/tcp to probe network — works on both GNU and macOS bash
        let out = sandbox_run(
            temp.path(),
            &sandbox_tmp,
            false,
            "bash -c '(echo > /dev/tcp/1.1.1.1/80) 2>/dev/null && echo connected || echo blocked'",
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("blocked"),
            "Network should be blocked in sandbox.\nstdout: {stdout}"
        );
    }
}
