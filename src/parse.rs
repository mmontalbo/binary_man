//! Parser for the probe language.
//!
//! Four concepts: context, vary, run, from.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;

/// A parsed probe file.
#[derive(Debug)]
pub struct Script {
    pub contexts: Vec<NamedContext>,
    pub runs: Vec<Run>,
}

/// A named execution context with setup commands.
#[derive(Debug, Clone)]
pub struct NamedContext {
    pub name: String,
    pub extends: Option<String>,
    pub commands: Vec<SetupCommand>,
}

/// A setup command.
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

/// A vary block.
#[derive(Debug)]
pub struct VaryBlock {
    pub base: String,
    pub perturbations: Vec<SetupCommand>,
}

/// A run invocation with optional diff reference and scoping.
#[derive(Debug)]
pub struct Run {
    pub args: Vec<String>,
    pub in_contexts: Option<Vec<String>>,
    pub stdin: Option<StdinSource>,
    /// If set, diff this run's results from the reference run.
    pub diff_from: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub enum StdinSource {
    Lines(Vec<String>),
    FromFile(String),
}

/// Parse a probe file.
pub fn parse_script(source: &str) -> Result<Script> {
    let mut contexts: Vec<NamedContext> = Vec::new();
    let mut vary_blocks: Vec<VaryBlock> = Vec::new();
    let mut runs: Vec<Run> = Vec::new();

    let mut current_context: Option<NamedContext> = None;
    let mut current_vary: Option<VaryBlock> = None;
    let mut current_run: Option<Run> = None;
    let mut current_from: Option<Vec<String>> = None; // args of the from-reference
    let mut current_in: Option<Vec<String>> = None; // block-level in scope

    for (line_num, raw_line) in source.lines().enumerate() {
        let line = strip_comment(raw_line.trim());
        let line_num = line_num + 1;

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with("expect ") {
            bail!("line {}: 'expect' is not supported yet", line_num);
        }

        if let Some(rest) = line.strip_prefix("context ") {
            flush_run(&mut current_run, &mut runs);
            flush_context(&mut current_context, &mut contexts);
            flush_vary(&mut current_vary, &mut vary_blocks);
            current_from = None;
            current_in = None;
            current_context = Some(parse_context_line(rest, line_num)?);
        } else if let Some(rest) = line.strip_prefix("vary from ") {
            flush_run(&mut current_run, &mut runs);
            flush_context(&mut current_context, &mut contexts);
            flush_vary(&mut current_vary, &mut vary_blocks);
            current_from = None;
            current_in = None;
            let tokens = tokenize(rest, line_num)?;
            if tokens.is_empty() {
                bail!("line {}: vary requires a base context name", line_num);
            }
            current_vary = Some(VaryBlock {
                base: tokens[0].clone(),
                perturbations: Vec::new(),
            });
        } else if let Some(rest) = line.strip_prefix("from ") {
            // Start a from-block: sets the diff reference for subsequent runs
            flush_run(&mut current_run, &mut runs);
            flush_context(&mut current_context, &mut contexts);
            flush_vary(&mut current_vary, &mut vary_blocks);
            let ref_args = parse_quoted_strings(rest.trim(), line_num)?;
            if ref_args.is_empty() {
                bail!("line {}: from requires reference args", line_num);
            }
            current_from = Some(ref_args);
        } else if let Some(rest) = line.strip_prefix("run ") {
            // run = observation invocation (always top-level)
            flush_run(&mut current_run, &mut runs);
            flush_context(&mut current_context, &mut contexts);
            flush_vary(&mut current_vary, &mut vary_blocks);

            let args = parse_quoted_strings(rest.trim(), line_num)?;
            current_run = Some(Run {
                args,
                in_contexts: current_in.clone(),
                stdin: None,
                diff_from: current_from.clone(),
            });
        } else if let Some(rest) = line.strip_prefix("in ") {
            // Always block-level: flush current run, scope subsequent runs
            flush_run(&mut current_run, &mut runs);
            flush_context(&mut current_context, &mut contexts);
            flush_vary(&mut current_vary, &mut vary_blocks);
            current_from = None;
            current_in = Some(parse_quoted_strings(rest.trim(), line_num)?);
        } else if let Some(rest) = line.strip_prefix("stdin ") {
            let run = current_run.as_mut().ok_or_else(|| {
                anyhow::anyhow!("line {}: 'stdin' outside of a run block", line_num)
            })?;
            let rest = rest.trim();
            if let Some(path) = rest.strip_prefix("from ") {
                let tokens = tokenize(path, line_num)?;
                if tokens.is_empty() {
                    bail!("line {}: stdin from requires a path", line_num);
                }
                run.stdin = Some(StdinSource::FromFile(tokens[0].clone()));
            } else {
                let lines = parse_quoted_strings(rest, line_num)?;
                run.stdin = Some(StdinSource::Lines(lines));
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

    flush_run(&mut current_run, &mut runs);
    flush_context(&mut current_context, &mut contexts);
    flush_vary(&mut current_vary, &mut vary_blocks);

    if contexts.is_empty() && vary_blocks.is_empty() {
        contexts.push(NamedContext {
            name: "(default)".to_string(),
            extends: None,
            commands: Vec::new(),
        });
    }

    resolve_extends(&mut contexts)?;
    resolve_vary(&mut contexts, &vary_blocks)?;

    Ok(Script { contexts, runs })
}

fn flush_run(run: &mut Option<Run>, runs: &mut Vec<Run>) {
    if let Some(r) = run.take() {
        runs.push(r);
    }
}

fn flush_context(ctx: &mut Option<NamedContext>, contexts: &mut Vec<NamedContext>) {
    if let Some(c) = ctx.take() {
        contexts.push(c);
    }
}

fn flush_vary(vary: &mut Option<VaryBlock>, vary_blocks: &mut Vec<VaryBlock>) {
    if let Some(v) = vary.take() {
        vary_blocks.push(v);
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
    Ok(NamedContext { name, extends, commands: Vec::new() })
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
            if tokens.len() >= 4 && tokens[2] == "->" {
                Ok(SetupCommand::CreateLink { path, target: tokens[3].clone() })
            } else if tokens.len() == 2 || (tokens.len() == 3 && tokens[2] == "empty") {
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
                Ok(SetupCommand::CreateFile { path, content: FileContent::Lines(tokens[2..].to_vec()) })
            }
        }
        "dir" => {
            if tokens.len() < 2 { bail!("line {}: dir requires a path", line_num); }
            Ok(SetupCommand::CreateDir { path: tokens[1].clone() })
        }
        "props" => {
            if tokens.len() < 3 { bail!("line {}: props requires path and property", line_num); }
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
            if tokens.len() < 3 { bail!("line {}: env requires VAR and value", line_num); }
            Ok(SetupCommand::SetEnv { var: tokens[1].clone(), value: tokens[2].clone() })
        }
        "remove" => {
            if tokens.len() < 2 { bail!("line {}: remove requires a target", line_num); }
            if tokens[1] == "env" {
                if tokens.len() < 3 { bail!("line {}: remove env requires a var name", line_num); }
                Ok(SetupCommand::RemoveEnv { var: tokens[2].clone() })
            } else {
                Ok(SetupCommand::Remove { path: tokens[1].clone() })
            }
        }
        "invoke" => {
            Ok(SetupCommand::Invoke { args: tokens[1..].to_vec() })
        }
        _ => bail!("line {}: unknown command '{}'", line_num, tokens[0]),
    }
}

fn resolve_extends(contexts: &mut [NamedContext]) -> Result<()> {
    // Store original (pre-resolution) commands per context.
    let own_cmds: HashMap<String, Vec<SetupCommand>> = contexts
        .iter()
        .map(|c| (c.name.clone(), c.commands.clone()))
        .collect();

    // Resolve each context, recursing into parents first.
    let mut resolved: HashMap<String, Vec<SetupCommand>> = HashMap::new();
    let names: Vec<String> = contexts.iter().map(|c| c.name.clone()).collect();
    let extends: HashMap<String, Option<String>> = contexts
        .iter()
        .map(|c| (c.name.clone(), c.extends.clone()))
        .collect();

    fn resolve_one(
        name: &str,
        own_cmds: &HashMap<String, Vec<SetupCommand>>,
        extends: &HashMap<String, Option<String>>,
        resolved: &mut HashMap<String, Vec<SetupCommand>>,
        depth: usize,
    ) -> Result<Vec<SetupCommand>> {
        if let Some(cmds) = resolved.get(name) {
            return Ok(cmds.clone());
        }
        if depth > 20 {
            anyhow::bail!("extends cycle detected at {:?}", name);
        }
        let my_cmds = own_cmds.get(name).cloned().unwrap_or_default();
        let result = if let Some(Some(parent)) = extends.get(name) {
            let parent_cmds = resolve_one(parent, own_cmds, extends, resolved, depth + 1)?;
            let mut merged = parent_cmds;
            merged.extend(my_cmds);
            merged
        } else {
            my_cmds
        };
        resolved.insert(name.to_string(), result.clone());
        Ok(result)
    }

    for name in &names {
        resolve_one(name, &own_cmds, &extends, &mut resolved, 0)?;
    }

    for ctx in contexts.iter_mut() {
        if let Some(cmds) = resolved.remove(&ctx.name) {
            ctx.commands = cmds;
        }
    }
    Ok(())
}

fn resolve_vary(contexts: &mut Vec<NamedContext>, vary_blocks: &[VaryBlock]) -> Result<()> {
    for vary in vary_blocks {
        let base = contexts.iter().find(|c| c.name == vary.base).ok_or_else(|| {
            anyhow::anyhow!("vary references unknown context {:?}", vary.base)
        })?;
        let base_cmds = base.commands.clone();

        for perturbation in &vary.perturbations {
            let variant_name = format!("{} / {}", vary.base, describe_perturbation(perturbation));
            let mut cmds = base_cmds.clone();

            cmds.push(perturbation.clone());

            contexts.push(NamedContext { name: variant_name, extends: None, commands: cmds });
        }
    }
    Ok(())
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
        SetupCommand::Invoke { args } => format!("run {:?}", args),
        _ => format!("{:?}", cmd),
    }
}

pub fn tokenize(line: &str, _line_num: usize) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() { chars.next(); continue; }
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
                s.push(c); chars.next();
            }
            tokens.push(s);
        }
    }
    Ok(tokens)
}

