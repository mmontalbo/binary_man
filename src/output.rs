//! Output formatting for observations and results.

use crate::execute::{FsChange, Observation};

/// Sensitive file patterns that should be flagged in trace output.
const SENSITIVE_PATHS: &[&str] = &[".netrc", ".git-credentials", ".ssh/", "credentials", "token"];

/// Check if an observation has anomalies worth expanding in default mode.
pub fn has_anomalies(obs: &Observation, majority_exit: Option<i32>) -> bool {
    let exit = obs.exit_code.unwrap_or(-1);
    // Signal death
    if exit > 128 { return true; }
    // Network attempts
    if !obs.trace_net.is_empty() { return true; }
    // Credential file access
    if obs.trace_reads.iter().chain(obs.trace_failed.iter())
        .any(|p| SENSITIVE_PATHS.iter().any(|s| p.contains(s))) { return true; }
    // Exit code diverges from majority
    if let Some(maj) = majority_exit {
        if exit != maj { return true; }
    }
    false
}

/// Format a one-line trace summary (counts + anomaly flags).
pub fn format_trace_summary(obs: &Observation) -> String {
    let mut parts = Vec::new();

    if !obs.trace_reads.is_empty() {
        parts.push(format!("{} reads", obs.trace_reads.len()));
    }
    if !obs.trace_failed.is_empty() {
        parts.push(format!("{} probes", obs.trace_failed.len()));
    }
    if !obs.trace_execs.is_empty() {
        parts.push(format!("{} execs", obs.trace_execs.len()));
    }
    if !obs.trace_net.is_empty() {
        let blocked = obs.trace_net.iter().filter(|n| n.contains("blocked")).count();
        if blocked > 0 {
            parts.push(format!("NET: {} blocked", blocked));
        } else {
            parts.push(format!("NET: {} connections", obs.trace_net.len()));
        }
    }
    if !obs.trace_signals.is_empty() {
        parts.push(format!("SIGNALS: {}", obs.trace_signals.len()));
    }

    // Flag sensitive file access
    let sensitive: Vec<&String> = obs.trace_reads.iter().chain(obs.trace_failed.iter())
        .filter(|p| SENSITIVE_PATHS.iter().any(|s| p.contains(s)))
        .collect();
    if !sensitive.is_empty() {
        parts.push(format!("SENSITIVE: {}", sensitive.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
    }

    parts.join(" | ")
}

/// Format resource usage as a compact summary.
pub fn format_resources(res: &crate::execute::ResourceUsage) -> String {
    if res.wall_time_ms > 0 {
        format!("{}ms", res.wall_time_ms)
    } else {
        String::new()
    }
}

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

/// Format an observation without trace file lists (for default mode).
pub fn format_obs_brief(out: &mut String, obs: &Observation, indent: &str) {
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

/// Format a single observation's output (full, including trace file lists).
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
    let has_trace = !obs.trace_reads.is_empty() || !obs.trace_failed.is_empty()
        || !obs.trace_execs.is_empty() || !obs.trace_net.is_empty()
        || !obs.trace_signals.is_empty();
    if has_trace {
        out.push_str(&format!("{}trace:\n", indent));
        if !obs.trace_reads.is_empty() {
            out.push_str(&format!("{}  reads: {}\n", indent, obs.trace_reads.join(", ")));
        }
        if !obs.trace_failed.is_empty() {
            out.push_str(&format!("{}  failed: {}\n", indent, obs.trace_failed.join(", ")));
        }
        if !obs.trace_execs.is_empty() {
            out.push_str(&format!("{}  execs: {}\n", indent, obs.trace_execs.join(", ")));
        }
        if !obs.trace_net.is_empty() {
            out.push_str(&format!("{}  net: {}\n", indent, obs.trace_net.join(", ")));
        }
        if !obs.trace_signals.is_empty() {
            out.push_str(&format!("{}  signals: {}\n", indent, obs.trace_signals.join(", ")));
        }
    }
}
