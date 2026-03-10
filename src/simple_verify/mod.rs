//! LM-driven behavior verification.
//!
//! Core verification loop:
//! ```text
//! bootstrap → [gather pending → lm_call → apply actions → save]* → done
//! ```
//!
//! # Design
//!
//! - **Single state file**: `state.json` captures all verification progress
//! - **LM decides actions**: The LM determines what to test and how
//! - **In-memory state**: No external databases or queries
//!
//! # Usage
//!
//! ```ignore
//! let result = run("git", &["diff"], pack_path, 20, "claude -p", true)?;
//! ```

mod apply;
mod bootstrap;
mod evidence;
mod lm;
mod prompt;
mod run;
mod types;
mod validate;

pub use run::{get_summary, run, RunResult};
pub use types::State;