fn parse_quoted_strings(s: &str, line_num: usize) -> Result<Vec<String>> {
    tokenize(s, line_num)
}

/// Strip inline comments: everything after an unquoted `#` is removed.
fn strip_comment(line: &str) -> &str {
    let mut in_quote = false;
    for (i, c) in line.char_indices() {
        if c == '"' { in_quote = !in_quote; }
        if c == '#' && !in_quote { return line[..i].trim_end(); }
    }
    line
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

run "."
run "." "-a"
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.contexts.len(), 1);
        assert_eq!(script.runs.len(), 2);
        assert!(script.runs[0].diff_from.is_none());
    }

    #[test]
    fn test_parse_from_block() {
        let source = r#"
context "base"
  file "a.txt" "hello"

run "."

from "."
  run "." "-a"
  run "." "-l"
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.runs.len(), 3);
        assert!(script.runs[0].diff_from.is_none()); // standalone run "."
        assert_eq!(script.runs[1].diff_from, Some(vec![".".to_string()])); // from "."
        assert_eq!(script.runs[2].diff_from, Some(vec![".".to_string()])); // from "."
    }

    #[test]
    fn test_parse_invoke_in_context() {
        let source = r#"
context "repo"
  invoke "init"
  file "readme.md" "hello"
  invoke "add" "."
  invoke "commit" "-m" "initial"

run "status"
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.contexts.len(), 1);
        assert_eq!(script.contexts[0].commands.len(), 4); // init, file, add, commit
        assert_eq!(script.runs.len(), 1); // only "status" is an observation run
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

