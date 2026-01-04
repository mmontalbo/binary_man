//! CLI entry points for claim extraction, validation, and regeneration.
//!
//! The claims workflow is designed to be deterministic: help text is captured
//! in a controlled environment, then parsed into a versioned, surface-level
//! claim set with provenance.

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

mod claims;
mod schema;
mod validate;
use crate::claims::parse_help_text;
use schema::{
    compute_binary_identity_with_env, BinaryIdentity, Claim, ClaimSource, ClaimsFile, EnvSnapshot,
    RegenerationReport, ValidationReport, ValidationResult, ValidationStatus,
};
use validate::{
    option_from_binding_claim_id, option_from_claim_id, validate_option_binding,
    validate_option_existence, validation_env,
};

#[derive(Parser, Debug)]
#[command(name = "bvm", version, about = "Binary-validated man page generator")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Parse help output into a claim set
    Claims(ClaimsArgs),
    /// Validate claims by executing the binary under controlled conditions
    Validate(ValidateArgs),
    /// Regenerate a man page and report from validated claims
    Regenerate(RegenerateArgs),
}

#[derive(Parser, Debug)]
struct ClaimsArgs {
    /// Path to the binary under test
    #[arg(long)]
    binary: PathBuf,

    /// Output path for generated claims JSON
    #[arg(long, value_name = "PATH")]
    out: PathBuf,
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

// Capture help output, parse claims, and write a claims file.
fn cmd_claims(args: ClaimsArgs) -> Result<()> {
    println!("Capturing help output into a claims JSON set.");
    println!("binary: {}", args.binary.display());
    println!("out: {}", args.out.display());
    let env = default_capture_env();
    let capture = capture_help(&args.binary, &env)?;

    let help_text = select_help_text(&capture).ok_or_else(|| {
        anyhow!(
            "--help capture produced no output: exit={:?}",
            capture.status.code()
        )
    })?;
    let source_path = format!("<captured:{}>", capture.arg);

    let binary_identity = compute_binary_identity_with_env(&args.binary, env.clone())?;
    if !capture.status.success() {
        return Err(anyhow!(
            "--help capture failed: exit={:?}, stderr={}",
            capture.status.code(),
            capture.stderr.trim()
        ));
    }
    let claims = parse_help_text(&source_path, help_text);
    println!(
        "Parsed {} surface claims from captured help text.",
        claims.len()
    );
    let claims = ClaimsFile {
        invoked_path: args.binary.clone(),
        binary_identity,
        claims,
    };
    write_json(&args.out, &claims)?;
    println!("Wrote claims file to {}", args.out.display());
    Ok(())
}

fn cmd_validate(args: ValidateArgs) -> Result<()> {
    let claims: ClaimsFile = read_json(&args.claims)?;
    let env = validation_env();
    let binary_identity = compute_binary_identity_with_env(&args.binary, env.clone())?;
    if !binary_identity_matches(&claims.binary_identity, &binary_identity) {
        return Err(anyhow!(
            "claims binary identity does not match --binary (expected {}, got {})",
            summarize_identity(&claims.binary_identity),
            summarize_identity(&binary_identity)
        ));
    }
    let mut results = Vec::new();

    for claim in claims.claims {
        if let Some(option) = option_from_claim_id(&claim.id) {
            let result = validate_option_existence(&args.binary, &claim, &option, &env);
            results.push(result);
        } else if option_from_binding_claim_id(&claim.id).is_some() {
            let result = validate_option_binding(&args.binary, &claim, &env);
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
    let claims_file: ClaimsFile = read_json(&args.claims)?;
    let report: ValidationReport = read_json(&args.results)?;
    let binary_identity = report.binary_identity;
    let results_map: BTreeMap<String, ValidationResult> = report
        .results
        .into_iter()
        .map(|result| (result.claim_id.clone(), result))
        .collect();

    let invocation = claims_file
        .invoked_path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .or_else(|| {
            args.binary
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .or_else(|| {
            binary_identity
                .path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "binary".to_string());
    let version = capture_version(&args.binary, &binary_identity.env);
    let man_page = render_man_page(
        &invocation,
        &claims_file.invoked_path,
        &binary_identity,
        version.as_deref(),
        &claims_file.claims,
        &results_map,
    );

    std::fs::write(&args.out_man, man_page)?;
    println!("Wrote regenerated man page to {}", args.out_man.display());

    if let Some(out_report) = &args.out_report {
        let report = RegenerationReport {
            binary_identity,
            claims_path: args.claims.clone(),
            results_path: args.results.clone(),
            out_man: args.out_man.clone(),
        };
        write_json(out_report, &report)?;
        println!("Wrote regeneration report to {}", out_report.display());
    }
    Ok(())
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

fn binary_identity_matches(expected: &BinaryIdentity, actual: &BinaryIdentity) -> bool {
    expected.path == actual.path
        && expected.hash.algo == actual.hash.algo
        && expected.hash.value == actual.hash.value
        && expected.platform.os == actual.platform.os
        && expected.platform.arch == actual.platform.arch
        && expected.env.locale == actual.env.locale
        && expected.env.tz == actual.env.tz
        && expected.env.term == actual.env.term
}

fn summarize_identity(identity: &BinaryIdentity) -> String {
    format!(
        "path={} hash={}:{} platform={}/{} env=LC_ALL={} TZ={} TERM={}",
        identity.path.display(),
        identity.hash.algo,
        identity.hash.value,
        identity.platform.os,
        identity.platform.arch,
        identity.env.locale,
        identity.env.tz,
        identity.env.term
    )
}

// Use a stable environment to reduce help output variance.
// - `LC_ALL=C` keeps messages in a consistent locale.
// - `TERM=dumb` discourages ANSI color/pager output.
// - `TZ=UTC` avoids time-dependent formatting.
fn default_capture_env() -> EnvSnapshot {
    EnvSnapshot {
        locale: "C".to_string(),
        tz: "UTC".to_string(),
        term: "dumb".to_string(),
    }
}

// Raw output from a single help invocation.
struct HelpCapture {
    stdout: String,
    stderr: String,
    status: ExitStatus,
    arg: String,
}

// Prefer stdout for help text, but fall back to stderr when needed.
// Some binaries emit help or usage text on stderr even when exit status is
// non-zero, so selection is content-based rather than status-based.
fn select_help_text(capture: &HelpCapture) -> Option<&str> {
    if !capture.stdout.trim().is_empty() {
        Some(capture.stdout.as_str())
    } else if !capture.stderr.trim().is_empty() {
        Some(capture.stderr.as_str())
    } else {
        None
    }
}

// Capture help output, trying `--help` first and falling back to `-h`.
// The first invocation with any non-empty output wins; this avoids rejecting
// help text from tools that print usage only on stderr.
fn capture_help(binary: &Path, env: &EnvSnapshot) -> Result<HelpCapture> {
    let primary = capture_help_with_arg(binary, env, "--help")?;
    if select_help_text(&primary).is_some() {
        return Ok(primary);
    }
    let fallback = capture_help_with_arg(binary, env, "-h")?;
    if select_help_text(&fallback).is_some() {
        return Ok(fallback);
    }
    Ok(primary)
}

// Invoke the binary with a help flag and return raw output.
// The environment is cleared to keep outputs deterministic; callers decide
// how to interpret exit status or stderr content.
fn capture_help_with_arg(binary: &Path, env: &EnvSnapshot, arg: &str) -> Result<HelpCapture> {
    let output = Command::new(binary)
        .arg(arg)
        .env_clear()
        .env("LC_ALL", &env.locale)
        .env("TZ", &env.tz)
        .env("TERM", &env.term)
        .output()?;

    Ok(HelpCapture {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        status: output.status,
        arg: arg.to_string(),
    })
}

#[derive(Clone, Copy, Debug)]
enum ClaimTier {
    T0,
    T1,
    Other,
}

#[derive(Clone, Copy, Debug)]
enum RegenStatus {
    Confirmed,
    Refuted,
    Undetermined,
}

#[derive(Debug, Default)]
struct TierBuckets {
    confirmed: Vec<ClaimSummary>,
    refuted: Vec<ClaimSummary>,
    undetermined: Vec<ClaimSummary>,
}

#[derive(Debug)]
struct ClaimSummary {
    id: String,
    text: String,
    source: String,
    reason: Option<String>,
}

fn capture_version(binary: &Path, env: &EnvSnapshot) -> Option<String> {
    let output = Command::new(binary)
        .arg("--version")
        .env_clear()
        .env("LC_ALL", &env.locale)
        .env("TZ", &env.tz)
        .env("TERM", &env.term)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .or_else(|| stderr.lines().find(|line| !line.trim().is_empty()))?;
    Some(line.trim().to_string())
}

fn render_man_page(
    invocation: &str,
    invoked_path: &Path,
    binary_identity: &BinaryIdentity,
    version: Option<&str>,
    claims: &[Claim],
    results_map: &BTreeMap<String, ValidationResult>,
) -> String {
    let mut t0 = TierBuckets::default();
    let mut t1 = TierBuckets::default();

    for claim in claims {
        let tier = tier_from_claim_id(&claim.id);
        if matches!(tier, ClaimTier::Other) {
            continue;
        }
        let result = results_map.get(&claim.id);
        let (status, reason) = regen_status_and_reason(claim, result);
        let summary = ClaimSummary {
            id: claim.id.clone(),
            text: claim.text.clone(),
            source: format_source(&claim.source),
            reason,
        };
        match tier {
            ClaimTier::T0 => push_summary(&mut t0, status, summary),
            ClaimTier::T1 => push_summary(&mut t1, status, summary),
            ClaimTier::Other => {}
        }
    }

    let mut out = String::new();
    let title = invocation
        .split_whitespace()
        .next()
        .unwrap_or("binary")
        .to_ascii_uppercase();
    push_raw_line(&mut out, &format!(".TH {} 1", title));
    push_raw_line(&mut out, ".SH NAME");
    push_text_line(
        &mut out,
        &format!("{invocation} - binary-validated man page"),
    );

    push_raw_line(&mut out, ".SH BINARY IDENTITY");
    push_raw_line(&mut out, ".nf");
    push_text_line(
        &mut out,
        &format!("Invoked Path: {}", invoked_path.display()),
    );
    push_text_line(
        &mut out,
        &format!("Path: {}", binary_identity.path.display()),
    );
    push_text_line(
        &mut out,
        &format!(
            "Hash: {}:{}",
            binary_identity.hash.algo, binary_identity.hash.value
        ),
    );
    push_text_line(
        &mut out,
        &format!("Version: {}", version.unwrap_or("unknown")),
    );
    push_text_line(
        &mut out,
        &format!(
            "Platform: {}/{}",
            binary_identity.platform.os, binary_identity.platform.arch
        ),
    );
    push_text_line(
        &mut out,
        &format!(
            "Environment: LC_ALL={} TZ={} TERM={}",
            binary_identity.env.locale, binary_identity.env.tz, binary_identity.env.term
        ),
    );
    push_raw_line(&mut out, ".fi");

    let coverage = coverage_from_results(results_map.values());
    render_coverage_summary(&mut out, &coverage);

    // Regeneration is claim-driven: only T0/T1 surface claims in the input file
    // are emitted; validation results without matching claims are intentionally ignored.
    render_tier_section(&mut out, "T0 OPTION EXISTENCE", &t0);
    render_tier_section(&mut out, "T1 PARAMETER BINDING", &t1);

    push_raw_line(&mut out, ".SH HIGHER TIERS");
    push_raw_line(&mut out, ".TP");
    push_text_line(&mut out, "T2 parameter form");
    push_text_line(&mut out, "Not evaluated.");
    push_raw_line(&mut out, ".TP");
    push_text_line(&mut out, "T3 parameter domain/type");
    push_text_line(&mut out, "Not evaluated.");
    push_raw_line(&mut out, ".TP");
    push_text_line(&mut out, "T4 behavior semantics");
    push_text_line(&mut out, "Not evaluated.");

    out
}

#[derive(Debug, Default)]
struct CoverageSummary {
    t0_confirmed: usize,
    t0_refuted: usize,
    t0_undetermined: usize,
    t1_confirmed: usize,
    t1_refuted: usize,
    t1_undetermined: usize,
}

fn coverage_from_results<'a, I>(results: I) -> CoverageSummary
where
    I: IntoIterator<Item = &'a ValidationResult>,
{
    let mut summary = CoverageSummary::default();
    for result in results {
        match tier_from_claim_id(&result.claim_id) {
            ClaimTier::T0 => match result.status {
                ValidationStatus::Confirmed => summary.t0_confirmed += 1,
                ValidationStatus::Refuted => summary.t0_refuted += 1,
                ValidationStatus::Undetermined => summary.t0_undetermined += 1,
            },
            ClaimTier::T1 => match result.status {
                ValidationStatus::Confirmed => summary.t1_confirmed += 1,
                ValidationStatus::Refuted => summary.t1_refuted += 1,
                ValidationStatus::Undetermined => summary.t1_undetermined += 1,
            },
            ClaimTier::Other => {}
        }
    }
    summary
}

fn render_coverage_summary(out: &mut String, summary: &CoverageSummary) {
    push_raw_line(out, ".SH SURFACE COVERAGE SUMMARY");
    push_raw_line(out, ".nf");
    push_text_line(
        out,
        &format!(
            "T0 (existence): confirmed {}; refuted {}; undetermined {}",
            summary.t0_confirmed, summary.t0_refuted, summary.t0_undetermined
        ),
    );
    let mut t1_line = format!(
        "T1 (parameter binding): confirmed {}; undetermined {}",
        summary.t1_confirmed, summary.t1_undetermined
    );
    if summary.t1_refuted > 0 {
        t1_line.push_str(&format!("; refuted {}", summary.t1_refuted));
    }
    push_text_line(out, &t1_line);
    push_text_line(out, "T2-T4: not evaluated");
    push_raw_line(out, ".fi");
}

fn tier_from_claim_id(id: &str) -> ClaimTier {
    if !id.starts_with("claim:option:opt=") {
        return ClaimTier::Other;
    }
    if id.ends_with(":exists") {
        ClaimTier::T0
    } else if id.ends_with(":binding") {
        ClaimTier::T1
    } else {
        ClaimTier::Other
    }
}

fn regen_status_and_reason(
    _claim: &Claim,
    result: Option<&ValidationResult>,
) -> (RegenStatus, Option<String>) {
    if let Some(result) = result {
        let status = match result.status {
            ValidationStatus::Confirmed => RegenStatus::Confirmed,
            ValidationStatus::Refuted => RegenStatus::Refuted,
            ValidationStatus::Undetermined => RegenStatus::Undetermined,
        };
        return (status, result.reason.clone());
    }
    (
        RegenStatus::Undetermined,
        Some("no validation result".to_string()),
    )
}

fn format_source(source: &ClaimSource) -> String {
    match source.line {
        Some(line) => format!("{}:{}", source.path, line),
        None => source.path.clone(),
    }
}

fn push_summary(bucket: &mut TierBuckets, status: RegenStatus, summary: ClaimSummary) {
    match status {
        RegenStatus::Confirmed => bucket.confirmed.push(summary),
        RegenStatus::Refuted => bucket.refuted.push(summary),
        RegenStatus::Undetermined => bucket.undetermined.push(summary),
    }
}

fn render_tier_section(out: &mut String, title: &str, bucket: &TierBuckets) {
    push_raw_line(out, &format!(".SH {}", title));
    render_status_section(out, "Confirmed", &bucket.confirmed);
    render_status_section(out, "Refuted", &bucket.refuted);
    render_status_section(out, "Undetermined", &bucket.undetermined);
}

fn render_status_section(out: &mut String, label: &str, entries: &[ClaimSummary]) {
    push_raw_line(out, &format!(".SS {}", label));
    if entries.is_empty() {
        push_text_line(out, "None.");
        return;
    }
    for entry in entries {
        push_raw_line(out, ".TP");
        push_text_line(out, &entry.id);
        let mut detail = format!("{} (source: {})", entry.text, entry.source);
        if let Some(reason) = &entry.reason {
            let trimmed = reason.trim();
            if !trimmed.is_empty() {
                detail.push_str(&format!("; reason: {}", trimmed));
            }
        }
        push_text_line(out, &detail);
    }
}

fn push_raw_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

fn push_text_line(out: &mut String, line: &str) {
    push_raw_line(out, &escape_roff_text(line));
}

fn escape_roff_text(text: &str) -> String {
    let mut escaped = text.replace('\\', "\\\\");
    if escaped.starts_with('.') || escaped.starts_with('\'') {
        escaped.insert_str(0, "\\&");
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ClaimKind, ClaimSourceType, ClaimStatus};
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

    #[test]
    fn missing_validation_results_are_undetermined() {
        let claim = make_claim(
            "claim:option:opt=--quiet:exists",
            ClaimSourceType::Help,
            "help.txt",
            Some(1),
        );
        let (status, reason) = regen_status_and_reason(&claim, None);
        assert!(matches!(status, RegenStatus::Undetermined));
        assert_eq!(reason.as_deref(), Some("no validation result"));
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

    fn make_claim(id: &str, source_type: ClaimSourceType, path: &str, line: Option<u64>) -> Claim {
        Claim {
            id: id.to_string(),
            text: "test".to_string(),
            kind: ClaimKind::Option,
            source: ClaimSource {
                source_type,
                path: path.to_string(),
                line,
            },
            status: ClaimStatus::Confirmed,
            extractor: "test".to_string(),
            raw_excerpt: "raw".to_string(),
            confidence: None,
        }
    }
}
