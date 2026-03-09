//! Simplified behavior verification workflow.
//!
//! This module implements a radically simpler alternative to the main verification
//! system. Instead of 11 state files, 7 priorities, and complex decision trees,
//! it uses a single LM-driven loop:
//!
//! ```text
//! bootstrap (--help only) → [gather pending → lm_call → apply actions → run scenarios → save]* → done
//! ```
//!
//! # Key Simplifications
//!
//! - **Single state file**: `state.json` captures all verification progress
//! - **No decision tree**: The LM decides all actions
//! - **No SQL queries**: In-memory state only
//! - **Simple evidence**: Direct subprocess execution, no binary_lens
//!
//! # Usage
//!
//! ```ignore
//! use simple_verify::run;
//!
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
