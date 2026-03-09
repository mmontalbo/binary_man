//! Convenience workflow for the `bman run <binary>` command.
//!
//! Uses the simplified LM-driven verification loop:
//! bootstrap → [gather pending → lm_call → apply actions → save]* → done

use crate::cli::{OutputFormat, RunArgs};
use crate::simple_verify;
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// Run the unified enrichment workflow.
///
/// Uses the simplified verification loop with LM-driven decision making.
pub fn run_run(args: &RunArgs) -> Result<()> {
    // Parse invocation: first element is binary, rest is context argv
    let binary_name = args
        .invocation
        .first()
        .map(|s| resolve_binary_name(s))
        .transpose()?
        .ok_or_else(|| anyhow!("invocation requires at least a binary name"))?;

    let context_argv: Vec<String> = args.invocation.iter().skip(1).cloned().collect();

    let pack_path = resolve_pack_path(args.doc_pack.as_deref(), &binary_name, &context_argv)?;

    // Print path-only output early if requested
    if matches!(args.output, OutputFormat::Path) {
        println!("{}", pack_path.display());
        return Ok(());
    }

    // Resolve LM command
    let lm_command = resolve_lm_command(args.lm.as_deref())
        .ok_or_else(|| anyhow!("LM command required: use --lm or set BMAN_LM_COMMAND"))?;

    if args.verbose {
        eprintln!("Binary: {}", binary_name);
        if !context_argv.is_empty() {
            eprintln!("Context: {}", context_argv.join(" "));
        }
        eprintln!("Pack: {}", pack_path.display());
        eprintln!("LM: {}", lm_command);
        eprintln!("Max cycles: {}", args.max_cycles);
    }

    // Run the simplified verification loop
    let result = simple_verify::run(
        &binary_name,
        &context_argv,
        &pack_path,
        args.max_cycles as u32,
        &lm_command,
        args.verbose,
    )?;

    // Load final state for output
    let state = simple_verify::State::load(&pack_path)?;
    let summary = simple_verify::get_summary(&state);

    // Output based on format
    match args.output {
        OutputFormat::Man => {
            // For now, show summary (man page rendering is out of scope for simple_verify)
            println!("\n{}", summary);
            print_result(&result);
        }
        OutputFormat::Json => {
            let json_output = serde_json::json!({
                "binary": summary.binary,
                "context_argv": summary.context_argv,
                "cycle": summary.cycle,
                "total": summary.total,
                "verified": summary.verified,
                "excluded": summary.excluded,
                "pending": summary.pending,
                "has_baseline": summary.has_baseline,
                "result": format!("{:?}", result),
            });
            println!("{}", serde_json::to_string_pretty(&json_output)?);
        }
        OutputFormat::Path => unreachable!("handled above"),
    }

    Ok(())
}

fn print_result(result: &simple_verify::RunResult) {
    match result {
        simple_verify::RunResult::Complete => {
            println!("\nResult: Complete");
        }
        simple_verify::RunResult::HitMaxCycles => {
            println!("\nResult: Hit max cycles limit");
        }
        simple_verify::RunResult::LmGaveUp => {
            println!("\nResult: LM gave up (returned no actions)");
        }
    }
}

fn resolve_binary_name(binary_arg: &str) -> Result<String> {
    let path = Path::new(binary_arg);
    if path.is_absolute() || binary_arg.contains('/') {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("cannot extract binary name from path: {}", binary_arg))
    } else {
        Ok(binary_arg.to_string())
    }
}

fn resolve_pack_path(
    explicit: Option<&Path>,
    binary_name: &str,
    context_argv: &[String],
) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    // Default to ~/.local/share/bman/packs/<binary>[-context]
    let data_dir = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow!("cannot determine home directory"))?;

    let pack_name = if context_argv.is_empty() {
        binary_name.to_string()
    } else {
        format!("{}-{}", binary_name, context_argv.join("-"))
    };

    Ok(data_dir.join("bman").join("packs").join(pack_name))
}

/// Resolve LM command with fallback: explicit arg > env var > default.
fn resolve_lm_command(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(|s| s.to_string())
        .or_else(|| std::env::var("BMAN_LM_COMMAND").ok())
        .or_else(|| Some("claude -p --model haiku".to_string()))
}
