use crate::scenarios::ScenarioExpect;
use regex::Regex;

pub(super) fn validate_scenario(
    expect: &ScenarioExpect,
    observed_exit_code: Option<i32>,
    observed_exit_signal: Option<i32>,
    observed_timed_out: bool,
    stdout: &str,
    stderr: &str,
) -> Vec<String> {
    let mut failures = Vec::new();

    if observed_timed_out {
        failures.push("timed out".to_string());
    }

    if let Some(expected_code) = expect.exit_code {
        if observed_exit_code != Some(expected_code) {
            failures.push(format!(
                "expected exit_code {}, observed {:?}",
                expected_code, observed_exit_code
            ));
        }
    }

    if let Some(expected_signal) = expect.exit_signal {
        if observed_exit_signal != Some(expected_signal) {
            failures.push(format!(
                "expected exit_signal {}, observed {:?}",
                expected_signal, observed_exit_signal
            ));
        }
    }

    validate_contains_all(stdout, &expect.stdout_contains_all, "stdout", &mut failures);
    validate_contains_any(stdout, &expect.stdout_contains_any, "stdout", &mut failures);
    validate_regex_all(stdout, &expect.stdout_regex_all, "stdout", &mut failures);
    validate_regex_any(stdout, &expect.stdout_regex_any, "stdout", &mut failures);
    validate_contains_all(stderr, &expect.stderr_contains_all, "stderr", &mut failures);
    validate_contains_any(stderr, &expect.stderr_contains_any, "stderr", &mut failures);
    validate_regex_all(stderr, &expect.stderr_regex_all, "stderr", &mut failures);
    validate_regex_any(stderr, &expect.stderr_regex_any, "stderr", &mut failures);

    failures
}

fn validate_contains_all(text: &str, needles: &[String], label: &str, failures: &mut Vec<String>) {
    if needles.is_empty() {
        return;
    }
    for needle in needles {
        if !text.contains(needle) {
            failures.push(format!("{label} missing substring {:?}", needle));
        }
    }
}

fn validate_contains_any(text: &str, needles: &[String], label: &str, failures: &mut Vec<String>) {
    if needles.is_empty() {
        return;
    }
    if !needles.iter().any(|needle| text.contains(needle)) {
        failures.push(format!("{label} missing any of {:?}", needles));
    }
}

fn validate_regex_all(text: &str, patterns: &[String], label: &str, failures: &mut Vec<String>) {
    if patterns.is_empty() {
        return;
    }
    for pattern in patterns {
        match Regex::new(pattern) {
            Ok(re) => {
                if !re.is_match(text) {
                    failures.push(format!("{label} missing regex match {:?}", pattern));
                }
            }
            Err(err) => failures.push(format!("invalid {label} regex {:?}: {err}", pattern)),
        }
    }
}

fn validate_regex_any(text: &str, patterns: &[String], label: &str, failures: &mut Vec<String>) {
    if patterns.is_empty() {
        return;
    }
    let mut invalid = Vec::new();
    let mut any_match = false;
    for pattern in patterns {
        match Regex::new(pattern) {
            Ok(re) => {
                if re.is_match(text) {
                    any_match = true;
                    break;
                }
            }
            Err(err) => invalid.push(format!("{pattern:?}: {err}")),
        }
    }
    if !invalid.is_empty() {
        failures.push(format!(
            "invalid {label} regex_any patterns: {}",
            invalid.join("; ")
        ));
    }
    if !any_match {
        failures.push(format!("{label} missing any regex of {:?}", patterns));
    }
}
