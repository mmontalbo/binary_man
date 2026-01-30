use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CONFIG_SCHEMA_VERSION: u32 = 1;
pub const BOOTSTRAP_SCHEMA_VERSION: u32 = 1;
pub const LOCK_SCHEMA_VERSION: u32 = 1;
pub const PLAN_SCHEMA_VERSION: u32 = 1;
pub const REPORT_SCHEMA_VERSION: u32 = 1;
pub const HISTORY_SCHEMA_VERSION: u32 = 1;

pub const SCENARIO_USAGE_LENS_TEMPLATE_REL: &str = "queries/usage_from_scenarios.sql";
pub const SUBCOMMANDS_FROM_SCENARIOS_TEMPLATE_REL: &str = "queries/subcommands_from_scenarios.sql";
pub const OPTIONS_FROM_SCENARIOS_TEMPLATE_REL: &str = "queries/options_from_scenarios.sql";
pub const VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL: &str =
    "queries/verification_from_scenarios.sql";
pub const ENRICH_AGENT_PROMPT_REL: &str = "enrich/agent_prompt.md";

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequirementId {
    Surface,
    Coverage,
    CoverageLedger,
    Verification,
    ExamplesReport,
    ManPage,
}

impl RequirementId {
    pub fn as_str(&self) -> &'static str {
        match self {
            RequirementId::Surface => "surface",
            RequirementId::Coverage => "coverage",
            RequirementId::CoverageLedger => "coverage_ledger",
            RequirementId::Verification => "verification",
            RequirementId::ExamplesReport => "examples_report",
            RequirementId::ManPage => "man_page",
        }
    }

    pub fn planned_action(&self) -> PlannedAction {
        match self {
            RequirementId::Surface => PlannedAction::SurfaceDiscovery,
            RequirementId::Coverage => PlannedAction::CoverageLedger,
            RequirementId::CoverageLedger => PlannedAction::CoverageLedger,
            RequirementId::Verification => PlannedAction::ScenarioRuns,
            RequirementId::ExamplesReport => PlannedAction::ScenarioRuns,
            RequirementId::ManPage => PlannedAction::RenderManPage,
        }
    }
}

impl fmt::Display for RequirementId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PlannedAction {
    CoverageLedger,
    RenderManPage,
    ScenarioRuns,
    SurfaceDiscovery,
}

impl PlannedAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlannedAction::CoverageLedger => "coverage_ledger",
            PlannedAction::RenderManPage => "render_man_page",
            PlannedAction::ScenarioRuns => "scenario_runs",
            PlannedAction::SurfaceDiscovery => "surface_discovery",
        }
    }
}

impl fmt::Display for PlannedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Ord for PlannedAction {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialOrd for PlannedAction {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequirementState {
    Met,
    Unmet,
    Blocked,
}

impl RequirementState {
    pub fn as_str(&self) -> &'static str {
        match self {
            RequirementState::Met => "met",
            RequirementState::Unmet => "unmet",
            RequirementState::Blocked => "blocked",
        }
    }
}

impl fmt::Display for RequirementState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Complete,
    Incomplete,
    Blocked,
}

