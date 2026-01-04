//! Validation helpers for surface-level option claims.
//!
//! Validation is intentionally conservative: it runs bounded, low-impact probes
//! and relies on explicit error markers rather than exit status alone.
//!
//! ## Pipeline summary
//! - **Existence**: run `<opt> --help` and look for unrecognized/ambiguous markers.
//! - **Binding**: run missing-arg and with-arg probes and compare responses.
//! - **Evidence**: store hashed stdout/stderr plus marker notes.
//!
//! ## Example walkthroughs
//! Existence claim (`--all`) confirmed when no unrecognized marker is present:
//! ```text
//! $ tool --all --help
//! (exit 0, no "unrecognized option" marker)
//! -> confirmed
//! ```
//! Existence claim (`--nope`) refuted when the error mentions the option:
//! ```text
//! $ tool --nope --help
//! error: unrecognized option '--nope'
//! -> refuted
//! ```
//! Required binding confirmed by missing-arg response:
//! ```text
//! $ tool --size --help
//! option '--size' requires an argument
//! $ tool --size __bvm__ --help
//! (no missing-arg error)
//! -> confirmed (required)
//! ```
//! Optional binding confirmed when missing-arg is OK but invalid arg is rejected:
//! ```text
//! $ tool --color --help
//! (exit 0)
//! $ tool --color __bvm__ --help
//! invalid argument '__bvm__' for '--color'
//! -> confirmed (optional)
//! ```

use crate::claims::parse_help_row_options;
use crate::schema::{
    Claim, Determinism, EnvSnapshot, Evidence, ValidationMethod, ValidationResult, ValidationStatus,
};
use regex::Regex;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

const UNRECOGNIZED_MARKERS: &[&str] = &[
    "unrecognized option",
    "unknown option",
    "invalid option",
    "illegal option",
    "unknown flag",
    "unrecognized flag",
    "invalid flag",
    "unknown switch",
    "invalid switch",
];

const AMBIGUOUS_MARKERS: &[&str] = &["ambiguous option", "option is ambiguous"];

const ARGUMENT_ERROR_MARKERS: &[&str] = &[
    "requires an argument",
    "requires a value",
    "option requires an argument",
    "option requires a value",
    "missing argument",
    "missing value",
    "invalid argument",
];

const MISSING_ARGUMENT_MARKERS: &[&str] = &[
    "requires an argument",
    "requires a value",
    "option requires an argument",
    "option requires a value",
    "missing argument",
    "missing value",
];

const ARGUMENT_NOT_ALLOWED_MARKERS: &[&str] = &[
    "doesn't allow an argument",
    "does not allow an argument",
    "doesn't allow a value",
    "does not allow a value",
    "doesn't take an argument",
    "does not take an argument",
    "doesn't take a value",
    "does not take a value",
    "doesn't accept an argument",
    "does not accept an argument",
    "takes no argument",
    "takes no value",
    "argument not allowed",
    "value not allowed",
];

const INVALID_ARGUMENT_MARKERS: &[&str] = &["invalid argument", "invalid value"];

const BINDING_DUMMY_VALUE: &str = "__bvm__";

