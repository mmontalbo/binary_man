use crate::schema::{
    Determinism, EnvSnapshot, Evidence, ValidationMethod, ValidationResult, ValidationStatus,
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

struct ExecutionAttempt {
    args: Vec<String>,
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    spawn_error: Option<String>,
}

pub fn validation_env() -> EnvSnapshot {
    EnvSnapshot {
        locale: "C".to_string(),
        tz: "UTC".to_string(),
        term: "dumb".to_string(),
    }
}

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

pub fn validate_option_existence(
    binary: &Path,
    claim_id: &str,
    option: &str,
    env: &EnvSnapshot,
) -> ValidationResult {
    let args = vec![option.to_string(), "--help".to_string()];
    let output = Command::new(binary)
        .args(&args)
        .env_clear()
        .env("LC_ALL", &env.locale)
        .env("TZ", &env.tz)
        .env("TERM", &env.term)
        .output();

    let attempt = match output {
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
    };

    let (status, notes) = classify_attempt(option, &attempt);
    let evidence = Evidence {
        args: attempt.args.clone(),
        env: env_map(env),
        exit_code: attempt.exit_code,
        stdout: Some(hash_bytes(&attempt.stdout)),
        stderr: Some(hash_bytes(&attempt.stderr)),
        notes,
    };

    ValidationResult {
        claim_id: claim_id.to_string(),
        status,
        method: ValidationMethod::AcceptanceTest,
        determinism: Some(Determinism::Deterministic),
        attempts: vec![evidence],
        observed: None,
        reason: None,
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

    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&attempt.stdout),
        String::from_utf8_lossy(&attempt.stderr)
    );
    let output_lower = output.to_lowercase();

    if let Some(marker) = find_marker(&output_lower, UNRECOGNIZED_MARKERS) {
        let reported = extract_reported_options(&output);
        if reported
            .iter()
            .any(|reported| option_matches(reported, option))
        {
            return (ValidationStatus::Refuted, None);
        }
        let note = if reported.is_empty() {
            format!("unrecognized option marker ({marker}) without option attribution")
        } else {
            format!("unrecognized option marker ({marker}) for {:?}", reported)
        };
        return (ValidationStatus::Undetermined, Some(note));
    }

    if let Some(marker) = find_marker(&output_lower, AMBIGUOUS_MARKERS) {
        return (
            ValidationStatus::Undetermined,
            Some(format!("ambiguous option response ({marker})")),
        );
    }

    let mut notes = Vec::new();
    if exit_code != 0 {
        notes.push(format!(
            "nonzero exit ({exit_code}) without unrecognized option marker"
        ));
    }
    if contains_any(&output_lower, ARGUMENT_ERROR_MARKERS) {
        notes.push("argument error reported".to_string());
    }

    let note = if notes.is_empty() {
        None
    } else {
        Some(notes.join("; "))
    };

    (ValidationStatus::Confirmed, note)
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
}
