//! Convenience workflow for the `bman run <binary>` command.
//!
//! This is a thin wrapper that calls `init` + `apply` for a convenient
//! single-command experience. All the actual work is done by those commands.

use crate::cli::{ApplyArgs, InitArgs, OutputFormat, RunArgs};
use crate::enrich::Decision;
use crate::util::resolve_flake_ref;
use crate::workflow::{run_apply, run_init};
use crate::workflow::status::status_summary_for_doc_pack;
use anyhow::{anyhow, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Run the unified enrichment workflow.
///
/// This is equivalent to running `bman init` followed by `bman apply --max-cycles N --lm CMD`.
pub fn run_run(args: &RunArgs) -> Result<()> {
    let lens_flake = resolve_flake_ref(&args.lens_flake)?;
    let binary_name = resolve_binary_name(&args.binary)?;
    let doc_pack_root = resolve_doc_pack_root(args.doc_pack.as_deref(), &binary_name)?;

    // Print path-only output early if requested
    if matches!(args.output, OutputFormat::Path) {
        println!("{}", doc_pack_root.display());
        return Ok(());
    }

    // Ensure doc pack is initialized
    ensure_doc_pack_initialized(&doc_pack_root, &binary_name, &lens_flake, args.refresh, args.verbose)?;

    // Run apply with the LM loop
    let apply_args = ApplyArgs {
        doc_pack: doc_pack_root.clone(),
        refresh_pack: false,
        verbose: args.verbose,
        rerun_all: false,
        rerun_failed: false,
        rerun_scenario_id: Vec::new(),
        lens_flake,
        lm_response: None,
        max_cycles: args.max_cycles,
        lm: args.lm.clone(),
    };
    run_apply(&apply_args)?;

    // Get final status for output
    let computation = status_summary_for_doc_pack(doc_pack_root.clone(), true, false)?;
    let summary = computation.summary;

    let unverified_count = summary
        .requirements
        .iter()
        .find(|r| r.id == crate::enrich::RequirementId::Verification)
        .and_then(|r| r.behavior_unverified_count)
        .unwrap_or(0);

    // Show final status summary if verbose
    if args.verbose {
        eprintln!(
            "run: finished (decision: {:?}, unverified: {})",
            summary.decision, unverified_count
        );
    }

    // Output based on format
    match args.output {
        OutputFormat::Man => {
            match render_man_output(&doc_pack_root) {
                Ok(()) => {}
                Err(_) if !matches!(summary.decision, Decision::Complete) => {
                    // Man page not available and not complete - show status summary instead
                    eprintln!(
                        "note: man page not yet rendered (unverified: {})",
                        unverified_count
                    );
                    eprintln!("      doc pack at: {}", doc_pack_root.display());
                    eprintln!("      use --output json to see full status");
                }
                Err(e) => return Err(e),
            }
        }
        OutputFormat::Json => {
            let text = serde_json::to_string_pretty(&summary)?;
            println!("{text}");
        }
        OutputFormat::Path => unreachable!("handled above"),
    }

    Ok(())
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

fn resolve_doc_pack_root(explicit: Option<&Path>, binary_name: &str) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    // Default to ~/.local/share/bman/packs/<binary>
    let data_dir = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow!("cannot determine home directory"))?;

    Ok(data_dir.join("bman").join("packs").join(binary_name))
}

fn ensure_doc_pack_initialized(
    doc_pack_root: &Path,
    binary_name: &str,
    lens_flake: &str,
    refresh: bool,
    verbose: bool,
) -> Result<()> {
    let config_path = doc_pack_root.join("enrich").join("config.json");

    if !config_path.is_file() || refresh {
        if verbose {
            eprintln!("run: initializing doc pack at {}", doc_pack_root.display());
        }
        let init_args = InitArgs {
            doc_pack: doc_pack_root.to_path_buf(),
            binary: Some(binary_name.to_string()),
            force: true,
            lens_flake: lens_flake.to_string(),
        };
        run_init(&init_args)?;
    }

    Ok(())
}

fn render_man_output(doc_pack_root: &Path) -> Result<()> {
    let man_page_path = doc_pack_root.join("outputs").join("man_page.md");

    if !man_page_path.is_file() {
        return Err(anyhow!(
            "man page not found at {}; enrichment may not have completed",
            man_page_path.display()
        ));
    }

    let content = fs::read_to_string(&man_page_path)?;
    println!("{content}");
    Ok(())
}
