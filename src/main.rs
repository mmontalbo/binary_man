use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

mod claims;
mod schema;
mod validate;
use crate::claims::parse_help_text;
use schema::{
    compute_binary_identity, compute_binary_identity_with_env, ClaimsFile, EnvSnapshot,
    RegenerationReport, ValidationReport,
};
use validate::{option_from_claim_id, validate_option_existence, validation_env};

#[derive(Parser, Debug)]
#[command(name = "bvm", version, about = "Binary-validated man page generator")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Parse inputs (man/help/source excerpts) into a claim set
    Claims(ClaimsArgs),
    /// Validate claims by executing the binary under controlled conditions
    Validate(ValidateArgs),
    /// Regenerate a man page and report from validated claims
    Regenerate(RegenerateArgs),
}

#[derive(Parser, Debug)]
struct ClaimsArgs {
    /// Path to the binary (required for capture mode)
    #[arg(long)]
    binary: Option<PathBuf>,

    /// Capture --help from the binary under a controlled environment
    #[arg(long, conflicts_with = "help_text", requires = "binary")]
    capture_help: bool,

    /// Optional argv0 label for help-derived claims
    #[arg(long)]
    argv0: Option<String>,

    /// Environment overrides for capture mode (LC_ALL=...,TZ=...,TERM=...)
    #[arg(long, value_name = "KV", requires = "capture_help")]
    env: Option<String>,

    /// Optional output path for captured --help text
    #[arg(long, value_name = "PATH", requires = "capture_help")]
    out_help: Option<PathBuf>,

    /// Path to an existing man page to parse
    #[arg(long)]
    man: Option<PathBuf>,

    /// Path to a file containing --help output
    #[arg(long, value_name = "PATH", conflicts_with = "capture_help")]
    help_text: Option<PathBuf>,

    /// Path to a file containing source excerpts to parse as claims
    #[arg(long, value_name = "PATH")]
    source: Option<PathBuf>,

