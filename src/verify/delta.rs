//! Structured delta analysis for bilateral comparison evidence.
//!
//! Computes structured behavioral facts from control vs option stdout,
//! going beyond the boolean "differs" check to describe WHAT changed.

#![allow(dead_code)] // Module is new; types will be wired into pipeline incrementally

use std::collections::HashSet;

/// High-level relationship between control and option output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryRelation {
    /// Option output contains all control entries plus additional ones.
    Superset { added: Vec<String> },
    /// Option output is missing some control entries.
    Subset { removed: Vec<String> },
    /// Same entries, different order.
    Reordered,
    /// Same entries present in both, but lines are formatted differently.
    EntriesPreserved { format_change: FormatChange },
    /// Outputs share no entries — completely different content.
    Disjoint,
    /// Both outputs are identical.
    Identical,
}

/// How the format changed when entries are preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatChange {
    /// Each option line is a control entry with something prepended.
    PrefixAdded,
    /// Each option line is a control entry with something appended.
    SuffixAdded,
    /// Some entries have a suffix appended (e.g., -F appends / to directories).
    SuffixAddedPartial,
    /// Lines have more fields/columns (e.g., -l adds permissions, size, date).
    FieldsExpanded,
    /// Lines are packed differently (multiple entries per line, or one per line).
    LayoutChanged,
    /// Entries wrapped in delimiters (quotes, escape sequences, hyperlinks).
    Wrapped,
    /// Content is the same but with decorations (ANSI codes, color).
    Decorated,
    /// Some characters escaped or transformed (e.g., spaces → backslash-space).
    Escaped,
    /// Cannot determine specific format change.
    Unknown,
}

/// Full structured delta between control and option output.
#[derive(Debug, Clone)]
pub struct StructuredDelta {
    /// Entry-level relationship (the main classification).
    pub relation: EntryRelation,
    /// Number of entries detected in control output.
    pub control_entry_count: usize,
    /// Number of entries detected in option output.
    pub option_entry_count: usize,
    /// Raw line counts.
    pub control_line_count: usize,
    pub option_line_count: usize,
}

