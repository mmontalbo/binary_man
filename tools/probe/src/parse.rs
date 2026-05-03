//! Parser for the probe test language.
//!
//! Layer 1: contexts (with extends, vary, remove) and test blocks.
//! Layer 2: #> observation lines (skipped during parsing).

use anyhow::{bail, Context, Result};
use std::collections::HashMap;

/// A parsed test file.
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
    CreateFile { path: String, content: FileContent },
    CreateDir { path: String },
    CreateLink { path: String, target: String },
    SetProps { path: String, props: Vec<Property> },
    SetEnv { var: String, value: String },
    Remove { path: String },
    RemoveEnv { var: String },
    Invoke { args: Vec<String> },
}

#[derive(Debug, Clone)]
pub enum FileContent {
    Lines(Vec<String>),
    Size(usize),
    Empty,
    From(String),
}

#[derive(Debug, Clone)]
pub enum Property {
    Executable,
    MtimeOld,
    MtimeRecent,
    ReadOnly,
}

/// A vary block: generates perturbation variants of a base context.
#[derive(Debug)]
pub struct VaryBlock {
    pub base: String,
    pub perturbations: Vec<SetupCommand>,
}

/// A test invocation.
#[derive(Debug)]
pub struct Test {
    pub args: Vec<String>,
    pub in_contexts: Option<Vec<String>>,
    pub stdin: Option<StdinSource>,
}

#[derive(Debug, Clone)]
pub enum StdinSource {
    Lines(Vec<String>),
    FromFile(String),
}

/// Parse a test script from source text.
pub fn parse_script(source: &str) -> Result<Script> {
    let mut contexts: Vec<NamedContext> = Vec::new();
    let mut vary_blocks: Vec<VaryBlock> = Vec::new();
    let mut tests: Vec<Test> = Vec::new();
    let mut current_context: Option<NamedContext> = None;
    let mut current_vary: Option<VaryBlock> = None;
    let mut current_test: Option<Test> = None;

    for (line_num, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim();
        let line_num = line_num + 1;

        // Skip empty lines, comments, and tool annotations
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Reject expect lines (layer 3, not supported yet)
        if line.starts_with("expect ") {
            bail!("line {}: 'expect' is not supported in this version (layer 3 not yet implemented)", line_num);
        }

        if let Some(rest) = line.strip_prefix("context ") {
            flush_current(&mut current_context, &mut current_vary, &mut current_test,
                         &mut contexts, &mut vary_blocks, &mut tests);
            current_context = Some(parse_context_line(rest, line_num)?);
        } else if let Some(rest) = line.strip_prefix("vary from ") {
            flush_current(&mut current_context, &mut current_vary, &mut current_test,
                         &mut contexts, &mut vary_blocks, &mut tests);
            let tokens = tokenize(rest, line_num)?;
            if tokens.is_empty() {
                bail!("line {}: vary requires a base context name", line_num);
            }
            current_vary = Some(VaryBlock {
                base: tokens[0].clone(),
                perturbations: Vec::new(),
            });
        } else if let Some(rest) = line.strip_prefix("test ") {
            flush_current(&mut current_context, &mut current_vary, &mut current_test,
                         &mut contexts, &mut vary_blocks, &mut tests);
            current_test = Some(parse_test_line(rest, line_num)?);
        } else if let Some(rest) = line.strip_prefix("in ") {
            let test = current_test.as_mut().ok_or_else(|| {
                anyhow::anyhow!("line {}: 'in' outside of a test block", line_num)
            })?;
            test.in_contexts = Some(parse_quoted_strings(rest.trim(), line_num)?);
        } else if let Some(rest) = line.strip_prefix("stdin ") {
            let test = current_test.as_mut().ok_or_else(|| {
                anyhow::anyhow!("line {}: 'stdin' outside of a test block", line_num)
            })?;
            let rest = rest.trim();
            if let Some(path) = rest.strip_prefix("from ") {
                let tokens = tokenize(path, line_num)?;
                if tokens.is_empty() {
                    bail!("line {}: stdin from requires a path", line_num);
                }
                test.stdin = Some(StdinSource::FromFile(tokens[0].clone()));
            } else {
                let lines = parse_quoted_strings(rest, line_num)?;
                test.stdin = Some(StdinSource::Lines(lines));
            }
        } else {
            // Setup command — goes into current context or vary block
            let cmd = parse_setup_line(line, line_num)?;
            if let Some(ref mut vary) = current_vary {
                vary.perturbations.push(cmd);
            } else if let Some(ref mut ctx) = current_context {
                ctx.commands.push(cmd);
            } else {
                bail!("line {}: setup command outside of context or vary block", line_num);
            }
        }
    }

    flush_current(&mut current_context, &mut current_vary, &mut current_test,
                 &mut contexts, &mut vary_blocks, &mut tests);

    // If no contexts at all, create an empty default
    if contexts.is_empty() && vary_blocks.is_empty() {
        contexts.push(NamedContext {
            name: "(default)".to_string(),
            extends: None,
            commands: Vec::new(),
        });
    }

    // Resolve extends
    resolve_extends(&mut contexts)?;

    // Resolve vary blocks into additional contexts
    resolve_vary(&mut contexts, &vary_blocks)?;

    Ok(Script { contexts, tests })
}

