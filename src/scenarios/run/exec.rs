//! Scenario execution and evidence capture.
//!
//! This module handles running commands and capturing their outputs using
//! sandbox isolation (via bubblewrap) or direct subprocess execution.

use super::validate::validate_scenario;
use super::{ScenarioExecution, ScenarioRunContext};
use crate::enrich;
use crate::sandbox::{self, FileEntry, NetMode, SandboxConfig, SandboxOutput, Seed as SandboxSeed};
use crate::scenarios::{
    ScenarioSeedSpec, SeedEntryKind, MAX_SCENARIO_EVIDENCE_BYTES, SCENARIO_EVIDENCE_SCHEMA_VERSION,
};
use crate::util::truncate_bytes;
use anyhow::{Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::scenarios::evidence::{
    stage_scenario_evidence, FileCheckResult, ScenarioEvidence, ScenarioIndexEntry,
    ScenarioOutcome, SetupCommandResult,
};
use crate::scenarios::BehaviorAssertion;
use std::collections::BTreeMap;

/// Default timeout for scenario execution in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Convert ScenarioSeedSpec to sandbox::Seed.
fn scenario_seed_to_sandbox(seed: &ScenarioSeedSpec) -> SandboxSeed {
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut symlinks = Vec::new();

    for entry in &seed.entries {
        match entry.kind {
            SeedEntryKind::Dir => directories.push(entry.path.clone()),
            SeedEntryKind::File => files.push(FileEntry {
                path: entry.path.clone(),
                content: entry.contents.clone().unwrap_or_default(),
            }),
            SeedEntryKind::Symlink => {
                if let Some(target) = &entry.target {
                    symlinks.push((entry.path.clone(), target.clone()));
                }
            }
        }
    }

    SandboxSeed {
        setup: seed.setup.clone(),
        files,
        directories,
        symlinks,
    }
}

/// Convert sandbox::SandboxOutput to DirectRunResult.
fn sandbox_output_to_direct(output: SandboxOutput) -> DirectRunResult {
    DirectRunResult {
        stdout: output.stdout,
        stderr: output.stderr,
        exit_code: output.exit_code,
        exit_signal: output.exit_signal,
        timed_out: output.timed_out,
        setup_failed: output.setup_failed,
        setup_results: output
            .setup_results
            .into_iter()
            .map(|r| SetupCommandResult {
                argv: r.command,
                success: r.success,
                exit_code: r.exit_code,
                timed_out: false,
                stderr: r.stderr,
            })
            .collect(),
        cwd_path: output.cwd_path,
        duration_ms: output.duration_ms,
    }
}

/// Parse net_mode string to sandbox NetMode.
fn parse_net_mode(net_mode: Option<&str>) -> NetMode {
    match net_mode {
        Some("host") => NetMode::Host,
        _ => NetMode::Off,
    }
}

/// Maximum bytes to capture for stdout/stderr in direct execution.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;


/// Result of direct scenario execution.
pub(super) struct DirectRunResult {
    /// Standard output (may be truncated).
    pub stdout: String,
    /// Standard error (may be truncated).
    pub stderr: String,
    /// Exit code (None if killed by signal).
    pub exit_code: Option<i32>,
    /// Exit signal (set when killed by signal on Unix).
    pub exit_signal: Option<i32>,
    /// Whether the command timed out.
    pub timed_out: bool,
    /// Whether seed setup commands failed.
    pub setup_failed: bool,
    /// Results of setup commands.
    pub setup_results: Vec<SetupCommandResult>,
    /// Path to the working directory (for file assertions).
    pub cwd_path: String,
    /// Duration of the main command execution.
    pub duration_ms: u128,
}

/// Run a scenario directly using subprocess execution.
///
/// This replaces the binary_lens-based execution with a simpler approach:
/// 1. Create a temporary directory
/// 2. Write seed files
/// 3. Run setup commands
/// 4. Run the main command with timeout
/// 5. Capture outputs and return evidence
///
/// By default, scenarios run in a bubblewrap sandbox. Set `no_sandbox` to true
/// to run directly without isolation. Stdin scenarios always fall back to direct
/// execution since sandbox doesn't support stdin yet.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_scenario_direct(
    scenario_id: &str,
    binary: &str,
    argv: &[String],
    seed: Option<&ScenarioSeedSpec>,
    stdin_content: Option<&str>,
    env_overrides: &BTreeMap<String, String>,
    timeout_seconds: Option<f64>,
    no_sandbox: bool,
    net_mode: Option<&str>,
) -> Result<DirectRunResult> {
    // Use sandbox unless:
    // 1. no_sandbox is explicitly set
    // 2. stdin is required (sandbox doesn't support stdin yet)
    let use_sandbox = !no_sandbox && stdin_content.is_none();

    if use_sandbox {
        return run_scenario_sandboxed(binary, argv, seed, env_overrides, timeout_seconds, net_mode);
    }
    // Create a temporary working directory
    let temp_dir = tempfile::Builder::new()
        .prefix(&format!("bman_{scenario_id}_"))
        .tempdir()
        .context("create temp directory for scenario")?;

    let work_dir = temp_dir.path();
    let cwd_path = work_dir.to_string_lossy().to_string();

    // Write seed files and run setup commands
    let (setup_failed, setup_results) = if let Some(seed) = seed {
        materialize_seed(work_dir, seed)?
    } else {
        (false, Vec::new())
    };

    // If setup failed, return early with partial result
    if setup_failed {
        return Ok(DirectRunResult {
            stdout: String::new(),
            stderr: "Setup commands failed".to_string(),
            exit_code: None,
            exit_signal: None,
            timed_out: false,
            setup_failed: true,
            setup_results,
            cwd_path,
            duration_ms: 0,
        });
    }

    // Build the main command
    let mut cmd = Command::new(binary);
    cmd.args(argv);
    cmd.current_dir(work_dir);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Apply environment overrides
    for (key, value) in env_overrides {
        cmd.env(key, value);
    }

    // Set up stdin if provided
    if stdin_content.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }

    // Execute with timeout
    let timeout = Duration::from_secs_f64(timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECS as f64));
    let started = Instant::now();
    let result = execute_with_timeout(&mut cmd, timeout, stdin_content);
    let duration_ms = started.elapsed().as_millis();

    match result {
        Ok((output, timed_out)) => {
            let stdout = truncate_output(&output.stdout);
            let stderr = truncate_output(&output.stderr);
            let exit_code = output.status.code();

            #[cfg(unix)]
            let exit_signal = {
                use std::os::unix::process::ExitStatusExt;
                output.status.signal()
            };
            #[cfg(not(unix))]
            let exit_signal = None;

            Ok(DirectRunResult {
                stdout,
                stderr,
                exit_code,
                exit_signal,
                timed_out,
                setup_failed: false,
                setup_results,
                cwd_path,
                duration_ms,
            })
        }
        Err(e) => Ok(DirectRunResult {
            stdout: String::new(),
            stderr: format!("Execution error: {}", e),
            exit_code: None,
            exit_signal: None,
            timed_out: false,
            setup_failed: false,
            setup_results,
            cwd_path,
            duration_ms,
        }),
    }
}

