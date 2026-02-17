//! Shared builders for contextual behavior exclusion notes.
//!
//! Used by both stubs.rs (manual stub generation) and progress.rs (auto-exclusion)
//! to generate informative exclusion notes and evidence from verification ledger data.

use crate::scenarios::VerificationEntry;

/// Build contextual note from verification entry.
pub fn build_exclusion_note(entry: Option<&VerificationEntry>) -> String {
    let Some(entry) = entry else {
        return "Auto-excluded after repeated failures".to_string();
    };

    let outcome = entry.delta_outcome.as_deref().unwrap_or("unknown");

    match outcome {
        "assertion_failed" => {
            let assertion = entry
                .behavior_unverified_assertion_kind
                .as_deref()
                .unwrap_or("unknown");
            format!("assertion_failed: {} assertion did not pass", assertion)
        }
        "outputs_equal" => build_outputs_equal_note(entry),
        "scenario_error" => "scenario_error: test scenario configuration invalid".to_string(),
        _ => format!("{}: verification inconclusive", outcome),
    }
}

fn build_outputs_equal_note(entry: &VerificationEntry) -> String {
    // Check stderr for known environmental limitations
    // Prefer behavior_stderr (actual scenario) but check auto_verify_stderr too for env hints
    let stderr = non_empty_str(entry.behavior_stderr.as_ref())
        .or_else(|| non_empty_str(entry.auto_verify_stderr.as_ref()));

    if let Some(stderr) = stderr {
        if stderr.contains("Permission denied") || stderr.contains("Operation not permitted") {
            return "outputs_equal: requires elevated permissions".to_string();
        }
        if stderr.contains("SELinux") || stderr.contains("selinux") {
            return "outputs_equal: requires SELinux context".to_string();
        }
        if stderr.contains("block device") || stderr.contains("block special") {
            return "outputs_equal: requires block device".to_string();
        }
        if stderr.contains("reflink")
            || stderr.contains("copy-on-write")
            || stderr.contains("FICLONE")
        {
            return "outputs_equal: requires CoW filesystem (btrfs/xfs)".to_string();
        }
    }

    // Scenario ran successfully but no output difference - likely metadata-only effect
    if entry.behavior_exit_code == Some(0) {
        return "outputs_equal: scenario succeeded but produced identical output to baseline"
            .to_string();
    }

    "outputs_equal: no observable difference from baseline".to_string()
}

/// Returns Some if string is non-empty after trimming, None otherwise.
fn non_empty_str(s: Option<&String>) -> Option<&str> {
    s.map(String::as_str).filter(|s| !s.trim().is_empty())
}

/// Build evidence JSON with available context.
///
/// Prefers `behavior_*` fields (from actual scenario) over `auto_verify_*` (from probe).
/// The probe runs with minimal args and often produces misleading errors like
/// "missing file operand" that don't reflect the actual scenario behavior.
pub fn build_exclusion_evidence(
    entry: Option<&VerificationEntry>,
    delta_path: &str,
) -> serde_json::Value {
    let mut evidence = serde_json::Map::new();
    evidence.insert("delta_variant_path".into(), delta_path.into());

    if let Some(entry) = entry {
        if let Some(outcome) = &entry.delta_outcome {
            evidence.insert("delta_outcome".into(), outcome.clone().into());
        }

        // Prefer behavior_* (actual scenario) over auto_verify_* (probe)
        // Only fall back to auto_verify if behavior is missing or empty
        let stderr = non_empty_str(entry.behavior_stderr.as_ref())
            .or_else(|| non_empty_str(entry.auto_verify_stderr.as_ref()));
        if let Some(err) = stderr {
            let preview = truncate_stderr(err, 150);
            if !preview.is_empty() {
                evidence.insert("stderr_preview".into(), preview.into());
            }
        }

        // Include exit code to show whether scenario succeeded
        let exit_code = entry.behavior_exit_code.or(entry.auto_verify_exit_code);
        if let Some(code) = exit_code {
            evidence.insert("exit_code".into(), code.into());
        }

        if let Some(kind) = &entry.behavior_unverified_assertion_kind {
            evidence.insert("assertion_kind".into(), kind.clone().into());
        }
    }

    serde_json::Value::Object(evidence)
}