fn flush_current(
    ctx: &mut Option<NamedContext>,
    vary: &mut Option<VaryBlock>,
    test: &mut Option<Test>,
    contexts: &mut Vec<NamedContext>,
    vary_blocks: &mut Vec<VaryBlock>,
    tests: &mut Vec<Test>,
) {
    if let Some(c) = ctx.take() {
        contexts.push(c);
    }
    if let Some(v) = vary.take() {
        vary_blocks.push(v);
    }
    if let Some(t) = test.take() {
        tests.push(t);
    }
}

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

fn parse_test_line(rest: &str, line_num: usize) -> Result<Test> {
    let rest = rest.trim();
    if !rest.starts_with("args") {
        bail!("line {}: test line must start with 'args'", line_num);
    }
    let args_str = rest[4..].trim();
    let args = if args_str.is_empty() {
        Vec::new()
    } else {
        parse_quoted_strings(args_str, line_num)?
    };
    Ok(Test {
        args,
        in_contexts: None,
        stdin: None,
    })
}

fn parse_setup_line(line: &str, line_num: usize) -> Result<SetupCommand> {
    let tokens = tokenize(line, line_num)?;
    if tokens.is_empty() {
        bail!("line {}: empty setup command", line_num);
    }

    match tokens[0].as_str() {
        "file" => {
            if tokens.len() < 2 {
                bail!("line {}: file requires a path", line_num);
            }
            let path = tokens[1].clone();
            if tokens.len() == 2 || (tokens.len() == 3 && tokens[2] == "empty") {
                Ok(SetupCommand::CreateFile { path, content: FileContent::Empty })
            } else if tokens.len() >= 3 && tokens[2] == "size" {
                if tokens.len() < 4 {
                    bail!("line {}: file size requires a number", line_num);
                }
                let size: usize = tokens[3].parse()
                    .with_context(|| format!("line {}: invalid size", line_num))?;
                Ok(SetupCommand::CreateFile { path, content: FileContent::Size(size) })
            } else if tokens.len() >= 3 && tokens[2] == "from" {
                if tokens.len() < 4 {
                    bail!("line {}: file from requires a path", line_num);
                }
                Ok(SetupCommand::CreateFile { path, content: FileContent::From(tokens[3].clone()) })
            } else {
                let lines = tokens[2..].to_vec();
                Ok(SetupCommand::CreateFile { path, content: FileContent::Lines(lines) })
            }
        }
        "dir" => {
            if tokens.len() < 2 {
                bail!("line {}: dir requires a path", line_num);
            }
            Ok(SetupCommand::CreateDir { path: tokens[1].clone() })
        }
        "link" => {
            if tokens.len() < 4 || tokens[2] != "->" {
                bail!("line {}: link syntax: link \"name\" -> \"target\"", line_num);
            }
            Ok(SetupCommand::CreateLink {
                path: tokens[1].clone(),
                target: tokens[3].clone(),
            })
        }
        "props" => {
            if tokens.len() < 3 {
                bail!("line {}: props requires path and property", line_num);
            }
            let path = tokens[1].clone();
            let mut props = Vec::new();
            for tok in &tokens[2..] {
                match tok.as_str() {
                    "executable" => props.push(Property::Executable),
                    "readonly" => props.push(Property::ReadOnly),
                    "mtime" => {}
                    "old" => props.push(Property::MtimeOld),
                    "recent" => props.push(Property::MtimeRecent),
                    _ => bail!("line {}: unknown property '{}'", line_num, tok),
                }
            }
            Ok(SetupCommand::SetProps { path, props })
        }
        "env" => {
            if tokens.len() < 3 {
                bail!("line {}: env requires VAR and value", line_num);
            }
            Ok(SetupCommand::SetEnv {
                var: tokens[1].clone(),
                value: tokens[2].clone(),
            })
        }
        "remove" => {
            if tokens.len() < 2 {
                bail!("line {}: remove requires a target", line_num);
            }
            if tokens[1] == "env" {
                if tokens.len() < 3 {
                    bail!("line {}: remove env requires a variable name", line_num);
                }
                Ok(SetupCommand::RemoveEnv { var: tokens[2].clone() })
            } else {
                Ok(SetupCommand::Remove { path: tokens[1].clone() })
            }
        }
        "invoke" => {
            let args = tokens[1..].to_vec();
            Ok(SetupCommand::Invoke { args })
        }
        _ => bail!("line {}: unknown command '{}'", line_num, tokens[0]),
    }
}

