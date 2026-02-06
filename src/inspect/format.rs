use crate::enrich;

use super::{PREVIEW_MAX_CHARS, PREVIEW_MAX_LINES};

pub(super) fn gate_label(present: bool, stale: bool) -> &'static str {
    if !present {
        "missing"
    } else if stale {
        "stale"
    } else {
        "fresh"
    }
}

pub(super) fn next_action_summary(action: &enrich::NextAction) -> String {
    match action {
        enrich::NextAction::Command {
            command, reason, ..
        } => {
            format!("command: {command} ({reason})")
        }
        enrich::NextAction::Edit { path, reason, .. } => {
            format!("edit: {path} ({reason})")
        }
    }
}

pub(super) fn truncate_text(text: &str, max_len: usize) -> String {
    if text.len() <= max_len || max_len <= 3 {
        return text.to_string();
    }
    let mut truncated = text[..max_len.saturating_sub(3)].to_string();
    truncated.push_str("...");
    truncated
}

pub(super) fn preview_text(text: &str) -> String {
    if text.trim().is_empty() {
        return "<empty>".to_string();
    }
    let mut out = String::new();
    let mut truncated = false;
    for (idx, line) in text.lines().enumerate() {
        if idx >= PREVIEW_MAX_LINES {
            truncated = true;
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
        if out.len() >= PREVIEW_MAX_CHARS {
            out.truncate(PREVIEW_MAX_CHARS);
            truncated = true;
            break;
        }
    }
    if truncated {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_label_variants() {
        assert_eq!(gate_label(false, false), "missing");
        assert_eq!(gate_label(true, true), "stale");
        assert_eq!(gate_label(true, false), "fresh");
    }

    #[test]
    fn preview_truncates() {
        let text = "line1\nline2\nline3";
        let preview = preview_text(text);
        assert!(preview.contains("line1"));
        assert!(preview.ends_with("..."));
    }
}
