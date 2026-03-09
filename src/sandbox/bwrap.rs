//! Bubblewrap (bwrap) sandbox execution.
//!
//! Provides isolated execution of commands using Linux namespaces via bwrap.
//! Commands run in a minimal environment with read-only system mounts and
//! a writable workspace directory.

use super::types::{NetMode, SandboxConfig, SandboxOutput, Seed, SetupResult};
use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{fs, thread};

/// Maximum output size in bytes before truncation.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Run a command in a sandboxed environment.
///
/// Creates a temporary workspace, materializes seed files, runs setup commands,
/// then executes the main command with the specified arguments.
pub fn run_sandboxed(
    argv: &[String],
    seed: &Seed,
    config: &SandboxConfig,
) -> Result<SandboxOutput> {
    let start = Instant::now();

    // 1. Create temp directory on host
    let work_dir = tempfile::Builder::new()
        .prefix("bman_sandbox_")
        .tempdir()
        .context("create sandbox workspace")?;

    // 2. Materialize seed files
    materialize_seed(work_dir.path(), seed)?;

    // 3. Resolve binary path
    let binary_path = which::which(&config.binary)
        .with_context(|| format!("resolve binary path for {}", config.binary))?;

    // 4. Run setup commands (in sandbox if enabled)
    let mut setup_results = Vec::new();
    for setup_cmd in &seed.setup {
        if setup_cmd.is_empty() {
            continue;
        }
        let result = run_setup_command(setup_cmd, work_dir.path(), config)?;
        let success = result.success;
        setup_results.push(result.clone());
        if !success {
            return Ok(SandboxOutput {
                setup_failed: true,
                setup_results,
                cwd_path: work_dir.path().to_string_lossy().to_string(),
                duration_ms: start.elapsed().as_millis(),
                ..Default::default()
            });
        }
    }

    // 5. Run main command with timeout
    let mut cmd = if config.no_sandbox {
        let mut c = Command::new(&binary_path);
        c.current_dir(work_dir.path());
        for (key, value) in &config.env {
            c.env(key, value);
        }
        c
    } else {
        build_bwrap_command(config, &binary_path, work_dir.path())
    };

    if !config.no_sandbox {
        cmd.arg("--").arg(&binary_path);
    }
    cmd.args(argv);

    let timeout = Duration::from_secs(config.timeout_secs);
    let (output, timed_out) = execute_with_timeout(&mut cmd, timeout)?;

    let duration_ms = start.elapsed().as_millis();

    if timed_out {
        return Ok(SandboxOutput {
            timed_out: true,
            setup_results,
            cwd_path: work_dir.path().to_string_lossy().to_string(),
            duration_ms,
            ..Default::default()
        });
    }

    Ok(SandboxOutput {
        stdout: truncate_output(&output.stdout),
        stderr: truncate_output(&output.stderr),
        exit_code: output.status.code(),
        exit_signal: extract_signal(&output.status),
        timed_out: false,
        setup_failed: false,
        setup_results,
        cwd_path: work_dir.path().to_string_lossy().to_string(),
        duration_ms,
    })
}

/// Materialize seed files, directories, and symlinks in the workspace.
fn materialize_seed(work_dir: &Path, seed: &Seed) -> Result<()> {
    // Create directories
    for dir in &seed.directories {
        let path = work_dir.join(dir);
        fs::create_dir_all(&path)
            .with_context(|| format!("create seed directory {}", dir))?;
    }

    // Create files
    for file in &seed.files {
        let path = work_dir.join(&file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, &file.content)
            .with_context(|| format!("write seed file {}", file.path))?;
    }

    // Create symlinks
    for (link_path, target) in &seed.symlinks {
        let link = work_dir.join(link_path);
        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &link)
            .with_context(|| format!("create symlink {} -> {}", link_path, target))?;
    }

    Ok(())
}

/// Run a setup command, either sandboxed or directly.
fn run_setup_command(
    cmd: &[String],
    work_dir: &Path,
    config: &SandboxConfig,
) -> Result<SetupResult> {
    if cmd.is_empty() {
        return Ok(SetupResult {
            command: vec![],
            exit_code: Some(0),
            success: true,
            stderr: String::new(),
        });
    }

    let setup_binary = which::which(&cmd[0])
        .with_context(|| format!("resolve setup binary {}", cmd[0]))?;

    let mut proc = if config.no_sandbox {
        let mut c = Command::new(&setup_binary);
        c.current_dir(work_dir);
        c.args(&cmd[1..]);
        c
    } else {
        let mut c = build_bwrap_command(config, &setup_binary, work_dir);
        c.arg("--").arg(&setup_binary);
        c.args(&cmd[1..]);
        c
    };

    proc.stdout(Stdio::null());
    proc.stderr(Stdio::piped());

    let output = proc.output()
        .with_context(|| format!("run setup command {:?}", cmd))?;

    Ok(SetupResult {
        command: cmd.to_vec(),
        exit_code: output.status.code(),
        success: output.status.success(),
        stderr: truncate_output(&output.stderr),
    })
}

