//! Core types for sandboxed scenario execution.
//!
//! These types define the sandbox configuration, seed files, and execution
//! outputs for isolated command execution.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Network isolation mode for sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetMode {
    /// Network disabled (--unshare-net)
    #[default]
    Off,
    /// Host network shared
    Host,
}

/// Configuration for sandbox execution.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Binary to execute.
    pub binary: String,
    /// Timeout in seconds.
    pub timeout_secs: u64,
    /// Network mode: Off (default) or Host.
    pub net_mode: NetMode,
    /// Environment variables to pass through.
    pub env: BTreeMap<String, String>,
    /// Whether sandbox is disabled (run directly without bwrap).
    pub no_sandbox: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            binary: String::new(),
            timeout_secs: 30,
            net_mode: NetMode::Off,
            env: BTreeMap::new(),
            no_sandbox: false,
        }
    }
}

/// Seed specification for scenario setup.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Seed {
    /// Setup commands to run before main command: [["git", "init"], ["touch", "file.txt"]]
    #[serde(default)]
    pub setup: Vec<Vec<String>>,
    /// Files to create in workspace.
    #[serde(default)]
    pub files: Vec<FileEntry>,
    /// Directories to create in workspace.
    #[serde(default)]
    pub directories: Vec<String>,
    /// Symlinks to create: [(link_path, target_path)]
    #[serde(default)]
    pub symlinks: Vec<(String, String)>,
}

/// A file to materialize in the workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Path relative to workspace root.
    pub path: String,
    /// File content.
    pub content: String,
}

/// Result of setup command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupResult {
    /// The command that was run.
    pub command: Vec<String>,
    /// Exit code (None if signaled).
    pub exit_code: Option<i32>,
    /// Whether the command succeeded.
    pub success: bool,
    /// Stderr output (truncated if large).
    pub stderr: String,
}

/// Output from sandboxed execution.
#[derive(Debug, Clone, Default)]
pub struct SandboxOutput {
    /// Standard output from command.
    pub stdout: String,
    /// Standard error from command.
    pub stderr: String,
    /// Exit code (None if signaled).
    pub exit_code: Option<i32>,
    /// Signal that terminated the process (if any).
    pub exit_signal: Option<i32>,
    /// Whether execution timed out.
    pub timed_out: bool,
    /// Whether setup commands failed.
    pub setup_failed: bool,
    /// Results of setup commands.
    pub setup_results: Vec<SetupResult>,
    /// Working directory path used.
    pub cwd_path: String,
    /// Execution duration in milliseconds.
    pub duration_ms: u128,
}