/// Run a scenario in a sandbox using bubblewrap.
fn run_scenario_sandboxed(
    binary: &str,
    argv: &[String],
    seed: Option<&ScenarioSeedSpec>,
    env_overrides: &BTreeMap<String, String>,
    timeout_seconds: Option<f64>,
    net_mode: Option<&str>,
) -> Result<DirectRunResult> {
    let sandbox_seed = seed
        .map(scenario_seed_to_sandbox)
        .unwrap_or_default();

    let config = SandboxConfig {
        binary: binary.to_string(),
        timeout_secs: timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECS as f64) as u64,
        env: env_overrides.clone(),
        net_mode: parse_net_mode(net_mode),
        ..Default::default()
    };

    let output = sandbox::run_sandboxed(argv, &sandbox_seed, &config)?;
    Ok(sandbox_output_to_direct(output))
}

/// Materialize seed files and run setup commands.
///
/// Returns (setup_failed, setup_results).
fn materialize_seed(
    work_dir: &Path,
    seed: &ScenarioSeedSpec,
) -> Result<(bool, Vec<SetupCommandResult>)> {
    // Create directories and files from seed entries
    for entry in &seed.entries {
        let entry_path = work_dir.join(&entry.path);

        match entry.kind {
            SeedEntryKind::Dir => {
                fs::create_dir_all(&entry_path)
                    .with_context(|| format!("create seed dir {}", entry.path))?;
            }
            SeedEntryKind::File => {
                if let Some(parent) = entry_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("create parent dirs for {}", entry.path))?;
                }
                let content = entry.contents.as_deref().unwrap_or("");
                fs::write(&entry_path, content)
                    .with_context(|| format!("write seed file {}", entry.path))?;

                // Set file mode on Unix
                #[cfg(unix)]
                if let Some(mode) = entry.mode {
                    use std::os::unix::fs::PermissionsExt;
                    let permissions = std::fs::Permissions::from_mode(mode);
                    fs::set_permissions(&entry_path, permissions)
                        .with_context(|| format!("set mode for {}", entry.path))?;
                }
            }
            SeedEntryKind::Symlink => {
                #[cfg(unix)]
                {
                    if let Some(parent) = entry_path.parent() {
                        fs::create_dir_all(parent)
                            .with_context(|| format!("create parent dirs for {}", entry.path))?;
                    }
                    let target = entry.target.as_deref().unwrap_or("");
                    std::os::unix::fs::symlink(target, &entry_path)
                        .with_context(|| format!("create symlink {}", entry.path))?;
                }
                #[cfg(not(unix))]
                {
                    // Symlinks not supported on non-Unix, skip silently
                }
            }
        }
    }

    // Run setup commands
    let mut setup_results = Vec::new();
    let mut setup_failed = false;

    for setup_cmd in &seed.setup {
        if setup_cmd.is_empty() {
            continue;
        }

        let _setup_started = Instant::now();
        let result = Command::new(&setup_cmd[0])
            .args(&setup_cmd[1..])
            .current_dir(work_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();

        let setup_result = match result {
            Ok(output) => {
                let success = output.status.success();
                let exit_code = output.status.code();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if !success {
                    setup_failed = true;
                }

                SetupCommandResult {
                    argv: setup_cmd.clone(),
                    success,
                    exit_code,
                    timed_out: false,
                    stderr: if success { String::new() } else { stderr },
                }
            }
            Err(e) => {
                setup_failed = true;
                SetupCommandResult {
                    argv: setup_cmd.clone(),
                    success: false,
                    exit_code: None,
                    timed_out: false,
                    stderr: format!("Failed to execute: {}", e),
                }
            }
        };

        setup_results.push(setup_result);

        if setup_failed {
            break;
        }
    }

    Ok((setup_failed, setup_results))
}

