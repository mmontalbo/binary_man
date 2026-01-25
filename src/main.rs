//! Doc-pack enrichment workflow entrypoint.

mod cli;
mod docpack;
mod enrich;
mod output;
mod pack;
mod render;
mod scenarios;
mod staging;
mod status;
mod surface;
mod templates;
mod util;
mod workflow;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let args = cli::RootArgs::parse();
    match args.command {
        cli::Command::Init(args) => workflow::run_init(args),
        cli::Command::Validate(args) => workflow::run_validate(args),
        cli::Command::Plan(args) => workflow::run_plan(args),
        cli::Command::Apply(args) => workflow::run_apply(args),
        cli::Command::Status(args) => workflow::run_status(args),
    }
}
