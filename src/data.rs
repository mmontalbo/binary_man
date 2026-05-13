//! Experiment design data — content levels, structure templates, perturbations.
//!
//! Content is loaded from fixture files in `fixtures/` via `include_str!`.
//! Structure builders and perturbations are defined here.
//! The context assignment in discover.rs consumes this data to construct grids.

use crate::parse::{FileContent, Property, SetupCommand};

// --- Content levels from fixture files ---
// Each fixture is a real-world data format or curated corpus.
// See fixtures/SOURCES.md for attribution.

fn lines(text: &str) -> Vec<String> {
    text.lines().map(String::from).collect()
}

/// Dictionary: 1500 sorted English words with mixed case, hyphens, accents.
pub fn content_words() -> Vec<String> { lines(include_str!("../fixtures/words.txt")) }

/// Numeric edge cases: integers, floats, hex, scientific notation, NaN, Infinity.
/// Source: Big List of Naughty Strings (MIT).
pub fn content_numbers() -> Vec<String> { lines(include_str!("../fixtures/numbers.txt")) }

/// Apache combined log format: IPs, timestamps, HTTP methods, status codes, user agents.
pub fn content_access_log() -> Vec<String> { lines(include_str!("../fixtures/access_log.txt")) }

/// RFC 4180 CSV: header row, quoted fields, accented names, empty fields, duplicates.
pub fn content_csv() -> Vec<String> { lines(include_str!("../fixtures/data.csv")) }

/// /etc/passwd format: colon-delimited, 7 fields, UIDs, shells, service accounts.
pub fn content_passwd() -> Vec<String> { lines(include_str!("../fixtures/passwd.txt")) }

/// BSD syslog format: timestamps, hostnames, PIDs, services, duplicate entries.
pub fn content_syslog() -> Vec<String> { lines(include_str!("../fixtures/syslog.txt")) }

/// Date/time strings: ISO 8601, RFC 2822, month names, timezones, edge cases.
pub fn content_dates() -> Vec<String> { lines(include_str!("../fixtures/dates.txt")) }

/// INI/env config: sections, key=value, comments, URLs, paths, booleans.
pub fn content_config() -> Vec<String> { lines(include_str!("../fixtures/config.txt")) }

/// Unix filesystem paths: absolute, relative, dotfiles, spaces, unicode, deep nesting.
pub fn content_paths() -> Vec<String> { lines(include_str!("../fixtures/paths.txt")) }

/// Whitespace edge cases: tabs, trailing spaces, blank lines, long lines, mixed indent.
pub fn content_formatted() -> Vec<String> { lines(include_str!("../fixtures/formatted.txt")) }

/// Unicode/emoji/RTL/CJK stress strings.
/// Source: Big List of Naughty Strings (MIT).
pub fn content_naughty() -> Vec<String> { lines(include_str!("../fixtures/naughty.txt")) }

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

/// Common subcommand verbs for behavioral subcommand discovery.
pub const SUBCOMMAND_CANDIDATES: &[&str] = &[
    "init", "add", "commit", "status", "diff", "log", "show",
    "clone", "push", "pull", "fetch", "merge", "rebase", "branch",
    "checkout", "reset", "rm", "mv", "tag", "stash", "remote",
    "build", "run", "test", "install", "clean", "update", "publish",
    "create", "delete", "list", "get", "set", "describe", "apply",
    "start", "stop", "restart", "exec", "inspect", "config",
    "new", "check", "fmt", "lint", "deploy", "serve", "migrate",
    "info", "version", "help",
];
