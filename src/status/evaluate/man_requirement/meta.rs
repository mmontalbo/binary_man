use crate::enrich;
use anyhow::Context;
use serde::Deserialize;
use std::fs;

#[derive(Deserialize, Default)]
pub(super) struct RenderSummaryMeta {
    #[serde(default)]
    pub(super) semantics_unmet: Vec<String>,
    #[serde(default)]
    pub(super) commands_entries: usize,
}

#[derive(Deserialize, Default)]
pub(super) struct ManMetaInputs {
    #[serde(default)]
    pub(super) inputs_hash: Option<String>,
    #[serde(default)]
    pub(super) render_summary: Option<RenderSummaryMeta>,
}

pub(super) fn load_meta(
    meta_path: &std::path::Path,
    meta_evidence: &enrich::EvidenceRef,
    blockers: &mut Vec<enrich::Blocker>,
    requirement_evidence: &[enrich::EvidenceRef],
    root: &std::path::Path,
) -> anyhow::Result<Option<ManMetaInputs>, enrich::RequirementStatus> {
    if !meta_path.is_file() {
        return Ok(None);
    }
    let bytes = match fs::read(meta_path).with_context(|| format!("read {}", meta_path.display())) {
        Ok(bytes) => bytes,
        Err(err) => {
            let blocker = enrich::Blocker {
                code: "man_meta_read_error".to_string(),
                message: err.to_string(),
                evidence: vec![meta_evidence.clone()],
                next_action: Some(format!("bman apply --doc-pack {}", root.display())),
            };
            blockers.push(blocker.clone());
            return Err(enrich::RequirementStatus {
                id: enrich::RequirementId::ManPage,
                status: enrich::RequirementState::Blocked,
                reason: "man metadata read error".to_string(),
                unverified_ids: Vec::new(),
                unverified_count: None,
                verification: None,
                evidence: requirement_evidence.to_vec(),
                blockers: vec![blocker],
            });
        }
    };
    match serde_json::from_slice::<ManMetaInputs>(&bytes) {
        Ok(meta) => Ok(Some(meta)),
        Err(err) => {
            let blocker = enrich::Blocker {
                code: "man_meta_parse_error".to_string(),
                message: err.to_string(),
                evidence: vec![meta_evidence.clone()],
                next_action: Some(format!("bman apply --doc-pack {}", root.display())),
            };
            blockers.push(blocker.clone());
            Err(enrich::RequirementStatus {
                id: enrich::RequirementId::ManPage,
                status: enrich::RequirementState::Blocked,
                reason: "man metadata parse error".to_string(),
                unverified_ids: Vec::new(),
                unverified_count: None,
                verification: None,
                evidence: requirement_evidence.to_vec(),
                blockers: vec![blocker],
            })
        }
    }
}

pub(super) fn apply_render_summary(
    summary: &RenderSummaryMeta,
    multi_command: bool,
    requirement_evidence: &[enrich::EvidenceRef],
    root: &std::path::Path,
) -> Option<(enrich::RequirementState, String, Vec<enrich::Blocker>)> {
    if !summary.semantics_unmet.is_empty() {
        let missing = summary.semantics_unmet.join(", ");
        return Some((
            enrich::RequirementState::Unmet,
            format!("rendered but semantics insufficient: {missing}"),
            Vec::new(),
        ));
    }
    if multi_command && summary.commands_entries == 0 {
        let blocker = enrich::Blocker {
            code: "man_commands_missing".to_string(),
            message: "man page missing COMMANDS section for subcommands".to_string(),
            evidence: requirement_evidence.to_vec(),
            next_action: Some(format!("bman apply --doc-pack {}", root.display())),
        };
        return Some((
            enrich::RequirementState::Blocked,
            "man page missing COMMANDS section".to_string(),
            vec![blocker],
        ));
    }
    None
}
