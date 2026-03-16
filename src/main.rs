//! bman - Binary documentation generator.
//!
//! Uses LM-driven verification to document CLI binaries.

use anyhow::Result;
use binary_man::cli;
use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let run_args = cli::RunArgs::parse();
    binary_man::workflow::run_run(&run_args)
}
