//! Typed paths into a doc-pack layout.
//!
//! Centralizing path construction keeps file access consistent across the
//! workflow and prevents drift when the layout evolves.
use super::{evidence_from_path, EvidenceRef, ENRICH_AGENT_PROMPT_REL};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Convenience wrapper for locating common doc-pack artifacts.
#[derive(Debug, Clone)]
pub struct DocPackPaths {
    root: PathBuf,
}

impl DocPackPaths {
    /// Create a new path helper rooted at the doc-pack root.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Return the doc-pack root used for path derivation.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Return the `enrich/` directory path.
    pub fn enrich_dir(&self) -> PathBuf {
        self.root.join("enrich")
    }

    /// Return the `enrich/config.json` path.
    pub fn config_path(&self) -> PathBuf {
        self.enrich_dir().join("config.json")
    }

    /// Return the `enrich/agent_prompt.md` path.
    pub fn agent_prompt_path(&self) -> PathBuf {
        self.root.join(ENRICH_AGENT_PROMPT_REL)
    }

    /// Return the `enrich/semantics.json` path.
    pub fn semantics_path(&self) -> PathBuf {
        self.enrich_dir().join("semantics.json")
    }

    /// Return the `enrich/prereqs.json` path.
    pub fn prereqs_path(&self) -> PathBuf {
        self.enrich_dir().join("prereqs.json")
    }

    /// Return the `enrich/lock.json` path.
    pub fn lock_path(&self) -> PathBuf {
        self.enrich_dir().join("lock.json")
    }

    /// Return the `enrich/plan.out.json` path.
    pub fn plan_path(&self) -> PathBuf {
        self.enrich_dir().join("plan.out.json")
    }

    /// Return the `enrich/report.json` path.
    pub fn report_path(&self) -> PathBuf {
        self.enrich_dir().join("report.json")
    }

    /// Return the `enrich/history.jsonl` path.
    pub fn history_path(&self) -> PathBuf {
        self.enrich_dir().join("history.jsonl")
    }

    /// Return the `enrich/lm_log.jsonl` path.
    pub fn lm_log_path(&self) -> PathBuf {
        self.enrich_dir().join("lm_log.jsonl")
    }

    /// Return the `enrich/lm_log/` directory path for full prompt/response storage.
    pub fn lm_log_dir(&self) -> PathBuf {
        self.enrich_dir().join("lm_log")
    }

    /// Return the `enrich/txns` directory path.
    pub fn txns_root(&self) -> PathBuf {
        self.enrich_dir().join("txns")
    }

    /// Return the per-transaction directory path.
    pub fn txn_root(&self, txn_id: &str) -> PathBuf {
        self.txns_root().join(txn_id)
    }

    /// Return the per-transaction staging directory path.
    pub fn txn_staging_root(&self, txn_id: &str) -> PathBuf {
        self.txn_root(txn_id).join("staging")
    }

    /// Return the `binary.lens/` pack root path.
    pub fn pack_root(&self) -> PathBuf {
        self.root.join("binary.lens")
    }

    /// Return the `binary.lens/manifest.json` path.
    pub fn pack_manifest_path(&self) -> PathBuf {
        self.pack_root().join("manifest.json")
    }

    /// Return the `binary_lens/` config directory path.
    pub fn binary_lens_dir(&self) -> PathBuf {
        self.root.join("binary_lens")
    }

    /// Return the `binary_lens/export_plan.json` path.
    pub fn binary_lens_export_plan_path(&self) -> PathBuf {
        self.binary_lens_dir().join("export_plan.json")
    }

    /// Return the `inventory/` directory path.
    pub fn inventory_dir(&self) -> PathBuf {
        self.root.join("inventory")
    }

    /// Return the `inventory/scenarios/` directory path.
    pub fn inventory_scenarios_dir(&self) -> PathBuf {
        self.inventory_dir().join("scenarios")
    }

    /// Return the `scenarios/` directory path.
    pub fn scenarios_dir(&self) -> PathBuf {
        self.root.join("scenarios")
    }

    /// Return the `scenarios/plan.json` path.
    pub fn scenarios_plan_path(&self) -> PathBuf {
        self.scenarios_dir().join("plan.json")
    }

    /// Return the `inventory/surface.json` path.
    pub fn surface_path(&self) -> PathBuf {
        self.inventory_dir().join("surface.json")
    }

    /// Return the `inventory/surface.overlays.json` path.
    pub fn surface_overlays_path(&self) -> PathBuf {
        self.inventory_dir().join("surface.overlays.json")
    }

    /// Return the `inventory/verification_progress.json` path.
    pub fn verification_progress_path(&self) -> PathBuf {
        self.inventory_dir().join("verification_progress.json")
    }

    /// Return the `man/` directory path.
    pub fn man_dir(&self) -> PathBuf {
        self.root.join("man")
    }

    /// Return the `man/<binary>.1` path for the provided binary name.
    pub fn man_page_path(&self, binary_name: &str) -> PathBuf {
        self.man_dir().join(format!("{binary_name}.1"))
    }

    /// Return the `man/examples_report.json` path.
    pub fn examples_report_path(&self) -> PathBuf {
        self.man_dir().join("examples_report.json")
    }

    /// Convert an absolute path into a doc-pack relative path string.
    pub fn rel_path(&self, path: &Path) -> Result<String> {
        rel_path(&self.root, path)
    }

    /// Build an evidence reference for a doc-pack path.
    pub fn evidence_from_path(&self, path: &Path) -> Result<EvidenceRef> {
        evidence_from_path(&self.root, path)
    }
}

/// Convert an absolute path into a doc-pack relative path string.
pub(crate) fn rel_path(doc_pack_root: &Path, path: &Path) -> Result<String> {
    let rel = path
        .strip_prefix(doc_pack_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    Ok(rel)
}
