//! External command plugin.
//!
//! Wraps any command that reads a prompt from stdin and writes a response
//! to stdout. Each prompt spawns a fresh process (stateless).
//! Useful for custom LM backends or BMAN_LM_COMMAND integration.

use super::LmPlugin;
use anyhow::{anyhow, Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Set process to die when parent exits (Linux-specific).
#[cfg(target_os = "linux")]
fn set_die_with_parent() {
    unsafe {
        // PR_SET_PDEATHSIG = 1, SIGKILL = 9
        libc::prctl(1, 9);
    }
}

/// External command plugin.
///
/// Wraps any command that reads prompts from stdin and writes
/// responses to stdout. Each prompt spawns a fresh process.
pub struct CommandPlugin {
    cmd: String,
}

impl CommandPlugin {
    /// Create a new command plugin.
    ///
    /// # Arguments
    /// * `cmd` - Shell command string (parsed with shell_words)
    pub fn new(cmd: &str) -> Self {
        Self {
            cmd: cmd.to_string(),
        }
    }
}

impl LmPlugin for CommandPlugin {
    fn init(&mut self) -> Result<()> {
        // No initialization needed - each prompt spawns fresh process
        Ok(())
    }

    fn prompt(&mut self, prompt: &str, timeout: Duration) -> Result<String> {
        let timeout_secs = timeout.as_secs();

        // Parse command into args
        let args = shell_words::split(&self.cmd)
            .with_context(|| format!("parse LM command: {}", self.cmd))?;

        if args.is_empty() {
            return Err(anyhow!("LM command is empty"));
        }

        // Use timeout command wrapper for the subprocess
        use std::os::unix::process::CommandExt;

        let mut cmd = Command::new("timeout");
        cmd.args([&timeout_secs.to_string()]);
        cmd.args(&args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::inherit());

        // Ensure child process dies when parent exits
        #[cfg(target_os = "linux")]
        unsafe {
            cmd.pre_exec(|| {
                set_die_with_parent();
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn command: {}", e))?;

        // Write prompt to stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes())?;
        }

        let output = child.wait_with_output()?;

        if !output.status.success() {
            // Check if it was a timeout (exit code 124)
            if output.status.code() == Some(124) {
                return Err(anyhow!("Command timed out after {} seconds", timeout_secs));
            }
            return Err(anyhow!("Command failed with status: {}", output.status));
        }

        String::from_utf8(output.stdout).map_err(|e| anyhow!("Invalid UTF-8 in response: {}", e))
    }

    fn reset(&mut self) -> Result<()> {
        // No-op for command plugin - each call is fresh
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        // No-op - no persistent state
        Ok(())
    }

    fn is_stateful(&self) -> bool {
        false
    }
}