struct ExecutionAttempt {
    args: Vec<String>,
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    spawn_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingExpectation {
    Required,
    Optional,
}

#[derive(Debug, Clone)]
struct BindingSpec {
    option: String,
    expectation: Option<BindingExpectation>,
    form: Option<BindingForm>,
}

#[derive(Debug, Clone)]
enum BindingForm {
    Attached(String),
    Trailing(String),
}

#[derive(Debug)]
struct AttemptAnalysis {
    unrecognized: bool,
    missing_arg: bool,
    arg_not_allowed: bool,
    invalid_arg: bool,
    ambiguous: bool,
    help_like: bool,
    argument_error: bool,
    exit_code: Option<i32>,
    notes: Vec<String>,
}

/// Default validation environment (`LC_ALL=C`, `TZ=UTC`, `TERM=dumb`).
pub fn validation_env() -> EnvSnapshot {
    EnvSnapshot {
        locale: "C".to_string(),
        tz: "UTC".to_string(),
        term: "dumb".to_string(),
    }
}

/// Extract the canonical option token from an option existence claim ID.
///
/// # Examples
/// ```ignore
/// use crate::validate::option_from_claim_id;
/// assert_eq!(
///     option_from_claim_id("claim:option:opt=--all:exists"),
///     Some("--all".to_string())
/// );
/// assert_eq!(option_from_claim_id("claim:option:opt=--all:binding"), None);
/// ```
pub fn option_from_claim_id(id: &str) -> Option<String> {
    const PREFIX: &str = "claim:option:opt=";
    const SUFFIX: &str = ":exists";
    if !id.starts_with(PREFIX) || !id.ends_with(SUFFIX) {
        return None;
    }
    let option = &id[PREFIX.len()..id.len().saturating_sub(SUFFIX.len())];
    if option.is_empty() {
        None
    } else {
        Some(option.to_string())
    }
}

/// Extract the canonical option token from a parameter binding claim ID.
///
/// # Examples
/// ```ignore
/// use crate::validate::option_from_binding_claim_id;
/// assert_eq!(
///     option_from_binding_claim_id("claim:option:opt=--size:binding"),
///     Some("--size".to_string())
/// );
/// assert_eq!(option_from_binding_claim_id("claim:option:opt=--size:exists"), None);
/// ```
pub fn option_from_binding_claim_id(id: &str) -> Option<String> {
    const PREFIX: &str = "claim:option:opt=";
    const SUFFIX: &str = ":binding";
    if !id.starts_with(PREFIX) || !id.ends_with(SUFFIX) {
        return None;
    }
    let option = &id[PREFIX.len()..id.len().saturating_sub(SUFFIX.len())];
    if option.is_empty() {
        None
    } else {
        Some(option.to_string())
    }
}

/// Execute a harmless invocation and classify the option existence claim.
///
/// The probe always appends `--help` to minimize side effects.
///
/// # Examples
/// ```ignore
/// # use crate::validate::validate_option_existence;
/// # use crate::validate::validation_env;
/// # use std::path::Path;
/// let env = validation_env();
/// let claim = crate::schema::Claim {
///     id: "claim:option:opt=--all:exists".to_string(),
///     text: "Option --all is listed in help output.".to_string(),
///     kind: crate::schema::ClaimKind::Option,
///     source: crate::schema::ClaimSource { source_type: crate::schema::ClaimSourceType::Help, path: "<captured:--help>".to_string(), line: None },
///     status: crate::schema::ClaimStatus::Unvalidated,
///     extractor: "parse:help:v1".to_string(),
///     raw_excerpt: "--all  show hidden entries".to_string(),
///     confidence: Some(0.9),
/// };
/// let result = validate_option_existence(Path::new("tool"), &claim, "--all", &env);
/// assert!(matches!(result.status, crate::schema::ValidationStatus::Confirmed | crate::schema::ValidationStatus::Undetermined));
/// ```
pub fn validate_option_existence(
    binary: &Path,
    claim: &Claim,
    option: &str,
    env: &EnvSnapshot,
) -> ValidationResult {
    let args = vec![option.to_string(), "--help".to_string()];
    let attempt = run_attempt(binary, args, env);
    let (status, notes) = classify_attempt(option, &attempt);
    let mut attempts = vec![evidence_for_attempt(attempt, env, notes)];

    for alias in alias_options_from_claim(claim, option) {
        let args = vec![alias.clone(), "--help".to_string()];
        let attempt = run_attempt(binary, args, env);
        let (alias_status, alias_note) = classify_attempt(&alias, &attempt);
        let note = alias_evidence_note(option, &alias, alias_status, alias_note);
        attempts.push(evidence_for_attempt(attempt, env, note));
    }

    ValidationResult {
        claim_id: claim.id.clone(),
        status,
        method: ValidationMethod::AcceptanceTest,
        determinism: Some(Determinism::Deterministic),
        attempts,
        observed: None,
        reason: None,
    }
}

/// Execute controlled invocations and classify the parameter binding claim.
///
/// The validator runs two probes:
/// - **Missing arg**: `<opt> --help`
/// - **With arg**: `<opt> __bvm__ --help` (or `--opt=__bvm__` when attached)
///
/// # Examples
/// ```ignore
/// # use crate::validate::validate_option_binding;
/// # use crate::validate::validation_env;
/// # use crate::schema::Claim;
/// # use std::path::Path;
/// let env = validation_env();
/// let claim = Claim {
///     id: "claim:option:opt=--size:binding".to_string(),
///     text: "Option --size requires a value in `--size=SIZE` form.".to_string(),
///     kind: crate::schema::ClaimKind::Option,
///     source: crate::schema::ClaimSource { source_type: crate::schema::ClaimSourceType::Help, path: "<captured:--help>".to_string(), line: None },
///     status: crate::schema::ClaimStatus::Unvalidated,
///     extractor: "parse:help:v1".to_string(),
///     raw_excerpt: "--size=SIZE".to_string(),
///     confidence: Some(0.7),
/// };
/// let result = validate_option_binding(Path::new("tool"), &claim, &env);
/// assert!(matches!(result.status, crate::schema::ValidationStatus::Confirmed | crate::schema::ValidationStatus::Undetermined | crate::schema::ValidationStatus::Refuted));
/// ```
pub fn validate_option_binding(
    binary: &Path,
    claim: &Claim,
    env: &EnvSnapshot,
) -> ValidationResult {
    let Some(spec) = binding_spec_from_claim(claim) else {
        return ValidationResult {
            claim_id: claim.id.clone(),
            status: ValidationStatus::Undetermined,
            method: ValidationMethod::AcceptanceTest,
            determinism: Some(Determinism::Deterministic),
            attempts: Vec::new(),
            observed: None,
            reason: Some("unrecognized parameter binding claim".to_string()),
        };
    };

    let missing_args = vec![spec.option.clone(), "--help".to_string()];
    let missing_attempt = run_attempt(binary, missing_args, env);
    let missing_analysis = attempt_signals(&spec.option, &missing_attempt);

    let mut with_arg_args = build_with_arg_args(spec.form.as_ref(), &spec.option);
    with_arg_args.push("--help".to_string());
    let with_arg_attempt = run_attempt(binary, with_arg_args, env);
    let with_arg_analysis = attempt_signals(&spec.option, &with_arg_attempt);

    let (mut status, mut reason) = match spec.expectation {
        Some(expectation) => {
            classify_binding_attempts(expectation, &missing_analysis, &with_arg_analysis)
        }
        None => (
            ValidationStatus::Undetermined,
            Some("parameter binding expectation missing".to_string()),
        ),
    };

    let mut attempts = vec![
        evidence_for_attempt(
            missing_attempt,
            env,
            build_attempt_notes("missing_arg", &missing_analysis),
        ),
        evidence_for_attempt(
            with_arg_attempt,
            env,
            build_attempt_notes("with_arg", &with_arg_analysis),
        ),
    ];
    if matches!(spec.expectation, Some(BindingExpectation::Required))
        && matches!(status, ValidationStatus::Undetermined)
    {
        let end_args = vec![spec.option.clone()];
        let end_attempt = run_attempt(binary, end_args, env);
        let end_analysis = attempt_signals(&spec.option, &end_attempt);
        attempts.push(evidence_for_attempt(
            end_attempt,
            env,
            build_attempt_notes("option_at_end", &end_analysis),
        ));
        let updated = apply_option_at_end_probe(status, reason, &end_analysis);
        status = updated.0;
        reason = updated.1;
    }

    ValidationResult {
        claim_id: claim.id.clone(),
        status,
        method: ValidationMethod::AcceptanceTest,
        determinism: Some(Determinism::Deterministic),
        attempts,
        observed: None,
        reason,
    }
}

fn classify_attempt(
    option: &str,
    attempt: &ExecutionAttempt,
) -> (ValidationStatus, Option<String>) {
    if let Some(err) = &attempt.spawn_error {
        return (
            ValidationStatus::Undetermined,
            Some(format!("spawn failed: {err}")),
        );
    }

    let Some(exit_code) = attempt.exit_code else {
        return (
            ValidationStatus::Undetermined,
            Some("terminated without exit code".to_string()),
        );
    };

    let signals = attempt_signals(option, attempt);
    if signals.unrecognized {
        return (ValidationStatus::Refuted, None);
    }
    if let Some(note) = find_note_with_prefix(&signals.notes, "unrecognized option marker") {
        return (ValidationStatus::Undetermined, Some(note));
    }
    if signals.ambiguous {
        let note = find_note_with_prefix(&signals.notes, "ambiguous option response")
            .or_else(|| Some("ambiguous option response".to_string()));
        return (ValidationStatus::Undetermined, note);
    }

    let mut notes = Vec::new();
    if exit_code != 0 {
        notes.push(format!(
            "nonzero exit ({exit_code}) without unrecognized option marker"
        ));
    }
    if signals.argument_error {
        notes.push("argument error reported".to_string());
    }

    let note = if notes.is_empty() {
        None
    } else {
        Some(notes.join("; "))
    };

    (ValidationStatus::Confirmed, note)
}

fn alias_options_from_claim(claim: &Claim, canonical: &str) -> Vec<String> {
    let mut aliases: Vec<String> = parse_help_row_options(&claim.raw_excerpt)
        .into_iter()
        .filter(|opt| opt != canonical)
        .filter(|opt| opt.starts_with('-') && !opt.starts_with("--"))
        .collect();
    aliases.sort();
    aliases.dedup();
    aliases
}

fn find_note_with_prefix(notes: &[String], prefix: &str) -> Option<String> {
    notes.iter().find(|note| note.starts_with(prefix)).cloned()
}

fn alias_evidence_note(
    canonical: &str,
    alias: &str,
    status: ValidationStatus,
    note: Option<String>,
) -> Option<String> {
    let mut parts = vec![
        "probe=alias".to_string(),
        format!("alias={alias}"),
        format!("alias_of={canonical}"),
        format!("alias_status={}", status_label(status)),
    ];
    if let Some(note) = note {
        parts.push(note);
    }
    Some(parts.join("; "))
}

fn status_label(status: ValidationStatus) -> &'static str {
    match status {
        ValidationStatus::Confirmed => "confirmed",
        ValidationStatus::Refuted => "refuted",
        ValidationStatus::Undetermined => "undetermined",
    }
}

