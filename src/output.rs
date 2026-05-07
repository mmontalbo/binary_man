//! Output formatting for observations and results.

use crate::execute::{FsChange, Observation};

/// Format run arguments as quoted strings.
pub fn format_args(args: &[String]) -> String {
    if args.is_empty() {
        "(no args)".into()
    } else {
        args.iter().map(|a| format!("\"{}\"", a)).collect::<Vec<_>>().join(" ")
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
    if !obs.trace_reads.is_empty() || !obs.trace_failed.is_empty() {
        out.push_str(&format!("{}trace:\n", indent));
        if !obs.trace_reads.is_empty() {
            out.push_str(&format!("{}  reads: {}\n", indent, obs.trace_reads.join(", ")));
        }
        if !obs.trace_failed.is_empty() {
            out.push_str(&format!("{}  failed: {}\n", indent, obs.trace_failed.join(", ")));
        }
    }
}
