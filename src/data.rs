//! Experiment design data — content levels, structure templates, perturbations.
//!
//! Separated from code so the experiment parameters are reviewable in one place
//! and modifiable without changing logic. The Latin square assignment and
//! structure builders in discover.rs consume this data to construct contexts.

use crate::parse::{FileContent, Property, SetupCommand};

/// Content levels — lines for input.txt in each content archetype.
/// Each level exercises a different text-processing dimension.
pub fn content_alpha() -> Vec<String> {
    vec!["cherry", "Apple", "banana", "Date", "elderberry", "BANANA", "apple"]
        .into_iter().map(String::from).collect()
}

pub fn content_numeric() -> Vec<String> {
    vec![
        "100", "2", "30", "1", "20", "3", "10", "50", "8", "200",
        "15", "99", "7", "42", "1000", "5",
    ].into_iter().map(String::from).collect()
}

pub fn content_fielded() -> Vec<String> {
    vec!["bob:30:sales", "alice:25:eng", "charlie:35:sales"]
        .into_iter().map(String::from).collect()
}

/// Tabular content: tab-delimited fields, repeated rows, long lines.
/// Exercises: cut -f, paste -d, uniq -c/-d/-u, fold -w, awk, sort -t.
pub fn content_tabular() -> Vec<String> {
    vec![
        "name\tage\tcity",
        "alice\t30\tnew york",
        "bob\t25\tsan francisco",
        "alice\t30\tnew york",
        "charlie\t35\tchicago",
        "bob\t25\tsan francisco",
        "diana\t28\tlos angeles",
        "a]very long line that exceeds eighty characters in total width to exercise fold and fmt and similar line-wrapping tools properly",
        "eve\t22\tseattle",
        "alice\t30\tnew york",
        "frank\t40\tboston",
        "grace\t33\tdenver",
    ].into_iter().map(String::from).collect()
}

/// Content with tabs, blank lines, trailing whitespace, and mixed formatting.
/// Exercises: cat -n/-b/-s/-E/-T, fold, fmt, nl, expand/unexpand, col.
pub fn content_formatted() -> Vec<String> {
    vec![
        "first line",
        "",
        "",
        "\tindented with tab",
        "trailing spaces   ",
        "",
        "  leading spaces",
        "normal line",
        "\ttwo\ttabs",
        "last line",
        "",
    ].into_iter().map(String::from).collect()
}

/// Structure level: minimal — just input.txt and other.txt.
pub fn structure_minimal(content: &[String]) -> Vec<SetupCommand> {
    vec![
        SetupCommand::CreateFile { path: "input.txt".into(),
            content: FileContent::Lines(content.to_vec()) },
        SetupCommand::CreateFile { path: "other.txt".into(),
            content: FileContent::Lines(vec!["hello world".into()]) },
    ]
}

/// Structure level: standard — hidden files, subdir, symlink, executable.
pub fn structure_standard(content: &[String]) -> Vec<SetupCommand> {
    vec![
        SetupCommand::CreateFile { path: "input.txt".into(),
            content: FileContent::Lines(content.to_vec()) },
        SetupCommand::CreateFile { path: "other.txt".into(),
            content: FileContent::Lines(vec!["other content".into(), "second line".into()]) },
        SetupCommand::CreateFile { path: "a.txt".into(),
            content: FileContent::Lines(vec!["first".into()]) },
        SetupCommand::CreateFile { path: "b.txt".into(),
            content: FileContent::Lines(vec!["second".into()]) },
        SetupCommand::CreateFile { path: ".hidden".into(),
            content: FileContent::Lines(vec!["secret".into()]) },
        SetupCommand::CreateDir { path: "subdir".into() },
        SetupCommand::CreateFile { path: "subdir/nested.txt".into(),
            content: FileContent::Lines(vec!["nested".into()]) },
        SetupCommand::CreateLink { path: "link.txt".into(), target: "input.txt".into() },
        SetupCommand::CreateFile { path: "exec.sh".into(),
            content: FileContent::Lines(vec!["#!/bin/sh\necho hello".into()]) },
        SetupCommand::SetProps { path: "exec.sh".into(), props: vec![Property::Executable] },
    ]
}

/// Structure level: deep — 3-level nesting with directory symlink.
pub fn structure_deep(content: &[String]) -> Vec<SetupCommand> {
    vec![
        SetupCommand::CreateFile { path: "input.txt".into(),
            content: FileContent::Lines(content.to_vec()) },
        SetupCommand::CreateFile { path: "other.txt".into(),
            content: FileContent::Lines(vec!["deep other".into(), "line two".into(), "line three".into()]) },
        SetupCommand::CreateDir { path: "level1".into() },
        SetupCommand::CreateDir { path: "level1/level2".into() },
        SetupCommand::CreateFile { path: "level1/a.txt".into(),
            content: FileContent::Lines(vec!["depth one".into()]) },
        SetupCommand::CreateFile { path: "level1/level2/b.txt".into(),
            content: FileContent::Lines(vec!["depth two".into()]) },
        SetupCommand::CreateLink { path: "link_to_dir".into(), target: "level1".into() },
    ]
}

/// Property modifier: default — no additional properties.
pub fn props_default(_cmds: &mut Vec<SetupCommand>) {}

/// Property modifier: varied permissions — readonly file, flag-like filename.
pub fn props_perms(cmds: &mut Vec<SetupCommand>) {
    cmds.push(SetupCommand::CreateFile { path: "readonly.dat".into(),
        content: FileContent::Lines(vec!["protected".into()]) });
    cmds.push(SetupCommand::SetProps { path: "readonly.dat".into(),
        props: vec![Property::ReadOnly] });
    cmds.push(SetupCommand::CreateFile { path: "-rf".into(),
        content: FileContent::Lines(vec!["flag-like filename".into()]) });
}

/// Property modifier: varied timestamps — old mtime, large file.
pub fn props_times(cmds: &mut Vec<SetupCommand>) {
    cmds.push(SetupCommand::CreateFile { path: "old.txt".into(),
        content: FileContent::Lines(vec!["ancient".into()]) });
    cmds.push(SetupCommand::SetProps { path: "old.txt".into(),
        props: vec![Property::MtimeOld] });
    cmds.push(SetupCommand::CreateFile { path: "big.bin".into(),
        content: FileContent::Size(10000) });
}

/// Single-factor perturbations applied to the richest base context.
pub fn perturbations() -> Vec<SetupCommand> {
    vec![
        SetupCommand::Remove { path: ".hidden".into() },
        SetupCommand::Remove { path: "subdir".into() },
        SetupCommand::Remove { path: "link.txt".into() },
        SetupCommand::CreateFile { path: "input.txt".into(), content: FileContent::Empty },
        SetupCommand::SetProps { path: "input.txt".into(), props: vec![Property::ReadOnly] },
        SetupCommand::SetProps { path: "input.txt".into(), props: vec![Property::MtimeOld] },
        SetupCommand::CreateFile { path: "input.txt".into(), content: FileContent::Size(1) },
        SetupCommand::SetEnv { var: "LC_ALL".into(), value: "en_US.UTF-8".into() },
        SetupCommand::SetEnv { var: "COLUMNS".into(), value: "40".into() },
    ]
}

/// Pattern archetypes for pattern-taking tools (grep, sed, awk).
/// Each exercises a different regex/matching dimension.
pub const PATTERN_ARCHETYPES: &[&str] = &[
    "alpha",    // literal match
    "Alpha",    // case-sensitive match
    "a.*e",     // regex metacharacter
    "zzzzz",    // non-matching
];
