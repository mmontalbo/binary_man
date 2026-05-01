//! Parser for the test script language.
//!
//! Format:
//!   context "name"
//!     file "path" "line1" "line2" ...
//!     dir "path"
//!     ...
//!
//!   context "other" extends "name"
//!     file "extra" "content"
//!
//!   test args "arg1" "arg2" ...
//!     in "ctx1" "ctx2"
//!     expect stdout <predicate>
//!     expect exit <predicate>

use anyhow::{bail, Context, Result};
use std::collections::HashMap;

/// A parsed test script.
#[derive(Debug)]
pub struct Script {
    pub contexts: Vec<NamedContext>,
    pub tests: Vec<Test>,
}

/// A named execution context with setup commands.
#[derive(Debug, Clone)]
pub struct NamedContext {
    pub name: String,
    pub extends: Option<String>,
    pub commands: Vec<SetupCommand>,
}

/// A setup command that modifies execution context state.
#[derive(Debug, Clone)]
pub enum SetupCommand {
    CreateFile {
        path: String,
        content: FileContent,
    },
    CreateDir {
        path: String,
    },
    CreateLink {
        path: String,
        target: String,
    },
    SetProps {
        path: String,
        props: Vec<Property>,
    },
    SetEnv {
        var: String,
        value: String,
    },
    Remove {
        path: String,
    },
}

#[derive(Debug, Clone)]
pub enum FileContent {
    Lines(Vec<String>),
    Size(usize),
    Empty,
}

#[derive(Debug, Clone)]
pub enum Property {
    Executable,
    MtimeOld,
    MtimeRecent,
    ReadOnly,
}

/// A test invocation with predictions.
#[derive(Debug)]
pub struct Test {
    pub args: Vec<String>,
    pub expectations: Vec<Expectation>,
    /// If set, only run in these contexts. None = all contexts.
    pub in_contexts: Option<Vec<String>>,
}

/// A single prediction about an output dimension.
#[derive(Debug, Clone)]
pub struct Expectation {
    pub dimension: OutputDimension,
    pub predicate: Predicate,
}

#[derive(Debug, Clone)]
pub enum OutputDimension {
    Stdout,
    Stderr,
    Exit,
}

#[derive(Debug, Clone)]
pub enum Predicate {
    // Stdout structural (vs another invocation)
    Empty,
    NotEmpty,
    Reordered { vs_args: Vec<String> },
    Superset { vs_args: Vec<String> },
    Subset { vs_args: Vec<String> },
    Preserved { vs_args: Vec<String> },

    // Stdout quantitative (vs another invocation)
    LinesSame { vs_args: Vec<String> },
    LinesMore { vs_args: Vec<String> },
    LinesFewer { vs_args: Vec<String> },
    LinesExactly(usize),

    // Stdout content
    Contains(String),
    NotContains(String),
    EveryLineMatches(String),

    // Stdout positional
    LineContains { line: usize, text: String },
    LineNotContains { line: usize, text: String },
    Before { first: String, second: String },

    // Stderr
    Unchanged { vs_args: Vec<String> },
    StderrEmpty,
    StderrNotEmpty,
    StderrContains(String),

    // Exit code
    ExitCode(i32),
    ExitUnchanged { vs_args: Vec<String> },
    ExitChanged { vs_args: Vec<String> },
}

