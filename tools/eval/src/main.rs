use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

mod baseline;
mod compare;
mod display;
mod runner;
mod stats;
mod summary;

#[derive(Parser, Debug)]
#[command(about = "A/B testing harness for bman verification")]
struct Args {
    /// Binary to evaluate (e.g., "ls", "git")
    #[arg(value_name = "BINARY")]
    binary: String,

    /// Entry point arguments (e.g., "diff" for "git diff")
    #[arg(value_name = "ENTRY_POINT")]
    entry_point: Vec<String>,

    /// Number of evaluation runs
    #[arg(long, default_value = "3")]
    runs: usize,

    /// Compare against a tagged baseline or commit
    #[arg(long)]
    compare: Option<String>,

    /// Tag current results as a named baseline
    #[arg(long)]
    tag_baseline: Option<String>,

    /// Output JSON instead of human-readable text
    #[arg(long)]
    json: bool,

    /// Max cycles per run (0 = unlimited)
    #[arg(long, default_value = "80")]
    max_cycles: u32,

    /// Timeout per run in seconds
    #[arg(long, default_value = "600")]
    timeout: u64,

    /// Run trials in parallel
    #[arg(long)]
    parallel: bool,

    /// Descriptive note for this evaluation
    #[arg(long)]
    note: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Handle --tag-baseline (no runs needed)
    if let Some(ref name) = args.tag_baseline {
        let pack_name = pack_name(&args.binary, &args.entry_point);
        return baseline::tag(&pack_name, name);
    }

    if args.runs == 0 {
        anyhow::bail!("--runs must be at least 1");
    }

    let git = git_info()?;
    let bman_bin = build_bman()?;
    let pack_name = pack_name(&args.binary, &args.entry_point);

    // Run trials (full end-to-end: bootstrap + characterize + verify)
    let runs = if args.parallel && args.runs > 1 {
        eprintln!("\nRunning {} trials in parallel...", args.runs);
        runner::run_parallel(
            &bman_bin,
            &args.binary,
            &args.entry_point,
            args.max_cycles,
            args.timeout,
            args.runs,
        )?
    } else {
        eprintln!("\nRunning {} trials sequentially...", args.runs);
        let mut runs = Vec::new();
        for i in 0..args.runs {
            let r = runner::run_single(
                &bman_bin,
                &args.binary,
                &args.entry_point,
                args.max_cycles,
                args.timeout,
                i,
            )?;
            display::print_run_progress(&r, i, args.runs);
            runs.push(r);
        }
        runs
    };

    // Save results
    let eval_dir = eval_data_dir(&pack_name, &git.commit);
    std::fs::create_dir_all(&eval_dir)
        .with_context(|| format!("create eval dir {}", eval_dir.display()))?;

    let current = summary::build(&runs);
    save_eval_data(&eval_dir, &runs, &current, &args, &git)?;
    eprintln!(
        "Results saved to {}",
        eval_dir.display()
    );

    // Display
    if let Some(ref baseline_ref) = args.compare {
        let baseline_data = baseline::load(&pack_name, baseline_ref)?;
        display::show_comparison(&current, &baseline_data, args.json);
    } else {
        display::show_standalone(&current, &git, &args, args.json);
    }

    Ok(())
}

/// Derive pack name from binary + entry point.
fn pack_name(binary: &str, entry_point: &[String]) -> String {
    let mut name = binary.to_string();
    for ep in entry_point {
        name.push('-');
        name.push_str(ep);
    }
    name
}

/// Git metadata for the current working directory.
#[derive(Debug, Clone, serde::Serialize)]
struct GitInfo {
    commit: String,
    subject: String,
    dirty: bool,
}

fn git_info() -> Result<GitInfo> {
    let commit = cmd_output("git", &["rev-parse", "--short=7", "HEAD"])?;
    let subject = cmd_output("git", &["log", "-1", "--format=%s"])?;
    let dirty = !cmd_output("git", &["status", "--porcelain"])?.is_empty();
    Ok(GitInfo {
        commit: commit.trim().to_string(),
        subject: subject.trim().to_string(),
        dirty,
    })
}

fn build_bman() -> Result<String> {
    eprintln!("Building bman (cargo build --release)...");
    let status = std::process::Command::new("cargo")
        .args(["build", "--release", "-p", "binary-man"])
        .status()
        .context("run cargo build")?;
    if !status.success() {
        anyhow::bail!("cargo build failed");
    }
    // Find the binary
    let output = cmd_output("cargo", &["metadata", "--format-version=1", "--no-deps"])?;
    let meta: serde_json::Value = serde_json::from_str(&output)?;
    let target_dir = meta["target_directory"]
        .as_str()
        .context("no target_directory in cargo metadata")?;
    let path = PathBuf::from(target_dir).join("release").join("bman");
    if !path.exists() {
        anyhow::bail!("bman binary not found at {}", path.display());
    }
    Ok(path.to_string_lossy().to_string())
}

fn eval_data_dir(pack_name: &str, commit: &str) -> PathBuf {
    PathBuf::from("tools/eval_data")
        .join(pack_name)
        .join(commit)
}

fn save_eval_data(
    dir: &std::path::Path,
    runs: &[summary::RunOutcome],
    current: &summary::Summary,
    args: &Args,
    git: &GitInfo,
) -> Result<()> {
    // Save each run (JSON + stderr log)
    for (i, run) in runs.iter().enumerate() {
        let path = dir.join(format!("run_{}.json", i));
        let json = serde_json::to_string_pretty(run)?;
        std::fs::write(&path, json)?;
        if !run.stderr.is_empty() {
            let stderr_path = dir.join(format!("run_{}_stderr.txt", i));
            std::fs::write(&stderr_path, &run.stderr)?;
        }
    }
    // Save summary
    let path = dir.join("summary.json");
    let json = serde_json::to_string_pretty(current)?;
    std::fs::write(&path, json)?;
    // Save meta
    let meta = serde_json::json!({
        "commit": git.commit,
        "subject": git.subject,
        "dirty": git.dirty,
        "runs": args.runs,
        "max_cycles": args.max_cycles,
        "timeout": args.timeout,
        "parallel": args.parallel,
        "note": args.note,
    });
    let path = dir.join("meta.json");
    let json = serde_json::to_string_pretty(&meta)?;
    std::fs::write(&path, json)?;
    Ok(())
}

fn cmd_output(program: &str, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("run {} {:?}", program, args))?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