fn evidence_for_attempt(
    attempt: ExecutionAttempt,
    env: &EnvSnapshot,
    notes: Option<String>,
) -> Evidence {
    Evidence {
        args: attempt.args,
        env: env_map(env),
        exit_code: attempt.exit_code,
        stdout: Some(hash_bytes(&attempt.stdout)),
        stderr: Some(hash_bytes(&attempt.stderr)),
        notes,
    }
}

fn binding_spec_from_claim(claim: &Claim) -> Option<BindingSpec> {
    let option = option_from_binding_claim_id(&claim.id)?;
    let expectation = parse_binding_expectation(&claim.text, &claim.raw_excerpt);
    let form = parse_binding_form(&claim.text);
    Some(BindingSpec {
        option,
        expectation,
        form,
    })
}

fn parse_binding_expectation(text: &str, raw_excerpt: &str) -> Option<BindingExpectation> {
    let lower = text.to_lowercase();
    if lower.contains("optional value") {
        return Some(BindingExpectation::Optional);
    }
    if lower.contains("requires a value") {
        return Some(BindingExpectation::Required);
    }
    if raw_excerpt.contains("[=") {
        return Some(BindingExpectation::Optional);
    }
    if raw_excerpt.contains('=') {
        return Some(BindingExpectation::Required);
    }
    None
}

