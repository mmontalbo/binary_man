use super::super::{format_preview, preview_ids};
use super::{LedgerEntries, VerificationEvalOutput};
use crate::enrich;

pub(super) struct AutoVerificationContext<'a> {
    pub(super) ledger_entries: Option<&'a LedgerEntries>,
    pub(super) evidence: &'a mut Vec<enrich::EvidenceRef>,
    pub(super) verification_next_action: &'a mut Option<enrich::NextAction>,
    pub(super) local_blockers: &'a [enrich::Blocker],
    pub(super) missing: &'a [String],
    pub(super) paths: &'a enrich::DocPackPaths,
}

pub(super) fn eval_auto_verification(
    auto_state: crate::status::verification::AutoVerificationState,
    ctx: &mut AutoVerificationContext<'_>,
) -> VerificationEvalOutput {
    let crate::status::verification::AutoVerificationState {
        targets,
        remaining_ids,
        remaining_by_kind,
        excluded,
        excluded_count,
        ..
    } = auto_state;
    if let Some(ledger_entries) = ctx.ledger_entries {
        for surface_id in &remaining_ids {
            if let Some(entry) = ledger_entries.get(surface_id) {
                ctx.evidence.extend(entry.evidence.iter().cloned());
            }
        }
    }
    let remaining_preview = preview_ids(&remaining_ids);
    let remaining_by_kind_summary = remaining_by_kind
        .iter()
        .map(|group| enrich::VerificationKindSummary {
            kind: group.kind.as_str().to_string(),
            target_count: group.target_count,
            remaining_count: group.remaining_ids.len(),
            remaining_preview: preview_ids(&group.remaining_ids),
        })
        .collect();
    let excluded_ids = excluded
        .iter()
        .map(|entry| entry.surface_id.clone())
        .collect::<Vec<_>>();
    let summary = enrich::VerificationTriageSummary {
        triaged_unverified_count: remaining_ids.len(),
        triaged_unverified_preview: remaining_preview,
        remaining_by_kind: remaining_by_kind_summary,
        excluded_count: (excluded_count > 0).then_some(excluded_count),
        behavior_excluded_count: excluded_count,
        behavior_excluded_preview: preview_ids(&excluded_ids),
        behavior_excluded_reasons: Vec::new(),
        excluded,
        behavior_unverified_reasons: Vec::new(),
        behavior_unverified_preview: Vec::new(),
        behavior_unverified_diagnostics: Vec::new(),
        behavior_warnings: Vec::new(),
        stub_blockers_preview: Vec::new(),
    };
    let summary_preview = format!(
        "auto verification: {} remaining ({})",
        summary.triaged_unverified_count,
        format_preview(
            summary.triaged_unverified_count,
            &summary.triaged_unverified_preview
        )
    );
    let has_unverified = summary.triaged_unverified_count > 0;

    let mut output = VerificationEvalOutput {
        triage_summary: Some(summary),
        unverified_ids: Vec::new(),
        behavior_verified_count: None,
        behavior_unverified_count: None,
    };
    if has_unverified {
        output.unverified_ids.push(summary_preview);
    }
    if ctx.verification_next_action.is_none()
        && !remaining_ids.is_empty()
        && ctx.local_blockers.is_empty()
        && ctx.missing.is_empty()
    {
        let remaining_total = remaining_ids.len();
        let by_kind = remaining_by_kind
            .iter()
            .map(|group| format!("{} {}", group.kind.as_str(), group.remaining_ids.len()))
            .collect::<Vec<_>>()
            .join(", ");
        let mut reason = format!("auto verification remaining: {remaining_total}");
        if !by_kind.is_empty() {
            reason.push_str(&format!(" ({by_kind})"));
        }
        reason.push_str(&format!(
            "; max_new_runs_per_apply={}",
            targets.max_new_runs_per_apply
        ));
        reason.push_str(&format!(
            "; set scenarios/plan.json.verification.policy.max_new_runs_per_apply >= {remaining_total} to finish in one apply"
        ));
        let root = ctx.paths.root().display();
        *ctx.verification_next_action = Some(enrich::NextAction::Command {
            command: format!("bman apply --doc-pack {root}"),
            reason,
            hint: Some("Run to execute auto verification".to_string()),
            payload: None,
        });
    }

    output
}
