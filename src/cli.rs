use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub const DEFAULT_LENS_FLAKE: &str = "../binary_lens#binary_lens";

/// CLI arguments for the doc-pack enrichment workflow.
#[derive(Parser, Debug)]
#[command(
    name = "bman",
    version,
    about = "Doc-pack enrichment workflow for binary man pages",
    after_help = "Commands:\n  init --doc-pack <dir> --binary <bin>  Bootstrap a doc pack (pack + config)\n  validate --doc-pack <dir>            Validate inputs and write enrich/lock.json\n  plan --doc-pack <dir>                Evaluate requirements and write enrich/plan.out.json\n  apply --doc-pack <dir>               Apply plan transactionally (writes enrich/report.json)\n  status --doc-pack <dir>              Summarize requirements and next action\n  inspect --doc-pack <dir>             Read-only TUI inspector for doc packs\n\nExamples:\n  bman init --doc-pack /tmp/ls-docpack --binary ls\n  bman validate --doc-pack /tmp/ls-docpack\n  bman plan --doc-pack /tmp/ls-docpack\n  bman apply --doc-pack /tmp/ls-docpack\n  bman status --doc-pack /tmp/ls-docpack --json\n  bman inspect --doc-pack /tmp/ls-docpack",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct RootArgs {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    Init(InitArgs),
    Validate(ValidateArgs),
    Plan(PlanArgs),
    Apply(ApplyArgs),
    Status(StatusArgs),
    Inspect(InspectArgs),
}

#[derive(Parser, Debug)]
#[command(about = "Summarize doc-pack status and next action")]
pub struct StatusArgs {
    /// Doc pack root containing pack, scenarios, fixtures, and outputs
    #[arg(long, value_name = "DIR")]
    pub doc_pack: PathBuf,

    /// Emit machine-readable JSON output
    #[arg(long)]
    pub json: bool,

    /// Include full verification triage lists in JSON output
    #[arg(long)]
    pub full: bool,

    /// Ignore missing/stale lock.json (recorded in report)
    #[arg(long)]
    pub force: bool,

    /// Emit a verbose transcript of the workflow
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Parser, Debug)]
#[command(about = "Initialize a doc-pack (pack + enrichment config)")]
pub struct InitArgs {
    /// Doc pack root containing pack, scenarios, fixtures, and outputs
    #[arg(long, value_name = "DIR")]
    pub doc_pack: PathBuf,

    /// Binary to analyze when generating a new pack
    #[arg(long, value_name = "BIN")]
    pub binary: Option<String>,

    /// Overwrite an existing config.json
    #[arg(long)]
    pub force: bool,

    /// Nix flake reference for binary_lens
    #[arg(long, value_name = "REF", default_value = DEFAULT_LENS_FLAKE)]
    pub lens_flake: String,
}

#[derive(Parser, Debug)]
#[command(about = "Validate enrich config and write lock.json")]
pub struct ValidateArgs {
    /// Doc pack root containing pack, scenarios, fixtures, and outputs
    #[arg(long, value_name = "DIR")]
    pub doc_pack: PathBuf,

    /// Emit a verbose transcript of the workflow
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Parser, Debug)]
#[command(about = "Plan enrichment actions based on a lock snapshot")]
pub struct PlanArgs {
    /// Doc pack root containing pack, scenarios, fixtures, and outputs
    #[arg(long, value_name = "DIR")]
    pub doc_pack: PathBuf,

    /// Ignore missing/stale lock.json (recorded in report)
    #[arg(long)]
    pub force: bool,

    /// Emit a verbose transcript of the workflow
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Parser, Debug)]
#[command(about = "Apply an enrichment plan transactionally")]
pub struct ApplyArgs {
    /// Doc pack root containing pack, scenarios, fixtures, and outputs
    #[arg(long, value_name = "DIR")]
    pub doc_pack: PathBuf,

    /// Ignore missing/stale lock.json (recorded in report)
    #[arg(long)]
    pub force: bool,

    /// Force regeneration of the pack before static extraction
    #[arg(long)]
    pub refresh_pack: bool,

    /// Emit a verbose transcript of the workflow
    #[arg(long)]
    pub verbose: bool,

    /// Rerun all scenarios, ignoring cached results
    #[arg(long, conflicts_with = "rerun_failed")]
    pub rerun_all: bool,

    /// Rerun only scenarios that previously failed
    #[arg(long, conflicts_with = "rerun_all")]
    pub rerun_failed: bool,

    /// Nix flake reference for binary_lens
    #[arg(long, value_name = "REF", default_value = DEFAULT_LENS_FLAKE)]
    pub lens_flake: String,
}

#[derive(Parser, Debug)]
#[command(about = "Inspect a doc pack in a read-only TUI")]
pub struct InspectArgs {
    /// Doc pack root containing pack, scenarios, fixtures, and outputs
    #[arg(long, value_name = "DIR")]
    pub doc_pack: PathBuf,
}
