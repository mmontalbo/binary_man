use super::data::load_state;
use super::format::{gate_label, next_action_summary};
use super::EvidenceFilter;
use crate::enrich;
use anyhow::Result;
use std::path::Path;

pub(super) fn run_text_summary(doc_pack_root: &Path) -> Result<()> {
    let show_all = [false; 4];
    let (summary, data) = load_state(doc_pack_root, &show_all, EvidenceFilter::All)?;
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

    let intent_preview = data
        .intent
        .iter()
        .take(3)
        .map(|entry| entry.rel_path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "intent: {} items (preview: {})",
        data.intent.len(),
        if intent_preview.is_empty() {
            "none"
        } else {
            &intent_preview
        }
    );

    let evidence_preview = data
        .evidence
        .entries
        .iter()
        .take(3)
        .map(|entry| entry.scenario_id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "evidence: {} scenarios (preview: {})",
        data.evidence.total_count,
        if evidence_preview.is_empty() {
            "none"
        } else {
            &evidence_preview
        }
    );

    let outputs_preview = data
        .outputs
        .iter()
        .take(3)
        .map(|entry| entry.rel_path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "outputs: {} items (preview: {})",
        data.outputs.len(),
        if outputs_preview.is_empty() {
            "none"
        } else {
            &outputs_preview
        }
    );

    let history_preview = data
        .history
        .iter()
        .take(2)
        .map(|entry| entry.rel_path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "history: {} items (preview: {})",
        data.history.len(),
        if history_preview.is_empty() {
            "none"
        } else {
            &history_preview
        }
    );
    Ok(())
}
