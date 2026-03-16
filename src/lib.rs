//! bman - LM-driven CLI binary verifier.
//!
//! Discovers every flag/option from `--help`, uses an LM to design test
//! scenarios that exercise each option in a sandbox, and verifies observable
//! behavior.

pub mod cli;
pub mod lm;
pub mod verify;
pub mod workflow;
