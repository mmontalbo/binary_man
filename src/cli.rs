//! CLI argument parsing for the doc-pack workflow.
//!
//! The CLI is intentionally thin: it wires a deterministic loop without embedding
//! policy or heuristics, so the same core logic can be reused elsewhere.
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Default nix flake ref for binary_lens so packs are reproducible by default.
pub const DEFAULT_LENS_FLAKE: &str = "../binary_lens#binary_lens";

/// Root CLI entrypoint for the enrichment workflow.
///
/// Keeping a single `RootArgs` type makes command routing obvious and avoids
/// hidden defaults in subcommand constructors.
#[derive(Parser, Debug)]
#[command(
    name = "bman",
    version,
    about = "Doc-pack enrichment workflow for binary man pages",
    after_help = "Usage:\n  bman <binary>                        Generate docs (auto-enriches)\n  bman [OPTIONS] <binary> [entrypoint] Scope to an entry point\n  bman status --doc-pack <dir>         Check status\n  bman inspect --doc-pack <dir>        Explore interactively\n\nExamples:\n  bman ls                              Generate docs for ls\n  bman git                             Generate docs for all of git\n  bman git config                      Scope to git config only\n  bman -v --max-cycles 5 git config   With options before command",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct RootArgs {
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level workflow commands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// View enrichment status and next action.
    Status(StatusArgs),
    /// Run a single enrichment cycle (for advanced use).
    Apply(ApplyArgs),
    /// Explore a doc pack interactively.
    Inspect(InspectArgs),
}

/// Run command inputs for the unified enrichment workflow.
#[derive(Parser, Debug)]
#[command(about = "Generate comprehensive documentation for a binary", trailing_var_arg = true)]
pub struct RunArgs {
    /// Doc pack root (defaults to `~/.local/share/bman/packs/<binary>`)
    #[arg(long, value_name = "DIR")]
    pub doc_pack: Option<std::path::PathBuf>,

    /// Maximum enrichment cycles before stopping (0 = unlimited)
    #[arg(long, default_value = "50")]
    pub max_cycles: usize,

    /// Show detailed progress during enrichment
    #[arg(long, short)]
    pub verbose: bool,

    /// Output format: man (default), json, or path (just print pack location)
    #[arg(long, default_value = "man")]
    pub output: OutputFormat,

    /// Force refresh of the pack before enrichment
    #[arg(long)]
    pub refresh: bool,

    /// Nix flake reference for binary_lens
    #[arg(long, value_name = "REF", default_value = DEFAULT_LENS_FLAKE)]
    pub lens_flake: String,

    /// LM command to invoke for behavior verification.
    /// Defaults to BMAN_LM_COMMAND env var, then "claude -p --model haiku".
    /// The prompt is passed via stdin, response expected on stdout as JSON.
    #[arg(long, value_name = "CMD")]
    pub lm: Option<String>,

    /// Entry point(s) to explore for surface discovery (repeatable).
    /// Generates help scenarios like `<binary> <entry-point> --help` to discover
    /// surface items not visible in the default help output.
    /// Example: --explore config --explore remote
    #[arg(long, value_name = "ENTRY_POINT")]
    pub explore: Vec<String>,

    /// Command to document: <binary> [entry-point...]
    #[arg(value_name = "COMMAND", required = true, num_args = 1..)]
    pub invocation: Vec<String>,
}

/// Output format for the run command.
#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum OutputFormat {
    /// Render as man page to stdout
    #[default]
    Man,
    /// Output status JSON
    Json,
    /// Just print the doc pack path
    Path,
}

/// Status command inputs for a single doc pack.
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

    /// Emit LM-friendly decision list with evidence for unverified items
    #[arg(long)]
    pub decisions: bool,
}

/// Init command inputs for bootstrapping a pack.
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

/// Validate command inputs used to snapshot and lock current config.
#[derive(Parser, Debug)]
#[command(about = "Advanced/debug: validate enrich config and write lock.json")]
pub struct ValidateArgs {
    /// Doc pack root containing pack, scenarios, fixtures, and outputs
    #[arg(long, value_name = "DIR")]
    pub doc_pack: PathBuf,

    /// Emit a verbose transcript of the workflow
    #[arg(long)]
    pub verbose: bool,
}

/// Plan command inputs used to evaluate requirements deterministically.
#[derive(Parser, Debug)]
#[command(about = "Advanced/debug: plan enrichment actions from a lock snapshot")]
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

/// Apply command inputs used to execute a plan transactionally.
#[derive(Parser, Debug)]
#[command(about = "Apply an enrichment plan transactionally")]
pub struct ApplyArgs {
    /// Doc pack root containing pack, scenarios, fixtures, and outputs
    #[arg(long, value_name = "DIR")]
    pub doc_pack: PathBuf,

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

    /// Force rerun for an exact scenario ID (repeatable)
    #[arg(long = "rerun-scenario-id", value_name = "ID")]
    pub rerun_scenario_id: Vec<String>,

    /// Nix flake reference for binary_lens
    #[arg(long, value_name = "REF", default_value = DEFAULT_LENS_FLAKE)]
    pub lens_flake: String,

    /// Path to LM response JSON file (from `bman status --decisions` workflow)
    #[arg(long, value_name = "FILE")]
    pub lm_response: Option<PathBuf>,

    /// Maximum enrichment cycles before stopping (0 = single apply, no loop)
    /// When > 0, apply will loop: run scenarios, invoke LM if configured, repeat.
    #[arg(long, default_value = "0")]
    pub max_cycles: usize,

    /// Override LM command (takes precedence over config and BMAN_LM_COMMAND)
    #[arg(long, value_name = "CMD")]
    pub lm: Option<String>,

    /// Entry point(s) to explore for surface discovery (repeatable).
    /// Generates help scenarios like `<binary> <entry-point> --help` to discover
    /// surface items not visible in the default help output.
    /// Example: --explore config --explore remote
    #[arg(long, value_name = "ENTRY_POINT")]
    pub explore: Vec<String>,

    /// Context path for scoped verification (internal, set by run command).
    /// When set, only verify surfaces with matching context_argv.
    #[arg(skip)]
    pub context: Vec<String>,
}

/// Inspect command inputs for the read-only TUI.
#[derive(Parser, Debug)]
#[command(about = "Inspect a doc pack in a read-only TUI")]
pub struct InspectArgs {
    /// Doc pack root containing pack, scenarios, fixtures, and outputs
    #[arg(long, value_name = "DIR")]
    pub doc_pack: PathBuf,
}
