//! Output formatting for observations and results.

use crate::execute::{FsChange, Observation};
use crate::parse::{SetupCommand, Property};

/// Check if an observation has anomalies worth expanding in default mode.
pub fn has_anomalies(obs: &Observation, majority_exit: Option<i32>) -> bool {
    let exit = obs.exit_code.unwrap_or(-1);
    if exit > 128 { return true; }
    if let Some(maj) = majority_exit {
        if exit != maj { return true; }
    }
    false
}

/// Extract quoted strings from a formatted run label like `"-b" "input.txt"`.
pub fn parse_label(label: &str) -> Vec<&str> {
    label.split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s)
        .collect()
}

/// Format run arguments as quoted strings.
pub fn format_args(args: &[crate::parse::Arg]) -> String {
    if args.is_empty() {
        "(no args)".into()
    } else {
        args.iter().map(|a| a.display()).collect::<Vec<_>>().join(" ")
    }
}

/// Format exit code with signal name when applicable.
pub fn format_exit(code: i32) -> String {
    if code > 128 {
        let sig = code - 128;
        let name = match sig {
            2 => "SIGINT",
            6 => "SIGABRT",
            9 => "SIGKILL",
            11 => "SIGSEGV",
            13 => "SIGPIPE",
            14 => "SIGALRM",
            15 => "SIGTERM",
            _ => "",
        };
        if name.is_empty() {
            format!("{} (signal {})", code, sig)
        } else {
            format!("{} ({})", code, name)
        }
    } else {
        code.to_string()
    }
}

/// Format a context group label.
pub fn format_context_group(names: &[&str], total: usize) -> String {
    if names.len() == 1 {
        names[0].to_string()
    } else if names.len() == total {
        "all contexts".into()
    } else {
        format!("{} contexts ({})", names.len(), names.join(", "))
    }
}

/// Format a single observation's output.
pub fn format_obs(out: &mut String, obs: &Observation, indent: &str) {
    let stdout_lines: Vec<&str> = obs.stdout.lines().collect();
    if stdout_lines.is_empty() {
        out.push_str(&format!("{}stdout: (empty)\n", indent));
    } else {
        out.push_str(&format!("{}stdout ({} lines):\n", indent, stdout_lines.len()));
        for line in stdout_lines.iter().take(20) {
            out.push_str(&format!("{}  {}\n", indent, line));
        }
        if stdout_lines.len() > 20 {
            out.push_str(&format!("{}  ... ({} more)\n", indent, stdout_lines.len() - 20));
        }
    }
    if !obs.stderr.trim().is_empty() {
        out.push_str(&format!("{}stderr: {}\n", indent, obs.stderr.trim()));
    }
    out.push_str(&format!("{}exit: {}\n", indent, format_exit(obs.exit_code.unwrap_or(-1))));
    if !obs.fs_changes.is_empty() {
        out.push_str(&format!("{}fs:\n", indent));
        for change in &obs.fs_changes {
            match change {
                FsChange::Created { path, size } => {
                    out.push_str(&format!("{}  created: {} ({} bytes)\n", indent, path, size));
                }
                FsChange::Deleted { path } => {
                    out.push_str(&format!("{}  deleted: {}\n", indent, path));
                }
                FsChange::Modified { path, detail } => {
                    out.push_str(&format!("{}  modified: {} ({})\n", indent, path, detail));
                }
            }
        }
    }
}

/// Strip ANSI escape sequences from a string for readable report output.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            if c.is_ascii_alphabetic() { in_esc = false; }
        } else if c == '\x1b' {
            in_esc = true;
        } else {
            out.push(c);
        }
    }
    out
}

/// Format a SetupCommand for display in skeleton/diagnostic output.
pub fn format_setup_cmd(cmd: &SetupCommand) -> String {
    match cmd {
        SetupCommand::CreateFile { path, content } => {
            let preview: String = match content {
                crate::parse::FileContent::Lines(lines) => {
                    let joined = lines.join("\\n");
                    if joined.len() > 60 { format!("{}...", &joined[..57]) } else { joined }
                }
                crate::parse::FileContent::Size(size) =>
                    format!("<generated {} bytes>", size),
                crate::parse::FileContent::Empty => "<empty>".into(),
                crate::parse::FileContent::From(src) =>
                    format!("<from {}>", src),
            };
            format!("write \"{}\" \"{}\"", path, preview)
        }
        SetupCommand::CreateDir { path } => format!("mkdir \"{}\"", path),
        SetupCommand::CreateLink { path, target } => format!("symlink \"{}\" -> \"{}\"", path, target),
        SetupCommand::SetProps { path, props } => {
            let p: Vec<&str> = props.iter().map(|prop| match prop {
                Property::Executable => "executable",
                Property::MtimeOld => "mtime old",
                Property::MtimeRecent => "mtime recent",
                Property::ReadOnly => "readonly",
            }).collect();
            format!("props \"{}\" {}", path, p.join(" "))
        }
        SetupCommand::SetEnv { var, value } => format!("env {} \"{}\"", var, value),
        SetupCommand::Remove { path } => format!("remove \"{}\"", path),
        SetupCommand::RemoveEnv { var } => format!("remove env {}", var),
        SetupCommand::Invoke { args } => {
            let quoted: Vec<String> = args.iter().map(|a| format!("\"{}\"", a)).collect();
            format!("invoke {}", quoted.join(" "))
        }
    }
}