/// Execute a command with a timeout.
///
/// Returns (Output, timed_out).
fn execute_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
    stdin_content: Option<&str>,
) -> Result<(std::process::Output, bool)> {
    let mut child = cmd.spawn().context("spawn command")?;

    // Write stdin if provided
    if let Some(content) = stdin_content {
        if let Some(mut stdin) = child.stdin.take() {
            // Write stdin in a separate thread to avoid blocking
            let content = content.to_string();
            std::thread::spawn(move || {
                let _ = stdin.write_all(content.as_bytes());
            });
        }
    }

    // Wait with timeout using a poll loop
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Process exited
                let stdout = if let Some(mut out) = child.stdout.take() {
                    let mut buf = Vec::new();
                    let _ = out.read_to_end(&mut buf);
                    buf
                } else {
                    Vec::new()
                };
                let stderr = if let Some(mut err) = child.stderr.take() {
                    let mut buf = Vec::new();
                    let _ = err.read_to_end(&mut buf);
                    buf
                } else {
                    Vec::new()
                };
                return Ok((
                    std::process::Output {
                        status,
                        stdout,
                        stderr,
                    },
                    false,
                ));
            }
            Ok(None) => {
                // Still running
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Return a synthetic output for timeout
                    return Ok((
                        std::process::Output {
                            status: std::process::ExitStatus::default(),
                            stdout: Vec::new(),
                            stderr: b"Command timed out".to_vec(),
                        },
                        true,
                    ));
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

/// Build execution result directly from DirectRunResult.
///
/// This is the new unified path that replaces the binary_lens-based flow.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_execution_from_direct_run(
    staging_root: Option<&Path>,
    context: &ScenarioRunContext<'_>,
    run_result: &DirectRunResult,
    verbose: bool,
) -> Result<ScenarioExecution> {
    let scenario = context.scenario;
    let run_config = context.run_config;

    // Check file paths for file-based assertions
    let files_checked = check_file_assertion_paths(Some(&run_result.cwd_path), &scenario.assertions);

    let mut evidence_paths = Vec::new();
    let mut evidence_epoch_ms = None;

    if let Some(staging_root) = staging_root {
        let mut argv_full = Vec::with_capacity(scenario.argv.len() + 1);
        argv_full.push(context.run_argv0.to_string());
        argv_full.extend(scenario.argv.iter().cloned());

        let generated_at_epoch_ms = enrich::now_epoch_ms()?;
        let is_auto = scenario
            .id
            .starts_with(crate::scenarios::AUTO_VERIFY_SCENARIO_PREFIX);

        let (stdout, stderr) = if is_auto {
            (
                bounded_snippet(
                    &run_result.stdout,
                    run_config.snippet_max_lines,
                    run_config.snippet_max_bytes,
                ),
                bounded_snippet(
                    &run_result.stderr,
                    run_config.snippet_max_lines,
                    run_config.snippet_max_bytes,
                ),
            )
        } else {
            (
                truncate_bytes(run_result.stdout.as_bytes(), MAX_SCENARIO_EVIDENCE_BYTES),
                truncate_bytes(run_result.stderr.as_bytes(), MAX_SCENARIO_EVIDENCE_BYTES),
            )
        };

        let evidence = ScenarioEvidence {
            schema_version: SCENARIO_EVIDENCE_SCHEMA_VERSION,
            generated_at_epoch_ms,
            scenario_id: scenario.id.clone(),
            argv: argv_full,
            env: run_config.env.clone(),
            cwd: run_config.cwd.clone(),
            timeout_seconds: run_config.timeout_seconds,
            net_mode: run_config.net_mode.clone(),
            no_sandbox: run_config.no_sandbox,
            no_strace: run_config.no_strace,
            snippet_max_lines: run_config.snippet_max_lines,
            snippet_max_bytes: run_config.snippet_max_bytes,
            exit_code: run_result.exit_code,
            exit_signal: run_result.exit_signal,
            timed_out: run_result.timed_out,
            duration_ms: run_result.duration_ms,
            stdout,
            stderr,
            files_checked,
            setup_failed: run_result.setup_failed,
            setup_results: run_result.setup_results.clone(),
        };

        let rel = stage_scenario_evidence(staging_root, &evidence)?;
        evidence_paths.push(rel);
        evidence_epoch_ms = Some(generated_at_epoch_ms);
    }

    let failures = validate_scenario(
        &scenario.expect,
        run_result.exit_code,
        run_result.exit_signal,
        run_result.timed_out,
        &run_result.stdout,
        &run_result.stderr,
    );
    let pass = failures.is_empty();

    let command_line = format_command_line(context.run_argv0, &scenario.argv);
    let stdout_snippet = bounded_snippet(
        &run_result.stdout,
        run_config.snippet_max_lines,
        run_config.snippet_max_bytes,
    );
    let stderr_snippet = bounded_snippet(
        &run_result.stderr,
        run_config.snippet_max_lines,
        run_config.snippet_max_bytes,
    );

    if verbose && !pass {
        eprintln!("scenario {} failed: {}", scenario.id, failures.join("; "));
    }

    let outcome = scenario.publish.then(|| ScenarioOutcome {
        scenario_id: scenario.id.clone(),
        publish: scenario.publish,
        argv: scenario.argv.clone(),
        env: run_config.env.clone(),
        cwd: run_config.cwd.clone(),
        timeout_seconds: run_config.timeout_seconds,
        net_mode: run_config.net_mode.clone(),
        no_sandbox: run_config.no_sandbox,
        no_strace: run_config.no_strace,
        snippet_max_lines: run_config.snippet_max_lines,
        snippet_max_bytes: run_config.snippet_max_bytes,
        run_argv0: context.run_argv0.to_string(),
        expected: scenario.expect.clone(),
        run_id: None, // No longer using binary_lens run IDs
        manifest_ref: None,
        stdout_ref: None,
        stderr_ref: None,
        observed_exit_code: run_result.exit_code,
        observed_exit_signal: run_result.exit_signal,
        observed_timed_out: run_result.timed_out,
        pass,
        failures: failures.clone(),
        command_line,
        stdout_snippet,
        stderr_snippet,
    });

    let index_entry = ScenarioIndexEntry {
        scenario_id: scenario.id.clone(),
        scenario_digest: run_config.scenario_digest.clone(),
        last_run_epoch_ms: evidence_epoch_ms,
        last_pass: Some(pass),
        failures,
        evidence_paths,
    };

    Ok(ScenarioExecution {
        outcome,
        index_entry,
    })
}