/// Parse a test script from source text.
pub fn parse_script(source: &str) -> Result<Script> {
    let mut contexts: Vec<NamedContext> = Vec::new();
    let mut tests: Vec<Test> = Vec::new();
    let mut current_test: Option<Test> = None;
    let mut current_context: Option<NamedContext> = None;
    // Collect top-level setup commands (no context block) for backward compat
    let mut bare_setup: Vec<SetupCommand> = Vec::new();
    let mut has_named_contexts = false;

    for (line_num, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim();
        let line_num = line_num + 1;

        // Skip empty lines and comments (including #> tool annotations)
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(rest) = line.strip_prefix("context ") {
            // Start a new context — flush previous context and test
            if let Some(t) = current_test.take() {
                tests.push(t);
            }
            if let Some(ctx) = current_context.take() {
                contexts.push(ctx);
            }
            has_named_contexts = true;
            let ctx = parse_context_line(rest, line_num)?;
            current_context = Some(ctx);
        } else if let Some(rest) = line.strip_prefix("expect ") {
            // Expectation line — must be inside a test
            let test = current_test.as_mut().ok_or_else(|| {
                anyhow::anyhow!("line {}: 'expect' outside of a test block", line_num)
            })?;
            let exp = parse_expectation(rest, line_num)?;
            test.expectations.push(exp);
        } else if let Some(rest) = line.strip_prefix("in ") {
            // Context scope — must be inside a test (before any expects)
            let test = current_test.as_mut().ok_or_else(|| {
                anyhow::anyhow!("line {}: 'in' outside of a test block", line_num)
            })?;
            let scope = parse_quoted_strings(rest.trim(), line_num)?;
            test.in_contexts = Some(scope);
        } else if let Some(rest) = line.strip_prefix("test ") {
            // Start a new test — flush previous
            if let Some(t) = current_test.take() {
                tests.push(t);
            }
            // Flush context if open (test blocks come after contexts)
            if let Some(ctx) = current_context.take() {
                contexts.push(ctx);
            }
            let args = parse_test_line(rest, line_num)?;
            current_test = Some(Test {
                args,
                expectations: Vec::new(),
                in_contexts: None,
            });
        } else {
            // Setup command — must be inside a context (or bare top-level)
            let cmd = parse_setup_line(line, line_num)?;
            if let Some(ctx) = current_context.as_mut() {
                ctx.commands.push(cmd);
            } else {
                // Flush any open test first
                if let Some(t) = current_test.take() {
                    tests.push(t);
                }
                bare_setup.push(cmd);
            }
        }
    }

    // Flush final context and test
    if let Some(ctx) = current_context.take() {
        contexts.push(ctx);
    }
    if let Some(t) = current_test.take() {
        tests.push(t);
    }

    // Backward compatibility: if no named contexts, wrap bare setup as default
    if !has_named_contexts && !bare_setup.is_empty() {
        contexts.push(NamedContext {
            name: "(default)".to_string(),
            extends: None,
            commands: bare_setup,
        });
    } else if has_named_contexts && !bare_setup.is_empty() {
        bail!("cannot mix top-level setup commands with named contexts");
    }

    // If no contexts at all, create an empty default
    if contexts.is_empty() {
        contexts.push(NamedContext {
            name: "(default)".to_string(),
            extends: None,
            commands: Vec::new(),
        });
    }

    // Resolve extends
    let resolved = resolve_extends(&contexts)?;

    Ok(Script {
        contexts: resolved,
        tests,
    })
}

/// Parse `context "name"` or `context "name" extends "parent"`.
fn parse_context_line(rest: &str, line_num: usize) -> Result<NamedContext> {
    let tokens = tokenize(rest.trim(), line_num)?;
    if tokens.is_empty() {
        bail!("line {}: context requires a name", line_num);
    }
    let name = tokens[0].clone();
    let extends = if tokens.len() >= 3 && tokens[1] == "extends" {
        Some(tokens[2].clone())
    } else {
        None
    };
    Ok(NamedContext {
        name,
        extends,
        commands: Vec::new(),
    })
}

/// Resolve `extends` by copying parent commands and applying removes.
fn resolve_extends(contexts: &[NamedContext]) -> Result<Vec<NamedContext>> {
    let by_name: HashMap<&str, &NamedContext> = contexts
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();

    let mut resolved = Vec::new();
    for ctx in contexts {
        let mut commands = Vec::new();

        if let Some(ref parent_name) = ctx.extends {
            let parent = by_name.get(parent_name.as_str()).ok_or_else(|| {
                anyhow::anyhow!("context {:?} extends unknown context {:?}", ctx.name, parent_name)
            })?;
            // Copy parent commands (recursion not needed — parent already resolved if ordered)
            // For simplicity, only support one level of extends for now
            commands.extend(parent.commands.clone());
        }

        // Apply own commands, handling removes
        for cmd in &ctx.commands {
            match cmd {
                SetupCommand::Remove { path } => {
                    commands.retain(|c| match c {
                        SetupCommand::CreateFile { path: p, .. }
                        | SetupCommand::CreateDir { path: p }
                        | SetupCommand::CreateLink { path: p, .. }
                        | SetupCommand::SetProps { path: p, .. } => p != path,
                        _ => true,
                    });
                }
                other => commands.push(other.clone()),
            }
        }

        resolved.push(NamedContext {
            name: ctx.name.clone(),
            extends: ctx.extends.clone(),
            commands,
        });
    }

    Ok(resolved)
}