/// Resolve extends by copying parent commands.
fn resolve_extends(contexts: &mut [NamedContext]) -> Result<()> {
    let by_name: HashMap<String, Vec<SetupCommand>> = contexts
        .iter()
        .map(|c| (c.name.clone(), c.commands.clone()))
        .collect();

    for ctx in contexts.iter_mut() {
        if let Some(ref parent_name) = ctx.extends {
            let parent_cmds = by_name.get(parent_name).ok_or_else(|| {
                anyhow::anyhow!("context {:?} extends unknown {:?}", ctx.name, parent_name)
            })?;
            let mut merged = parent_cmds.clone();
            // Apply child commands: removes filter, others append
            for cmd in &ctx.commands {
                match cmd {
                    SetupCommand::Remove { path } => {
                        merged.retain(|c| !matches_path(c, path));
                    }
                    SetupCommand::RemoveEnv { var } => {
                        merged.retain(|c| !matches!(c, SetupCommand::SetEnv { var: v, .. } if v == var));
                    }
                    other => merged.push(other.clone()),
                }
            }
            ctx.commands = merged;
        }
    }
    Ok(())
}

/// Resolve vary blocks into named variant contexts.
fn resolve_vary(contexts: &mut Vec<NamedContext>, vary_blocks: &[VaryBlock]) -> Result<()> {
    for vary in vary_blocks {
        let base = contexts.iter().find(|c| c.name == vary.base).ok_or_else(|| {
            anyhow::anyhow!("vary references unknown context {:?}", vary.base)
        })?;
        let base_cmds = base.commands.clone();

        for perturbation in &vary.perturbations {
            let variant_name = format!("{} / {}", vary.base, describe_perturbation(perturbation));
            let mut cmds = base_cmds.clone();

            match perturbation {
                SetupCommand::Remove { path } => {
                    cmds.retain(|c| !matches_path(c, path));
                }
                SetupCommand::RemoveEnv { var } => {
                    cmds.retain(|c| !matches!(c, SetupCommand::SetEnv { var: v, .. } if v == var));
                }
                other => {
                    // For file/props overrides, remove existing commands for same path then add
                    if let Some(path) = get_path(other) {
                        cmds.retain(|c| {
                            if let Some(p) = get_path(c) {
                                p != path
                            } else {
                                true
                            }
                        });
                    }
                    cmds.push(other.clone());
                }
            }

            contexts.push(NamedContext {
                name: variant_name,
                extends: None,
                commands: cmds,
            });
        }
    }
    Ok(())
}