/// Build a bwrap command with appropriate bind mounts and namespace isolation.
fn build_bwrap_command(config: &SandboxConfig, binary_path: &Path, work_dir: &Path) -> Command {
    let mut cmd = Command::new("bwrap");

    // Namespaces for isolation
    cmd.args(["--unshare-user", "--unshare-ipc", "--unshare-pid", "--unshare-uts"]);
    if config.net_mode == NetMode::Off {
        cmd.arg("--unshare-net");
    }

    // Die when parent dies (cleanup on crash)
    cmd.arg("--die-with-parent");

    // Read-only system mounts
    add_ro_bind_if_exists(&mut cmd, "/usr");
    add_ro_bind_if_exists(&mut cmd, "/lib");
    add_ro_bind_if_exists(&mut cmd, "/lib64");
    add_ro_bind_if_exists(&mut cmd, "/bin");
    add_ro_bind_if_exists(&mut cmd, "/sbin");
    add_ro_bind_if_exists(&mut cmd, "/etc");
    add_ro_bind_if_exists(&mut cmd, "/nix");  // For NixOS systems

    // Proc/dev/tmp
    cmd.args(["--proc", "/proc"]);
    cmd.args(["--dev", "/dev"]);
    cmd.args(["--tmpfs", "/tmp"]);

    // Read-only home (prevents writes to user config)
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        cmd.args(["--ro-bind", &home_str, &home_str]);
    }

    // Writable workspace
    let work_str = work_dir.to_string_lossy();
    cmd.args(["--bind", &work_str, "/workspace"]);
    cmd.args(["--chdir", "/workspace"]);

    // Bind the binary being tested (read-only)
    let binary_str = binary_path.to_string_lossy();
    cmd.args(["--ro-bind", &binary_str, &binary_str]);

    // Also bind the binary's directory for shared libraries
    if let Some(parent) = binary_path.parent() {
        let parent_str = parent.to_string_lossy();
        cmd.args(["--ro-bind", &parent_str, &parent_str]);
    }

    // Clear environment and set minimal defaults
    cmd.arg("--clearenv");
    cmd.args(["--setenv", "HOME", "/workspace"]);
    cmd.args(["--setenv", "PATH", "/usr/bin:/bin:/usr/local/bin:/nix/var/nix/profiles/default/bin"]);
    cmd.args(["--setenv", "TERM", "dumb"]);
    cmd.args(["--setenv", "LANG", "C.UTF-8"]);

    // Prevent pagers
    cmd.args(["--setenv", "PAGER", "cat"]);
    cmd.args(["--setenv", "GIT_PAGER", "cat"]);
    cmd.args(["--setenv", "MANPAGER", "cat"]);

    // User-specified environment variables
    for (key, value) in &config.env {
        cmd.args(["--setenv", key, value]);
    }

    cmd
}

/// Add a read-only bind mount if the path exists.
fn add_ro_bind_if_exists(cmd: &mut Command, path: &str) {
    if Path::new(path).exists() {
        cmd.args(["--ro-bind", path, path]);
    }
}

/// Execute a command with timeout, returning output and whether it timed out.
fn execute_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
) -> Result<(std::process::Output, bool)> {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().context("spawn command")?;

    let start = Instant::now();
    let poll_interval = Duration::from_millis(50);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Process finished
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_end(&mut stderr);
                }
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
                if start.elapsed() >= timeout {
                    // Timeout - kill the process
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok((
                        std::process::Output {
                            status: std::process::ExitStatus::default(),
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                        },
                        true,
                    ));
                }
                thread::sleep(poll_interval);
            }
            Err(e) => {
                return Err(e).context("wait for process");
            }
        }
    }
}

