//! Claude Code CLI plugin - manages persistent Claude process.
//!
//! Uses Claude CLI's stream-json mode for efficient conversation handling.
//! The process stays alive across multiple prompts, reducing startup overhead.
//!
//! Note: LM processes are not sandboxed because they need network access
//! for API calls. Security is enforced at the scenario execution layer
//! where LM-suggested commands are sandboxed with bwrap.

use super::LmPlugin;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Set process to die when parent exits (Linux-specific).
#[cfg(target_os = "linux")]
fn set_die_with_parent() {
    unsafe {
        // PR_SET_PDEATHSIG = 1, SIGKILL = 9
        libc::prctl(1, 9);
    }
}

/// Input message format for Claude CLI stream-json mode.
#[derive(Serialize)]
struct ClaudeInput {
    #[serde(rename = "type")]
    msg_type: String,
    message: ClaudeMessage,
}

/// Message content for Claude CLI.
#[derive(Serialize)]
struct ClaudeMessage {
    role: String,
    content: String,
}

/// Output message from Claude CLI stream-json mode.
#[derive(Deserialize)]
struct ClaudeOutput {
    #[serde(rename = "type")]
    msg_type: String,
    result: Option<String>,
    #[serde(default)]
    is_error: bool,
}

/// Claude Code CLI plugin.
///
/// Manages a persistent Claude CLI process using stream-json mode for
/// efficient multi-turn interactions without process restart overhead.
pub struct ClaudeCodePlugin {
    model: String,
    process: Option<Child>,
    stdin: Option<BufWriter<std::process::ChildStdin>>,
    reader_thread: Option<thread::JoinHandle<()>>,
    response_rx: Option<mpsc::Receiver<Result<String>>>,
}

impl ClaudeCodePlugin {
    /// Create a new Claude Code plugin for the specified model.
    ///
    /// # Arguments
    /// * `model` - Model variant: "haiku", "sonnet", "opus"
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            process: None,
            stdin: None,
            reader_thread: None,
            response_rx: None,
        }
    }

    /// Spawn the Claude CLI process.
    fn spawn_claude(&mut self) -> Result<()> {
        use std::os::unix::process::CommandExt;

        let mut cmd = Command::new("claude");
        cmd.args([
            "-p",
            "--verbose",
            "--model",
            &self.model,
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ]);
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
            .map_err(|e| anyhow!("Failed to spawn claude: {}. Is claude CLI installed?", e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdout"))?;

        self.stdin = Some(BufWriter::new(stdin));
        self.process = Some(child);

        // Spawn reader thread to handle blocking I/O
        let (tx, rx) = mpsc::channel();
        let reader_thread = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        // EOF - process closed
                        let _ = tx.send(Err(anyhow!("Claude process closed stdout")));
                        break;
                    }
                    Ok(_) => {
                        // Parse JSON output
                        match serde_json::from_str::<ClaudeOutput>(&line) {
                            Ok(output) if output.msg_type == "result" => {
                                if output.is_error {
                                    let _ = tx.send(Err(anyhow!(
                                        "Claude error: {}",
                                        output.result.unwrap_or_default()
                                    )));
                                } else {
                                    let _ = tx.send(Ok(output.result.unwrap_or_default()));
                                }
                            }
                            Ok(_) => continue, // Ignore non-result messages (progress, etc)
                            Err(e) => {
                                // Log parse warning but continue - might be partial line
                                eprintln!("[claude-code] Parse warning: {}", e);
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(anyhow!("Read error: {}", e)));
                        break;
                    }
                }
            }
        });

        self.reader_thread = Some(reader_thread);
        self.response_rx = Some(rx);

        Ok(())
    }

    /// Kill the Claude process and clean up resources.
    fn kill_claude(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
            let _ = process.wait();
        }
        self.stdin = None;
        self.response_rx = None;
        // Reader thread will exit when stdout closes
        self.reader_thread = None;
    }
}

impl LmPlugin for ClaudeCodePlugin {
    fn init(&mut self) -> Result<()> {
        self.spawn_claude()
    }

    fn prompt(&mut self, prompt: &str, timeout: Duration) -> Result<String> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("Plugin not initialized"))?;
        let rx = self
            .response_rx
            .as_ref()
            .ok_or_else(|| anyhow!("Plugin not initialized"))?;

        // Build and send the input message
        let input = ClaudeInput {
            msg_type: "user".to_string(),
            message: ClaudeMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            },
        };
        let json = serde_json::to_string(&input)?;
        writeln!(stdin, "{}", json)?;
        stdin.flush()?;

        // Wait for response with timeout
        rx.recv_timeout(timeout)
            .map_err(|e| anyhow!("Timeout or channel error: {}", e))?
    }

    fn reset(&mut self) -> Result<()> {
        self.kill_claude();
        self.spawn_claude()
    }

    fn shutdown(&mut self) -> Result<()> {
        self.kill_claude();
        Ok(())
    }

    fn is_stateful(&self) -> bool {
        true
    }
}

impl Drop for ClaudeCodePlugin {
    fn drop(&mut self) {
        self.kill_claude();
    }
}