fn parse_test_line(rest: &str, line_num: usize) -> Result<Vec<String>> {
    let rest = rest.trim();
    if !rest.starts_with("args") {
        bail!("line {}: test line must start with 'args'", line_num);
    }
    let args_str = rest[4..].trim();
    parse_quoted_strings(args_str, line_num)
}

fn parse_setup_line(line: &str, line_num: usize) -> Result<SetupCommand> {
    let tokens = tokenize(line, line_num)?;
    if tokens.is_empty() {
        bail!("line {}: empty setup command", line_num);
    }

    match tokens[0].as_str() {
        "file" => {
            if tokens.len() < 2 {
                bail!("line {}: file command requires a path", line_num);
            }
            let path = tokens[1].clone();
            if tokens.len() == 2 || (tokens.len() == 3 && tokens[2] == "empty") {
                Ok(SetupCommand::CreateFile {
                    path,
                    content: FileContent::Empty,
                })
            } else if tokens.len() == 4 && tokens[2] == "size" {
                let size: usize = tokens[3]
                    .parse()
                    .with_context(|| format!("line {}: invalid size", line_num))?;
                Ok(SetupCommand::CreateFile {
                    path,
                    content: FileContent::Size(size),
                })
            } else {
                let lines = tokens[2..].to_vec();
                Ok(SetupCommand::CreateFile {
                    path,
                    content: FileContent::Lines(lines),
                })
            }
        }
        "dir" => {
            if tokens.len() < 2 {
                bail!("line {}: dir command requires a path", line_num);
            }
            Ok(SetupCommand::CreateDir {
                path: tokens[1].clone(),
            })
        }
        "link" => {
            if tokens.len() < 4 || tokens[2] != "->" {
                bail!(
                    "line {}: link syntax: link \"name\" -> \"target\"",
                    line_num
                );
            }
            Ok(SetupCommand::CreateLink {
                path: tokens[1].clone(),
                target: tokens[3].clone(),
            })
        }
        "props" => {
            if tokens.len() < 3 {
                bail!("line {}: props command requires path and property", line_num);
            }
            let path = tokens[1].clone();
            let mut props = Vec::new();
            for tok in &tokens[2..] {
                match tok.as_str() {
                    "executable" => props.push(Property::Executable),
                    "readonly" => props.push(Property::ReadOnly),
                    "mtime" => {} // consumed by next token
                    "old" => props.push(Property::MtimeOld),
                    "recent" => props.push(Property::MtimeRecent),
                    _ => bail!("line {}: unknown property '{}'", line_num, tok),
                }
            }
            Ok(SetupCommand::SetProps { path, props })
        }
        "env" => {
            if tokens.len() < 3 {
                bail!("line {}: env command requires VAR and value", line_num);
            }
            Ok(SetupCommand::SetEnv {
                var: tokens[1].clone(),
                value: tokens[2].clone(),
            })
        }
        "remove" => {
            if tokens.len() < 2 {
                bail!("line {}: remove command requires a path", line_num);
            }
            Ok(SetupCommand::Remove {
                path: tokens[1].clone(),
            })
        }
        _ => bail!("line {}: unknown command '{}'", line_num, tokens[0]),
    }
}

fn parse_expectation(rest: &str, line_num: usize) -> Result<Expectation> {
    let rest = rest.trim();
    let (dimension, predicate_str) = if let Some(r) = rest.strip_prefix("stdout ") {
        (OutputDimension::Stdout, r.trim())
    } else if let Some(r) = rest.strip_prefix("stderr ") {
        (OutputDimension::Stderr, r.trim())
    } else if let Some(r) = rest.strip_prefix("exit ") {
        (OutputDimension::Exit, r.trim())
    } else {
        bail!("line {}: expect requires stdout|stderr|exit", line_num);
    };

    let predicate = parse_predicate(&dimension, predicate_str, line_num)?;
    Ok(Expectation {
        dimension,
        predicate,
    })
}

