//! bman - Binary documentation generator.
//!
//! Uses LM-driven verification to document CLI binaries.

mod cli;
mod simple_verify;
mod workflow;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    // Initialize tracing with RUST_LOG env filter (default: warn)
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let run_args = cli::RunArgs::parse();
    workflow::run_run(&run_args)
}
