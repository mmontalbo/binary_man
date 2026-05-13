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

/// Build the full set of execution contexts for a grid.
/// Latin square of 5 content types × 3 structure levels × 3 property levels,
/// plus breadth-only extras, perturbations, and locale variation.
pub fn build_contexts() -> Vec<crate::parse::NamedContext> {
    use crate::parse::{self, NamedContext};

    let build = |name: &str, content: &[String],
                 structure_fn: fn(&[String]) -> Vec<SetupCommand>,
                 props_fn: fn(&mut Vec<SetupCommand>)| -> NamedContext {
        let mut cmds = structure_fn(content);
        props_fn(&mut cmds);
        NamedContext { name: name.into(), extends: None, commands: cmds, stdin: None }
    };

    let words = content_words();
    let numbers = content_numbers();
    let passwd = content_passwd();
    let formatted = content_formatted();
    let csv = content_csv();

    let mut contexts = vec![
        build("words_minimal",      &words,     structure_minimal,  props_default),
        build("words_standard",     &words,     structure_standard, props_perms),
        build("words_deep",         &words,     structure_deep,     props_times),
        build("numbers_minimal",    &numbers,   structure_minimal,  props_times),
        build("numbers_standard",   &numbers,   structure_standard, props_default),
        build("numbers_deep",       &numbers,   structure_deep,     props_perms),
        build("passwd_minimal",     &passwd,    structure_minimal,  props_perms),
        build("passwd_standard",    &passwd,    structure_standard, props_times),
        build("passwd_deep",        &passwd,    structure_deep,     props_default),
        build("formatted_minimal",  &formatted, structure_minimal,  props_default),
        build("formatted_standard", &formatted, structure_standard, props_times),
        build("formatted_deep",     &formatted, structure_deep,     props_perms),
        build("csv_minimal",        &csv,       structure_minimal,  props_times),
        build("csv_standard",       &csv,       structure_standard, props_perms),
        build("csv_deep",           &csv,       structure_deep,     props_default),
        NamedContext { name: "empty_dir".into(), extends: None, commands: vec![], stdin: None },
    ];

    // Breadth-only extras (minimal structure)
    for (name, content) in [
        ("access_log", content_access_log()),
        ("syslog",     content_syslog()),
        ("dates",      content_dates()),
        ("config",     content_config()),
        ("paths",      content_paths()),
        ("naughty",    content_naughty()),
    ] {
        contexts.push(build(&format!("{}_minimal", name), &content, structure_minimal, props_default));
    }

    // Perturbations from numbers_standard
    let base = contexts.iter().find(|c| c.name == "numbers_standard").unwrap().clone();
    for p in &perturbations() {
        let mut cmds = base.commands.clone();
        cmds.push(p.clone());
        contexts.push(NamedContext {
            name: format!("numbers_standard / {}", parse::describe_perturbation(p)),
            extends: None, commands: cmds, stdin: None,
        });
    }

    // Locale perturbation on alpha content
    let alpha = contexts.iter().find(|c| c.name == "words_minimal").unwrap().clone();
    let mut cmds = alpha.commands.clone();
    cmds.push(SetupCommand::SetEnv { var: "LC_ALL".into(), value: "en_US.UTF-8".into() });
    contexts.push(NamedContext {
        name: "words_minimal / env LC_ALL=en_US.UTF-8".into(),
        extends: None, commands: cmds, stdin: None,
    });

    // Stdin contexts: content piped via stdin exercises stdin-primary tools.
    // Multiple stdin variants exercise different splitting/delimiter behavior.
    let stdin_lines = parse::StdinSource::Lines(
        vec!["cherry".into(), "apple".into(), "banana".into()]
    );
    let stdin_delimited = parse::StdinSource::Lines(
        vec!["a,b,c".into(), "d:e:f".into(), "g h i".into()]
    );
    for (base_name, stdin) in [
        ("words_minimal", &stdin_lines),
        ("numbers_minimal", &stdin_lines),
        ("passwd_minimal", &stdin_delimited),
    ] {
        if let Some(base) = contexts.iter().find(|c| c.name == base_name).cloned() {
            contexts.push(NamedContext {
                name: format!("{} / stdin", base_name),
                extends: None,
                commands: base.commands,
                stdin: Some(stdin.clone()),
            });
        }
    }

    contexts
}

