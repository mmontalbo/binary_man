//! Extract raw help text from binaries for context.

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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

struct HelpCapture {
    arg: String,
    output: CaptureOutput,
}

#[derive(Serialize)]
struct EnvContract {
    lc_all: String,
    tz: String,
    term: String,
}

#[derive(Serialize)]
struct ContextMetadata {
    binary_path: String,
    binary_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    binary_hash: Option<String>,
    help_arg_used: String,
    exit_code: Option<i32>,
    env: EnvContract,
    timestamp: u64,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let capture = capture_help(&args.binary)?;
    let help_text = select_help_text(&capture.output).ok_or_else(|| {
        anyhow!(
            "help capture produced no output: exit={:?}",
            capture.output.exit_code
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
    let help_path = out_dir.join("help.txt");
    let stdout_path = out_dir.join("help.stdout.txt");
    let stderr_path = out_dir.join("help.stderr.txt");
    let context_path = out_dir.join("context.json");

    std::fs::write(&stdout_path, &capture.output.stdout)
        .with_context(|| format!("write stdout {}", stdout_path.display()))?;
    std::fs::write(&stderr_path, &capture.output.stderr)
        .with_context(|| format!("write stderr {}", stderr_path.display()))?;
    std::fs::write(&help_path, help_text)
        .with_context(|| format!("write help text {}", help_path.display()))?;

    let binary_hash = match compute_binary_hash(&args.binary) {
        Ok(hash) => Some(hash),
        Err(err) => {
            eprintln!("warning: failed to hash binary: {err}");
            None
        }
    };
    let metadata = ContextMetadata {
        binary_path: args.binary.display().to_string(),
        binary_name: name.to_string(),
        binary_hash,
        help_arg_used: capture.arg,
        exit_code: capture.output.exit_code,
        env: EnvContract {
            lc_all: "C".to_string(),
            tz: "UTC".to_string(),
            term: "dumb".to_string(),
        },
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    let metadata_json =
        serde_json::to_string_pretty(&metadata).context("serialize context metadata")?;
    std::fs::write(&context_path, metadata_json)
        .with_context(|| format!("write context metadata {}", context_path.display()))?;

    println!("help: {}", help_path.display());
    Ok(())
}

fn capture_help(binary: &Path) -> Result<HelpCapture> {
    let primary = capture_output(binary, &["--help"])?;
    if select_help_text(&primary).is_some() {
        return Ok(HelpCapture {
            arg: "--help".to_string(),
            output: primary,
        });
    }
    let fallback = capture_output(binary, &["-h"])?;
    if select_help_text(&fallback).is_some() {
        return Ok(HelpCapture {
            arg: "-h".to_string(),
            output: fallback,
        });
    }
    Ok(HelpCapture {
        arg: "--help".to_string(),
        output: primary,
    })
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

fn compute_binary_hash(binary: &Path) -> Result<String> {
    let mut file = std::fs::File::open(binary)
        .with_context(|| format!("open binary for hashing {}", binary.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}
