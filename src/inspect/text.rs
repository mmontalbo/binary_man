//! Text-based output for inspect (non-TUI mode).

use super::data::load_state;
use super::format::{gate_label, next_action_summary};
use crate::enrich;
use anyhow::Result;
use std::path::Path;

pub(super) fn run_text_summary(doc_pack_root: &Path) -> Result<()> {
    let show_all = [false; 3];
    let (summary, data) = load_state(doc_pack_root, &show_all)?;
    print_text_summary(doc_pack_root, &summary, &data)
}

fn print_text_summary(
    doc_pack_root: &Path,
    summary: &enrich::StatusSummary,
    data: &super::data::InspectData,
) -> Result<()> {
    let binary = summary
        .binary_name
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let lock_label = gate_label(summary.lock.present, summary.lock.stale);
    let plan_label = gate_label(summary.plan.present, summary.plan.stale);
    let decision_label = summary.decision.as_str();
    let next_action = next_action_summary(&summary.next_action);

    println!("doc_pack: {}", doc_pack_root.display());
    println!("binary: {binary}");
    println!("lock: {lock_label}");
    println!("plan: {plan_label}");
    println!("decision: {decision_label}");
    println!("next_action: {next_action}");
    println!();

    // Work tab summary
    let work_preview = data
        .work
        .flat_items()
        .iter()
        .take(3)
        .filter_map(|(_, item)| item.map(|i| i.surface_id.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "work: {} items (preview: {})",
        data.work
            .flat_items()
            .iter()
            .filter(|(_, i)| i.is_some())
            .count(),
        if work_preview.is_empty() {
            "none"
        } else {
            &work_preview
        }
    );

    // Log tab summary
    let log_preview = data
        .log
        .iter()
        .take(3)
        .map(|entry| format!("cycle{}", entry.cycle))
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "log: {} entries (preview: {})",
        data.log.len(),
        if log_preview.is_empty() {
            "none"
        } else {
            &log_preview
        }
    );

    // Browse tab summary
    let browse_preview = data
        .browse
        .iter()
        .take(3)
        .map(|entry| entry.rel_path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "browse: {} items (preview: {})",
        data.browse.len(),
        if browse_preview.is_empty() {
            "none"
        } else {
            &browse_preview
        }
    );

    Ok(())
}
