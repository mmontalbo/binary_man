use crate::enrich::{self, DocPackPaths};
use crate::scenarios;
use crate::workflow;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::format::preview_text;
use super::{EvidenceFilter, Tab, PREVIEW_LIMIT};

#[derive(Debug, Clone)]
pub(super) struct ArtifactEntry {
    pub(super) rel_path: String,
    pub(super) path: PathBuf,
    pub(super) exists: bool,
}

#[derive(Debug, Clone)]
pub(super) struct EvidenceEntry {
    pub(super) scenario_id: String,
    pub(super) path: Option<PathBuf>,
    pub(super) exists: bool,
    pub(super) exit_code: Option<i32>,
    pub(super) exit_signal: Option<i32>,
    pub(super) timed_out: Option<bool>,
    pub(super) stdout_preview: Option<String>,
    pub(super) stderr_preview: Option<String>,
    pub(super) error: Option<String>,
}

#[derive(Debug)]
pub(super) struct EvidenceList {
    pub(super) total_count: usize,
    pub(super) counts: EvidenceCounts,
    pub(super) entries: Vec<EvidenceEntry>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct EvidenceCounts {
    pub(super) total: usize,
    pub(super) help: usize,
    pub(super) auto: usize,
    pub(super) manual: usize,
}

impl EvidenceCounts {
    pub(super) fn count_for(&self, filter: EvidenceFilter) -> usize {
        match filter {
            EvidenceFilter::All => self.total,
            EvidenceFilter::Help => self.help,
            EvidenceFilter::Auto => self.auto,
            EvidenceFilter::Manual => self.manual,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct VerificationPolicySummary {
    pub(super) max_new_runs_per_apply: usize,
    pub(super) kinds: Vec<String>,
    pub(super) excludes_count: usize,
}

#[derive(Debug)]
pub(super) struct InspectData {
    pub(super) intent: Vec<ArtifactEntry>,
    pub(super) evidence: EvidenceList,
    pub(super) outputs: Vec<ArtifactEntry>,
    pub(super) history: Vec<ArtifactEntry>,
    pub(super) man_warnings: Vec<String>,
    pub(super) last_history: Option<HistoryEntryPreview>,
    pub(super) last_txn_id: Option<String>,
    pub(super) man_page_path: Option<PathBuf>,
    pub(super) verification_policy: Option<VerificationPolicySummary>,
}

impl InspectData {
    fn load(
        doc_pack_root: &Path,
        summary: &enrich::StatusSummary,
        show_all: &[bool; 4],
        evidence_filter: EvidenceFilter,
    ) -> Result<Self> {
        let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
        let intent = build_intent_entries(&paths)?;
        let evidence =
            build_evidence_entries(&paths, evidence_filter, show_all[Tab::Evidence.index()])?;
        let man_page_path = resolve_man_page_path(&paths, summary.binary_name.as_deref());
        let outputs = build_output_entries(&paths, &man_page_path)?;
        let history = build_history_entries(&paths)?;
        let last_history = read_last_history_entry(&paths).unwrap_or(None);
        let last_txn_id = find_last_txn_id(&paths);
        let verification_policy = load_verification_policy(&paths);
        Ok(Self {
            intent,
            evidence,
            outputs,
            history,
            man_warnings: summary.man_warnings.clone(),
            last_history,
            last_txn_id,
            man_page_path,
            verification_policy,
        })
    }
}

#[derive(Deserialize)]
struct EvidencePreview {
    scenario_id: String,
    generated_at_epoch_ms: u128,
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct HistoryEntryPreview {
    pub(super) step: String,
    pub(super) success: bool,
    pub(super) force_used: bool,
}

pub(super) fn load_state(
    doc_pack_root: &Path,
    show_all: &[bool; 4],
    evidence_filter: EvidenceFilter,
) -> Result<(enrich::StatusSummary, InspectData)> {
    let computation =
        workflow::status_summary_for_doc_pack(doc_pack_root.to_path_buf(), false, false)?;
    let summary = computation.summary;
    let data = InspectData::load(doc_pack_root, &summary, show_all, evidence_filter)?;
    Ok((summary, data))
}

fn push_artifact_entry(entries: &mut Vec<ArtifactEntry>, paths: &DocPackPaths, path: PathBuf) {
    let rel_path = paths
        .rel_path(&path)
        .unwrap_or_else(|_| path.display().to_string());
    let exists = path.exists();
    entries.push(ArtifactEntry {
        rel_path,
        path,
        exists,
    });
}

fn build_intent_entries(paths: &DocPackPaths) -> Result<Vec<ArtifactEntry>> {
    let mut entries = Vec::new();
    push_artifact_entry(&mut entries, paths, paths.scenarios_plan_path());
    push_artifact_entry(&mut entries, paths, paths.semantics_path());
    push_artifact_entry(&mut entries, paths, paths.config_path());
    push_artifact_entry(&mut entries, paths, paths.binary_lens_export_plan_path());

    let queries_dir = paths.root().join("queries");
    if queries_dir.is_dir() {
        let mut query_paths = fs::read_dir(&queries_dir)
            .with_context(|| format!("read queries dir {}", queries_dir.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("sql"))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        query_paths.sort();
        for path in query_paths {
            push_artifact_entry(&mut entries, paths, path);
        }
    }

    Ok(entries)
}

fn build_output_entries(
    paths: &DocPackPaths,
    man_page_path: &Option<PathBuf>,
) -> Result<Vec<ArtifactEntry>> {
    let mut entries = Vec::new();
    push_artifact_entry(&mut entries, paths, paths.surface_path());
    push_artifact_entry(&mut entries, paths, paths.man_dir().join("meta.json"));
    if let Some(path) = man_page_path.as_ref() {
        let rel_path = paths
            .rel_path(path)
            .unwrap_or_else(|_| path.display().to_string());
        entries.push(ArtifactEntry {
            rel_path,
            path: path.clone(),
            exists: path.exists(),
        });
    }
    Ok(entries)
}

fn build_history_entries(paths: &DocPackPaths) -> Result<Vec<ArtifactEntry>> {
    let mut entries = Vec::new();
    push_artifact_entry(&mut entries, paths, paths.report_path());
    push_artifact_entry(&mut entries, paths, paths.history_path());
    Ok(entries)
}

fn resolve_man_page_path(paths: &DocPackPaths, binary_name: Option<&str>) -> Option<PathBuf> {
    if let Some(name) = binary_name {
        let path = paths.man_page_path(name);
        if path.is_file() {
            return Some(path);
        }
    }
    let man_dir = paths.man_dir();
    let entries = fs::read_dir(&man_dir).ok()?;
    let mut man_pages = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("1"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    man_pages.sort();
    if man_pages.len() == 1 {
        return Some(man_pages.remove(0));
    }
    None
}

fn build_evidence_entries(
    paths: &DocPackPaths,
    filter: EvidenceFilter,
    show_all: bool,
) -> Result<EvidenceList> {
    let index_path = paths.inventory_scenarios_dir().join("index.json");
    if index_path.is_file() {
        let bytes =
            fs::read(&index_path).with_context(|| format!("read {}", index_path.display()))?;
        let mut index: scenarios::ScenarioIndex = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", index_path.display()))?;
        index
            .scenarios
            .sort_by(|a, b| a.scenario_id.cmp(&b.scenario_id));
        let counts = evidence_counts_from_ids(
            index
                .scenarios
                .iter()
                .map(|entry| entry.scenario_id.as_str()),
        );
        let total_count = counts.count_for(filter);
        let limit = if show_all { total_count } else { PREVIEW_LIMIT };
        let entries = index
            .scenarios
            .into_iter()
            .filter(|entry| filter.matches(&entry.scenario_id))
            .take(limit)
            .map(|entry| evidence_entry_from_index(paths, entry))
            .collect::<Vec<_>>();
        return Ok(EvidenceList {
            total_count,
            counts,
            entries,
        });
    }

    let scenarios_dir = paths.inventory_scenarios_dir();
    let mut map: std::collections::BTreeMap<String, (u128, EvidenceEntry)> =
        std::collections::BTreeMap::new();
    if scenarios_dir.is_dir() {
        for entry in fs::read_dir(&scenarios_dir)
            .with_context(|| format!("read {}", scenarios_dir.display()))?
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.file_name().and_then(|name| name.to_str()) == Some("index.json") {
                continue;
            }
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| !ext.eq_ignore_ascii_case("json"))
                .unwrap_or(true)
            {
                continue;
            }
            let preview = read_evidence_preview(&path);
            let entry = match preview {
                Ok(preview) => {
                    let generated_at = preview.generated_at_epoch_ms;
                    let entry = evidence_entry_from_preview(&path, preview);
                    map.entry(entry.scenario_id.clone())
                        .and_modify(|(existing_at, existing)| {
                            if generated_at > *existing_at {
                                *existing_at = generated_at;
                                *existing = entry.clone();
                            }
                        })
                        .or_insert((generated_at, entry));
                    continue;
                }
                Err(err) => EvidenceEntry {
                    scenario_id: path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    path: Some(path.clone()),
                    exists: true,
                    exit_code: None,
                    exit_signal: None,
                    timed_out: None,
                    stdout_preview: None,
                    stderr_preview: None,
                    error: Some(err.to_string()),
                },
            };
            map.entry(entry.scenario_id.clone()).or_insert((0, entry));
        }
    }
    let mut entries = map
        .into_values()
        .map(|(_, entry)| entry)
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.scenario_id.cmp(&b.scenario_id));
    let counts = evidence_counts_from_ids(entries.iter().map(|entry| entry.scenario_id.as_str()));
    let total_count = counts.count_for(filter);
    let limit = if show_all { total_count } else { PREVIEW_LIMIT };
    let entries = entries
        .into_iter()
        .filter(|entry| filter.matches(&entry.scenario_id))
        .take(limit)
        .collect();
    Ok(EvidenceList {
        total_count,
        counts,
        entries,
    })
}

fn evidence_entry_from_index(
    paths: &DocPackPaths,
    entry: scenarios::ScenarioIndexEntry,
) -> EvidenceEntry {
    let evidence_path = entry
        .evidence_paths
        .last()
        .map(|rel| paths.root().join(rel));
    if let Some(path) = evidence_path.as_ref() {
        if let Ok(preview) = read_evidence_preview(path) {
            return evidence_entry_from_preview(path, preview);
        }
        return EvidenceEntry {
            scenario_id: entry.scenario_id,
            path: Some(path.clone()),
            exists: path.exists(),
            exit_code: None,
            exit_signal: None,
            timed_out: None,
            stdout_preview: None,
            stderr_preview: None,
            error: Some("failed to parse evidence".to_string()),
        };
    }
    EvidenceEntry {
        scenario_id: entry.scenario_id,
        path: None,
        exists: false,
        exit_code: None,
        exit_signal: None,
        timed_out: None,
        stdout_preview: None,
        stderr_preview: None,
        error: Some("no evidence".to_string()),
    }
}

fn evidence_entry_from_preview(path: &Path, preview: EvidencePreview) -> EvidenceEntry {
    EvidenceEntry {
        scenario_id: preview.scenario_id,
        path: Some(path.to_path_buf()),
        exists: true,
        exit_code: preview.exit_code,
        exit_signal: preview.exit_signal,
        timed_out: Some(preview.timed_out),
        stdout_preview: Some(preview_text(&preview.stdout)),
        stderr_preview: Some(preview_text(&preview.stderr)),
        error: None,
    }
}

fn evidence_counts_from_ids<'a, I>(ids: I) -> EvidenceCounts
where
    I: IntoIterator<Item = &'a str>,
{
    let mut counts = EvidenceCounts {
        total: 0,
        help: 0,
        auto: 0,
        manual: 0,
    };
    for scenario_id in ids {
        counts.total += 1;
        match EvidenceFilter::from_scenario_id(scenario_id) {
            EvidenceFilter::Help => counts.help += 1,
            EvidenceFilter::Auto => counts.auto += 1,
            EvidenceFilter::Manual => counts.manual += 1,
            EvidenceFilter::All => {}
        }
    }
    counts
}

fn read_evidence_preview(path: &Path) -> Result<EvidencePreview> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let preview: EvidencePreview =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    Ok(preview)
}

fn load_verification_policy(paths: &DocPackPaths) -> Option<VerificationPolicySummary> {
    let plan_path = paths.scenarios_plan_path();
    let plan = match scenarios::load_plan_if_exists(&plan_path, paths.root()) {
        Ok(Some(plan)) => plan,
        _ => return None,
    };
    let policy = plan.verification.policy.as_ref()?;
    let kinds = policy
        .kinds
        .iter()
        .map(|kind| kind.as_str().to_string())
        .collect();
    let (_excluded_entries, excluded_ids) = plan.collect_queue_exclusions();
    Some(VerificationPolicySummary {
        max_new_runs_per_apply: policy.max_new_runs_per_apply,
        kinds,
        excludes_count: excluded_ids.len(),
    })
}

fn read_last_history_entry(paths: &DocPackPaths) -> Result<Option<HistoryEntryPreview>> {
    let path = paths.history_path();
    if !path.is_file() {
        return Ok(None);
    }
    let tail = read_tail(&path, 16 * 1024)?;
    let line = tail.lines().rev().find(|line| !line.trim().is_empty());
    let Some(line) = line else {
        return Ok(None);
    };
    let entry: HistoryEntryPreview = serde_json::from_str(line)
        .with_context(|| format!("parse history line from {}", path.display()))?;
    Ok(Some(entry))
}

fn read_tail(path: &Path, max_bytes: usize) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let len = file
        .metadata()
        .with_context(|| format!("metadata {}", path.display()))?
        .len();
    let start = len.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start))
        .with_context(|| format!("seek {}", path.display()))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn find_last_txn_id(paths: &DocPackPaths) -> Option<String> {
    let txns_dir = paths.txns_root();
    let entries = fs::read_dir(&txns_dir).ok()?;
    let mut ids = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    ids.sort();
    ids.pop()
}
