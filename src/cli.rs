//! CLI argument parsing for bman.
use clap::Parser;

/// bman - Binary documentation generator.
///
/// Uses LM-driven verification to document CLI binaries.
#[derive(Parser, Debug)]
#[command(
    name = "bman",
    version,
    about = "LM-driven binary documentation generator",
    after_help = "Usage:\n  bman <binary>                        Generate docs\n  bman [OPTIONS] <binary> [entrypoint] Scope to an entry point\n\nExamples:\n  bman ls                              Document ls\n  bman git                             Document all of git\n  bman git config                      Document git config only\n  bman -v --max-cycles 5 ls            Verbose with cycle limit\n  bman ls --output json                Output as JSON"
)]
pub struct RunArgs {
    /// Doc pack root (defaults to `~/.local/share/bman/packs/<binary>`)
    #[arg(long, value_name = "DIR")]
    pub doc_pack: Option<std::path::PathBuf>,

    /// Maximum verification cycles before stopping (0 = unlimited)
    #[arg(long, default_value = "15")]
    pub max_cycles: usize,

    /// Show detailed progress during verification
    #[arg(long, short)]
    pub verbose: bool,

    /// Output format: man (default), json, or path
    #[arg(long, default_value = "man")]
    pub output: OutputFormat,

    /// LM plugin to use for verification.
    /// Native: "claude:haiku", "claude:sonnet" (persistent process, faster).
    /// Legacy: any command string (or set BMAN_LM_COMMAND env var).
    #[arg(long, value_name = "LM", default_value = "claude:haiku")]
    pub lm: String,

    /// Context mode for stateful LM plugins.
    /// auto: Use incremental for stateful plugins, full for stateless (default).
    /// full: Send complete state every cycle (works with all plugins).
    /// reset: Reset LM session after each cycle (stateless behavior for native plugins).
    /// incremental: Send only changes since last cycle (efficient for stateful plugins).
    #[arg(long, default_value = "auto")]
    pub context_mode: ContextMode,

    /// Command to document: <binary> [entry-point...]
    #[arg(value_name = "COMMAND", required = true, num_args = 1..)]
    pub invocation: Vec<String>,
}

/// Context handling mode for LM plugins.
#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum ContextMode {
    /// Auto-select: incremental for stateful plugins, full for stateless
    #[default]
    Auto,
    /// Send complete state every cycle (works with all plugins)
    Full,
    /// Reset LM session after each cycle (stateless behavior for native plugins)
    Reset,
    /// Send only changes since last cycle (efficient for stateful plugins)
    Incremental,
}

/// Output format for the run command.
#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum OutputFormat {
    /// Show summary to stdout
    #[default]
    Man,
    /// Output status JSON
    Json,
    /// Just print the doc pack path
    Path,
}