/// Extract signal number from exit status (Unix only).
#[cfg(unix)]
fn extract_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn extract_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Truncate output to maximum size, converting to string.
fn truncate_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    if text.len() <= MAX_OUTPUT_BYTES {
        text.to_string()
    } else {
        let mut truncated = String::new();
        for ch in text.chars() {
            if truncated.len() + ch.len_utf8() > MAX_OUTPUT_BYTES {
                break;
            }
            truncated.push(ch);
        }
        truncated.push_str("\n... (truncated)");
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SandboxConfig {
        SandboxConfig {
            binary: "echo".to_string(),
            no_sandbox: true, // Use no_sandbox for tests to avoid bwrap dependency
            ..Default::default()
        }
    }

    #[test]
    fn test_sandbox_echo() {
        let mut config = test_config();
        config.binary = "echo".to_string();

        let output = run_sandboxed(
            &["hello".to_string()],
            &Seed::default(),
            &config,
        )
        .unwrap();

        assert_eq!(output.stdout.trim(), "hello");
        assert_eq!(output.exit_code, Some(0));
        assert!(!output.timed_out);
        assert!(!output.setup_failed);
    }

    #[test]
    fn test_sandbox_with_seed_files() {
        let mut config = test_config();
        config.binary = "cat".to_string();

        let seed = Seed {
            files: vec![super::super::types::FileEntry {
                path: "input.txt".to_string(),
                content: "test content".to_string(),
            }],
            ..Default::default()
        };

        let output = run_sandboxed(
            &["input.txt".to_string()],
            &seed,
            &config,
        )
        .unwrap();

        assert_eq!(output.stdout.trim(), "test content");
        assert_eq!(output.exit_code, Some(0));
    }

    #[test]
    fn test_sandbox_with_seed_directories() {
        let mut config = test_config();
        config.binary = "ls".to_string();

        let seed = Seed {
            directories: vec!["subdir/nested".to_string()],
            files: vec![super::super::types::FileEntry {
                path: "subdir/nested/file.txt".to_string(),
                content: "nested file".to_string(),
            }],
            ..Default::default()
        };

        let output = run_sandboxed(
            &["subdir/nested".to_string()],
            &seed,
            &config,
        )
        .unwrap();

        assert!(output.stdout.contains("file.txt"));
        assert_eq!(output.exit_code, Some(0));
    }

    #[test]
    fn test_sandbox_setup_commands() {
        let mut config = test_config();
        config.binary = "cat".to_string();

        let seed = Seed {
            setup: vec![
                vec!["touch".to_string(), "created.txt".to_string()],
            ],
            ..Default::default()
        };

        let output = run_sandboxed(
            &["created.txt".to_string()],
            &seed,
            &config,
        )
        .unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert!(!output.setup_failed);
        assert_eq!(output.setup_results.len(), 1);
        assert!(output.setup_results[0].success);
    }

    #[test]
    fn test_sandbox_setup_failure() {
        let mut config = test_config();
        config.binary = "true".to_string();

        let seed = Seed {
            setup: vec![
                vec!["false".to_string()], // This will fail
            ],
            ..Default::default()
        };

        let output = run_sandboxed(&[], &seed, &config).unwrap();

        assert!(output.setup_failed);
        assert_eq!(output.setup_results.len(), 1);
        assert!(!output.setup_results[0].success);
    }

    #[test]
    fn test_sandbox_timeout() {
        let mut config = test_config();
        config.binary = "sleep".to_string();
        config.timeout_secs = 1;

        let output = run_sandboxed(
            &["10".to_string()],
            &Seed::default(),
            &config,
        )
        .unwrap();

        assert!(output.timed_out);
    }

    #[test]
    fn test_truncate_output() {
        let short = b"short";
        assert_eq!(truncate_output(short), "short");

        let long = vec![b'x'; MAX_OUTPUT_BYTES + 100];
        let truncated = truncate_output(&long);
        assert!(truncated.len() <= MAX_OUTPUT_BYTES + 20); // Allow for truncation message
        assert!(truncated.ends_with("... (truncated)"));
    }

    #[test]
    fn test_materialize_seed_files() {
        let temp = tempfile::tempdir().unwrap();
        let seed = Seed {
            files: vec![
                super::super::types::FileEntry {
                    path: "a.txt".to_string(),
                    content: "content a".to_string(),
                },
                super::super::types::FileEntry {
                    path: "dir/b.txt".to_string(),
                    content: "content b".to_string(),
                },
            ],
            directories: vec!["empty_dir".to_string()],
            ..Default::default()
        };

        materialize_seed(temp.path(), &seed).unwrap();

        assert!(temp.path().join("a.txt").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("a.txt")).unwrap(),
            "content a"
        );
        assert!(temp.path().join("dir/b.txt").exists());
        assert!(temp.path().join("empty_dir").is_dir());
    }
}