/// Compute a structured delta between control and option stdout.
///
/// Treats each non-empty control line as an "entry" and checks whether
/// those entries appear (as substrings) in the option output lines.
/// This is binary-agnostic: it works for any command that produces
/// line-oriented output where the "entity name" is preserved across
/// format changes.
pub fn compute_structured_delta(control_stdout: &str, option_stdout: &str) -> StructuredDelta {
    let ctrl_lines: Vec<&str> = control_stdout.lines().filter(|l| !l.is_empty()).collect();
    let opt_lines: Vec<&str> = option_stdout.lines().filter(|l| !l.is_empty()).collect();

    // Identical check
    if control_stdout == option_stdout {
        return StructuredDelta {
            relation: EntryRelation::Identical,
            control_entry_count: ctrl_lines.len(),
            option_entry_count: opt_lines.len(),
            control_line_count: ctrl_lines.len(),
            option_line_count: opt_lines.len(),
        };
    }

    // Treat each control line as an "entry" (entity name).
    // Check if each entry appears as a substring in any option line.
    let ctrl_entries: Vec<&str> = ctrl_lines.iter().map(|l| l.trim()).collect();
    let opt_entries_set: HashSet<&str> = opt_lines.iter().map(|l| l.trim()).collect();

    // Exact line-set comparison first
    let ctrl_set: HashSet<&str> = ctrl_entries.iter().copied().collect();

    // Check for reordering: same line sets, different order
    if ctrl_set == opt_entries_set
        && ctrl_entries.len() == opt_lines.len()
        && ctrl_entries != opt_lines.iter().map(|l| l.trim()).collect::<Vec<_>>()
    {
        return StructuredDelta {
            relation: EntryRelation::Reordered,
            control_entry_count: ctrl_entries.len(),
            option_entry_count: opt_lines.len(),
            control_line_count: ctrl_lines.len(),
            option_line_count: opt_lines.len(),
        };
    }

    // Check for superset: all control lines present in option, plus extras
    if ctrl_set.is_subset(&opt_entries_set) && opt_entries_set.len() > ctrl_set.len() {
        let added: Vec<String> = opt_entries_set
            .difference(&ctrl_set)
            .map(|s| s.to_string())
            .collect();
        return StructuredDelta {
            relation: EntryRelation::Superset { added },
            control_entry_count: ctrl_entries.len(),
            option_entry_count: opt_lines.len(),
            control_line_count: ctrl_lines.len(),
            option_line_count: opt_lines.len(),
        };
    }

    // Check for subset: all option lines present in control, minus some
    if opt_entries_set.is_subset(&ctrl_set) && ctrl_set.len() > opt_entries_set.len() {
        let removed: Vec<String> = ctrl_set
            .difference(&opt_entries_set)
            .map(|s| s.to_string())
            .collect();
        return StructuredDelta {
            relation: EntryRelation::Subset { removed },
            control_entry_count: ctrl_entries.len(),
            option_entry_count: opt_lines.len(),
            control_line_count: ctrl_lines.len(),
            option_line_count: opt_lines.len(),
        };
    }

    // Lines don't match exactly — try entry-level (substring) matching.
    // Check if each control entry appears as a substring in any option line.
    // Also try matching after stripping escape characters and quotes from option lines.
    let mut ctrl_found_in_opt = 0;
    for entry in &ctrl_entries {
        let found = opt_lines.iter().any(|ol| {
            ol.contains(entry)
                || strip_escapes(ol).contains(entry)
        });
        if found {
            ctrl_found_in_opt += 1;
        }
    }

    let ctrl_match_rate = if ctrl_entries.is_empty() {
        0.0
    } else {
        ctrl_found_in_opt as f64 / ctrl_entries.len() as f64
    };

    // If most control entries appear as substrings in option lines,
    // the entries are preserved but formatted differently.
    if ctrl_match_rate >= 0.8 {
        let format_change = detect_format_change(&ctrl_entries, &opt_lines);
        return StructuredDelta {
            relation: EntryRelation::EntriesPreserved { format_change },
            control_entry_count: ctrl_entries.len(),
            option_entry_count: opt_lines.len(),
            control_line_count: ctrl_lines.len(),
            option_line_count: opt_lines.len(),
        };
    }

    // Also check reverse: option entries in control (for cases where option
    // packs multiple entries per line, like ls -C)
    let mut opt_found_in_ctrl = 0;
    for opt_line in &opt_lines {
        // Split option line by whitespace and check each token
        for token in opt_line.split_whitespace() {
            if ctrl_set.contains(token) {
                opt_found_in_ctrl += 1;
            }
        }
    }

    let opt_token_match_rate = if ctrl_entries.is_empty() {
        0.0
    } else {
        opt_found_in_ctrl as f64 / ctrl_entries.len() as f64
    };

    if opt_token_match_rate >= 0.8 {
        return StructuredDelta {
            relation: EntryRelation::EntriesPreserved {
                format_change: FormatChange::LayoutChanged,
            },
            control_entry_count: ctrl_entries.len(),
            option_entry_count: opt_lines.len(),
            control_line_count: ctrl_lines.len(),
            option_line_count: opt_lines.len(),
        };
    }

    // Nothing matched — outputs are disjoint
    StructuredDelta {
        relation: EntryRelation::Disjoint,
        control_entry_count: ctrl_entries.len(),
        option_entry_count: opt_lines.len(),
        control_line_count: ctrl_lines.len(),
        option_line_count: opt_lines.len(),
    }
}

/// Strip common escape patterns from a string for fuzzy entry matching.
/// Handles backslash-escaping, quote-wrapping, and ANSI/OSC escape sequences.
fn strip_escapes(s: &str) -> String {
    let mut result = s.to_string();
    // Strip ANSI color codes: \x1b[...m
    while let Some(start) = result.find("\x1b[") {
        if let Some(end) = result[start..].find('m') {
            result.replace_range(start..start + end + 1, "");
        } else {
            break;
        }
    }
    // Strip OSC8 hyperlink sequences: \x1b]8;;...\x07
    while let Some(start) = result.find("\x1b]8;") {
        if let Some(end) = result[start..].find('\x07') {
            result.replace_range(start..start + end + 1, "");
        } else {
            break;
        }
    }
    // Strip surrounding quotes
    let trimmed = result.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        result = trimmed[1..trimmed.len() - 1].to_string();
    }
    // Unescape backslash-space and backslash-backslash
    result = result.replace("\\ ", " ").replace("\\\\", "\\");
    result
}

