//! Entry point for `bman <binary>` command.
//!
//! Runs the LM-driven verification loop to document a binary.

use crate::cli::{OutputFormat, RunArgs};
use crate::lm;
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

    // Resolve LM plugin config
    let env_lm = std::env::var("BMAN_LM_COMMAND").ok();
    let lm_config = lm::parse_lm_arg(&args.lm, env_lm.as_deref());

    if args.verbose {
        eprintln!("Binary: {}", binary_name);
        if !context_argv.is_empty() {
            eprintln!("Context: {}", context_argv.join(" "));
        }
        eprintln!("Pack: {}", pack_path.display());
        eprintln!("LM: {:?}", lm_config);
        eprintln!("Context mode: {:?}", args.context_mode);
        eprintln!("Max cycles: {}", args.max_cycles);
        if args.session_size > 0 {
            eprintln!("Session size: {}", args.session_size);
            if args.parallel {
                eprintln!("Parallel sessions: enabled");
            }
        }
    }

    // Run the simplified verification loop
    let result = simple_verify::run(
        &binary_name,
        &context_argv,
        &pack_path,
        args.max_cycles as u32,
        &lm_config,
        args.verbose,
        args.context_mode,
        args.session_size,
        args.parallel,
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