fn parse_predicate(dim: &OutputDimension, s: &str, line_num: usize) -> Result<Predicate> {
    match dim {
        OutputDimension::Stdout => parse_stdout_predicate(s, line_num),
        OutputDimension::Stderr => parse_stderr_predicate(s, line_num),
        OutputDimension::Exit => parse_exit_predicate(s, line_num),
    }
}

fn parse_stdout_predicate(s: &str, line_num: usize) -> Result<Predicate> {
    if s == "empty" {
        return Ok(Predicate::Empty);
    }
    if s == "not-empty" {
        return Ok(Predicate::NotEmpty);
    }
    if let Some(rest) = s.strip_prefix("contains ") {
        let text = parse_single_quoted(rest.trim(), line_num)?;
        return Ok(Predicate::Contains(text));
    }
    if let Some(rest) = s.strip_prefix("not-contains ") {
        let text = parse_single_quoted(rest.trim(), line_num)?;
        return Ok(Predicate::NotContains(text));
    }
    if let Some(rest) = s.strip_prefix("every-line-matches ") {
        let pat = parse_single_quoted(rest.trim(), line_num)?;
        return Ok(Predicate::EveryLineMatches(pat));
    }
    if let Some(rest) = s.strip_prefix("lines exactly ") {
        let n: usize = rest
            .trim()
            .parse()
            .with_context(|| format!("line {}: invalid line count", line_num))?;
        return Ok(Predicate::LinesExactly(n));
    }
    if let Some(rest) = s.strip_prefix("line ") {
        let rest = rest.trim();
        let (n_str, remainder) = rest.split_once(' ')
            .ok_or_else(|| anyhow::anyhow!("line {}: expected 'line N contains/not-contains'", line_num))?;
        let n: usize = n_str.parse()
            .with_context(|| format!("line {}: invalid line number", line_num))?;
        let remainder = remainder.trim();
        if let Some(rest) = remainder.strip_prefix("contains ") {
            let text = parse_single_quoted(rest.trim(), line_num)?;
            return Ok(Predicate::LineContains { line: n, text });
        }
        if let Some(rest) = remainder.strip_prefix("not-contains ") {
            let text = parse_single_quoted(rest.trim(), line_num)?;
            return Ok(Predicate::LineNotContains { line: n, text });
        }
        bail!("line {}: expected 'contains' or 'not-contains' after line number", line_num);
    }
    if s.contains("\" before \"") {
        let tokens = parse_quoted_strings(s.replace(" before ", " ").trim(), line_num)?;
        if tokens.len() == 2 {
            return Ok(Predicate::Before {
                first: tokens[0].clone(),
                second: tokens[1].clone(),
            });
        }
        bail!("line {}: expected '\"X\" before \"Y\"'", line_num);
    }

    #[allow(clippy::type_complexity)]
    let relational: &[(&str, fn(Vec<String>) -> Predicate)] = &[
        ("preserved vs ", |a| Predicate::Preserved { vs_args: a }),
        ("reordered vs ", |a| Predicate::Reordered { vs_args: a }),
        ("superset vs ", |a| Predicate::Superset { vs_args: a }),
        ("subset vs ", |a| Predicate::Subset { vs_args: a }),
        ("lines same as ", |a| Predicate::LinesSame { vs_args: a }),
        ("lines more than ", |a| Predicate::LinesMore { vs_args: a }),
        ("lines fewer than ", |a| Predicate::LinesFewer { vs_args: a }),
    ];

    for (prefix, constructor) in relational {
        if let Some(rest) = s.strip_prefix(prefix) {
            let vs_args = parse_quoted_strings(rest.trim(), line_num)?;
            return Ok(constructor(vs_args));
        }
    }

    bail!("line {}: unknown stdout predicate: '{}'", line_num, s)
}

fn parse_stderr_predicate(s: &str, line_num: usize) -> Result<Predicate> {
    if s == "empty" {
        return Ok(Predicate::StderrEmpty);
    }
    if s == "not-empty" {
        return Ok(Predicate::StderrNotEmpty);
    }
    if let Some(rest) = s.strip_prefix("unchanged vs ") {
        let vs_args = parse_quoted_strings(rest.trim(), line_num)?;
        return Ok(Predicate::Unchanged { vs_args });
    }
    if let Some(rest) = s.strip_prefix("contains ") {
        let text = parse_single_quoted(rest.trim(), line_num)?;
        return Ok(Predicate::StderrContains(text));
    }
    bail!("line {}: unknown stderr predicate: '{}'", line_num, s)
}

