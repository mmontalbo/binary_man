//! Extract raw help text from binaries for context.

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_OUT_DIR: &str = "out";

#[derive(Parser, Debug)]
#[command(name = "bvm", version, about = "Extract --help text for LM context")]
struct Args {
    /// Path to the binary under test
    binary: PathBuf,

    /// Output directory for extracted context
    #[arg(long, value_name = "DIR", default_value = DEFAULT_OUT_DIR)]
    out_dir: PathBuf,
}

struct CaptureOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let capture = capture_help(&args.binary)?;
    let help_text = select_help_text(&capture).ok_or_else(|| {
        anyhow!(
            "help capture produced no output: exit={:?}",
            capture.exit_code
        )
    })?;

    let name = args
        .binary
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("binary");
    let out_dir = args.out_dir.join("context").join(name);
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("create output dir {}", out_dir.display()))?;
    let out_path = out_dir.join("help.txt");
    std::fs::write(&out_path, help_text)
        .with_context(|| format!("write help text {}", out_path.display()))?;

    println!("help: {}", out_path.display());
    Ok(())
}

fn capture_help(binary: &Path) -> Result<CaptureOutput> {
    let primary = capture_output(binary, &["--help"])?;
    if select_help_text(&primary).is_some() {
        return Ok(primary);
    }
    let fallback = capture_output(binary, &["-h"])?;
    if select_help_text(&fallback).is_some() {
        return Ok(fallback);
    }
    Ok(primary)
}

fn select_help_text(capture: &CaptureOutput) -> Option<&str> {
    if !capture.stdout.trim().is_empty() {
        Some(capture.stdout.as_str())
    } else if !capture.stderr.trim().is_empty() {
        Some(capture.stderr.as_str())
    } else {
        None
    }
}

fn capture_output(binary: &Path, args: &[&str]) -> Result<CaptureOutput> {
    let output = Command::new(binary)
        .args(args)
        .env_clear()
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("TERM", "dumb")
        .output()
        .with_context(|| format!("run {} {:?}", binary.display(), args))?;

    Ok(CaptureOutput {
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}
