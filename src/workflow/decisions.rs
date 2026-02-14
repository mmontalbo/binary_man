//! LM-friendly decision list with evidence for unverified items.
//!
//! This module extracts structured evidence for items requiring semantic
//! interpretation, making it easier for an LM to provide scenario edits.
use crate::enrich::DocPackPaths;
use crate::scenarios::{VerificationEntry, VerificationLedger};
use crate::semantics::Semantics;
use crate::surface::SurfaceInventory;
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;

/// A single decision item requiring LM interpretation.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionItem {
    /// The surface item ID (e.g., "--verbose", "status").
    pub surface_id: String,

    /// The kind of surface item (e.g., "option", "subcommand").
    pub kind: String,

    /// The reason code for why this item is unverified.
    pub reason_code: String,

    /// Optional description from the surface inventory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Forms showing usage patterns (e.g., "-v, --verbose").
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub forms: Vec<String>,

    /// Lines from the man page containing this surface_id.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub man_excerpts: Vec<String>,

    /// ID of the current scenario covering this item, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_scenario_id: Option<String>,

    /// ID of the baseline scenario for comparison.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_scenario_id: Option<String>,

    /// The assertion kind that failed (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertion_kind: Option<String>,

    /// The seed path referenced in the failed assertion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertion_seed_path: Option<String>,

    /// The stdout token that was expected but not found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertion_token: Option<String>,

    /// Delta outcome from workaround attempts (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_outcome: Option<String>,

    /// Evidence paths from delta reruns.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub delta_evidence_paths: Vec<String>,

    /// Auto-verify exit code (helps understand why auto_verify failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_verify_exit_code: Option<i64>,

    /// Auto-verify stderr preview (helps discover fixture requirements).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_verify_stderr: Option<String>,

    /// Suggested prereq based on stderr pattern match (from semantics.json.verification.prereq_suggestions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_prereq: Option<String>,
}

/// Output container for the decisions list.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionsOutput {
    /// The binary name.
    pub binary_name: Option<String>,

    /// List of items requiring decisions, grouped by reason_code.
    pub decisions: Vec<DecisionItem>,

    /// Summary counts by reason code.
    pub summary: BTreeMap<String, usize>,
}

/// Build the decisions output from verification ledger and surface inventory.
pub fn build_decisions(
    paths: &DocPackPaths,
    binary_name: Option<&str>,
    ledger: &VerificationLedger,
    surface: &SurfaceInventory,
    semantics: Option<&Semantics>,
) -> Result<DecisionsOutput> {
    let surface_map: BTreeMap<&str, &crate::surface::SurfaceItem> = surface
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();

    let man_content = binary_name
        .map(|name| paths.man_page_path(name))
        .filter(|p| p.is_file())
        .and_then(|p| fs::read_to_string(&p).ok());

    // Extract prereq suggestions from semantics for pattern matching
    let empty_suggestions = Vec::new();
    let prereq_suggestions = semantics
        .map(|s| &s.verification.prereq_suggestions)
        .unwrap_or(&empty_suggestions);

    let mut decisions = Vec::new();
    let mut summary: BTreeMap<String, usize> = BTreeMap::new();

    for entry in &ledger.entries {
        // Skip verified items
        if entry.behavior_status == "verified" || entry.behavior_status == "excluded" {
            continue;
        }

        let reason_code = entry
            .behavior_unverified_reason_code
            .as_deref()
            .unwrap_or("unknown");

        // Get surface item data
        let surface_item = surface_map.get(entry.surface_id.as_str());
        // Derive kind from item structure
        let kind = surface_item
            .map(|item| {
                // Entry points (id in context_argv) are commands/subcommands
                if item.context_argv.last().map(|s| s.as_str()) == Some(item.id.as_str()) {
                    "subcommand".to_string()
                } else {
                    "option".to_string()
                }
            })
            .unwrap_or_else(|| "unknown".to_string());
        let description = surface_item.and_then(|item| item.description.clone());
        let forms = surface_item
            .map(|item| item.forms.clone())
            .unwrap_or_default();

        // Extract man page excerpts
        let man_excerpts = man_content
            .as_ref()
            .map(|content| extract_man_excerpts(content, &entry.surface_id))
            .unwrap_or_default();

        // Get scenario info
        let current_scenario_id = entry.behavior_unverified_scenario_id.clone();
        let baseline_scenario_id = find_baseline_scenario_id(entry);

        // Match stderr against prereq suggestions
        let suggested_prereq = entry.auto_verify_stderr.as_ref().and_then(|stderr| {
            prereq_suggestions
                .iter()
                .find(|s| stderr.contains(&s.stderr_contains))
                .map(|s| s.suggest.clone())
        });

        let item = DecisionItem {
            surface_id: entry.surface_id.clone(),
            kind,
            reason_code: reason_code.to_string(),
            description,
            forms,
            man_excerpts,
            current_scenario_id,
            baseline_scenario_id,
            assertion_kind: entry.behavior_unverified_assertion_kind.clone(),
            assertion_seed_path: entry.behavior_unverified_assertion_seed_path.clone(),
            assertion_token: entry.behavior_unverified_assertion_token.clone(),
            delta_outcome: entry.delta_outcome.clone(),
            delta_evidence_paths: entry.delta_evidence_paths.clone(),
            auto_verify_exit_code: entry.auto_verify_exit_code,
            auto_verify_stderr: entry.auto_verify_stderr.clone(),
            suggested_prereq,
        };

        *summary.entry(reason_code.to_string()).or_insert(0) += 1;
        decisions.push(item);
    }

    // Sort by reason_code for grouping, then by surface_id
    decisions.sort_by(|a, b| {
        a.reason_code
            .cmp(&b.reason_code)
            .then_with(|| a.surface_id.cmp(&b.surface_id))
    });

    Ok(DecisionsOutput {
        binary_name: binary_name.map(String::from),
        decisions,
        summary,
    })
}