    /// Output path for generated claims JSON
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct ValidateArgs {
    /// Path to the binary under test
    #[arg(long)]
    binary: PathBuf,

    /// Path to claims JSON
    #[arg(long)]
    claims: PathBuf,

    /// Output path for validation report JSON
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct RegenerateArgs {
    /// Path to the binary under test
    #[arg(long)]
    binary: PathBuf,

    /// Path to claims JSON
    #[arg(long)]
    claims: PathBuf,

    /// Path to validation results JSON
    #[arg(long)]
    results: PathBuf,

    /// Output path for regenerated man page
    #[arg(long, value_name = "PATH")]
    out_man: PathBuf,

    /// Output path for machine-readable report
    #[arg(long, value_name = "PATH")]
    out_report: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Claims(args) => cmd_claims(args),
        Commands::Validate(args) => cmd_validate(args),
        Commands::Regenerate(args) => cmd_regenerate(args),
    }
}

fn cmd_claims(args: ClaimsArgs) -> Result<()> {
    println!("TODO: parse documentation inputs into a claims JSON set.");
    print_opt_path("binary", &args.binary);
    println!("capture_help: {}", args.capture_help);
    if let Some(argv0) = &args.argv0 {
        println!("argv0: {argv0}");
    }
    if let Some(env) = &args.env {
        println!("env: {env}");
    }
    print_opt_path("out_help", &args.out_help);
    print_opt_path("man", &args.man);
    print_opt_path("help_text", &args.help_text);
    print_opt_path("source", &args.source);
    print_opt_path("out", &args.out);

    let mut claims = Vec::new();
    let mut binary_identity = None;
    let mut capture_error = None;

    if args.capture_help {
        let binary = args
            .binary
            .as_ref()
            .ok_or_else(|| anyhow!("--binary is required for --capture-help"))?;
        let env = parse_capture_env(args.env.as_deref())?;
        let capture = capture_help(binary, &env)?;

        let source_path = if let Some(out_help) = &args.out_help {
            std::fs::write(out_help, &capture.stdout)?;
            out_help.display().to_string()
        } else {
            "<captured:--help>".to_string()
        };

        binary_identity = Some(compute_binary_identity_with_env(binary, env.clone())?);

        if !capture.status.success() || !capture.stderr.trim().is_empty() {
            capture_error = Some(format!(
                "--help capture failed: exit={:?}, stderr={}",
                capture.status.code(),
                capture.stderr.trim()
            ));
        } else {
            let help_claims = parse_help_text(&source_path, &capture.stdout);
            println!(
                "Parsed {} surface claims from captured help text.",
                help_claims.len()
            );
            claims.extend(help_claims);
        }
    } else if let Some(help_path) = &args.help_text {
        let content = std::fs::read_to_string(help_path)?;
        let source_path = help_path.display().to_string();
        let help_claims = parse_help_text(&source_path, &content);
        println!(
            "Parsed {} surface claims from help text.",
            help_claims.len()
        );
        claims.extend(help_claims);
    }
    if let Some(out) = &args.out {
        if args.help_text.is_some() && args.binary.is_some() && !args.capture_help {
            println!(
                "Note: binary identity omitted for file-derived help text. Bind identity during validation."
            );
        }
        let invocation = resolve_invocation(&args);
        let claims = ClaimsFile {
            binary_identity,
            invocation,
            capture_error,
            claims,
        };
        write_json(out, &claims)?;
        println!("Wrote claims file to {}", out.display());
    }
    println!("Next: implement parsers and claim schema serialization.");
    Ok(())
}

fn cmd_validate(args: ValidateArgs) -> Result<()> {
    let claims: ClaimsFile = read_json(&args.claims)?;
    let env = validation_env();
    let binary_identity = compute_binary_identity_with_env(&args.binary, env.clone())?;
    let mut results = Vec::new();

    for claim in claims.claims {
        if let Some(option) = option_from_claim_id(&claim.id) {
            let result = validate_option_existence(&args.binary, &claim.id, &option, &env);
            results.push(result);
        }
    }

    if let Some(out) = &args.out {
        let report = ValidationReport {
            binary_identity,
            results,
        };
        write_json(out, &report)?;
        println!("Wrote validation report to {}", out.display());
    }
    Ok(())
}

fn cmd_regenerate(args: RegenerateArgs) -> Result<()> {
    println!("TODO: regenerate a man page and report from validated claims.");
    println!("binary: {}", args.binary.display());
    println!("claims: {}", args.claims.display());
    println!("results: {}", args.results.display());
    println!("out_man: {}", args.out_man.display());
    print_opt_path("out_report", &args.out_report);
    if let Some(out_report) = &args.out_report {
        let binary_identity = compute_binary_identity(&args.binary)?;
        let report = RegenerationReport {
            binary_identity,
            claims_path: args.claims.clone(),
            results_path: args.results.clone(),
            out_man: args.out_man.clone(),
        };
        write_json(out_report, &report)?;
        println!("Wrote regeneration skeleton to {}", out_report.display());
    }
    println!("Next: implement formatter, unknowns handling, and output binding to binary hash.");
    Ok(())
}

fn print_opt_path(label: &str, path: &Option<PathBuf>) {
    let rendered = match path {
        Some(p) => p.display().to_string(),
        None => "<none>".to_string(),
    };
    println!("{label}: {rendered}");
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    std::fs::write(path, json)?;
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let content = std::fs::read_to_string(path)?;
    let value = serde_json::from_str(&content)?;
    Ok(value)
}

fn resolve_invocation(args: &ClaimsArgs) -> Option<String> {
    if let Some(argv0) = &args.argv0 {
        return Some(argv0.clone());
    }
    if args.capture_help {
        if let Some(binary) = &args.binary {
            if let Some(name) = binary.file_name() {
                return Some(name.to_string_lossy().to_string());
            }
        }
    }
    None
}

fn default_capture_env() -> EnvSnapshot {
    EnvSnapshot {
        locale: "C".to_string(),
        tz: "UTC".to_string(),
        term: "dumb".to_string(),
    }
}

fn parse_capture_env(raw: Option<&str>) -> Result<EnvSnapshot> {
    let mut env = default_capture_env();
    let Some(raw) = raw else {
        return Ok(env);
    };

    for pair in raw.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid env override: {pair}"))?;
        match key {
            "LC_ALL" => env.locale = value.to_string(),
            "TZ" => env.tz = value.to_string(),
            "TERM" => env.term = value.to_string(),
            _ => return Err(anyhow!("unsupported env override key: {key}")),
        }
    }
    Ok(env)
}

struct HelpCapture {
    stdout: String,
    stderr: String,
    status: ExitStatus,
}

fn capture_help(binary: &Path, env: &EnvSnapshot) -> Result<HelpCapture> {
    let output = Command::new(binary)
        .arg("--help")
        .env_clear()
        .env("LC_ALL", &env.locale)
        .env("TZ", &env.tz)
        .env("TERM", &env.term)
        .output()?;

    Ok(HelpCapture {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        status: output.status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn capture_help_from_true_if_available() {
        let Some(binary) = find_in_path("true") else {
            return;
        };

        let env = default_capture_env();
        let capture = match capture_help(&binary, &env) {
            Ok(result) => result,
            Err(_) => return,
        };

        if !capture.status.success() || !capture.stderr.trim().is_empty() {
            return;
        }

        assert!(!capture.stdout.trim().is_empty());
    }

    fn find_in_path(name: &str) -> Option<PathBuf> {
        let path_var = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }
}