fn parse_exit_predicate(s: &str, line_num: usize) -> Result<Predicate> {
    if let Some(rest) = s.strip_prefix("unchanged vs ") {
        let vs_args = parse_quoted_strings(rest.trim(), line_num)?;
        return Ok(Predicate::ExitUnchanged { vs_args });
    }
    if let Some(rest) = s.strip_prefix("changed vs ") {
        let vs_args = parse_quoted_strings(rest.trim(), line_num)?;
        return Ok(Predicate::ExitChanged { vs_args });
    }
    let code: i32 = s
        .trim()
        .parse()
        .with_context(|| format!("line {}: invalid exit code", line_num))?;
    Ok(Predicate::ExitCode(code))
}

/// Tokenize a line, respecting quoted strings.
fn tokenize(line: &str, _line_num: usize) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '"' {
            chars.next();
            let mut s = String::new();
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some('\\') => {
                        if let Some(next) = chars.next() {
                            match next {
                                'n' => s.push('\n'),
                                't' => s.push('\t'),
                                '\\' => s.push('\\'),
                                '"' => s.push('"'),
                                other => {
                                    s.push('\\');
                                    s.push(other);
                                }
                            }
                        }
                    }
                    Some(c) => s.push(c),
                    None => break,
                }
            }
            tokens.push(s);
        } else {
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                s.push(c);
                chars.next();
            }
            tokens.push(s);
        }
    }

    Ok(tokens)
}

fn parse_quoted_strings(s: &str, line_num: usize) -> Result<Vec<String>> {
    tokenize(s, line_num)
}

fn parse_single_quoted(s: &str, line_num: usize) -> Result<String> {
    let tokens = tokenize(s, line_num)?;
    if tokens.len() != 1 {
        bail!(
            "line {}: expected single quoted string, got {} tokens",
            line_num,
            tokens.len()
        );
    }
    Ok(tokens[0].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_legacy_format() {
        let source = r#"
# Setup
file "data.txt" "hello" "world"
dir "subdir"

# Tests
test args "."
  expect stdout not-empty
  expect exit 0

test args "." "-a"
  expect stdout lines more than "."
  expect exit 0
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.contexts.len(), 1);
        assert_eq!(script.contexts[0].name, "(default)");
        assert_eq!(script.contexts[0].commands.len(), 2);
        assert_eq!(script.tests.len(), 2);
        assert_eq!(script.tests[0].args, vec!["."]);
        assert_eq!(script.tests[1].args, vec![".", "-a"]);
        assert!(script.tests[0].in_contexts.is_none());
    }

    #[test]
    fn test_parse_named_contexts() {
        let source = r#"
context "base"
  file "visible.txt" "hello"
  file ".hidden" "secret"

context "empty"

test args "."
  expect stdout not-empty

test args "." "-a"
  in "base"
  expect stdout superset vs "."
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.contexts.len(), 2);
        assert_eq!(script.contexts[0].name, "base");
        assert_eq!(script.contexts[0].commands.len(), 2);
        assert_eq!(script.contexts[1].name, "empty");
        assert_eq!(script.contexts[1].commands.len(), 0);
        assert_eq!(script.tests.len(), 2);
        assert!(script.tests[0].in_contexts.is_none());
        assert_eq!(script.tests[1].in_contexts, Some(vec!["base".to_string()]));
    }

    #[test]
    fn test_parse_extends() {
        let source = r#"
context "base"
  file "a.txt" "hello"
  file ".hidden" "secret"

context "with backup" extends "base"
  file "backup~" "old"

test args "."
  expect stdout not-empty
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.contexts.len(), 2);
        // "with backup" should have base's 2 commands + its own 1
        assert_eq!(script.contexts[1].commands.len(), 3);
    }

    #[test]
    fn test_parse_remove() {
        let source = r#"
context "base"
  file "a.txt" "hello"
  file ".hidden" "secret"

context "no hidden" extends "base"
  remove ".hidden"

test args "."
  expect stdout not-empty
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.contexts[1].commands.len(), 1); // only a.txt remains
    }
}