fn parse_binding_form(text: &str) -> Option<BindingForm> {
    let form = extract_form_text(text)?;
    parse_binding_form_text(&form)
}

fn extract_form_text(text: &str) -> Option<String> {
    let start = text.find('`')?;
    let rest = &text[start + 1..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

fn parse_binding_form_text(form: &str) -> Option<BindingForm> {
    let trimmed = form.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(idx) = trimmed.find("[=") {
        let option = trimmed[..idx].trim();
        if !option.is_empty() {
            return Some(BindingForm::Attached(option.to_string()));
        }
    }
    if let Some(idx) = trimmed.find('=') {
        let option = trimmed[..idx].trim();
        if !option.is_empty() {
            return Some(BindingForm::Attached(option.to_string()));
        }
    }
    if let Some(token) = trimmed.split_whitespace().next() {
        if token != trimmed {
            return Some(BindingForm::Trailing(token.to_string()));
        }
    }
    None
}

fn build_with_arg_args(form: Option<&BindingForm>, option: &str) -> Vec<String> {
    build_with_value_args(form, option, BINDING_DUMMY_VALUE)
}

fn build_with_value_args(form: Option<&BindingForm>, option: &str, value: &str) -> Vec<String> {
    if let Some(form) = form {
        match form {
            BindingForm::Attached(option) => {
                return vec![format!("{option}={value}")];
            }
            BindingForm::Trailing(option) => {
                return vec![option.to_string(), value.to_string()];
            }
        }
    }
    if option.starts_with("--") {
        vec![format!("{option}={value}")]
    } else {
        vec![option.to_string(), value.to_string()]
    }
}

fn run_attempt(binary: &Path, args: Vec<String>, env: &EnvSnapshot) -> ExecutionAttempt {
    let output = Command::new(binary)
        .args(&args)
        .env_clear()
        .env("LC_ALL", &env.locale)
        .env("TZ", &env.tz)
        .env("TERM", &env.term)
        .output();

    match output {
        Ok(output) => ExecutionAttempt {
            args,
            exit_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
            spawn_error: None,
        },
        Err(err) => ExecutionAttempt {
            args,
            exit_code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            spawn_error: Some(err.to_string()),
        },
    }
}

fn attempt_signals(option: &str, attempt: &ExecutionAttempt) -> AttemptAnalysis {
    let mut notes = Vec::new();
    if let Some(err) = &attempt.spawn_error {
        notes.push(format!("spawn failed: {err}"));
        return AttemptAnalysis {
            unrecognized: false,
            missing_arg: false,
            arg_not_allowed: false,
            invalid_arg: false,
            ambiguous: false,
            help_like: false,
            argument_error: false,
            exit_code: attempt.exit_code,
            notes,
        };
    }

    if attempt.exit_code.is_none() {
        notes.push("terminated without exit code".to_string());
    }

    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&attempt.stdout),
        String::from_utf8_lossy(&attempt.stderr)
    );
    let output_lower = output.to_lowercase();
    let help_like = is_help_like_output(&output_lower);
    let argument_error = contains_any(&output_lower, ARGUMENT_ERROR_MARKERS);

    let mut unrecognized = false;
    if let Some(marker) = find_marker(&output_lower, UNRECOGNIZED_MARKERS) {
        let reported = extract_reported_options(&output);
        if reported
            .iter()
            .any(|reported| option_matches(reported, option))
        {
            unrecognized = true;
        } else if reported.is_empty() {
            notes.push(format!(
                "unrecognized option marker ({marker}) without option attribution"
            ));
        } else {
            notes.push(format!(
                "unrecognized option marker ({marker}) for {:?}",
                reported
            ));
        }
    }

    let missing_options = extract_missing_argument_options(&output);
    let mut missing_arg = missing_options
        .iter()
        .any(|reported| option_matches(reported, option));
    if !missing_options.is_empty() && !missing_arg {
        notes.push(format!("missing argument marker for {:?}", missing_options));
    } else if missing_options.is_empty() && contains_any(&output_lower, MISSING_ARGUMENT_MARKERS) {
        notes.push(
            "missing argument marker without option attribution; attributed to tested option"
                .to_string(),
        );
        missing_arg = true;
    }

    let not_allowed_options = extract_argument_not_allowed_options(&output);
    let arg_not_allowed = not_allowed_options
        .iter()
        .any(|reported| option_matches(reported, option));
    if !not_allowed_options.is_empty() && !arg_not_allowed {
        notes.push(format!(
            "argument not allowed marker for {:?}",
            not_allowed_options
        ));
    } else if not_allowed_options.is_empty()
        && contains_any(&output_lower, ARGUMENT_NOT_ALLOWED_MARKERS)
    {
        notes.push("argument not allowed marker without option attribution".to_string());
    }

    let invalid_options = extract_invalid_argument_options(&output);
    let mut invalid_arg = invalid_options
        .iter()
        .any(|reported| option_matches(reported, option));
    if !invalid_options.is_empty() && !invalid_arg {
        notes.push(format!("invalid argument marker for {:?}", invalid_options));
    }
    if !invalid_arg {
        if infer_invalid_argument_for_option(option, &output_lower) {
            invalid_arg = true;
            notes.push(
                "invalid argument marker without option attribution; attributed to tested option"
                    .to_string(),
            );
        } else if invalid_options.is_empty()
            && contains_any(&output_lower, INVALID_ARGUMENT_MARKERS)
        {
            notes.push("invalid argument marker without option attribution".to_string());
        }
    }

    let mut ambiguous = false;
    if let Some(marker) = find_marker(&output_lower, AMBIGUOUS_MARKERS) {
        ambiguous = true;
        notes.push(format!("ambiguous option response ({marker})"));
    }

    AttemptAnalysis {
        unrecognized,
        missing_arg,
        arg_not_allowed,
        invalid_arg,
        ambiguous,
        help_like,
        argument_error,
        exit_code: attempt.exit_code,
        notes,
    }
}

