//! Doc-pack enrichment workflow entrypoint.
//!
//! The binary keeps orchestration thin: the core loop (init → apply → status, with validate/plan
//! auto-run by apply) is deterministic and driven by pack-owned artifacts (JSON + SQL). This
//! makes the CLI a small dispatch layer so other consumers (e.g., the inspector) can reuse the
//! same internal logic.
#![warn(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::redundant_clone,
    clippy::cloned_instead_of_copied,
    clippy::if_then_some_else_none,
    clippy::match_bool
)]

mod cli;
mod docpack;
mod enrich;
mod inspect;
mod output;
mod pack;
mod render;
mod scenarios;
mod semantics;
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
        cli::Command::Init(args) => workflow::run_init(&args),
        cli::Command::Validate(args) => workflow::run_validate(&args),
        cli::Command::Plan(args) => workflow::run_plan(&args),
        cli::Command::Apply(args) => workflow::run_apply(&args),
        cli::Command::Status(args) => workflow::run_status(&args),
        cli::Command::MergeBehaviorEdit(args) => workflow::run_merge_behavior_edit(&args),
        cli::Command::Inspect(args) => inspect::run(&args),
    }
}