fn matches_path(cmd: &SetupCommand, path: &str) -> bool {
    match cmd {
        SetupCommand::CreateFile { path: p, .. }
        | SetupCommand::CreateDir { path: p }
        | SetupCommand::CreateLink { path: p, .. }
        | SetupCommand::SetProps { path: p, .. } => p == path,
        _ => false,
    }
}

fn get_path(cmd: &SetupCommand) -> Option<&str> {
    match cmd {
        SetupCommand::CreateFile { path, .. }
        | SetupCommand::CreateDir { path }
        | SetupCommand::CreateLink { path, .. }
        | SetupCommand::SetProps { path, .. } => Some(path),
        _ => None,
    }
}

fn describe_perturbation(cmd: &SetupCommand) -> String {
    match cmd {
        SetupCommand::Remove { path } => format!("remove {}", path),
        SetupCommand::RemoveEnv { var } => format!("remove env {}", var),
        SetupCommand::CreateFile { path, content } => {
            match content {
                FileContent::Size(n) => format!("{}=size:{}", path, n),
                FileContent::Lines(l) if l.len() == 1 => format!("{}={:?}", path, l[0]),
                FileContent::Lines(l) => {
                    let preview = &l[0];
                    let truncated = if preview.len() > 20 {
                        format!("{}...", &preview[..20])
                    } else {
                        preview.clone()
                    };
                    format!("{}={:?}+{}lines", path, truncated, l.len())
                }
                FileContent::Empty => format!("{}=empty", path),
                FileContent::From(src) => format!("{}=from:{}", path, src),
            }
        }
        SetupCommand::SetProps { path, props } => {
            let p: Vec<&str> = props.iter().map(|p| match p {
                Property::Executable => "executable",
                Property::MtimeOld => "mtime=old",
                Property::MtimeRecent => "mtime=recent",
                Property::ReadOnly => "readonly",
            }).collect();
            format!("{} {}", path, p.join(" "))
        }
        SetupCommand::SetEnv { var, value } => format!("env {}={}", var, value),
        SetupCommand::Invoke { args } => format!("invoke {:?}", args),
        _ => format!("{:?}", cmd),
    }
}

/// Tokenize a line, respecting quoted strings.
pub fn tokenize(line: &str, _line_num: usize) -> Result<Vec<String>> {
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
                                other => { s.push('\\'); s.push(other); }
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
                if c.is_whitespace() { break; }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic() {
        let source = r#"
context "base"
  file "a.txt" "hello"
  dir "sub"

test args "."
test args "." "-a"
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.contexts.len(), 1);
        assert_eq!(script.contexts[0].name, "base");
        assert_eq!(script.tests.len(), 2);
    }

    #[test]
    fn test_parse_extends() {
        let source = r#"
context "base"
  file "a.txt" "hello"

context "extra" extends "base"
  file "b.txt" "world"

test args "."
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.contexts.len(), 2);
        assert_eq!(script.contexts[1].commands.len(), 2); // inherited + own
    }

    #[test]
    fn test_parse_vary() {
        let source = r#"
context "base"
  file "a.txt" "hello"
  file ".hidden" "secret"

vary from "base"
  remove ".hidden"
  file "a.txt" size 1000

test args "."
"#;
        let script = parse_script(source).unwrap();
        // base + 2 variants = 3 contexts
        assert_eq!(script.contexts.len(), 3);
        assert!(script.contexts[1].name.contains("remove .hidden"));
        assert!(script.contexts[2].name.contains("size:1000"));
    }

    #[test]
    fn test_parse_test_with_in_and_stdin() {
        let source = r#"
context "base"
  file "a.txt" "hello"

test args "." "-a"
  in "base"

test args "pattern"
  stdin "line1" "line2"

test args
  stdin from "data.txt"
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.tests.len(), 3);
        assert_eq!(script.tests[0].in_contexts, Some(vec!["base".to_string()]));
        assert!(matches!(script.tests[1].stdin, Some(StdinSource::Lines(_))));
        assert!(matches!(script.tests[2].stdin, Some(StdinSource::FromFile(_))));
    }

    #[test]
    fn test_reject_expect() {
        let source = r#"
context "base"
  file "a.txt" "hello"

test args "."
  expect stdout not-empty
"#;
        let result = parse_script(source);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expect"));
    }
}
