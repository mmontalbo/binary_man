//! Structured delta computation between control and option stdout.
//! Simplified version from the main crate's delta.rs.

use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryRelation {
    Superset,
    Subset,
    Reordered,
    Preserved,
    PreservedPrefixAdded,
    PreservedFieldsExpanded,
    PreservedWrapped,
    Complement,
    Collapsed,
    Disjoint,
    Identical,
}

/// Compute the structural relationship between two stdout strings.
pub fn classify_stdout(control: &str, option: &str) -> EntryRelation {
    if control == option {
        return EntryRelation::Identical;
    }

    let ctrl_lines: Vec<&str> = control.lines().filter(|l| !l.is_empty()).collect();
    let opt_lines: Vec<&str> = option.lines().filter(|l| !l.is_empty()).collect();

    if ctrl_lines.is_empty() && opt_lines.is_empty() {
        return EntryRelation::Identical;
    }
    if ctrl_lines.is_empty() {
        return EntryRelation::Superset;
    }
    if opt_lines.is_empty() {
        return EntryRelation::Collapsed;
    }

    let ctrl_set: HashSet<&str> = ctrl_lines.iter().copied().collect();
    let opt_set: HashSet<&str> = opt_lines.iter().copied().collect();

    // Exact line-set comparisons
    if ctrl_set == opt_set && ctrl_lines != opt_lines {
        return EntryRelation::Reordered;
    }
    if ctrl_set.is_subset(&opt_set) && opt_set.len() > ctrl_set.len() {
        return EntryRelation::Superset;
    }
    if opt_set.is_subset(&ctrl_set) && ctrl_set.len() > opt_set.len() {
        return EntryRelation::Subset;
    }

    // Entry preservation via substring matching
    let found = ctrl_lines
        .iter()
        .filter(|c| opt_lines.iter().any(|o| o.contains(*c)))
        .count();
    let rate = found as f64 / ctrl_lines.len().max(1) as f64;

    if rate >= 0.8 {
        if ctrl_lines.len() == opt_lines.len() {
            // Detect format change sub-type
            let n = ctrl_lines.len();
            let fields_exp = ctrl_lines
                .iter()
                .zip(&opt_lines)
                .filter(|(c, o)| o.split_whitespace().count() > c.split_whitespace().count() + 2)
                .count();
            let prefix = ctrl_lines
                .iter()
                .zip(&opt_lines)
                .filter(|(c, o)| o.ends_with(*c) && o.len() > c.len())
                .count();
            let wrapped = ctrl_lines
                .iter()
                .zip(&opt_lines)
                .filter(|(c, o)| {
                    *c != *o && o.contains(*c) && !o.starts_with(*c) && !o.ends_with(*c)
                })
                .count();

            if fields_exp as f64 / n as f64 > 0.5 {
                return EntryRelation::PreservedFieldsExpanded;
            }
            if prefix as f64 / n as f64 > 0.5 {
                return EntryRelation::PreservedPrefixAdded;
            }
            let changed = ctrl_lines.iter().zip(&opt_lines).filter(|(c, o)| c != o).count();
            if changed > 0 && wrapped as f64 / changed as f64 > 0.5 {
                return EntryRelation::PreservedWrapped;
            }
            return EntryRelation::Preserved;
        }
        return EntryRelation::Preserved;
    }

    // Complement check
    if !ctrl_set.is_empty()
        && opt_set.len() >= 2
        && ctrl_set.is_disjoint(&opt_set)
        && rate == 0.0
    {
        return EntryRelation::Complement;
    }

    // Collapsed check (much shorter output)
    if opt_lines.len() as f64 / ctrl_lines.len() as f64 <= 0.5 {
        return EntryRelation::Collapsed;
    }

    EntryRelation::Disjoint
}