fn truncate_stderr(s: &str, max_len: usize) -> String {
    let cleaned = s.lines().take(3).collect::<Vec<_>>().join(" | ");
    if cleaned.len() > max_len {
        format!("{}...", &cleaned[..max_len])
    } else {
        cleaned
    }
}

/// Derive reason_code from delta_outcome.
/// - outputs_equal → fixture_gap (can't produce different output)
/// - assertion_failed → assertion_gap (produced different output but assertion failed)
/// - scenario_error → assertion_gap (scenario configuration issue)
pub fn derive_reason_code(entry: Option<&VerificationEntry>) -> &'static str {
    let Some(entry) = entry else {
        return "assertion_gap";
    };
    match entry.delta_outcome.as_deref() {
        Some("outputs_equal") => "fixture_gap",
        Some("assertion_failed") | Some("scenario_error") => "assertion_gap",
        Some(_) | None => "assertion_gap",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(delta_outcome: Option<&str>, assertion_kind: Option<&str>) -> VerificationEntry {
        VerificationEntry {
            surface_id: "--test".to_string(),
            status: "unverified".to_string(),
            behavior_status: "unverified".to_string(),
            behavior_exclusion_reason_code: None,
            behavior_unverified_reason_code: None,
            behavior_unverified_scenario_id: None,
            behavior_unverified_assertion_kind: assertion_kind.map(String::from),
            behavior_unverified_assertion_seed_path: None,
            behavior_unverified_assertion_token: None,
            scenario_ids: Vec::new(),
            scenario_paths: Vec::new(),
            behavior_scenario_ids: Vec::new(),
            behavior_assertion_scenario_ids: Vec::new(),
            behavior_scenario_paths: Vec::new(),
            delta_outcome: delta_outcome.map(String::from),
            delta_evidence_paths: Vec::new(),
            behavior_confounded_scenario_ids: Vec::new(),
            behavior_confounded_extra_surface_ids: Vec::new(),
            evidence: Vec::new(),
            auto_verify_exit_code: None,
            auto_verify_stderr: None,
            behavior_exit_code: None,
            behavior_stderr: None,
        }
    }

    #[test]
    fn test_build_note_assertion_failed() {
        let entry = make_entry(Some("assertion_failed"), Some("file_exists"));
        let note = build_exclusion_note(Some(&entry));
        assert_eq!(note, "assertion_failed: file_exists assertion did not pass");
    }

    #[test]
    fn test_build_note_outputs_equal_no_stderr() {
        let entry = make_entry(Some("outputs_equal"), None);
        let note = build_exclusion_note(Some(&entry));
        assert_eq!(
            note,
            "outputs_equal: no observable difference from baseline"
        );
    }

    #[test]
    fn test_build_note_outputs_equal_selinux() {
        let mut entry = make_entry(Some("outputs_equal"), None);
        entry.auto_verify_stderr = Some("cp: failed to set SELinux context".to_string());
        let note = build_exclusion_note(Some(&entry));
        assert_eq!(note, "outputs_equal: requires SELinux context");
    }

    #[test]
    fn test_build_note_none_entry() {
        let note = build_exclusion_note(None);
        assert_eq!(note, "Auto-excluded after repeated failures");
    }

    #[test]
    fn test_derive_reason_code_outputs_equal() {
        let entry = make_entry(Some("outputs_equal"), None);
        assert_eq!(derive_reason_code(Some(&entry)), "fixture_gap");
    }

    #[test]
    fn test_derive_reason_code_assertion_failed() {
        let entry = make_entry(Some("assertion_failed"), None);
        assert_eq!(derive_reason_code(Some(&entry)), "assertion_gap");
    }

    #[test]
    fn test_derive_reason_code_none() {
        assert_eq!(derive_reason_code(None), "assertion_gap");
    }

    #[test]
    fn test_build_evidence_with_entry() {
        let mut entry = make_entry(Some("assertion_failed"), Some("stdout_contains"));
        entry.behavior_stderr = Some("error: expected output".to_string());
        let evidence = build_exclusion_evidence(Some(&entry), "path/to/scenario.json");
        let obj = evidence.as_object().unwrap();

        assert_eq!(
            obj.get("delta_variant_path").unwrap(),
            "path/to/scenario.json"
        );
        assert_eq!(obj.get("delta_outcome").unwrap(), "assertion_failed");
        assert_eq!(obj.get("assertion_kind").unwrap(), "stdout_contains");
        assert_eq!(obj.get("stderr_preview").unwrap(), "error: expected output");
    }

    #[test]
    fn test_build_evidence_none_entry() {
        let evidence = build_exclusion_evidence(None, "path/to/scenario.json");
        let obj = evidence.as_object().unwrap();

        assert_eq!(
            obj.get("delta_variant_path").unwrap(),
            "path/to/scenario.json"
        );
        assert!(obj.get("delta_outcome").is_none());
        assert!(obj.get("assertion_kind").is_none());
    }

    #[test]
    fn test_truncate_stderr_short() {
        let result = truncate_stderr("short error", 150);
        assert_eq!(result, "short error");
    }

    #[test]
    fn test_truncate_stderr_multiline() {
        let result = truncate_stderr("line1\nline2\nline3\nline4\nline5", 150);
        assert_eq!(result, "line1 | line2 | line3");
    }

    #[test]
    fn test_truncate_stderr_long() {
        let long_line = "x".repeat(200);
        let result = truncate_stderr(&long_line, 150);
        assert!(result.len() <= 153); // 150 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_outputs_equal_with_successful_scenario() {
        let mut entry = make_entry(Some("outputs_equal"), None);
        entry.behavior_exit_code = Some(0);
        let note = build_exclusion_note(Some(&entry));
        assert_eq!(
            note,
            "outputs_equal: scenario succeeded but produced identical output to baseline"
        );
    }

    #[test]
    fn test_prefer_behavior_stderr_over_auto_verify() {
        let mut entry = make_entry(Some("assertion_failed"), None);
        entry.behavior_stderr = Some("actual scenario error".to_string());
        entry.auto_verify_stderr = Some("probe error: missing operand".to_string());
        let evidence = build_exclusion_evidence(Some(&entry), "path/to/scenario.json");
        let obj = evidence.as_object().unwrap();
        // Should use behavior_stderr, not auto_verify_stderr
        assert_eq!(obj.get("stderr_preview").unwrap(), "actual scenario error");
    }

    #[test]
    fn test_fallback_to_auto_verify_stderr_when_behavior_empty() {
        let mut entry = make_entry(Some("assertion_failed"), None);
        entry.behavior_stderr = Some("".to_string()); // empty
        entry.auto_verify_stderr = Some("probe error".to_string());
        let evidence = build_exclusion_evidence(Some(&entry), "path/to/scenario.json");
        let obj = evidence.as_object().unwrap();
        // Should fall back to auto_verify_stderr
        assert_eq!(obj.get("stderr_preview").unwrap(), "probe error");
    }

    #[test]
    fn test_evidence_includes_exit_code() {
        let mut entry = make_entry(Some("outputs_equal"), None);
        entry.behavior_exit_code = Some(0);
        let evidence = build_exclusion_evidence(Some(&entry), "path/to/scenario.json");
        let obj = evidence.as_object().unwrap();
        assert_eq!(obj.get("exit_code").unwrap(), 0);
    }

    #[test]
    fn test_evidence_exit_code_fallback_to_auto_verify() {
        let mut entry = make_entry(Some("assertion_failed"), None);
        entry.auto_verify_exit_code = Some(1);
        let evidence = build_exclusion_evidence(Some(&entry), "path/to/scenario.json");
        let obj = evidence.as_object().unwrap();
        assert_eq!(obj.get("exit_code").unwrap(), 1);
    }

    #[test]
    fn test_non_empty_str_with_content() {
        let s = "hello".to_string();
        assert_eq!(non_empty_str(Some(&s)), Some("hello"));
    }

    #[test]
    fn test_non_empty_str_with_empty() {
        let s = "".to_string();
        assert_eq!(non_empty_str(Some(&s)), None);
    }

    #[test]
    fn test_non_empty_str_with_whitespace() {
        let s = "   ".to_string();
        assert_eq!(non_empty_str(Some(&s)), None);
    }

    #[test]
    fn test_non_empty_str_with_none() {
        assert_eq!(non_empty_str(None), None);
    }
}
