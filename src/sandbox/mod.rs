//! Sandboxed scenario execution with bubblewrap.
//!
//! This module provides isolated execution of commands using Linux namespaces
//! via bubblewrap (bwrap). Key features:
//!
//! - **Isolation**: Network, filesystem, and namespace isolation
//! - **Seed materialization**: Files, directories, symlinks, and setup commands
//! - **Evidence capture**: Full execution context and output recording
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    Sandbox Execution                        │
//! ├─────────────────────────────────────────────────────────────┤
//! │  1. Create temp workspace                                   │
//! │  2. Materialize seed (files, dirs, symlinks)                │
//! │  3. Run setup commands (in sandbox if enabled)              │
//! │  4. Execute main command with timeout                       │
//! │  5. Capture evidence (stdout, stderr, exit code, timing)    │
//! │  6. Return SandboxOutput                                    │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```ignore
//! use sandbox::{SandboxConfig, Seed, FileEntry, run_sandboxed};
//!
//! let config = SandboxConfig {
//!     binary: "cat".to_string(),
//!     timeout_secs: 30,
//!     ..Default::default()
//! };
//!
//! let seed = Seed {
//!     files: vec![FileEntry {
//!         path: "input.txt".to_string(),
//!         content: "hello world".to_string(),
//!     }],
//!     ..Default::default()
//! };
//!
//! let output = run_sandboxed(&["input.txt".to_string()], &seed, &config)?;
//! ```

mod bwrap;
mod types;

// Re-export public API
pub use bwrap::run_sandboxed;
pub use types::{FileEntry, NetMode, SandboxConfig, SandboxOutput, Seed};