run "."
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.contexts.len(), 3); // base + 2 variants
    }

    #[test]
    fn test_parse_in_block() {
        let source = r#"
context "base"
  file "a.txt" "hello"

context "other"
  file "b.txt" "world"

in "base"
  run "."

  from "."
    run "." "-a"
    run "." "-l"
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.runs.len(), 3);
        // All runs scoped to "base"
        for run in &script.runs {
            assert_eq!(run.in_contexts, Some(vec!["base".to_string()]));
        }
        // from-block runs have diff_from
        assert!(script.runs[0].diff_from.is_none());
        assert_eq!(script.runs[1].diff_from, Some(vec![".".to_string()]));
        assert_eq!(script.runs[2].diff_from, Some(vec![".".to_string()]));
    }

    #[test]
    fn test_parse_in_block_multiple() {
        let source = r#"
context "base"
  file "a.txt" "hello"

context "other"

in "base"
  run "."

in "other"
  run "." "-v"
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.runs.len(), 2);
        assert_eq!(script.runs[0].in_contexts, Some(vec!["base".to_string()]));
        // Second in-block scopes to "other"
        assert_eq!(script.runs[1].in_contexts, Some(vec!["other".to_string()]));
    }

    #[test]
    fn test_parse_in_block_cleared_by_context() {
        let source = r#"
context "base"
  file "a.txt" "hello"

in "base"
  run "." "-a"

context "fresh"
  file "b.txt" "world"

run "."
"#;
        let script = parse_script(source).unwrap();
        assert_eq!(script.runs.len(), 2);
        assert_eq!(script.runs[0].in_contexts, Some(vec!["base".to_string()]));
        // After new context, in-scope is cleared
        assert!(script.runs[1].in_contexts.is_none());
    }

    #[test]
    fn test_reject_expect() {
        let source = "context \"b\"\n  file \"a\" \"b\"\n\nrun \".\"\n  expect stdout not-empty\n";
        assert!(parse_script(source).is_err());
    }
}