fn bounded_snippet(text: &str, max_lines: usize, max_bytes: usize) -> String {
    let marker = "\n[... output truncated ...]\n";
    if max_lines == 0 || max_bytes == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut truncated = false;

    for (line_idx, chunk) in text.split_inclusive('\n').enumerate() {
        if line_idx >= max_lines {
            truncated = true;
            break;
        }
        if out.len() + chunk.len() > max_bytes {
            let remaining = max_bytes.saturating_sub(out.len());
            out.push_str(truncate_utf8(chunk, remaining));
            truncated = true;
            break;
        }
        out.push_str(chunk);
    }

    if !truncated && out.len() < text.len() {
        truncated = true;
    }

    if truncated {
        if max_bytes <= marker.len() {
            return truncate_utf8(marker, max_bytes).to_string();
        }
        let available = max_bytes - marker.len();
        if out.len() > available {
            out = truncate_utf8(&out, available).to_string();
        }
        out.push_str(marker);
    }

    out
}

fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Maximum bytes to capture for file content preview in FileContains assertions.
const FILE_CONTENT_PREVIEW_MAX_BYTES: usize = 4096;

/// Check file paths for file-based assertions.
///
/// Returns a map of relative paths to their check results.
fn check_file_assertion_paths(
    cwd_host: Option<&str>,
    assertions: &[BehaviorAssertion],
) -> BTreeMap<String, FileCheckResult> {
    let mut results = BTreeMap::new();

    let cwd_path = match cwd_host {
        Some(cwd) if !cwd.is_empty() => std::path::Path::new(cwd),
        _ => return results,
    };

    for assertion in assertions {
        let (path, needs_content) = match assertion {
            BehaviorAssertion::FileExists { path } => (path.as_str(), false),
            BehaviorAssertion::FileMissing { path } => (path.as_str(), false),
            BehaviorAssertion::FileRemoved { path } => (path.as_str(), false),
            BehaviorAssertion::DirExists { path } => (path.as_str(), false),
            BehaviorAssertion::DirMissing { path } => (path.as_str(), false),
            BehaviorAssertion::FileContains { path, .. } => (path.as_str(), true),
            _ => continue,
        };

        let trimmed = path.trim();
        if trimmed.is_empty() || results.contains_key(trimmed) {
            continue;
        }

        let full_path = cwd_path.join(trimmed);
        let metadata = std::fs::metadata(&full_path);

        let result = match metadata {
            Ok(meta) => {
                let is_dir = meta.is_dir();
                let size = if is_dir { None } else { Some(meta.len()) };
                let content_preview = if needs_content && !is_dir {
                    read_file_preview(&full_path, FILE_CONTENT_PREVIEW_MAX_BYTES)
                } else {
                    None
                };
                FileCheckResult {
                    exists: true,
                    is_dir,
                    size,
                    content_preview,
                }
            }
            Err(_) => FileCheckResult {
                exists: false,
                is_dir: false,
                size: None,
                content_preview: None,
            },
        };

        results.insert(trimmed.to_string(), result);
    }

    results
}