impl Decision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Decision::Complete => "complete",
            Decision::Incomplete => "incomplete",
            Decision::Blocked => "blocked",
        }
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EnrichConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub usage_lens_templates: Vec<String>,
    #[serde(default)]
    pub surface_lens_templates: Vec<String>,
    #[serde(default)]
    pub scenario_catalogs: Vec<String>,
    #[serde(default)]
    pub requirements: Vec<RequirementId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_tier: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EnrichBootstrap {
    pub schema_version: u32,
    pub binary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens_flake: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EnrichLock {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: Option<String>,
    pub config_path: String,
    pub inputs: Vec<String>,
    pub inputs_hash: String,
    pub selected_inputs: SelectedInputs,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct SelectedInputs {
    #[serde(default)]
    pub usage_lens_templates: Vec<String>,
    #[serde(default)]
    pub surface_lens_templates: Vec<String>,
    #[serde(default)]
    pub scenario_plan: Option<String>,
    #[serde(default)]
    pub scenario_catalogs: Vec<String>,
    #[serde(default)]
    pub fixtures_root: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LockStatus {
    pub present: bool,
    pub stale: bool,
    pub inputs_hash: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PlanStatus {
    pub present: bool,
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_inputs_hash: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VerificationExclusion {
    pub surface_id: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VerificationTriageSummary {
    #[serde(default)]
    pub discovered_untriaged_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovered_untriaged_preview: Vec<String>,
    #[serde(default)]
    pub triaged_unverified_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triaged_unverified_preview: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<VerificationExclusion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered_untriaged_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triaged_unverified_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RequirementStatus {
    pub id: RequirementId,
    pub status: RequirementState,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unverified_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationTriageSummary>,
    pub evidence: Vec<EvidenceRef>,
    pub blockers: Vec<Blocker>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EvidenceRef {
    pub path: String,
    pub sha256: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ScenarioFailure {
    pub scenario_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LensSummary {
    pub kind: String,
    pub template_path: String,
    pub status: String,
    pub evidence: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub fn dedupe_evidence_refs(entries: &mut Vec<EvidenceRef>) {
    let mut seen = BTreeSet::new();
    entries.retain(|entry| seen.insert(entry.path.clone()));
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Blocker {
    pub code: String,
    pub message: String,
    pub evidence: Vec<EvidenceRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NextAction {
    Command {
        command: String,
        reason: String,
    },
    Edit {
        path: String,
        content: String,
        reason: String,
    },
}

#[derive(Debug, Serialize, Clone)]
pub struct StatusSummary {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: Option<String>,
    pub lock: LockStatus,
    pub plan: PlanStatus,
    pub requirements: Vec<RequirementStatus>,
    pub missing_artifacts: Vec<String>,
    pub blockers: Vec<Blocker>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenario_failures: Vec<ScenarioFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lens_summary: Vec<LensSummary>,
    pub decision: Decision,
    pub decision_reason: Option<String>,
    pub next_action: NextAction,
    pub warnings: Vec<String>,
    pub man_warnings: Vec<String>,
    pub force_used: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct EnrichRunSummary {
    pub step: String,
    pub started_at_epoch_ms: u128,
    pub finished_at_epoch_ms: u128,
    pub success: bool,
    pub inputs_hash: Option<String>,
    pub outputs_hash: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EnrichHistoryEntry {
    pub schema_version: u32,
    pub started_at_epoch_ms: u128,
    pub finished_at_epoch_ms: u128,
    pub step: String,
    pub inputs_hash: Option<String>,
    pub outputs_hash: Option<String>,
    pub success: bool,
    pub message: Option<String>,
    pub force_used: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EnrichPlan {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: Option<String>,
    pub lock: EnrichLock,
    pub requirements: Vec<RequirementStatus>,
    pub planned_actions: Vec<PlannedAction>,
    pub next_action: NextAction,
    pub decision: Decision,
    pub decision_reason: Option<String>,
    pub force_used: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct EnrichReport {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: Option<String>,
    pub lock: Option<EnrichLock>,
    pub requirements: Vec<RequirementStatus>,
    pub blockers: Vec<Blocker>,
    pub missing_artifacts: Vec<String>,
    pub decision: Decision,
    pub decision_reason: Option<String>,
    pub next_action: NextAction,
    pub last_run: Option<EnrichRunSummary>,
    pub force_used: bool,
}

#[derive(Debug, Clone)]
pub struct DocPackPaths {
    root: PathBuf,
}

impl DocPackPaths {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn enrich_dir(&self) -> PathBuf {
        self.root.join("enrich")
    }

    pub fn config_path(&self) -> PathBuf {
        self.enrich_dir().join("config.json")
    }

    pub fn bootstrap_path(&self) -> PathBuf {
        self.enrich_dir().join("bootstrap.json")
    }

    pub fn agent_prompt_path(&self) -> PathBuf {
        self.root.join(ENRICH_AGENT_PROMPT_REL)
    }

    pub fn semantics_path(&self) -> PathBuf {
        self.enrich_dir().join("semantics.json")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.enrich_dir().join("lock.json")
    }

    pub fn plan_path(&self) -> PathBuf {
        self.enrich_dir().join("plan.out.json")
    }

    pub fn report_path(&self) -> PathBuf {
        self.enrich_dir().join("report.json")
    }

    pub fn history_path(&self) -> PathBuf {
        self.enrich_dir().join("history.jsonl")
    }

    pub fn txns_root(&self) -> PathBuf {
        self.enrich_dir().join("txns")
    }

    pub fn txn_root(&self, txn_id: &str) -> PathBuf {
        self.txns_root().join(txn_id)
    }

    pub fn txn_staging_root(&self, txn_id: &str) -> PathBuf {
        self.txn_root(txn_id).join("staging")
    }

    pub fn pack_root(&self) -> PathBuf {
        self.root.join("binary.lens")
    }

    pub fn pack_manifest_path(&self) -> PathBuf {
        self.pack_root().join("manifest.json")
    }

    pub fn binary_lens_dir(&self) -> PathBuf {
        self.root.join("binary_lens")
    }

    pub fn binary_lens_export_plan_path(&self) -> PathBuf {
        self.binary_lens_dir().join("export_plan.json")
    }

    pub fn inventory_dir(&self) -> PathBuf {
        self.root.join("inventory")
    }

    pub fn inventory_scenarios_dir(&self) -> PathBuf {
        self.inventory_dir().join("scenarios")
    }

    pub fn scenarios_dir(&self) -> PathBuf {
        self.root.join("scenarios")
    }

    pub fn scenarios_plan_path(&self) -> PathBuf {
        self.scenarios_dir().join("plan.json")
    }

    pub fn surface_path(&self) -> PathBuf {
        self.inventory_dir().join("surface.json")
    }

    pub fn surface_seed_path(&self) -> PathBuf {
        self.inventory_dir().join("surface.seed.json")
    }

    pub fn man_dir(&self) -> PathBuf {
        self.root.join("man")
    }

    pub fn man_page_path(&self, binary_name: &str) -> PathBuf {
        self.man_dir().join(format!("{binary_name}.1"))
    }

    pub fn examples_report_path(&self) -> PathBuf {
        self.man_dir().join("examples_report.json")
    }

    pub fn rel_path(&self, path: &Path) -> Result<String> {
        rel_path(&self.root, path)
    }

    pub fn evidence_from_path(&self, path: &Path) -> Result<EvidenceRef> {
        evidence_from_path(&self.root, path)
    }
}

pub fn default_requirements() -> Vec<RequirementId> {
    vec![
        RequirementId::Surface,
        RequirementId::Verification,
        RequirementId::ManPage,
    ]
}

pub fn default_config() -> EnrichConfig {
    let usage_lens_templates = vec![SCENARIO_USAGE_LENS_TEMPLATE_REL.to_string()];
    let surface_lens_templates = vec![
        OPTIONS_FROM_SCENARIOS_TEMPLATE_REL.to_string(),
        SUBCOMMANDS_FROM_SCENARIOS_TEMPLATE_REL.to_string(),
    ];
    EnrichConfig {
        schema_version: CONFIG_SCHEMA_VERSION,
        usage_lens_templates,
        surface_lens_templates,
        scenario_catalogs: Vec::new(),
        requirements: default_requirements(),
        verification_tier: Some("accepted".to_string()),
    }
}

pub fn config_stub() -> String {
    let config = default_config();
    serde_json::to_string_pretty(&config).expect("serialize config stub")
}

pub fn bootstrap_stub() -> String {
    let stub = EnrichBootstrap {
        schema_version: BOOTSTRAP_SCHEMA_VERSION,
        binary: "REPLACE_ME".to_string(),
        lens_flake: None,
    };
    serde_json::to_string_pretty(&stub).expect("serialize bootstrap stub")
}

pub fn load_config(doc_pack_root: &Path) -> Result<EnrichConfig> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.config_path();
    let bytes = fs::read(&path).with_context(|| format!("read config {}", path.display()))?;
    let config: EnrichConfig =
        serde_json::from_slice(&bytes).context("parse enrich config JSON")?;
    Ok(config)
}

pub fn load_bootstrap_optional(doc_pack_root: &Path) -> Result<Option<EnrichBootstrap>> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.bootstrap_path();
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path).with_context(|| format!("read bootstrap {}", path.display()))?;
    let bootstrap: EnrichBootstrap =
        serde_json::from_slice(&bytes).context("parse enrich bootstrap JSON")?;
    validate_bootstrap(&bootstrap)?;
    Ok(Some(bootstrap))
}

fn validate_bootstrap(bootstrap: &EnrichBootstrap) -> Result<()> {
    if bootstrap.schema_version != BOOTSTRAP_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported bootstrap schema_version {}",
            bootstrap.schema_version
        ));
    }
    if bootstrap.binary.trim().is_empty() {
        return Err(anyhow!("bootstrap binary must be non-empty"));
    }
    Ok(())
}

pub fn write_config(doc_pack_root: &Path, config: &EnrichConfig) -> Result<()> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create enrich dir")?;
    }
    let text = serde_json::to_string_pretty(config).context("serialize enrich config")?;
    fs::write(&path, text.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn load_lock(doc_pack_root: &Path) -> Result<EnrichLock> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.lock_path();
    let bytes = fs::read(&path).with_context(|| format!("read lock {}", path.display()))?;
    let lock: EnrichLock = serde_json::from_slice(&bytes).context("parse enrich lock JSON")?;
    Ok(lock)
}

pub fn write_lock(doc_pack_root: &Path, lock: &EnrichLock) -> Result<()> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.lock_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create enrich dir")?;
    }
    let text = serde_json::to_string_pretty(lock).context("serialize enrich lock")?;
    fs::write(&path, text.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn write_report(doc_pack_root: &Path, report: &EnrichReport) -> Result<()> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.report_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create enrich dir")?;
    }
    let text = serde_json::to_string_pretty(report).context("serialize enrich report")?;
    fs::write(&path, text.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn append_history(doc_pack_root: &Path, entry: &EnrichHistoryEntry) -> Result<()> {
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let path = paths.history_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create enrich dir")?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let line = serde_json::to_string(entry).context("serialize enrich history entry")?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn normalized_requirements(config: &EnrichConfig) -> Vec<RequirementId> {
    if config.requirements.is_empty() {
        return default_requirements();
    }
    config.requirements.clone()
}

pub fn validate_config(config: &EnrichConfig) -> Result<()> {
    if config.schema_version != CONFIG_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported enrich config schema_version {}",
            config.schema_version
        ));
    }
    let requirements = normalized_requirements(config);
    if config.scenario_catalogs.len() > 1 {
        return Err(anyhow!(
            "only a single scenario catalog is supported (got {})",
            config.scenario_catalogs.len()
        ));
    }
    if let Some(tier) = config.verification_tier.as_deref() {
        if tier != "accepted" && tier != "behavior" {
            return Err(anyhow!(
                "verification_tier must be \"accepted\" or \"behavior\" (got {tier:?})"
            ));
        }
    }
    let needs_lens = requirements
        .iter()
        .any(|req| matches!(req, RequirementId::ManPage));
    if needs_lens && config.usage_lens_templates.is_empty() {
        return Err(anyhow!(
            "usage_lens_templates must include at least one entry for man/coverage requirements"
        ));
    }
    validate_relative_list(&config.usage_lens_templates, "usage_lens_templates")?;
    validate_relative_list(&config.surface_lens_templates, "surface_lens_templates")?;
    validate_relative_list(&config.scenario_catalogs, "scenario_catalogs")?;
    Ok(())
}

pub fn resolve_inputs(config: &EnrichConfig, doc_pack_root: &Path) -> Result<SelectedInputs> {
    let usage_lens_templates = config.usage_lens_templates.clone();
    let surface_lens_templates = config.surface_lens_templates.clone();
    let scenario_catalogs = config.scenario_catalogs.clone();
    let scenario_plan = "scenarios/plan.json".to_string();
    for rel in usage_lens_templates
        .iter()
        .chain(surface_lens_templates.iter())
        .chain(scenario_catalogs.iter())
        .chain(std::iter::once(&scenario_plan))
    {
        validate_relative_path(rel, "input")?;
        let path = doc_pack_root.join(rel);
        if !path.exists() {
            return Err(anyhow!("missing input {}", rel));
        }
    }
    let fixtures_root = if doc_pack_root.join("fixtures").is_dir() {
        Some("fixtures".to_string())
    } else {
        None
    };
    Ok(SelectedInputs {
        usage_lens_templates,
        surface_lens_templates,
        scenario_plan: Some(scenario_plan),
        scenario_catalogs,
        fixtures_root,
    })
}

pub fn build_lock(
    doc_pack_root: &Path,
    config: &EnrichConfig,
    binary_name: Option<&str>,
) -> Result<EnrichLock> {
    validate_config(config)?;
    let selected_inputs = resolve_inputs(config, doc_pack_root)?;
    let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
    let mut inputs = vec![paths.config_path()];
    for rel in selected_inputs
        .usage_lens_templates
        .iter()
        .chain(selected_inputs.surface_lens_templates.iter())
        .chain(selected_inputs.scenario_catalogs.iter())
    {
        inputs.push(doc_pack_root.join(rel));
    }
    if let Some(rel) = selected_inputs.scenario_plan.as_ref() {
        inputs.push(doc_pack_root.join(rel));
    }
    inputs.push(paths.semantics_path());
    inputs.push(paths.surface_seed_path());
    inputs.push(paths.binary_lens_export_plan_path());
    inputs.push(paths.pack_manifest_path());
    if let Some(fixtures_root) = selected_inputs.fixtures_root.as_ref() {
        inputs.push(doc_pack_root.join(fixtures_root));
    }
    let verification_template = doc_pack_root.join(VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);
    if verification_template.exists() {
        inputs.push(verification_template);
    }
    inputs.sort();
    inputs.dedup();
    let inputs_hash = hash_paths(doc_pack_root, &inputs)?;
    let inputs_rel = inputs
        .iter()
        .map(|path| rel_path(doc_pack_root, path))
        .collect::<Result<Vec<_>>>()?;
    Ok(EnrichLock {
        schema_version: LOCK_SCHEMA_VERSION,
        generated_at_epoch_ms: now_epoch_ms()?,
        binary_name: binary_name.map(|name| name.to_string()),
        config_path: rel_path(doc_pack_root, &paths.config_path())?,
        inputs: inputs_rel,
        inputs_hash,
        selected_inputs,
    })
}

pub fn lock_status(doc_pack_root: &Path, lock: Option<&EnrichLock>) -> Result<LockStatus> {
    let Some(lock) = lock else {
        return Ok(LockStatus {
            present: false,
            stale: false,
            inputs_hash: None,
        });
    };
    let input_paths = lock
        .inputs
        .iter()
        .map(|rel| doc_pack_root.join(rel))
        .collect::<Vec<_>>();
    let current_hash = hash_paths(doc_pack_root, &input_paths)?;
    let stale = current_hash != lock.inputs_hash;
    Ok(LockStatus {
        present: true,
        stale,
        inputs_hash: Some(lock.inputs_hash.clone()),
    })
}

fn rel_path(doc_pack_root: &Path, path: &Path) -> Result<String> {
    let rel = path
        .strip_prefix(doc_pack_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    Ok(rel)
}

fn validate_relative_path(rel: &str, label: &str) -> Result<()> {
    let path = Path::new(rel);
    if path.is_absolute() || has_parent_components(path) {
        return Err(anyhow!(
            "{label} entries must be relative paths without '..' (got {rel:?})"
        ));
    }
    Ok(())
}

fn validate_relative_list(entries: &[String], label: &str) -> Result<()> {
    for rel in entries {
        validate_relative_path(rel, label)?;
    }
    Ok(())
}

fn has_parent_components(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

pub fn hash_paths(doc_pack_root: &Path, paths: &[PathBuf]) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut sorted = paths.to_vec();
    sorted.sort();
    for path in sorted {
        hash_path(&mut hasher, doc_pack_root, &path)?;
    }
    let digest = hasher.finalize();
    Ok(format!("{:x}", digest))
}

pub fn now_epoch_ms() -> Result<u128> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("compute timestamp")?
        .as_millis())
}

fn hash_path(hasher: &mut Sha256, root: &Path, path: &Path) -> Result<()> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    if !path.exists() {
        hasher.update(b"missing:");
        hasher.update(rel.to_string_lossy().as_bytes());
        return Ok(());
    }
    let meta = fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        hasher.update(b"symlink:");
        hasher.update(rel.to_string_lossy().as_bytes());
        let target = fs::read_link(path).with_context(|| format!("read {}", path.display()))?;
        hasher.update(target.to_string_lossy().as_bytes());
        return Ok(());
    }
    if file_type.is_dir() {
        hasher.update(b"dir:");
        hasher.update(rel.to_string_lossy().as_bytes());
        let mut entries: Vec<_> = fs::read_dir(path)
            .with_context(|| format!("read {}", path.display()))?
            .filter_map(|entry| entry.ok())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            hash_path(hasher, root, &entry.path())?;
        }
        return Ok(());
    }
    if file_type.is_file() {
        hasher.update(b"file:");
        hasher.update(rel.to_string_lossy().as_bytes());
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        if is_binary_lens_manifest_path(path) {
            if let Some(stable_bytes) = stable_binary_lens_manifest_bytes(&bytes) {
                hasher.update(b":stable_manifest:");
                hasher.update(&stable_bytes);
                return Ok(());
            }
        }
        hasher.update(&bytes);
        return Ok(());
    }
    Ok(())
}

fn is_binary_lens_manifest_path(path: &Path) -> bool {
    path.file_name() == Some(OsStr::new("manifest.json"))
        && path
            .parent()
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| name == OsStr::new("binary.lens"))
}

fn stable_binary_lens_manifest_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut manifest: Value = serde_json::from_slice(bytes).ok()?;
    if let Some(digest) = manifest
        .get("export_config_digest")
        .and_then(|v| v.as_str())
    {
        return serde_json::to_vec(&serde_json::json!({ "export_config_digest": digest })).ok();
    }

    if let Some(obj) = manifest.as_object_mut() {
        obj.remove("created_at");
        obj.remove("created_at_epoch_seconds");
        obj.remove("created_at_source");
        obj.remove("coverage_summary");
    }
    serde_json::to_vec(&manifest).ok()
}

pub fn evidence_from_path(doc_pack_root: &Path, path: &Path) -> Result<EvidenceRef> {
    let rel = rel_path(doc_pack_root, path)?;
    let sha256 = if path.exists() {
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        Some(format!("{:x}", hasher.finalize()))
    } else {
        None
    };
    Ok(EvidenceRef { path: rel, sha256 })
}

pub fn evidence_from_rel(doc_pack_root: &Path, rel: &str) -> Result<EvidenceRef> {
    let path = doc_pack_root.join(rel);
    evidence_from_path(doc_pack_root, &path)
}