/// Detect what kind of format change occurred when entries are preserved.
fn detect_format_change(ctrl_entries: &[&str], opt_lines: &[&str]) -> FormatChange {
    if ctrl_entries.len() != opt_lines.len() {
        return FormatChange::LayoutChanged;
    }

    let n = ctrl_entries.len();
    if n == 0 {
        return FormatChange::Unknown;
    }

    // Count per-line transformation patterns
    let mut identical_count = 0;
    let mut prefix_count = 0;
    let mut suffix_count = 0;
    let mut suffix_partial_count = 0; // entry + 1 char appended (like / or *)
    let mut fields_expanded_count = 0;
    let mut wrapped_count = 0;
    let mut escaped_count = 0;

    for (ctrl, opt) in ctrl_entries.iter().zip(opt_lines.iter()) {
        let ct = ctrl.trim();
        let ot = opt.trim();

        if ct == ot {
            identical_count += 1;
            continue;
        }

        // Fields expanded: option line has many more whitespace-separated tokens
        let ctrl_fields = ct.split_whitespace().count();
        let opt_fields = ot.split_whitespace().count();
        if opt_fields > ctrl_fields + 2 {
            fields_expanded_count += 1;
            continue;
        }

        // Prefix: option line ends with control entry
        if ot.ends_with(ct) && ot.len() > ct.len() {
            prefix_count += 1;
            continue;
        }

        // Wrapped: control entry appears inside option line with chars on both sides
        // e.g., "app.log" (quoted) or \e]8;;...\e\\app.log\e]8;;\e\\ (hyperlinked)
        if ot.contains(ct) && !ot.starts_with(ct) && !ot.ends_with(ct) {
            wrapped_count += 1;
            continue;
        }

        // Suffix partial: entry + 1-2 chars appended (like / @ * for -F, -p)
        if ot.starts_with(ct) && ot.len() <= ct.len() + 2 {
            suffix_partial_count += 1;
            continue;
        }

        // Suffix: option line starts with control entry + more
        if ot.starts_with(ct) && ot.len() > ct.len() {
            suffix_count += 1;
            continue;
        }

        // Escaped: lines differ by character escaping (e.g., space → \\ space)
        // Simple heuristic: option line contains backslash-escaped versions of ctrl chars
        if ct.contains(' ') && ot.contains("\\ ") && ot.replace("\\ ", " ") == *ct {
            escaped_count += 1;
            continue;
        }
    }

    let changed = n - identical_count;
    if changed == 0 {
        return FormatChange::Unknown; // shouldn't happen if entries differ
    }

    // Check for decoration first (ANSI/OSC codes affect the raw string)
    let has_ansi = opt_lines.iter().any(|l| l.contains("\x1b["));
    let has_osc = opt_lines.iter().any(|l| l.contains("\x1b]"));
    if has_ansi && !ctrl_entries.iter().any(|l| l.contains("\x1b[")) {
        return FormatChange::Decorated;
    }

    // Check patterns by frequency among changed lines
    if fields_expanded_count > 0 && fields_expanded_count as f64 / n as f64 > 0.5 {
        return FormatChange::FieldsExpanded;
    }
    if prefix_count > 0 && prefix_count as f64 / n as f64 > 0.5 {
        return FormatChange::PrefixAdded;
    }
    if suffix_count > 0 && suffix_count as f64 / n as f64 > 0.5 {
        return FormatChange::SuffixAdded;
    }

    // Partial patterns: only some lines changed
    if wrapped_count > 0 && wrapped_count as f64 / changed as f64 > 0.5 {
        // If wrapping applies to most changed lines AND includes OSC codes → Wrapped
        if has_osc {
            return FormatChange::Wrapped;
        }
        // Check for quote wrapping
        let has_new_quotes = opt_lines.iter().any(|l| l.contains('"'))
            && !ctrl_entries.iter().any(|l| l.contains('"'));
        if has_new_quotes {
            return FormatChange::Wrapped;
        }
        return FormatChange::Wrapped;
    }

    if suffix_partial_count > 0 && suffix_partial_count as f64 / changed as f64 > 0.5 {
        return FormatChange::SuffixAddedPartial;
    }

    if escaped_count > 0 && escaped_count as f64 / changed as f64 > 0.5 {
        return FormatChange::Escaped;
    }

    FormatChange::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical() {
        let d = compute_structured_delta("foo\nbar\n", "foo\nbar\n");
        assert_eq!(d.relation, EntryRelation::Identical);
    }

    #[test]
    fn test_superset() {
        let ctrl = "app.log\ndata.csv\nhello.txt\n";
        let opt = ".\n..\n.hidden\napp.log\ndata.csv\nhello.txt\n";
        let d = compute_structured_delta(ctrl, opt);
        match &d.relation {
            EntryRelation::Superset { added } => {
                assert!(added.contains(&".".to_string()));
                assert!(added.contains(&"..".to_string()));
                assert!(added.contains(&".hidden".to_string()));
            }
            other => panic!("Expected Superset, got {:?}", other),
        }
    }

    #[test]
    fn test_subset() {
        let ctrl = "app.log\ndata.csv\nhello.txt\npattern.txt\n";
        let opt = "app.log\nhello.txt\n";
        let d = compute_structured_delta(ctrl, opt);
        match &d.relation {
            EntryRelation::Subset { removed } => {
                assert!(removed.contains(&"data.csv".to_string()));
                assert!(removed.contains(&"pattern.txt".to_string()));
            }
            other => panic!("Expected Subset, got {:?}", other),
        }
    }

    #[test]
    fn test_reordered() {
        let ctrl = "app.log\ndata.csv\nhello.txt\n";
        let opt = "hello.txt\ndata.csv\napp.log\n";
        let d = compute_structured_delta(ctrl, opt);
        assert_eq!(d.relation, EntryRelation::Reordered);
    }

    #[test]
    fn test_entries_preserved_prefix() {
        let ctrl = "app.log\ndata.csv\nhello.txt\n";
        let opt = "12345 app.log\n67890 data.csv\n11111 hello.txt\n";
        let d = compute_structured_delta(ctrl, opt);
        match &d.relation {
            EntryRelation::EntriesPreserved { format_change } => {
                assert_eq!(*format_change, FormatChange::PrefixAdded);
            }
            other => panic!("Expected EntriesPreserved(PrefixAdded), got {:?}", other),
        }
    }

    #[test]
    fn test_entries_preserved_fields_expanded() {
        let ctrl = "app.log\ndata.csv\n";
        let opt = "-rw-r--r-- 1 user group 1234 Jan 01 app.log\n-rw-r--r-- 1 user group 5678 Jan 01 data.csv\n";
        let d = compute_structured_delta(ctrl, opt);
        match &d.relation {
            EntryRelation::EntriesPreserved { format_change } => {
                assert_eq!(*format_change, FormatChange::FieldsExpanded);
            }
            other => panic!("Expected EntriesPreserved(FieldsExpanded), got {:?}", other),
        }
    }

    #[test]
    fn test_entries_preserved_suffix_partial() {
        // -F: appends / to dirs, @ to symlinks, * to executables (only some lines change)
        let ctrl = "app.log\nemptydir\nlink.txt\nscript.sh\nsubdir\n";
        let opt = "app.log\nemptydir/\nlink.txt@\nscript.sh*\nsubdir/\n";
        let d = compute_structured_delta(ctrl, opt);
        match &d.relation {
            EntryRelation::EntriesPreserved { format_change } => {
                assert_eq!(*format_change, FormatChange::SuffixAddedPartial);
            }
            other => panic!("Expected EntriesPreserved(SuffixAddedPartial), got {:?}", other),
        }
    }

    #[test]
    fn test_entries_preserved_wrapped_quotes() {
        // -Q: wraps every entry in quotes
        let ctrl = "app.log\ndata.csv\nhello.txt\n";
        let opt = "\"app.log\"\n\"data.csv\"\n\"hello.txt\"\n";
        let d = compute_structured_delta(ctrl, opt);
        match &d.relation {
            EntryRelation::EntriesPreserved { format_change } => {
                assert_eq!(*format_change, FormatChange::Wrapped);
            }
            other => panic!("Expected EntriesPreserved(Wrapped), got {:?}", other),
        }
    }

    #[test]
    fn test_entries_preserved_escaped() {
        // -b: escapes spaces
        let ctrl = "app.log\nspaces in name.txt\nhello.txt\n";
        let opt = "app.log\nspaces\\ in\\ name.txt\nhello.txt\n";
        let d = compute_structured_delta(ctrl, opt);
        match &d.relation {
            EntryRelation::EntriesPreserved { format_change } => {
                assert_eq!(*format_change, FormatChange::Escaped);
            }
            other => panic!("Expected EntriesPreserved(Escaped), got {:?}", other),
        }
    }

    #[test]
    fn test_disjoint() {
        let ctrl = "foo\nbar\nbaz\n";
        let opt = "completely\ndifferent\noutput\n";
        let d = compute_structured_delta(ctrl, opt);
        assert_eq!(d.relation, EntryRelation::Disjoint);
    }
}