/// Read the first N bytes of a file as UTF-8, or None on error.
fn read_file_preview(path: &std::path::Path, max_bytes: usize) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = vec![0u8; max_bytes];
    let bytes_read = file.read(&mut buffer).ok()?;
    buffer.truncate(bytes_read);
    String::from_utf8(buffer).ok()
}

fn format_command_line(binary_name: &str, argv: &[String]) -> String {
    let mut parts = Vec::with_capacity(argv.len() + 1);
    parts.push(shell_quote(binary_name));
    for arg in argv {
        parts.push(shell_quote(arg));
    }
    parts.join(" ")
}

fn shell_quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    let safe = arg.chars().all(|ch| {
        matches!(
            ch,
            'a'..='z'
                | 'A'..='Z'
                | '0'..='9'
                | '_'
                | '-'
                | '.'
                | '/'
                | ':'
                | '@'
                | '+'
                | '='
        )
    });
    if safe {
        return arg.to_string();
    }
    let escaped = arg.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::ScenarioSeedEntry;

    #[test]
    fn test_scenario_seed_to_sandbox_files() {
        let scenario_seed = ScenarioSeedSpec {
            entries: vec![ScenarioSeedEntry {
                path: "file.txt".to_string(),
                kind: SeedEntryKind::File,
                contents: Some("content".to_string()),
                mode: None,
                target: None,
            }],
            setup: vec![vec!["touch".to_string(), "extra.txt".to_string()]],
        };

        let sandbox_seed = scenario_seed_to_sandbox(&scenario_seed);
        assert_eq!(sandbox_seed.files.len(), 1);
        assert_eq!(sandbox_seed.files[0].path, "file.txt");
        assert_eq!(sandbox_seed.files[0].content, "content");
        assert_eq!(sandbox_seed.setup.len(), 1);
        assert_eq!(sandbox_seed.directories.len(), 0);
        assert_eq!(sandbox_seed.symlinks.len(), 0);
    }

    #[test]
    fn test_scenario_seed_to_sandbox_directories() {
        let scenario_seed = ScenarioSeedSpec {
            entries: vec![ScenarioSeedEntry {
                path: "mydir".to_string(),
                kind: SeedEntryKind::Dir,
                contents: None,
                mode: None,
                target: None,
            }],
            setup: vec![],
        };

        let sandbox_seed = scenario_seed_to_sandbox(&scenario_seed);
        assert_eq!(sandbox_seed.directories.len(), 1);
        assert_eq!(sandbox_seed.directories[0], "mydir");
        assert_eq!(sandbox_seed.files.len(), 0);
    }

    #[test]
    fn test_scenario_seed_to_sandbox_symlinks() {
        let scenario_seed = ScenarioSeedSpec {
            entries: vec![ScenarioSeedEntry {
                path: "link".to_string(),
                kind: SeedEntryKind::Symlink,
                contents: None,
                mode: None,
                target: Some("target".to_string()),
            }],
            setup: vec![],
        };

        let sandbox_seed = scenario_seed_to_sandbox(&scenario_seed);
        assert_eq!(sandbox_seed.symlinks.len(), 1);
        assert_eq!(sandbox_seed.symlinks[0], ("link".to_string(), "target".to_string()));
    }

    #[test]
    fn test_parse_net_mode() {
        assert_eq!(parse_net_mode(None), NetMode::Off);
        assert_eq!(parse_net_mode(Some("off")), NetMode::Off);
        assert_eq!(parse_net_mode(Some("host")), NetMode::Host);
    }

    #[test]
    fn test_sandboxed_echo() {
        // Test echo with sandbox (no_sandbox=false)
        let result = run_scenario_direct(
            "test_echo",
            "echo",
            &["hello".to_string()],
            None,
            None,
            &BTreeMap::new(),
            Some(5.0),
            false, // use sandbox
            None,
        )
        .unwrap();

        assert_eq!(result.stdout.trim(), "hello");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.timed_out);
        assert!(!result.setup_failed);
    }

    #[test]
    fn test_direct_echo_with_no_sandbox() {
        // Test echo without sandbox (no_sandbox=true)
        let result = run_scenario_direct(
            "test_echo_direct",
            "echo",
            &["direct".to_string()],
            None,
            None,
            &BTreeMap::new(),
            Some(5.0),
            true, // no sandbox
            None,
        )
        .unwrap();

        assert_eq!(result.stdout.trim(), "direct");
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn test_sandboxed_with_seed_file() {
        let seed = ScenarioSeedSpec {
            entries: vec![ScenarioSeedEntry {
                path: "input.txt".to_string(),
                kind: SeedEntryKind::File,
                contents: Some("file content".to_string()),
                mode: None,
                target: None,
            }],
            setup: vec![],
        };

        let result = run_scenario_direct(
            "test_cat",
            "cat",
            &["input.txt".to_string()],
            Some(&seed),
            None,
            &BTreeMap::new(),
            Some(5.0),
            false,
            None,
        )
        .unwrap();

        assert_eq!(result.stdout.trim(), "file content");
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn test_sandboxed_with_setup_command() {
        let seed = ScenarioSeedSpec {
            entries: vec![],
            setup: vec![vec!["touch".to_string(), "created.txt".to_string()]],
        };

        let result = run_scenario_direct(
            "test_setup",
            "ls",
            &["-la".to_string()],
            Some(&seed),
            None,
            &BTreeMap::new(),
            Some(5.0),
            false,
            None,
        )
        .unwrap();

        assert!(result.stdout.contains("created.txt"));
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.setup_failed);
        assert_eq!(result.setup_results.len(), 1);
        assert!(result.setup_results[0].success);
    }

    #[test]
    fn test_stdin_falls_back_to_direct() {
        // When stdin is provided, should fall back to direct execution
        let result = run_scenario_direct(
            "test_stdin",
            "cat",
            &[],
            None,
            Some("stdin content"),
            &BTreeMap::new(),
            Some(5.0),
            false, // no_sandbox=false, but stdin should force direct
            None,
        )
        .unwrap();

        assert_eq!(result.stdout.trim(), "stdin content");
        assert_eq!(result.exit_code, Some(0));
    }
}