fn classify_binding_attempts(
    expectation: BindingExpectation,
    missing: &AttemptAnalysis,
    with_arg: &AttemptAnalysis,
) -> (ValidationStatus, Option<String>) {
    if missing.unrecognized || with_arg.unrecognized {
        return (
            ValidationStatus::Refuted,
            Some("unrecognized option response".to_string()),
        );
    }

    let ambiguous_note = if missing.ambiguous || with_arg.ambiguous {
        Some("ambiguous option response".to_string())
    } else {
        None
    };
    let help_consumed = !missing.help_like && with_arg.help_like;

    match expectation {
        BindingExpectation::Required => {
            if missing.missing_arg {
                (
                    ValidationStatus::Confirmed,
                    Some("missing argument response observed".to_string()),
                )
            } else if missing.invalid_arg {
                (
                    ValidationStatus::Confirmed,
                    Some("invalid argument response observed for missing probe".to_string()),
                )
            } else if help_consumed && with_arg.invalid_arg {
                (
                    ValidationStatus::Confirmed,
                    Some(
                        "missing probe likely consumed --help; invalid argument observed"
                            .to_string(),
                    ),
                )
            } else if with_arg.arg_not_allowed {
                (
                    ValidationStatus::Refuted,
                    Some("argument not allowed response observed".to_string()),
                )
            } else if help_consumed {
                (
                    ValidationStatus::Undetermined,
                    Some(
                        "missing probe likely consumed --help; insufficient invalid-value evidence"
                            .to_string(),
                    ),
                )
            } else {
                (ValidationStatus::Undetermined, ambiguous_note)
            }
        }
        BindingExpectation::Optional => {
            if missing.missing_arg {
                (
                    ValidationStatus::Refuted,
                    Some("missing argument response observed".to_string()),
                )
            } else if with_arg.arg_not_allowed {
                (
                    ValidationStatus::Refuted,
                    Some("argument not allowed response observed".to_string()),
                )
            } else if with_arg.invalid_arg {
                (
                    ValidationStatus::Confirmed,
                    Some("argument accepted with invalid value".to_string()),
                )
            } else if missing.exit_code == Some(0) && with_arg.exit_code == Some(0) {
                (
                    ValidationStatus::Confirmed,
                    Some("no argument errors detected".to_string()),
                )
            } else {
                (ValidationStatus::Undetermined, ambiguous_note)
            }
        }
    }
}

fn apply_option_at_end_probe(
    status: ValidationStatus,
    reason: Option<String>,
    analysis: &AttemptAnalysis,
) -> (ValidationStatus, Option<String>) {
    if !matches!(status, ValidationStatus::Undetermined) {
        return (status, reason);
    }
    if analysis.missing_arg && analysis.exit_code.unwrap_or(0) != 0 {
        return (
            ValidationStatus::Confirmed,
            Some("missing argument response observed (option at end probe)".to_string()),
        );
    }
    let reason = reason.or_else(|| {
        Some("option-at-end probe did not yield missing-argument response".to_string())
    });
    (status, reason)
}

fn build_attempt_notes(probe: &str, analysis: &AttemptAnalysis) -> Option<String> {
    let mut parts = Vec::new();
    parts.push(format!("probe={probe}"));
    parts.extend(analysis.notes.iter().cloned());
    Some(parts.join("; "))
}

fn env_map(env: &EnvSnapshot) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert("LC_ALL".to_string(), env.locale.clone());
    map.insert("TZ".to_string(), env.tz.clone());
    map.insert("TERM".to_string(), env.term.clone());
    map
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn find_marker<'a>(output: &'a str, markers: &[&'a str]) -> Option<&'a str> {
    markers
        .iter()
        .copied()
        .find(|marker| output.contains(marker))
}

fn contains_any(output: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| output.contains(marker))
}

fn is_help_like_output(output_lower: &str) -> bool {
    output_lower.contains("usage:")
}

fn infer_invalid_argument_for_option(option: &str, output_lower: &str) -> bool {
    match option {
        "--tabsize" => output_lower.contains("invalid tab size"),
        "--width" => output_lower.contains("invalid line width"),
        _ => false,
    }
}