/// Extract lines from man page content that contain the given surface_id.
fn extract_man_excerpts(content: &str, surface_id: &str) -> Vec<String> {
    let mut excerpts = Vec::new();
    let surface_id_lower = surface_id.to_lowercase();

    for line in content.lines() {
        let line_lower = line.to_lowercase();
        // Match the surface_id as a word boundary to avoid partial matches
        if contains_surface_id(&line_lower, &surface_id_lower) {
            let trimmed = line.trim();
            if !trimmed.is_empty() && excerpts.len() < 10 {
                excerpts.push(trimmed.to_string());
            }
        }
    }

    excerpts
}

/// Check if line contains surface_id with appropriate word boundaries.
fn contains_surface_id(line: &str, surface_id: &str) -> bool {
    // For options like "--verbose", look for exact matches
    if surface_id.starts_with('-') {
        return line.contains(surface_id);
    }

    // For subcommands, look for word boundaries
    let words: Vec<&str> = line.split_whitespace().collect();
    words.iter().any(|word| {
        let word_trimmed = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
        word_trimmed == surface_id
    })
}

/// Find the baseline scenario ID from a verification entry.
fn find_baseline_scenario_id(entry: &VerificationEntry) -> Option<String> {
    // The baseline is typically referenced in the scenario spec, but we can infer
    // from common patterns. For now, return None and let the caller provide it.
    // A more complete implementation would load the scenario plan and look it up.
    if entry.behavior_scenario_ids.is_empty() {
        return None;
    }

    // Check if any scenario is named "baseline*"
    entry
        .behavior_scenario_ids
        .iter()
        .find(|id| id.starts_with("baseline"))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_man_excerpts_option() {
        let content = r#"
OPTIONS
       --verbose, -v
              Increase verbosity. Can be given multiple times.

       --quiet, -q
              Decrease verbosity.

       --help Show this help message.
"#;

        let excerpts = extract_man_excerpts(content, "--verbose");
        assert_eq!(excerpts.len(), 1);
        assert!(excerpts[0].contains("--verbose"));
    }

    #[test]
    fn test_extract_man_excerpts_limit() {
        let content = (0..20)
            .map(|i| format!("Line {i} with --test option"))
            .collect::<Vec<_>>()
            .join("\n");

        let excerpts = extract_man_excerpts(&content, "--test");
        assert_eq!(excerpts.len(), 10);
    }

    #[test]
    fn test_contains_surface_id_option() {
        assert!(contains_surface_id("--verbose option", "--verbose"));
        assert!(!contains_surface_id("some verbose text", "--verbose"));
    }

    #[test]
    fn test_contains_surface_id_subcommand() {
        assert!(contains_surface_id("git status shows", "status"));
        assert!(contains_surface_id("the status command", "status"));
        // Partial matches should not work
        assert!(!contains_surface_id("statuses are shown", "status"));
    }
}