fn extract_reported_options(output: &str) -> Vec<String> {
    let mut options = Vec::new();
    let direct = Regex::new(
        r#"(?i)(?:unrecognized|unknown|invalid|illegal)\s+(?:option|flag|switch)(?:\s+|[:=])\s*['"`]?([^\s'"`]+)"#,
    )
    .expect("regex for direct option errors");
    for cap in direct.captures_iter(output) {
        let token = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        let cleaned = clean_option_token(token);
        if cleaned.is_empty() || cleaned == "-" || cleaned == "--" {
            continue;
        }
        options.push(cleaned);
    }

    let getopt = Regex::new(
        r#"(?i)(?:invalid|illegal|unknown|unrecognized)\s+option\s+--\s*['"]?([A-Za-z0-9])['"]?"#,
    )
    .expect("regex for getopt option errors");
    for cap in getopt.captures_iter(output) {
        let ch = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        if !ch.is_empty() {
            options.push(format!("-{}", ch));
        }
    }

    options
}

fn extract_missing_argument_options(output: &str) -> Vec<String> {
    let mut options = Vec::new();
    let direct = Regex::new(
        r#"(?i)(?:option|flag|switch)\s+['"`]?([^\s'"`]+)['"`]?\s+requires\s+(?:an?\s+)?(?:argument|value)"#,
    )
    .expect("regex for required argument errors");
    for cap in direct.captures_iter(output) {
        let token = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        let cleaned = clean_option_token(token);
        if cleaned.is_empty() || cleaned == "-" || cleaned == "--" {
            continue;
        }
        options.push(cleaned);
    }

    let missing = Regex::new(r#"(?i)missing\s+(?:argument|value)\s+for\s+['"`]?([^\s'"`]+)['"`]?"#)
        .expect("regex for missing argument errors");
    for cap in missing.captures_iter(output) {
        let token = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        let cleaned = clean_option_token(token);
        if cleaned.is_empty() || cleaned == "-" || cleaned == "--" {
            continue;
        }
        options.push(cleaned);
    }

    let required_for = Regex::new(
        r#"(?i)requires\s+(?:an?\s+)?(?:argument|value)\s+for\s+['"`]?([^\s'"`]+)['"`]?"#,
    )
    .expect("regex for required argument for option errors");
    for cap in required_for.captures_iter(output) {
        let token = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        let cleaned = clean_option_token(token);
        if cleaned.is_empty() || cleaned == "-" || cleaned == "--" {
            continue;
        }
        options.push(cleaned);
    }

    let getopt = Regex::new(
        r#"(?i)option\s+requires\s+(?:an?\s+)?(?:argument|value)\s+--\s*['"]?([A-Za-z0-9])['"]?"#,
    )
    .expect("regex for getopt missing argument errors");
    for cap in getopt.captures_iter(output) {
        let ch = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        if !ch.is_empty() {
            options.push(format!("-{}", ch));
        }
    }

    options
}

fn extract_argument_not_allowed_options(output: &str) -> Vec<String> {
    let mut options = Vec::new();
    let direct = Regex::new(
        r#"(?i)option\s+['"`]?([^\s'"`]+)['"`]?\s+does(?:n't| not)\s+(?:allow|take|accept)\s+(?:an?\s+)?(?:argument|value)"#,
    )
    .expect("regex for argument not allowed errors");
    for cap in direct.captures_iter(output) {
        let token = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        let cleaned = clean_option_token(token);
        if cleaned.is_empty() || cleaned == "-" || cleaned == "--" {
            continue;
        }
        options.push(cleaned);
    }

    let takes_no =
        Regex::new(r#"(?i)option\s+['"`]?([^\s'"`]+)['"`]?\s+takes?\s+no\s+(?:argument|value)"#)
            .expect("regex for takes no argument errors");
    for cap in takes_no.captures_iter(output) {
        let token = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        let cleaned = clean_option_token(token);
        if cleaned.is_empty() || cleaned == "-" || cleaned == "--" {
            continue;
        }
        options.push(cleaned);
    }

    let not_allowed =
        Regex::new(r#"(?i)(?:argument|value)\s+not\s+allowed\s+for\s+['"`]?([^\s'"`]+)['"`]?"#)
            .expect("regex for argument not allowed for option errors");
    for cap in not_allowed.captures_iter(output) {
        let token = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        let cleaned = clean_option_token(token);
        if cleaned.is_empty() || cleaned == "-" || cleaned == "--" {
            continue;
        }
        options.push(cleaned);
    }

    options
}

fn extract_invalid_argument_options(output: &str) -> Vec<String> {
    let mut options = Vec::new();
    let invalid = Regex::new(
        r#"(?i)invalid\s+(?:argument|value)\s+['"`]?[^'"`]+['"`]?\s+for\s+['"`]?([^\s'"`]+)['"`]?"#,
    )
    .expect("regex for invalid argument errors");
    for cap in invalid.captures_iter(output) {
        let token = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        let cleaned = clean_option_token(token);
        if cleaned.is_empty() || cleaned == "-" || cleaned == "--" {
            continue;
        }
        options.push(cleaned);
    }

    let invalid_option =
        Regex::new(r#"(?i)invalid\s+([^\s'"`]+)\s+(?:argument|value)\s+['"`]?[^'"`]+['"`]?"#)
            .expect("regex for invalid option argument errors");
    for cap in invalid_option.captures_iter(output) {
        let token = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        if !token.starts_with('-') {
            continue;
        }
        let cleaned = clean_option_token(token);
        if cleaned.is_empty() || cleaned == "-" || cleaned == "--" {
            continue;
        }
        options.push(cleaned);
    }

    options
}

fn clean_option_token(token: &str) -> String {
    token
        .trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '.' | ')' | ']' | '('))
        .to_string()
}

fn option_matches(reported: &str, tested: &str) -> bool {
    let reported = reported.to_lowercase();
    let tested = tested.to_lowercase();
    if reported == tested {
        return true;
    }
    if let Some((prefix, _)) = reported.split_once('=') {
        if prefix == tested {
            return true;
        }
    }
    if reported.len() == 1 && tested.len() == 2 && tested.starts_with('-') {
        return tested.chars().nth(1) == reported.chars().next();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt_from_output(stderr: &str) -> ExecutionAttempt {
        ExecutionAttempt {
            args: Vec::new(),
            exit_code: Some(2),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
            spawn_error: None,
        }
    }

    fn attempt_from_output_with_code(stderr: &str, exit_code: i32) -> ExecutionAttempt {
        ExecutionAttempt {
            args: Vec::new(),
            exit_code: Some(exit_code),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
            spawn_error: None,
        }
    }

    fn attempt_from_output_with_stdout(
        stdout: &str,
        stderr: &str,
        exit_code: i32,
    ) -> ExecutionAttempt {
        ExecutionAttempt {
            args: Vec::new(),
            exit_code: Some(exit_code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            spawn_error: None,
        }
    }

    #[test]
    fn refutes_when_unrecognized_option_matches_claim() {
        let attempt = attempt_from_output("error: unrecognized option '--nope'");
        let (status, note) = classify_attempt("--nope", &attempt);
        assert!(matches!(status, ValidationStatus::Refuted));
        assert!(note.is_none());
    }

    #[test]
    fn undetermined_when_unrecognized_option_is_different() {
        let attempt = attempt_from_output("error: unrecognized option '--help'");
        let (status, note) = classify_attempt("--all", &attempt);
        assert!(matches!(status, ValidationStatus::Undetermined));
        assert!(note.is_some());
    }

    #[test]
    fn confirms_when_argument_missing() {
        let attempt = attempt_from_output("option '--block-size' requires an argument");
        let (status, note) = classify_attempt("--block-size", &attempt);
        assert!(matches!(status, ValidationStatus::Confirmed));
        assert!(note.is_some());
    }

    #[test]
    fn refutes_getopt_short_option_error() {
        let attempt = attempt_from_output("invalid option -- 'i'");
        let (status, note) = classify_attempt("-i", &attempt);
        assert!(matches!(status, ValidationStatus::Refuted));
        assert!(note.is_none());
    }

    #[test]
    fn undetermined_on_spawn_error() {
        let attempt = ExecutionAttempt {
            args: Vec::new(),
            exit_code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            spawn_error: Some("boom".to_string()),
        };
        let (status, note) = classify_attempt("--all", &attempt);
        assert!(matches!(status, ValidationStatus::Undetermined));
        assert_eq!(note.as_deref(), Some("spawn failed: boom"));
    }

    #[test]
    fn confirms_required_binding_on_missing_argument() {
        let missing = attempt_from_output("option '--size' requires an argument");
        let with_arg = attempt_from_output("");
        let missing_analysis = attempt_signals("--size", &missing);
        let with_arg_analysis = attempt_signals("--size", &with_arg);
        let (status, _) = classify_binding_attempts(
            BindingExpectation::Required,
            &missing_analysis,
            &with_arg_analysis,
        );
        assert!(matches!(status, ValidationStatus::Confirmed));
    }

    #[test]
    fn refutes_optional_binding_on_missing_argument() {
        let missing = attempt_from_output("option '--size' requires an argument");
        let with_arg = attempt_from_output("");
        let missing_analysis = attempt_signals("--size", &missing);
        let with_arg_analysis = attempt_signals("--size", &with_arg);
        let (status, _) = classify_binding_attempts(
            BindingExpectation::Optional,
            &missing_analysis,
            &with_arg_analysis,
        );
        assert!(matches!(status, ValidationStatus::Refuted));
    }

    #[test]
    fn confirms_optional_binding_on_invalid_argument() {
        let missing = attempt_from_output_with_code("", 0);
        let with_arg = attempt_from_output("invalid argument 'nope' for '--color'");
        let missing_analysis = attempt_signals("--color", &missing);
        let with_arg_analysis = attempt_signals("--color", &with_arg);
        let (status, _) = classify_binding_attempts(
            BindingExpectation::Optional,
            &missing_analysis,
            &with_arg_analysis,
        );
        assert!(matches!(status, ValidationStatus::Confirmed));
    }

    #[test]
    fn confirms_required_binding_on_invalid_argument_in_missing_probe() {
        let missing = attempt_from_output("ls: invalid argument '--help' for '--format'");
        let with_arg = attempt_from_output("ls: invalid argument '__bvm__' for '--format'");
        let missing_analysis = attempt_signals("--format", &missing);
        let with_arg_analysis = attempt_signals("--format", &with_arg);
        let (status, _) = classify_binding_attempts(
            BindingExpectation::Required,
            &missing_analysis,
            &with_arg_analysis,
        );
        assert!(matches!(status, ValidationStatus::Confirmed));
    }

    #[test]
    fn confirms_required_binding_on_invalid_option_argument_in_missing_probe() {
        let missing = attempt_from_output("ls: invalid --block-size argument '--help'");
        let with_arg = attempt_from_output("ls: invalid --block-size argument '__bvm__'");
        let missing_analysis = attempt_signals("--block-size", &missing);
        let with_arg_analysis = attempt_signals("--block-size", &with_arg);
        let (status, _) = classify_binding_attempts(
            BindingExpectation::Required,
            &missing_analysis,
            &with_arg_analysis,
        );
        assert!(matches!(status, ValidationStatus::Confirmed));
    }

    #[test]
    fn confirms_required_binding_with_option_at_end_probe() {
        let end_attempt = attempt_from_output("ls: option '--hide' requires an argument");
        let end_analysis = attempt_signals("--hide", &end_attempt);
        let (status, reason) =
            apply_option_at_end_probe(ValidationStatus::Undetermined, None, &end_analysis);
        assert!(matches!(status, ValidationStatus::Confirmed));
        assert!(reason
            .as_deref()
            .unwrap_or_default()
            .contains("option at end probe"));
    }

    #[test]
    fn attributes_missing_argument_without_option_to_tested_option() {
        let attempt = attempt_from_output("missing argument");
        let analysis = attempt_signals("--size", &attempt);
        assert!(analysis.missing_arg);
        assert!(analysis
            .notes
            .iter()
            .any(|note| note.contains("attributed to tested option")));
    }

    #[test]
    fn attributes_invalid_tab_size_to_tabsize() {
        let attempt = attempt_from_output("ls: invalid tab size: '--help'");
        let analysis = attempt_signals("--tabsize", &attempt);
        assert!(analysis.invalid_arg);
        assert!(analysis
            .notes
            .iter()
            .any(|note| note.contains("attributed to tested option")));
    }

    #[test]
    fn attributes_invalid_line_width_to_width() {
        let attempt = attempt_from_output("ls: invalid line width: '--help'");
        let analysis = attempt_signals("--width", &attempt);
        assert!(analysis.invalid_arg);
        assert!(analysis
            .notes
            .iter()
            .any(|note| note.contains("attributed to tested option")));
    }

    #[test]
    fn build_with_arg_args_uses_trailing_form() {
        let form = parse_binding_form_text("--output FILE");
        let args = build_with_arg_args(form.as_ref(), "--output");
        assert_eq!(
            args,
            vec!["--output".to_string(), BINDING_DUMMY_VALUE.to_string()]
        );
    }

    #[test]
    fn build_with_arg_args_uses_trailing_short_form() {
        let form = parse_binding_form_text("-o FILE");
        let args = build_with_arg_args(form.as_ref(), "-o");
        assert_eq!(
            args,
            vec!["-o".to_string(), BINDING_DUMMY_VALUE.to_string()]
        );
    }

    #[test]
    fn option_matches_attached_value() {
        assert!(option_matches("--sort=__bvm__", "--sort"));
    }

    #[test]
    fn does_not_confirm_required_binding_on_help_consumed_without_invalid_value() {
        let missing = attempt_from_output_with_stdout("file1\nfile2", "", 0);
        let with_arg = attempt_from_output_with_stdout("Usage: tool [OPTION]", "", 0);
        let missing_analysis = attempt_signals("--hide", &missing);
        let with_arg_analysis = attempt_signals("--hide", &with_arg);
        let (status, reason) = classify_binding_attempts(
            BindingExpectation::Required,
            &missing_analysis,
            &with_arg_analysis,
        );
        assert!(matches!(status, ValidationStatus::Undetermined));
        assert!(reason
            .as_deref()
            .unwrap_or_default()
            .contains("missing probe likely consumed --help"));
    }

    #[test]
    fn confirms_required_binding_when_help_consumed_and_invalid_value() {
        let missing = attempt_from_output_with_stdout("file1\nfile2", "", 0);
        let with_arg = attempt_from_output_with_stdout(
            "Usage: tool [OPTION]",
            "invalid argument '__bvm__' for '--hide'",
            1,
        );
        let missing_analysis = attempt_signals("--hide", &missing);
        let with_arg_analysis = attempt_signals("--hide", &with_arg);
        let (status, _) = classify_binding_attempts(
            BindingExpectation::Required,
            &missing_analysis,
            &with_arg_analysis,
        );
        assert!(matches!(status, ValidationStatus::Confirmed));
    }

    #[test]
    fn refutes_optional_binding_on_argument_not_allowed() {
        let missing = attempt_from_output_with_code("", 0);
        let with_arg = attempt_from_output("option '--all' doesn't allow an argument");
        let missing_analysis = attempt_signals("--all", &missing);
        let with_arg_analysis = attempt_signals("--all", &with_arg);
        let (status, _) = classify_binding_attempts(
            BindingExpectation::Optional,
            &missing_analysis,
            &with_arg_analysis,
        );
        assert!(matches!(status, ValidationStatus::Refuted));
    }

    #[test]
    fn refutes_required_binding_on_unrecognized_option() {
        let missing = attempt_from_output("error: unrecognized option '--ghost'");
        let with_arg = attempt_from_output("");
        let missing_analysis = attempt_signals("--ghost", &missing);
        let with_arg_analysis = attempt_signals("--ghost", &with_arg);
        let (status, _) = classify_binding_attempts(
            BindingExpectation::Required,
            &missing_analysis,
            &with_arg_analysis,
        );
        assert!(matches!(status, ValidationStatus::Refuted));
    }
}
