use crate::enrich;
use crate::pack;
use crate::staging::{collect_files_recursive, write_staged_json};
use crate::surface;
use crate::templates;
use crate::util::{display_path, sha256_hex, truncate_bytes};
use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_SNIPPET_MAX_BYTES: usize = 4096;
const DEFAULT_SNIPPET_MAX_LINES: usize = 60;
const MAX_SCENARIO_EVIDENCE_BYTES: usize = 64 * 1024;
const SCENARIO_PLAN_SCHEMA_VERSION: u32 = 3;
const SCENARIO_EVIDENCE_SCHEMA_VERSION: u32 = 3;
const SCENARIO_INDEX_SCHEMA_VERSION: u32 = 1;
const MAX_SEED_ENTRIES: usize = 128;
const MAX_SEED_TOTAL_BYTES: usize = 64 * 1024;

fn default_true() -> bool {
    true
}

fn default_scenario_kind() -> ScenarioKind {
    ScenarioKind::Behavior
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioKind {
    Help,
    Behavior,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScenarioRunMode {
    Default,
    RerunAll,
    RerunFailed,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SeedEntryKind {
    Dir,
    File,
    Symlink,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ScenarioSeedEntry {
    pub path: String,
    pub kind: SeedEntryKind,
    #[serde(default)]
    pub contents: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub mode: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ScenarioSeedSpec {
    #[serde(default)]
    pub entries: Vec<ScenarioSeedEntry>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct ScenarioDefaults {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_sandbox: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_strace: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_max_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_max_bytes: Option<usize>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ScenarioPlan {
    pub schema_version: u32,
    #[serde(default)]
    pub binary: Option<String>,
    #[serde(default)]
    pub default_env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<ScenarioDefaults>,
    #[serde(default)]
    pub coverage: Option<CoverageNotes>,
    #[serde(default)]
    pub verification: VerificationPlan,
    #[serde(default)]
    pub scenarios: Vec<ScenarioSpec>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct VerificationPlan {
    #[serde(default)]
    pub queue: Vec<VerificationQueueEntry>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct VerificationQueueEntry {
    pub surface_id: String,
    pub intent: VerificationIntent,
    #[serde(default)]
    pub prereqs: Vec<VerificationPrereq>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_invocation: Option<VerificationInvocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationIntent {
    VerifyAccepted,
    VerifyBehavior,
    Exclude,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationPrereq {
    NeedsArgValue,
    NeedsSeedFs,
    NeedsRepo,
    NeedsNetwork,
    NeedsInteractive,
    NeedsPrivilege,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct VerificationInvocation {
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub argv: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CoverageNotes {
    #[serde(default)]
    pub blocked: Vec<CoverageBlocked>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CoverageBlocked {
    #[serde(default, alias = "option_ids")]
    pub item_ids: Vec<String>,
    pub reason: String,
    #[serde(default)]
    pub details: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ScenarioSpec {
    pub id: String,
    #[serde(default = "default_scenario_kind")]
    pub kind: ScenarioKind,
    #[serde(default = "default_true")]
    pub publish: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<ScenarioSeedSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_sandbox: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_strace: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_max_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_max_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_tier: Option<String>,
    #[serde(
        default,
        alias = "covers_options",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub covers: Vec<String>,
    #[serde(default)]
    pub coverage_ignore: bool,
    #[serde(default, skip_serializing_if = "ScenarioExpect::is_empty")]
    pub expect: ScenarioExpect,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScenarioExpect {
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    #[serde(default)]
    pub stdout_contains_all: Vec<String>,
    #[serde(default)]
    pub stdout_contains_any: Vec<String>,
    #[serde(default)]
    pub stdout_regex_all: Vec<String>,
    #[serde(default)]
    pub stdout_regex_any: Vec<String>,
    #[serde(default)]
    pub stderr_contains_all: Vec<String>,
    #[serde(default)]
    pub stderr_contains_any: Vec<String>,
    #[serde(default)]
    pub stderr_regex_all: Vec<String>,
    #[serde(default)]
    pub stderr_regex_any: Vec<String>,
}

impl ScenarioExpect {
    fn is_empty(&self) -> bool {
        self.exit_code.is_none()
            && self.exit_signal.is_none()
            && self.stdout_contains_all.is_empty()
            && self.stdout_contains_any.is_empty()
            && self.stdout_regex_all.is_empty()
            && self.stdout_regex_any.is_empty()
            && self.stderr_contains_all.is_empty()
            && self.stderr_contains_any.is_empty()
            && self.stderr_regex_all.is_empty()
            && self.stderr_regex_any.is_empty()
    }
}

#[derive(Debug, Deserialize)]
struct RunsIndex {
    #[serde(default)]
    runs: Vec<RunIndexEntry>,
}

#[derive(Debug, Deserialize)]
struct RunIndexEntry {
    run_id: String,
    #[serde(default)]
    manifest_ref: Option<String>,
    #[serde(default)]
    stdout_ref: Option<String>,
    #[serde(default)]
    stderr_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RunManifest {
    #[serde(default)]
    result: RunResult,
}

#[derive(Debug, Default, Deserialize)]
struct RunResult {
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
    #[serde(default)]
    timed_out: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ExamplesReport {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: String,
    pub pack_root: String,
    pub scenarios_path: String,
    pub scenario_count: usize,
    pub pass_count: usize,
    pub fail_count: usize,
    pub run_ids: Vec<String>,
    pub scenarios: Vec<ScenarioOutcome>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ScenarioOutcome {
    pub scenario_id: String,
    pub publish: bool,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub seed_dir: Option<String>,
    pub cwd: Option<String>,
    pub timeout_seconds: Option<f64>,
    pub net_mode: Option<String>,
    pub no_sandbox: Option<bool>,
    pub no_strace: Option<bool>,
    pub snippet_max_lines: usize,
    pub snippet_max_bytes: usize,
    pub run_argv0: String,
    pub expected: ScenarioExpect,
    pub run_id: Option<String>,
    pub manifest_ref: Option<String>,
    pub stdout_ref: Option<String>,
    pub stderr_ref: Option<String>,
    pub observed_exit_code: Option<i32>,
    pub observed_exit_signal: Option<i32>,
    pub observed_timed_out: bool,
    pub pass: bool,
    pub failures: Vec<String>,
    pub command_line: String,
    pub stdout_snippet: String,
    pub stderr_snippet: String,
}

pub fn publishable_examples_report(mut report: ExamplesReport) -> Option<ExamplesReport> {
    let scenarios: Vec<ScenarioOutcome> = report
        .scenarios
        .into_iter()
        .filter(|scenario| scenario.publish)
        .collect();
    if scenarios.is_empty() {
        return None;
    }
    let pass_count = scenarios.iter().filter(|scenario| scenario.pass).count();
    let fail_count = scenarios.len() - pass_count;
    let mut run_id_set = BTreeSet::new();
    for scenario in &scenarios {
        if let Some(run_id) = scenario.run_id.as_ref() {
            run_id_set.insert(run_id.clone());
        }
    }
    report.scenario_count = scenarios.len();
    report.pass_count = pass_count;
    report.fail_count = fail_count;
    report.run_ids = run_id_set.into_iter().collect();
    report.scenarios = scenarios;
    Some(report)
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ScenarioEvidence {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub scenario_id: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub seed_dir: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub timeout_seconds: Option<f64>,
    pub net_mode: Option<String>,
    pub no_sandbox: Option<bool>,
    pub no_strace: Option<bool>,
    pub snippet_max_lines: usize,
    pub snippet_max_bytes: usize,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ScenarioIndex {
    pub schema_version: u32,
    pub scenarios: Vec<ScenarioIndexEntry>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ScenarioIndexEntry {
    pub scenario_id: String,
    pub scenario_digest: String,
    #[serde(default)]
    pub last_run_epoch_ms: Option<u128>,
    #[serde(default)]
    pub last_pass: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_paths: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CoverageLedger {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: String,
    pub scenarios_path: String,
    pub validation_source: String,
    pub items_total: usize,
    pub behavior_count: usize,
    pub rejected_count: usize,
    pub acceptance_count: usize,
    pub blocked_count: usize,
    pub uncovered_count: usize,
    pub items: Vec<CoverageItemEntry>,
    pub unknown_items: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CoverageItemEntry {
    pub item_id: String,
    pub aliases: Vec<String>,
    pub status: String,
    pub behavior_scenarios: Vec<String>,
    pub rejection_scenarios: Vec<String>,
    pub acceptance_scenarios: Vec<String>,
    pub blocked_reason: Option<String>,
    pub blocked_details: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<enrich::EvidenceRef>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct VerificationLedger {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: String,
    pub scenarios_path: String,
    pub surface_path: String,
    pub total_count: usize,
    pub verified_count: usize,
    pub unverified_count: usize,
    pub unverified_ids: Vec<String>,
    pub behavior_verified_count: usize,
    pub behavior_unverified_count: usize,
    pub behavior_unverified_ids: Vec<String>,
    pub entries: Vec<VerificationEntry>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct VerificationEntry {
    pub surface_id: String,
    pub status: String,
    pub behavior_status: String,
    #[serde(default)]
    pub scenario_ids: Vec<String>,
    #[serde(default)]
    pub scenario_paths: Vec<String>,
    #[serde(default)]
    pub behavior_scenario_ids: Vec<String>,
    #[serde(default)]
    pub behavior_scenario_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<enrich::EvidenceRef>,
}

#[derive(Debug, Deserialize)]
struct VerificationRow {
    #[serde(default)]
    surface_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    behavior_status: Option<String>,
    #[serde(default)]
    scenario_ids: Vec<String>,
    #[serde(default)]
    scenario_paths: Vec<String>,
    #[serde(default)]
    behavior_scenario_ids: Vec<String>,
    #[serde(default)]
    behavior_scenario_paths: Vec<String>,
}

struct VerificationQueryRoot {
    root: PathBuf,
    cleanup: bool,
}

impl Drop for VerificationQueryRoot {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

pub fn load_plan(path: &Path, doc_pack_root: &Path) -> Result<ScenarioPlan> {
    let bytes =
        fs::read(path).with_context(|| format!("read scenarios plan {}", path.display()))?;
    let plan: ScenarioPlan = serde_json::from_slice(&bytes).context("parse scenarios plan JSON")?;
    validate_plan(&plan, doc_pack_root)?;
    Ok(plan)
}

pub(crate) fn load_plan_if_exists(
    path: &Path,
    doc_pack_root: &Path,
) -> Result<Option<ScenarioPlan>> {
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(load_plan(path, doc_pack_root)?))
}

pub fn validate_plan(plan: &ScenarioPlan, doc_pack_root: &Path) -> Result<()> {
    if plan.schema_version != SCENARIO_PLAN_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported scenarios plan schema_version {}",
            plan.schema_version
        ));
    }
    if plan.scenarios.is_empty() {
        return Err(anyhow!("scenarios plan contains no scenarios"));
    }
    if let Some(coverage) = plan.coverage.as_ref() {
        for blocked in &coverage.blocked {
            if blocked.item_ids.is_empty() {
                return Err(anyhow!("coverage.blocked entries must include item_ids"));
            }
            if blocked.reason.trim().is_empty() {
                return Err(anyhow!("coverage.blocked reason must not be empty"));
            }
        }
    }
    if let Some(defaults) = plan.defaults.as_ref() {
        validate_scenario_defaults(defaults, doc_pack_root)
            .context("validate scenario defaults")?;
    }
    for (idx, entry) in plan.verification.queue.iter().enumerate() {
        if entry.surface_id.trim().is_empty() {
            return Err(anyhow!(
                "verification.queue[{idx}] surface_id must not be empty"
            ));
        }
        if entry.intent == VerificationIntent::Exclude {
            let reason = entry.reason.as_deref().unwrap_or("");
            if reason.trim().is_empty() {
                return Err(anyhow!(
                    "verification.queue[{idx}] exclude intent requires reason"
                ));
            }
        }
    }
    for scenario in &plan.scenarios {
        validate_scenario_spec(scenario)
            .with_context(|| format!("validate scenario {}", scenario.id))?;
    }
    Ok(())
}

pub fn plan_stub(binary_name: Option<&str>) -> String {
    let mut plan: ScenarioPlan = serde_json::from_str(templates::SCENARIOS_PLAN_JSON)
        .expect("parse scenarios plan template");
    if let Some(binary) = binary_name {
        plan.binary = Some(binary.to_string());
    }
    serde_json::to_string_pretty(&plan).expect("serialize scenarios plan stub")
}

pub fn run_scenarios(
    pack_root: &Path,
    run_root: &Path,
    binary_name: &str,
    scenarios_path: &Path,
    lens_flake: &str,
    display_root: Option<&Path>,
    staging_root: Option<&Path>,
    kind_filter: Option<ScenarioKind>,
    run_mode: ScenarioRunMode,
    verbose: bool,
) -> Result<ExamplesReport> {
    let plan = load_plan(scenarios_path, run_root)?;
    if let Some(plan_binary) = plan.binary.as_deref() {
        if plan_binary != binary_name {
            return Err(anyhow!(
                "scenarios plan binary {:?} does not match pack binary {:?}",
                plan_binary,
                binary_name
            ));
        }
    }

    let pack_root = pack_root
        .canonicalize()
        .with_context(|| format!("resolve pack root {}", pack_root.display()))?;

    let scenarios_index_path = run_root
        .join("inventory")
        .join("scenarios")
        .join("index.json");
    let index_state = load_scenario_index_state(&scenarios_index_path, &plan, verbose);
    let has_existing_index = index_state.existing.is_some();
    let mut index_entries = index_state.entries;
    let mut index_changed = index_state.changed;

    let mut previous_outcomes = load_previous_outcomes(run_root, verbose);
    let cache_ready = previous_outcomes.available && has_existing_index;
    if verbose && !cache_ready {
        let report_state = if previous_outcomes.available {
            "present"
        } else {
            "missing"
        };
        let index_status = if has_existing_index {
            "present"
        } else {
            "missing"
        };
        eprintln!(
            "note: scenario cache incomplete (report {report_state}, index {index_status}); rerunning all scenarios"
        );
    }
    let mut outcomes = Vec::new();

    let scenarios = plan.scenarios.iter().filter(|scenario| match kind_filter {
        Some(kind) => scenario.kind == kind,
        None => true,
    });

    for scenario in scenarios {
        let run_config = effective_scenario_config(&plan, scenario)?;
        let reportable = scenario.publish;
        let has_index_entry = index_entries.contains_key(&scenario.id);
        let has_previous_outcome =
            cache_ready && previous_outcomes.outcomes.contains_key(&scenario.id);
        let allow_index_cache = !reportable && has_index_entry;
        // Skip when we can reuse prior outcomes for reportable scenarios, or when
        // non-reportable scenarios already have indexed evidence.
        let should_run = should_run_scenario(
            run_mode,
            &run_config.scenario_digest,
            index_entries.get(&scenario.id),
            has_previous_outcome || allow_index_cache,
        );

        if !should_run {
            if reportable {
                if let Some(outcome) = previous_outcomes.outcomes.remove(&scenario.id) {
                    outcomes.push(outcome);
                }
            }
            continue;
        }

        if verbose {
            eprintln!("running scenario {} {}", binary_name, scenario.id);
        }

        let run_argv0 = binary_name.to_string();
        let materialized_seed = if let Some(seed) = scenario.seed.as_ref() {
            let staging_root = staging_root.ok_or_else(|| {
                anyhow!(
                    "inline seed requires a staging root for scenario {}",
                    scenario.id
                )
            })?;
            Some(materialize_inline_seed(
                staging_root,
                run_root,
                &scenario.id,
                seed,
            )?)
        } else {
            None
        };
        let run_seed_dir = materialized_seed
            .as_ref()
            .map(|seed| seed.rel_path.as_str())
            .or(run_config.seed_dir.as_deref());
        let run_kv_args = build_run_kv_args(
            &run_argv0,
            run_seed_dir,
            run_config.cwd.as_deref(),
            run_config.timeout_seconds,
            run_config.net_mode.as_deref(),
            run_config.no_sandbox,
            run_config.no_strace,
        )?;
        let before = read_runs_index(&pack_root).context("read runs index (before)")?;

        let started = std::time::Instant::now();
        let status = invoke_binary_lens_run(
            &pack_root,
            run_root,
            lens_flake,
            &run_kv_args,
            &scenario.argv,
            &run_config.env,
        )
        .with_context(|| format!("invoke binary_lens for scenario {}", scenario.id))?;
        let duration_ms = started.elapsed().as_millis();
        if !status.success() {
            let argv = scenario.argv.clone();
            let command_line = format_command_line(binary_name, &argv);
            let failures = vec![format!(
                "binary_lens run failed with status {}",
                exit_status_string(&status)
            )];
            if reportable {
                outcomes.push(ScenarioOutcome {
                    scenario_id: scenario.id.clone(),
                    publish: scenario.publish,
                    argv,
                    env: run_config.env.clone(),
                    seed_dir: run_config.seed_dir.clone(),
                    cwd: run_config.cwd.clone(),
                    timeout_seconds: run_config.timeout_seconds,
                    net_mode: run_config.net_mode.clone(),
                    no_sandbox: run_config.no_sandbox,
                    no_strace: run_config.no_strace,
                    snippet_max_lines: run_config.snippet_max_lines,
                    snippet_max_bytes: run_config.snippet_max_bytes,
                    run_argv0,
                    expected: scenario.expect.clone(),
                    run_id: None,
                    manifest_ref: None,
                    stdout_ref: None,
                    stderr_ref: None,
                    observed_exit_code: None,
                    observed_exit_signal: None,
                    observed_timed_out: false,
                    pass: false,
                    failures: failures.clone(),
                    command_line,
                    stdout_snippet: String::new(),
                    stderr_snippet: String::new(),
                });
            }
            index_entries.insert(
                scenario.id.clone(),
                ScenarioIndexEntry {
                    scenario_id: scenario.id.clone(),
                    scenario_digest: run_config.scenario_digest.clone(),
                    last_run_epoch_ms: Some(enrich::now_epoch_ms()?),
                    last_pass: Some(false),
                    failures,
                    evidence_paths: Vec::new(),
                },
            );
            index_changed = true;
            continue;
        }

        let after = read_runs_index(&pack_root).context("read runs index (after)")?;
        let (run_id, entry) = resolve_new_run(&before, &after)
            .with_context(|| format!("resolve new run for scenario {}", scenario.id))?;

        let manifest_ref = entry
            .manifest_ref
            .clone()
            .unwrap_or_else(|| format!("runs/{run_id}/manifest.json"));
        let stdout_ref = entry
            .stdout_ref
            .clone()
            .unwrap_or_else(|| format!("runs/{run_id}/stdout.txt"));
        let stderr_ref = entry
            .stderr_ref
            .clone()
            .unwrap_or_else(|| format!("runs/{run_id}/stderr.txt"));

        let run_manifest: RunManifest = read_json(&pack_root, &manifest_ref)
            .with_context(|| format!("read run manifest {manifest_ref}"))?;

        let stdout_bytes = read_ref_bytes(&pack_root, &stdout_ref)
            .with_context(|| format!("read stdout {stdout_ref}"))?;
        let stderr_bytes = read_ref_bytes(&pack_root, &stderr_ref)
            .with_context(|| format!("read stderr {stderr_ref}"))?;
        let stdout_text = String::from_utf8_lossy(&stdout_bytes);
        let stderr_text = String::from_utf8_lossy(&stderr_bytes);

        let observed_exit_code = run_manifest.result.exit_code;
        let observed_exit_signal = run_manifest.result.exit_signal;
        let observed_timed_out = run_manifest.result.timed_out;

        let mut evidence_paths = Vec::new();
        let mut evidence_epoch_ms = None;
        if let Some(staging_root) = staging_root {
            let mut argv_full = Vec::with_capacity(scenario.argv.len() + 1);
            argv_full.push(run_argv0.clone());
            argv_full.extend(scenario.argv.iter().cloned());
            let generated_at_epoch_ms = enrich::now_epoch_ms()?;
            let evidence = ScenarioEvidence {
                schema_version: SCENARIO_EVIDENCE_SCHEMA_VERSION,
                generated_at_epoch_ms,
                scenario_id: scenario.id.clone(),
                argv: argv_full,
                env: run_config.env.clone(),
                seed_dir: run_seed_dir.map(|value| value.to_string()),
                cwd: run_config.cwd.clone(),
                timeout_seconds: run_config.timeout_seconds,
                net_mode: run_config.net_mode.clone(),
                no_sandbox: run_config.no_sandbox,
                no_strace: run_config.no_strace,
                snippet_max_lines: run_config.snippet_max_lines,
                snippet_max_bytes: run_config.snippet_max_bytes,
                exit_code: observed_exit_code,
                exit_signal: observed_exit_signal,
                timed_out: observed_timed_out,
                duration_ms,
                stdout: truncate_bytes(&stdout_bytes, MAX_SCENARIO_EVIDENCE_BYTES),
                stderr: truncate_bytes(&stderr_bytes, MAX_SCENARIO_EVIDENCE_BYTES),
            };
            let rel = stage_scenario_evidence(staging_root, &evidence)?;
            evidence_paths.push(rel);
            evidence_epoch_ms = Some(generated_at_epoch_ms);
        }

        let failures = validate_scenario(
            &scenario.expect,
            observed_exit_code,
            observed_exit_signal,
            observed_timed_out,
            stdout_text.as_ref(),
            stderr_text.as_ref(),
        );
        let pass = failures.is_empty();

        let command_line = format_command_line(binary_name, &scenario.argv);
        let stdout_snippet = bounded_snippet(
            stdout_text.as_ref(),
            run_config.snippet_max_lines,
            run_config.snippet_max_bytes,
        );
        let stderr_snippet = bounded_snippet(
            stderr_text.as_ref(),
            run_config.snippet_max_lines,
            run_config.snippet_max_bytes,
        );

        if verbose && !pass {
            eprintln!("scenario {} failed: {}", scenario.id, failures.join("; "));
        }

        if reportable {
            outcomes.push(ScenarioOutcome {
                scenario_id: scenario.id.clone(),
                publish: scenario.publish,
                argv: scenario.argv.clone(),
                env: run_config.env.clone(),
                seed_dir: run_config.seed_dir.clone(),
                cwd: run_config.cwd.clone(),
                timeout_seconds: run_config.timeout_seconds,
                net_mode: run_config.net_mode.clone(),
                no_sandbox: run_config.no_sandbox,
                no_strace: run_config.no_strace,
                snippet_max_lines: run_config.snippet_max_lines,
                snippet_max_bytes: run_config.snippet_max_bytes,
                run_argv0,
                expected: scenario.expect.clone(),
                run_id: Some(run_id),
                manifest_ref: Some(manifest_ref),
                stdout_ref: Some(stdout_ref),
                stderr_ref: Some(stderr_ref),
                observed_exit_code,
                observed_exit_signal,
                observed_timed_out,
                pass,
                failures: failures.clone(),
                command_line,
                stdout_snippet,
                stderr_snippet,
            });
        }
        index_entries.insert(
            scenario.id.clone(),
            ScenarioIndexEntry {
                scenario_id: scenario.id.clone(),
                scenario_digest: run_config.scenario_digest.clone(),
                last_run_epoch_ms: evidence_epoch_ms,
                last_pass: Some(pass),
                failures,
                evidence_paths,
            },
        );
        index_changed = true;
    }

    write_scenario_index_if_needed(
        staging_root,
        index_entries,
        has_existing_index,
        index_changed,
    )?;

    let pass_count = outcomes.iter().filter(|outcome| outcome.pass).count();
    let fail_count = outcomes.len() - pass_count;
    if verbose {
        eprintln!(
            "examples report summary: {} total, {} passed, {} failed",
            outcomes.len(),
            pass_count,
            fail_count
        );
    }
    let mut run_id_set = BTreeSet::new();
    for outcome in &outcomes {
        if let Some(run_id) = outcome.run_id.as_ref() {
            run_id_set.insert(run_id.clone());
        }
    }
    let run_ids: Vec<String> = run_id_set.into_iter().collect();
    let generated_at_epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("compute timestamp")?
        .as_millis();

    Ok(ExamplesReport {
        schema_version: 1,
        generated_at_epoch_ms,
        binary_name: binary_name.to_string(),
        pack_root: display_path(&pack_root, display_root),
        scenarios_path: display_path(scenarios_path, display_root),
        scenario_count: outcomes.len(),
        pass_count,
        fail_count,
        run_ids,
        scenarios: outcomes,
    })
}

fn invoke_binary_lens_run(
    pack_root: &Path,
    run_root: &Path,
    lens_flake: &str,
    run_kv_args: &[String],
    scenario_argv: &[String],
    env_overrides: &BTreeMap<String, String>,
) -> Result<std::process::ExitStatus> {
    let pack_root_str = pack_root
        .to_str()
        .ok_or_else(|| anyhow!("pack root path is not valid UTF-8"))?;

    let mut cmd = Command::new("nix");
    cmd.args(["run", lens_flake, "--"]);
    cmd.args(run_kv_args);
    cmd.arg(pack_root_str);
    cmd.args(scenario_argv);
    for (key, value) in env_overrides {
        cmd.env(key, value);
    }
    cmd.current_dir(run_root);
    let status = cmd.status().context("spawn nix run")?;
    Ok(status)
}

fn stage_scenario_evidence(staging_root: &Path, evidence: &ScenarioEvidence) -> Result<String> {
    let rel = scenario_output_rel_path(&evidence.scenario_id, evidence.generated_at_epoch_ms);
    write_staged_json(staging_root, &rel, evidence)?;
    Ok(rel)
}

fn scenario_output_rel_path(scenario_id: &str, generated_at_epoch_ms: u128) -> String {
    format!(
        "inventory/scenarios/{}-{}.json",
        scenario_id, generated_at_epoch_ms
    )
}

pub fn build_coverage_ledger(
    binary_name: &str,
    surface: &surface::SurfaceInventory,
    doc_pack_root: &Path,
    scenarios_path: &Path,
    display_root: Option<&Path>,
) -> Result<CoverageLedger> {
    let plan = load_plan(scenarios_path, doc_pack_root)?;
    if let Some(plan_binary) = plan.binary.as_deref() {
        if plan_binary != binary_name {
            return Err(anyhow!(
                "scenarios plan binary {:?} does not match pack binary {:?}",
                plan_binary,
                binary_name
            ));
        }
    }

    let surface_path = doc_pack_root.join("inventory").join("surface.json");
    let surface_evidence = enrich::evidence_from_path(doc_pack_root, &surface_path)?;
    let plan_evidence = enrich::evidence_from_path(doc_pack_root, scenarios_path)?;
    let mut items: BTreeMap<String, CoverageState> = BTreeMap::new();
    for item in surface
        .items
        .iter()
        .filter(|item| is_surface_item_kind(&item.kind))
    {
        let aliases = if item.display != item.id {
            vec![item.display.clone()]
        } else {
            Vec::new()
        };
        items.insert(
            item.id.clone(),
            CoverageState {
                aliases,
                evidence: item.evidence.clone(),
                ..CoverageState::default()
            },
        );
    }

    let mut warnings = Vec::new();
    let mut unknown_items = BTreeSet::new();
    let mut blocked_map: HashMap<String, BlockedInfo> = HashMap::new();
    if let Some(coverage) = plan.coverage.as_ref() {
        for blocked in &coverage.blocked {
            for item_id in &blocked.item_ids {
                let normalized = normalize_surface_id(item_id);
                if normalized.is_empty() {
                    continue;
                }
                let entry = blocked_map.entry(normalized).or_insert(BlockedInfo {
                    reason: blocked.reason.clone(),
                    details: blocked.details.clone(),
                    tags: blocked.tags.clone(),
                });
                if entry.reason != blocked.reason {
                    entry.reason = format!("{}, {}", entry.reason, blocked.reason);
                }
                if let Some(details) = blocked.details.as_ref() {
                    let updated = match entry.details.take() {
                        Some(existing) if existing != *details => {
                            format!("{existing}; {details}")
                        }
                        Some(existing) => existing,
                        None => details.clone(),
                    };
                    entry.details = Some(updated);
                }
                for tag in &blocked.tags {
                    if !entry.tags.contains(tag) {
                        entry.tags.push(tag.clone());
                    }
                }
                entry.tags.sort();
                entry.tags.dedup();
            }
        }
    }

    for scenario in &plan.scenarios {
        if scenario.coverage_ignore {
            continue;
        }
        if scenario.covers.is_empty() {
            warnings.push(format!(
                "scenario {:?} missing covers for coverage",
                scenario.id
            ));
            continue;
        }
        let tier = coverage_tier(scenario);
        let option_ids = scenario_surface_ids(scenario);
        for item_id in option_ids {
            match items.get_mut(&item_id) {
                Some(entry) => match tier {
                    CoverageTier::Behavior => {
                        entry.behavior_scenarios.insert(scenario.id.clone());
                    }
                    CoverageTier::Rejection => {
                        entry.rejection_scenarios.insert(scenario.id.clone());
                    }
                    CoverageTier::Acceptance => {
                        entry.acceptance_scenarios.insert(scenario.id.clone());
                    }
                },
                None => {
                    unknown_items.insert(item_id);
                }
            }
        }
    }

    for (option_id, blocked) in blocked_map {
        match items.get_mut(&option_id) {
            Some(entry) => {
                if !entry.behavior_scenarios.is_empty() {
                    warnings.push(format!(
                        "item {:?} marked blocked but has behavior coverage",
                        option_id
                    ));
                }
                entry.blocked = Some(blocked);
            }
            None => {
                warnings.push(format!(
                    "blocked item {:?} not found in surface inventory",
                    option_id
                ));
                unknown_items.insert(option_id);
            }
        }
    }

    let mut entries = Vec::new();
    let mut behavior_count = 0;
    let mut rejected_count = 0;
    let mut acceptance_count = 0;
    let mut blocked_count = 0;
    let mut uncovered_count = 0;

    for (item_id, entry) in items {
        let behavior_scenarios: Vec<String> = entry.behavior_scenarios.into_iter().collect();
        let rejection_scenarios: Vec<String> = entry.rejection_scenarios.into_iter().collect();
        let acceptance_scenarios: Vec<String> = entry.acceptance_scenarios.into_iter().collect();
        let (blocked_reason, blocked_details, blocked_tags, is_blocked) =
            match entry.blocked.as_ref() {
                Some(blocked) => (
                    Some(blocked.reason.clone()),
                    blocked.details.clone(),
                    blocked.tags.clone(),
                    true,
                ),
                None => (None, None, Vec::new(), false),
            };
        let status = if !behavior_scenarios.is_empty() {
            behavior_count += 1;
            "behavior"
        } else if !rejection_scenarios.is_empty() {
            rejected_count += 1;
            "rejected"
        } else if !acceptance_scenarios.is_empty() {
            acceptance_count += 1;
            "acceptance"
        } else if is_blocked {
            blocked_count += 1;
            "blocked"
        } else {
            uncovered_count += 1;
            "uncovered"
        };

        let mut evidence = entry.evidence.clone();
        evidence.push(surface_evidence.clone());
        evidence.push(plan_evidence.clone());
        enrich::dedupe_evidence_refs(&mut evidence);

        entries.push(CoverageItemEntry {
            item_id,
            aliases: entry.aliases,
            status: status.to_string(),
            behavior_scenarios,
            rejection_scenarios,
            acceptance_scenarios,
            blocked_reason,
            blocked_details,
            blocked_tags,
            evidence,
        });
    }

    let generated_at_epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("compute timestamp")?
        .as_millis();

    Ok(CoverageLedger {
        schema_version: 3,
        generated_at_epoch_ms,
        binary_name: binary_name.to_string(),
        scenarios_path: display_path(scenarios_path, display_root),
        validation_source: "plan".to_string(),
        items_total: entries.len(),
        behavior_count,
        rejected_count,
        acceptance_count,
        blocked_count,
        uncovered_count,
        items: entries,
        unknown_items: unknown_items.into_iter().collect(),
        warnings,
    })
}

pub fn build_verification_ledger(
    binary_name: &str,
    surface: &surface::SurfaceInventory,
    doc_pack_root: &Path,
    scenarios_path: &Path,
    template_path: &Path,
    staging_root: Option<&Path>,
    display_root: Option<&Path>,
) -> Result<VerificationLedger> {
    let template_sql = fs::read_to_string(template_path)
        .with_context(|| format!("read {}", template_path.display()))?;
    let (query_root, rows) = run_verification_query(doc_pack_root, staging_root, &template_sql)?;

    let mut surface_evidence_map: BTreeMap<String, Vec<enrich::EvidenceRef>> = BTreeMap::new();
    for item in surface
        .items
        .iter()
        .filter(|item| is_surface_item_kind(&item.kind))
    {
        let id = item.id.trim();
        if id.is_empty() {
            continue;
        }
        surface_evidence_map
            .entry(id.to_string())
            .or_default()
            .extend(item.evidence.iter().cloned());
    }

    let surface_evidence = enrich::evidence_from_rel(&query_root.root, "inventory/surface.json")?;
    let plan_evidence = enrich::evidence_from_rel(&query_root.root, "scenarios/plan.json")?;

    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut verified_count = 0;
    let mut unverified_ids = Vec::new();
    let mut behavior_verified_count = 0;
    let mut behavior_unverified_ids = Vec::new();

    for row in rows {
        let Some(surface_id) = row.surface_id.clone() else {
            warnings.push("verification row missing surface_id".to_string());
            continue;
        };
        let status = row.status.clone().unwrap_or_else(|| "unknown".to_string());
        let behavior_status = row
            .behavior_status
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        if status == "verified" {
            verified_count += 1;
        } else {
            unverified_ids.push(surface_id.clone());
        }
        if behavior_status == "verified" {
            behavior_verified_count += 1;
        } else {
            behavior_unverified_ids.push(surface_id.clone());
        }

        let mut evidence = surface_evidence_map
            .get(&surface_id)
            .cloned()
            .unwrap_or_default();
        evidence.push(surface_evidence.clone());
        evidence.push(plan_evidence.clone());
        for scenario_path in row
            .scenario_paths
            .iter()
            .chain(row.behavior_scenario_paths.iter())
        {
            match enrich::evidence_from_rel(&query_root.root, scenario_path) {
                Ok(evidence_ref) => evidence.push(evidence_ref),
                Err(err) => warnings.push(err.to_string()),
            }
        }
        enrich::dedupe_evidence_refs(&mut evidence);

        entries.push(VerificationEntry {
            surface_id,
            status,
            behavior_status,
            scenario_ids: row.scenario_ids,
            scenario_paths: row.scenario_paths,
            behavior_scenario_ids: row.behavior_scenario_ids,
            behavior_scenario_paths: row.behavior_scenario_paths,
            evidence,
        });
    }

    entries.sort_by(|a, b| a.surface_id.cmp(&b.surface_id));
    unverified_ids.sort();
    unverified_ids.dedup();
    behavior_unverified_ids.sort();
    behavior_unverified_ids.dedup();

    let generated_at_epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("compute timestamp")?
        .as_millis();

    Ok(VerificationLedger {
        schema_version: 2,
        generated_at_epoch_ms,
        binary_name: binary_name.to_string(),
        scenarios_path: display_path(scenarios_path, display_root),
        surface_path: display_path(
            &doc_pack_root.join("inventory").join("surface.json"),
            display_root,
        ),
        total_count: entries.len(),
        verified_count,
        unverified_count: unverified_ids.len(),
        unverified_ids,
        behavior_verified_count,
        behavior_unverified_count: behavior_unverified_ids.len(),
        behavior_unverified_ids,
        entries,
        warnings,
    })
}

fn run_verification_query(
    doc_pack_root: &Path,
    staging_root: Option<&Path>,
    template_sql: &str,
) -> Result<(VerificationQueryRoot, Vec<VerificationRow>)> {
    let query_root = prepare_verification_root(doc_pack_root, staging_root)?;
    let output = pack::run_duckdb_query(template_sql, &query_root.root)?;
    let rows: Vec<VerificationRow> =
        if output.is_empty() || output.iter().all(|byte| byte.is_ascii_whitespace()) {
            Vec::new()
        } else {
            serde_json::from_slice(&output).context("parse verification query output")?
        };
    Ok((query_root, rows))
}

fn prepare_verification_root(
    doc_pack_root: &Path,
    staging_root: Option<&Path>,
) -> Result<VerificationQueryRoot> {
    let (root, cleanup) = if let Some(staging_root) = staging_root {
        let txn_root = staging_root
            .parent()
            .ok_or_else(|| anyhow!("staging root has no parent"))?;
        let now = enrich::now_epoch_ms()?;
        (
            txn_root
                .join("scratch")
                .join(format!("verification_root-{now}")),
            false,
        )
    } else {
        let now = enrich::now_epoch_ms()?;
        (
            std::env::temp_dir().join(format!("bman-verification-{now}")),
            true,
        )
    };
    fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;

    let inventory_root = root.join("inventory");
    let scenarios_root = root.join("scenarios");
    let enrich_root = root.join("enrich");
    fs::create_dir_all(inventory_root.join("scenarios"))
        .with_context(|| format!("create {}", inventory_root.display()))?;
    fs::create_dir_all(&scenarios_root)
        .with_context(|| format!("create {}", scenarios_root.display()))?;
    fs::create_dir_all(&enrich_root)
        .with_context(|| format!("create {}", enrich_root.display()))?;

    let plan_src = doc_pack_root.join("scenarios").join("plan.json");
    let plan_dest = scenarios_root.join("plan.json");
    fs::copy(&plan_src, &plan_dest).with_context(|| format!("copy {}", plan_src.display()))?;

    let semantics_src = doc_pack_root.join("enrich").join("semantics.json");
    let semantics_dest = enrich_root.join("semantics.json");
    fs::copy(&semantics_src, &semantics_dest)
        .with_context(|| format!("copy {}", semantics_src.display()))?;

    let staged_surface = staging_root
        .map(|root| root.join("inventory").join("surface.json"))
        .filter(|path| path.is_file());
    let surface_src =
        staged_surface.unwrap_or_else(|| doc_pack_root.join("inventory").join("surface.json"));
    let surface_dest = inventory_root.join("surface.json");
    fs::copy(&surface_src, &surface_dest)
        .with_context(|| format!("copy {}", surface_src.display()))?;

    copy_scenario_evidence(
        &doc_pack_root.join("inventory").join("scenarios"),
        &inventory_root.join("scenarios"),
    )?;
    if let Some(staging_root) = staging_root {
        copy_scenario_evidence(
            &staging_root.join("inventory").join("scenarios"),
            &inventory_root.join("scenarios"),
        )?;
    }

    let placeholder = format!(
        concat!(
            "{{\"schema_version\":{schema},",
            "\"generated_at_epoch_ms\":0,",
            "\"scenario_id\":null,",
            "\"argv\":[],",
            "\"env\":{{}},",
            "\"seed_dir\":null,",
            "\"cwd\":null,",
            "\"timeout_seconds\":null,",
            "\"net_mode\":null,",
            "\"no_sandbox\":null,",
            "\"no_strace\":null,",
            "\"snippet_max_lines\":0,",
            "\"snippet_max_bytes\":0,",
            "\"exit_code\":null,",
            "\"exit_signal\":null,",
            "\"timed_out\":false,",
            "\"duration_ms\":0,",
            "\"stdout\":\"\",",
            "\"stderr\":\"\"}}"
        ),
        schema = SCENARIO_EVIDENCE_SCHEMA_VERSION
    );
    let placeholder_path = inventory_root.join("scenarios").join("schema.json");
    fs::write(&placeholder_path, placeholder.as_bytes())
        .with_context(|| format!("write placeholder {}", placeholder_path.display()))?;

    Ok(VerificationQueryRoot { root, cleanup })
}

fn copy_scenario_evidence(src_root: &Path, dest_root: &Path) -> Result<usize> {
    if !src_root.is_dir() {
        return Ok(0);
    }
    let mut copied = 0usize;
    for file in collect_files_recursive(src_root)? {
        if file.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let rel = file
            .strip_prefix(src_root)
            .context("strip scenario evidence prefix")?;
        let dest = dest_root.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        fs::copy(&file, &dest)
            .with_context(|| format!("copy {} to {}", file.display(), dest.display()))?;
        copied += 1;
    }
    Ok(copied)
}

#[derive(Debug, Clone)]
struct BlockedInfo {
    reason: String,
    details: Option<String>,
    tags: Vec<String>,
}

#[derive(Debug, Default)]
struct CoverageState {
    aliases: Vec<String>,
    evidence: Vec<enrich::EvidenceRef>,
    behavior_scenarios: BTreeSet<String>,
    rejection_scenarios: BTreeSet<String>,
    acceptance_scenarios: BTreeSet<String>,
    blocked: Option<BlockedInfo>,
}

#[derive(Debug)]
enum CoverageTier {
    Behavior,
    Rejection,
    Acceptance,
}

fn coverage_tier(scenario: &ScenarioSpec) -> CoverageTier {
    match scenario.coverage_tier.as_deref() {
        Some("behavior") => CoverageTier::Behavior,
        Some("rejection") => CoverageTier::Rejection,
        _ => CoverageTier::Acceptance,
    }
}

fn scenario_surface_ids(scenario: &ScenarioSpec) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for token in &scenario.covers {
        let normalized = normalize_surface_id(token);
        if !normalized.is_empty() {
            ids.insert(normalized);
        }
    }
    ids.into_iter().collect()
}

pub fn normalize_surface_id(token: &str) -> String {
    let trimmed = token.trim();
    if let Some((head, _)) = trimmed.split_once('=') {
        head.to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_surface_item_kind(kind: &str) -> bool {
    matches!(kind, "option" | "command" | "subcommand")
}

pub(crate) fn read_runs_index_bytes(pack_root: &Path) -> Result<Option<Vec<u8>>> {
    let index_path = pack_root.join("runs").join("index.json");
    if !index_path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&index_path).with_context(|| format!("read {}", index_path.display()))?;
    Ok(Some(bytes))
}

fn read_runs_index(pack_root: &Path) -> Result<Vec<RunIndexEntry>> {
    let Some(bytes) = read_runs_index_bytes(pack_root)? else {
        return Ok(Vec::new());
    };
    let index: RunsIndex = serde_json::from_slice(&bytes).context("parse runs index JSON")?;
    Ok(index.runs)
}

pub(crate) fn read_scenario_index(path: &Path) -> Result<Option<ScenarioIndex>> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let index: ScenarioIndex = serde_json::from_slice(&bytes).context("parse scenarios index")?;
    if index.schema_version != SCENARIO_INDEX_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported scenarios index schema_version {}",
            index.schema_version
        ));
    }
    Ok(Some(index))
}

struct ScenarioIndexState {
    existing: Option<ScenarioIndex>,
    entries: BTreeMap<String, ScenarioIndexEntry>,
    changed: bool,
}

fn load_scenario_index_state(
    scenarios_index_path: &Path,
    plan: &ScenarioPlan,
    verbose: bool,
) -> ScenarioIndexState {
    let existing = match read_scenario_index(scenarios_index_path) {
        Ok(index) => index,
        Err(err) => {
            if verbose {
                eprintln!(
                    "warning: failed to read scenario index {}: {err}",
                    scenarios_index_path.display()
                );
            }
            None
        }
    };
    let mut entries = BTreeMap::new();
    if let Some(index) = existing.as_ref() {
        for entry in &index.scenarios {
            entries.insert(entry.scenario_id.clone(), entry.clone());
        }
    }
    let plan_ids: BTreeSet<String> = plan.scenarios.iter().map(|s| s.id.clone()).collect();
    let before_retain = entries.len();
    entries.retain(|id, _| plan_ids.contains(id));
    let changed = before_retain != entries.len();
    ScenarioIndexState {
        existing,
        entries,
        changed,
    }
}

struct PreviousOutcomes {
    available: bool,
    outcomes: HashMap<String, ScenarioOutcome>,
}

fn load_previous_outcomes(doc_pack_root: &Path, verbose: bool) -> PreviousOutcomes {
    let report_path = doc_pack_root.join("man").join("examples_report.json");
    if !report_path.is_file() {
        return PreviousOutcomes {
            available: false,
            outcomes: HashMap::new(),
        };
    }
    let bytes = match fs::read(&report_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            if verbose {
                eprintln!("warning: failed to read {}: {err}", report_path.display());
            }
            return PreviousOutcomes {
                available: false,
                outcomes: HashMap::new(),
            };
        }
    };
    let report: ExamplesReport = match serde_json::from_slice(&bytes) {
        Ok(report) => report,
        Err(err) => {
            if verbose {
                eprintln!("warning: failed to parse {}: {err}", report_path.display());
            }
            return PreviousOutcomes {
                available: false,
                outcomes: HashMap::new(),
            };
        }
    };
    let outcomes = report
        .scenarios
        .into_iter()
        .map(|scenario| (scenario.scenario_id.clone(), scenario))
        .collect();
    PreviousOutcomes {
        available: true,
        outcomes,
    }
}

fn should_run_scenario(
    run_mode: ScenarioRunMode,
    scenario_digest: &str,
    entry: Option<&ScenarioIndexEntry>,
    has_previous_outcome: bool,
) -> bool {
    if !has_previous_outcome {
        return true;
    }
    match run_mode {
        ScenarioRunMode::RerunAll => true,
        ScenarioRunMode::RerunFailed => match entry {
            Some(entry) => entry.last_pass != Some(true),
            None => true,
        },
        ScenarioRunMode::Default => match entry {
            Some(entry) => {
                entry.last_pass != Some(true) || entry.scenario_digest != scenario_digest
            }
            None => true,
        },
    }
}

fn write_scenario_index_if_needed(
    staging_root: Option<&Path>,
    entries: BTreeMap<String, ScenarioIndexEntry>,
    has_existing_index: bool,
    index_changed: bool,
) -> Result<()> {
    let Some(staging_root) = staging_root else {
        return Ok(());
    };
    if index_changed || !has_existing_index {
        let mut entries: Vec<ScenarioIndexEntry> = entries.into_values().collect();
        entries.sort_by(|a, b| a.scenario_id.cmp(&b.scenario_id));
        let index = ScenarioIndex {
            schema_version: SCENARIO_INDEX_SCHEMA_VERSION,
            scenarios: entries,
        };
        write_staged_json(staging_root, "inventory/scenarios/index.json", &index)?;
    }
    Ok(())
}

fn resolve_new_run(
    before: &[RunIndexEntry],
    after: &[RunIndexEntry],
) -> Result<(String, RunIndexEntry)> {
    let before_ids: HashSet<&str> = before.iter().map(|entry| entry.run_id.as_str()).collect();
    let mut new_entries: Vec<&RunIndexEntry> = after
        .iter()
        .filter(|entry| !before_ids.contains(entry.run_id.as_str()))
        .collect();
    if new_entries.is_empty() {
        return Err(anyhow!("runs index did not append a new run"));
    }
    let picked = new_entries
        .pop()
        .ok_or_else(|| anyhow!("runs index did not append a new run"))?;
    Ok((
        picked.run_id.clone(),
        RunIndexEntry {
            run_id: picked.run_id.clone(),
            manifest_ref: picked.manifest_ref.clone(),
            stdout_ref: picked.stdout_ref.clone(),
            stderr_ref: picked.stderr_ref.clone(),
        },
    ))
}

fn read_ref_bytes(pack_root: &Path, reference: &str) -> Result<Vec<u8>> {
    let path = resolve_ref(pack_root, reference);
    fs::read(&path).with_context(|| format!("read {}", path.display()))
}

fn read_json<T: for<'de> Deserialize<'de>>(pack_root: &Path, reference: &str) -> Result<T> {
    let bytes = read_ref_bytes(pack_root, reference)?;
    let parsed = serde_json::from_slice(&bytes).context("parse JSON")?;
    Ok(parsed)
}

fn resolve_ref(pack_root: &Path, reference: &str) -> PathBuf {
    let ref_path = Path::new(reference);
    if ref_path.is_absolute() {
        ref_path.to_path_buf()
    } else {
        pack_root.join(ref_path)
    }
}

fn validate_scenario(
    expect: &ScenarioExpect,
    observed_exit_code: Option<i32>,
    observed_exit_signal: Option<i32>,
    observed_timed_out: bool,
    stdout: &str,
    stderr: &str,
) -> Vec<String> {
    let mut failures = Vec::new();

    if observed_timed_out {
        failures.push("timed out".to_string());
    }

    if let Some(expected_code) = expect.exit_code {
        if observed_exit_code != Some(expected_code) {
            failures.push(format!(
                "expected exit_code {}, observed {:?}",
                expected_code, observed_exit_code
            ));
        }
    }

    if let Some(expected_signal) = expect.exit_signal {
        if observed_exit_signal != Some(expected_signal) {
            failures.push(format!(
                "expected exit_signal {}, observed {:?}",
                expected_signal, observed_exit_signal
            ));
        }
    }

    if !expect.stdout_contains_all.is_empty() {
        for needle in &expect.stdout_contains_all {
            if !stdout.contains(needle) {
                failures.push(format!("stdout missing substring {:?}", needle));
            }
        }
    }
    if !expect.stdout_contains_any.is_empty()
        && !expect
            .stdout_contains_any
            .iter()
            .any(|needle| stdout.contains(needle))
    {
        failures.push(format!(
            "stdout missing any of {:?}",
            expect.stdout_contains_any
        ));
    }

    if !expect.stdout_regex_all.is_empty() {
        for pattern in &expect.stdout_regex_all {
            match Regex::new(pattern) {
                Ok(re) => {
                    if !re.is_match(stdout) {
                        failures.push(format!("stdout missing regex match {:?}", pattern));
                    }
                }
                Err(err) => failures.push(format!("invalid stdout regex {:?}: {err}", pattern)),
            }
        }
    }
    if !expect.stdout_regex_any.is_empty() {
        let mut invalid = Vec::new();
        let mut any_match = false;
        for pattern in &expect.stdout_regex_any {
            match Regex::new(pattern) {
                Ok(re) => {
                    if re.is_match(stdout) {
                        any_match = true;
                        break;
                    }
                }
                Err(err) => invalid.push(format!("{pattern:?}: {err}")),
            }
        }
        if !invalid.is_empty() {
            failures.push(format!(
                "invalid stdout regex_any patterns: {}",
                invalid.join("; ")
            ));
        }
        if !any_match {
            failures.push(format!(
                "stdout missing any regex of {:?}",
                expect.stdout_regex_any
            ));
        }
    }

    if !expect.stderr_contains_all.is_empty() {
        for needle in &expect.stderr_contains_all {
            if !stderr.contains(needle) {
                failures.push(format!("stderr missing substring {:?}", needle));
            }
        }
    }
    if !expect.stderr_contains_any.is_empty()
        && !expect
            .stderr_contains_any
            .iter()
            .any(|needle| stderr.contains(needle))
    {
        failures.push(format!(
            "stderr missing any of {:?}",
            expect.stderr_contains_any
        ));
    }

    if !expect.stderr_regex_all.is_empty() {
        for pattern in &expect.stderr_regex_all {
            match Regex::new(pattern) {
                Ok(re) => {
                    if !re.is_match(stderr) {
                        failures.push(format!("stderr missing regex match {:?}", pattern));
                    }
                }
                Err(err) => failures.push(format!("invalid stderr regex {:?}: {err}", pattern)),
            }
        }
    }
    if !expect.stderr_regex_any.is_empty() {
        let mut invalid = Vec::new();
        let mut any_match = false;
        for pattern in &expect.stderr_regex_any {
            match Regex::new(pattern) {
                Ok(re) => {
                    if re.is_match(stderr) {
                        any_match = true;
                        break;
                    }
                }
                Err(err) => invalid.push(format!("{pattern:?}: {err}")),
            }
        }
        if !invalid.is_empty() {
            failures.push(format!(
                "invalid stderr regex_any patterns: {}",
                invalid.join("; ")
            ));
        }
        if !any_match {
            failures.push(format!(
                "stderr missing any regex of {:?}",
                expect.stderr_regex_any
            ));
        }
    }

    failures
}

fn merge_env(
    defaults: &BTreeMap<String, String>,
    overrides: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut merged = defaults.clone();
    for (key, value) in overrides {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

struct ScenarioRunConfig {
    env: BTreeMap<String, String>,
    seed_dir: Option<String>,
    cwd: Option<String>,
    timeout_seconds: Option<f64>,
    net_mode: Option<String>,
    no_sandbox: Option<bool>,
    no_strace: Option<bool>,
    snippet_max_lines: usize,
    snippet_max_bytes: usize,
    scenario_digest: String,
}

#[derive(Serialize)]
struct ScenarioSeedEntryDigest {
    path: String,
    kind: SeedEntryKind,
    contents: Option<String>,
    target: Option<String>,
    mode: Option<u32>,
}

#[derive(Serialize)]
struct ScenarioSeedSpecDigest {
    entries: Vec<ScenarioSeedEntryDigest>,
}

#[derive(Serialize)]
struct ScenarioDigestInput {
    argv: Vec<String>,
    expect: ScenarioExpect,
    scope: Vec<String>,
    seed_dir: Option<String>,
    seed: Option<ScenarioSeedSpecDigest>,
    cwd: Option<String>,
    timeout_seconds: Option<f64>,
    net_mode: Option<String>,
    no_sandbox: Option<bool>,
    no_strace: Option<bool>,
    snippet_max_lines: usize,
    snippet_max_bytes: usize,
    env: BTreeMap<String, String>,
}

fn effective_scenario_config(
    plan: &ScenarioPlan,
    scenario: &ScenarioSpec,
) -> Result<ScenarioRunConfig> {
    let defaults = plan.defaults.as_ref();

    let mut env = plan.default_env.clone();
    if let Some(defaults) = defaults {
        env = merge_env(&env, &defaults.env);
    }
    env = merge_env(&env, &scenario.env);

    let seed_dir = if scenario.seed.is_some() {
        None
    } else {
        scenario
            .seed_dir
            .clone()
            .or_else(|| defaults.and_then(|value| value.seed_dir.clone()))
    };

    let cwd = scenario
        .cwd
        .clone()
        .or_else(|| defaults.and_then(|value| value.cwd.clone()));
    let timeout_seconds = scenario
        .timeout_seconds
        .or_else(|| defaults.and_then(|value| value.timeout_seconds));
    let net_mode = scenario
        .net_mode
        .clone()
        .or_else(|| defaults.and_then(|value| value.net_mode.clone()));
    let no_sandbox = scenario
        .no_sandbox
        .or_else(|| defaults.and_then(|value| value.no_sandbox));
    let no_strace = scenario
        .no_strace
        .or_else(|| defaults.and_then(|value| value.no_strace));
    let snippet_max_lines = scenario
        .snippet_max_lines
        .or_else(|| defaults.and_then(|value| value.snippet_max_lines))
        .unwrap_or(DEFAULT_SNIPPET_MAX_LINES);
    let snippet_max_bytes = scenario
        .snippet_max_bytes
        .or_else(|| defaults.and_then(|value| value.snippet_max_bytes))
        .unwrap_or(DEFAULT_SNIPPET_MAX_BYTES);

    let scenario_digest = scenario_digest(
        scenario,
        &env,
        seed_dir.as_deref(),
        cwd.as_deref(),
        timeout_seconds,
        net_mode.as_deref(),
        no_sandbox,
        no_strace,
        snippet_max_lines,
        snippet_max_bytes,
    )?;

    Ok(ScenarioRunConfig {
        env,
        seed_dir,
        cwd,
        timeout_seconds,
        net_mode,
        no_sandbox,
        no_strace,
        snippet_max_lines,
        snippet_max_bytes,
        scenario_digest,
    })
}

fn scenario_digest(
    scenario: &ScenarioSpec,
    env: &BTreeMap<String, String>,
    seed_dir: Option<&str>,
    cwd: Option<&str>,
    timeout_seconds: Option<f64>,
    net_mode: Option<&str>,
    no_sandbox: Option<bool>,
    no_strace: Option<bool>,
    snippet_max_lines: usize,
    snippet_max_bytes: usize,
) -> Result<String> {
    let seed = if let Some(seed) = scenario.seed.as_ref() {
        let mut entries: Vec<ScenarioSeedEntryDigest> = seed
            .entries
            .iter()
            .map(|entry| {
                let path = normalize_seed_path(&entry.path)
                    .with_context(|| format!("seed entry path {:?}", entry.path))?;
                let target = match entry.target.as_ref() {
                    Some(target) => Some(
                        normalize_seed_path(target)
                            .with_context(|| format!("seed entry target {:?}", target))?,
                    ),
                    None => None,
                };
                Ok(ScenarioSeedEntryDigest {
                    path,
                    kind: entry.kind,
                    contents: entry.contents.clone(),
                    target,
                    mode: entry.mode,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Some(ScenarioSeedSpecDigest { entries })
    } else {
        None
    };

    let payload = ScenarioDigestInput {
        argv: scenario.argv.clone(),
        expect: scenario.expect.clone(),
        scope: scenario.scope.clone(),
        seed_dir: seed_dir.map(|value| value.to_string()),
        seed,
        cwd: cwd.map(|value| value.to_string()),
        timeout_seconds,
        net_mode: net_mode.map(|value| value.to_string()),
        no_sandbox,
        no_strace,
        snippet_max_lines,
        snippet_max_bytes,
        env: env.clone(),
    };
    let bytes = serde_json::to_vec(&payload).context("serialize scenario digest input")?;
    Ok(sha256_hex(&bytes))
}

fn bounded_snippet(text: &str, max_lines: usize, max_bytes: usize) -> String {
    let marker = "\n[... output truncated ...]\n";
    if max_lines == 0 || max_bytes == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut truncated = false;

    for (line_idx, chunk) in text.split_inclusive('\n').enumerate() {
        if line_idx >= max_lines {
            truncated = true;
            break;
        }
        if out.len() + chunk.len() > max_bytes {
            let remaining = max_bytes.saturating_sub(out.len());
            out.push_str(truncate_utf8(chunk, remaining));
            truncated = true;
            break;
        }
        out.push_str(chunk);
    }

    if !truncated && out.len() < text.len() {
        truncated = true;
    }

    if truncated {
        if max_bytes <= marker.len() {
            return truncate_utf8(marker, max_bytes).to_string();
        }
        let available = max_bytes - marker.len();
        if out.len() > available {
            out = truncate_utf8(&out, available).to_string();
        }
        out.push_str(marker);
    }

    out
}

fn validate_scenario_defaults(defaults: &ScenarioDefaults, doc_pack_root: &Path) -> Result<()> {
    if let Some(timeout_seconds) = defaults.timeout_seconds {
        if !timeout_seconds.is_finite() || timeout_seconds < 0.0 {
            return Err(anyhow!("defaults.timeout_seconds must be >= 0"));
        }
    }
    if let Some(seed_dir) = defaults.seed_dir.as_deref() {
        let trimmed = seed_dir.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("defaults.seed_dir must not be empty"));
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            return Err(anyhow!("defaults.seed_dir must be a relative path"));
        }
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(anyhow!("defaults.seed_dir must not contain '..'"));
        }
        let resolved = doc_pack_root.join(trimmed);
        if !resolved.is_dir() {
            return Err(anyhow!(
                "defaults.seed_dir does not exist at {}",
                resolved.display()
            ));
        }
    }
    if let Some(cwd) = defaults.cwd.as_deref() {
        let trimmed = cwd.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("defaults.cwd must not be empty"));
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            return Err(anyhow!("defaults.cwd must be a relative path"));
        }
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(anyhow!("defaults.cwd must not contain '..'"));
        }
    }
    if let Some(net_mode) = defaults.net_mode.as_deref() {
        if net_mode != "off" && net_mode != "inherit" {
            return Err(anyhow!(
                "defaults.net_mode must be \"off\" or \"inherit\" (got {net_mode:?})"
            ));
        }
    }
    if let Some(max_lines) = defaults.snippet_max_lines {
        if max_lines == 0 {
            return Err(anyhow!("defaults.snippet_max_lines must be > 0"));
        }
    }
    if let Some(max_bytes) = defaults.snippet_max_bytes {
        if max_bytes == 0 {
            return Err(anyhow!("defaults.snippet_max_bytes must be > 0"));
        }
    }
    Ok(())
}

fn validate_scenario_spec(scenario: &ScenarioSpec) -> Result<()> {
    let id = scenario.id.trim();
    if id.is_empty() {
        return Err(anyhow!("scenario id must not be empty"));
    }
    if id.contains('/') || id.contains('\\') {
        return Err(anyhow!("scenario id must not include path separators"));
    }
    for scope in &scenario.scope {
        let trimmed = scope.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("scope entries must not be empty"));
        }
        if trimmed.contains('/') || trimmed.contains('\\') {
            return Err(anyhow!("scope entries must not include path separators"));
        }
    }
    if scenario.seed_dir.is_some() && scenario.seed.is_some() {
        return Err(anyhow!("use only one of seed_dir or seed"));
    }
    if let Some(seed) = scenario.seed.as_ref() {
        validate_seed_spec(seed)?;
    }
    if let Some(timeout_seconds) = scenario.timeout_seconds {
        if !timeout_seconds.is_finite() || timeout_seconds < 0.0 {
            return Err(anyhow!("timeout_seconds must be >= 0"));
        }
    }
    if let Some(seed_dir) = scenario.seed_dir.as_deref() {
        let trimmed = seed_dir.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("seed_dir must not be empty"));
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            return Err(anyhow!("seed_dir must be a relative path"));
        }
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(anyhow!("seed_dir must not contain '..'"));
        }
    }
    if let Some(cwd) = scenario.cwd.as_deref() {
        let trimmed = cwd.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("cwd must not be empty"));
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            return Err(anyhow!("cwd must be a relative path"));
        }
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(anyhow!("cwd must not contain '..'"));
        }
    }
    if let Some(net_mode) = scenario.net_mode.as_deref() {
        if net_mode != "off" && net_mode != "inherit" {
            return Err(anyhow!(
                "net_mode must be \"off\" or \"inherit\" (got {net_mode:?})"
            ));
        }
    }
    if let Some(max_lines) = scenario.snippet_max_lines {
        if max_lines == 0 {
            return Err(anyhow!("snippet_max_lines must be > 0"));
        }
    }
    if let Some(max_bytes) = scenario.snippet_max_bytes {
        if max_bytes == 0 {
            return Err(anyhow!("snippet_max_bytes must be > 0"));
        }
    }
    if let Some(coverage_tier) = scenario.coverage_tier.as_deref() {
        if coverage_tier != "acceptance"
            && coverage_tier != "behavior"
            && coverage_tier != "rejection"
        {
            return Err(anyhow!(
                "coverage_tier must be \"acceptance\", \"behavior\", or \"rejection\" (got {coverage_tier:?})"
            ));
        }
    }
    for option_id in &scenario.covers {
        if option_id.trim().is_empty() {
            return Err(anyhow!("covers entries must not be empty"));
        }
    }
    if !scenario.coverage_ignore && !scenario.covers.is_empty() {
        let has_argv = scenario.argv.iter().any(|token| !token.trim().is_empty());
        if !has_argv {
            return Err(anyhow!(
                "scenarios that cover items must include argv tokens"
            ));
        }
    }
    validate_scenario_expect(&scenario.expect)?;
    Ok(())
}

fn validate_scenario_expect(expect: &ScenarioExpect) -> Result<()> {
    validate_regex_patterns(&expect.stdout_regex_all, "stdout_regex_all")?;
    validate_regex_patterns(&expect.stdout_regex_any, "stdout_regex_any")?;
    validate_regex_patterns(&expect.stderr_regex_all, "stderr_regex_all")?;
    validate_regex_patterns(&expect.stderr_regex_any, "stderr_regex_any")?;
    Ok(())
}

fn validate_regex_patterns(patterns: &[String], field: &str) -> Result<()> {
    for pattern in patterns {
        Regex::new(pattern)
            .with_context(|| format!("invalid {field} regex pattern {pattern:?}"))?;
    }
    Ok(())
}

fn build_run_kv_args(
    run_argv0: &str,
    run_seed_dir: Option<&str>,
    cwd: Option<&str>,
    timeout_seconds: Option<f64>,
    net_mode: Option<&str>,
    no_sandbox: Option<bool>,
    no_strace: Option<bool>,
) -> Result<Vec<String>> {
    let mut args = vec![String::from("run=1"), format!("run_argv0={run_argv0}")];

    if let Some(seed_dir) = run_seed_dir {
        args.push(format!("run_seed_dir={seed_dir}"));
    }
    if let Some(cwd) = cwd {
        args.push(format!("run_cwd={cwd}"));
    }
    if let Some(timeout_seconds) = timeout_seconds {
        args.push(format!("run_timeout_seconds={timeout_seconds}"));
    }
    if let Some(net_mode) = net_mode {
        args.push(format!("run_net={net_mode}"));
    }
    if let Some(no_sandbox) = no_sandbox {
        args.push(format!("run_no_sandbox={}", if no_sandbox { 1 } else { 0 }));
    }
    if let Some(no_strace) = no_strace {
        args.push(format!("run_no_strace={}", if no_strace { 1 } else { 0 }));
    }

    Ok(args)
}

struct MaterializedSeed {
    rel_path: String,
    _abs_path: PathBuf,
}

fn materialize_inline_seed(
    staging_root: &Path,
    run_root: &Path,
    scenario_id: &str,
    seed: &ScenarioSeedSpec,
) -> Result<MaterializedSeed> {
    validate_seed_spec(seed).with_context(|| format!("validate seed for {scenario_id}"))?;
    let now = enrich::now_epoch_ms()?;
    let txn_root = staging_root
        .parent()
        .ok_or_else(|| anyhow!("staging root has no parent"))?;
    let seed_root = txn_root
        .join("scratch")
        .join("seeds")
        .join(format!("{scenario_id}-{now}"));
    fs::create_dir_all(&seed_root)
        .with_context(|| format!("create seed root {}", seed_root.display()))?;

    let mut seen = HashSet::new();
    let mut total_bytes = 0usize;

    for entry in &seed.entries {
        let rel_path = normalize_seed_path(&entry.path)
            .with_context(|| format!("seed entry path {:?}", entry.path))?;
        if !seen.insert(rel_path.clone()) {
            return Err(anyhow!("seed entry path {:?} is duplicated", rel_path));
        }
        let target_path = seed_root.join(&rel_path);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        match entry.kind {
            SeedEntryKind::Dir => {
                if entry.contents.is_some() {
                    return Err(anyhow!("seed dir {:?} must not include contents", rel_path));
                }
                if entry.target.is_some() {
                    return Err(anyhow!("seed dir {:?} must not include target", rel_path));
                }
                fs::create_dir_all(&target_path)
                    .with_context(|| format!("create dir {}", target_path.display()))?;
                apply_seed_mode(&target_path, entry.mode)?;
            }
            SeedEntryKind::File => {
                if entry.target.is_some() {
                    return Err(anyhow!("seed file {:?} must not include target", rel_path));
                }
                let contents = entry
                    .contents
                    .as_ref()
                    .ok_or_else(|| anyhow!("seed file {:?} missing contents", rel_path))?;
                total_bytes = total_bytes
                    .checked_add(contents.len())
                    .ok_or_else(|| anyhow!("seed size overflow"))?;
                if total_bytes > MAX_SEED_TOTAL_BYTES {
                    return Err(anyhow!(
                        "seed exceeds max total bytes ({MAX_SEED_TOTAL_BYTES})"
                    ));
                }
                fs::write(&target_path, contents.as_bytes())
                    .with_context(|| format!("write {}", target_path.display()))?;
                apply_seed_mode(&target_path, entry.mode)?;
            }
            SeedEntryKind::Symlink => {
                if entry.contents.is_some() {
                    return Err(anyhow!(
                        "seed symlink {:?} must not include contents",
                        rel_path
                    ));
                }
                let target = entry
                    .target
                    .as_ref()
                    .ok_or_else(|| anyhow!("seed symlink {:?} missing target", rel_path))?;
                let target_rel = normalize_seed_path(target)
                    .with_context(|| format!("symlink target {target:?}"))?;
                total_bytes = total_bytes
                    .checked_add(target_rel.len())
                    .ok_or_else(|| anyhow!("seed size overflow"))?;
                if total_bytes > MAX_SEED_TOTAL_BYTES {
                    return Err(anyhow!(
                        "seed exceeds max total bytes ({MAX_SEED_TOTAL_BYTES})"
                    ));
                }
                apply_seed_symlink(&target_rel, &target_path)?;
            }
        }
    }

    let rel_path = seed_root
        .strip_prefix(run_root)
        .with_context(|| format!("seed root {} outside run root", seed_root.display()))?
        .to_string_lossy()
        .to_string();

    Ok(MaterializedSeed {
        rel_path,
        _abs_path: seed_root,
    })
}

fn validate_seed_spec(seed: &ScenarioSeedSpec) -> Result<()> {
    if seed.entries.len() > MAX_SEED_ENTRIES {
        return Err(anyhow!("seed exceeds max entries ({MAX_SEED_ENTRIES})"));
    }
    let mut seen = HashSet::new();
    let mut total_bytes = 0usize;
    for entry in &seed.entries {
        let rel_path = normalize_seed_path(&entry.path)
            .with_context(|| format!("seed entry path {:?}", entry.path))?;
        if !seen.insert(rel_path) {
            return Err(anyhow!("seed entry paths must be unique"));
        }
        match entry.kind {
            SeedEntryKind::Dir => {
                if entry.contents.is_some() {
                    return Err(anyhow!("seed dir must not include contents"));
                }
                if entry.target.is_some() {
                    return Err(anyhow!("seed dir must not include target"));
                }
            }
            SeedEntryKind::File => {
                if entry.target.is_some() {
                    return Err(anyhow!("seed file must not include target"));
                }
                let contents = entry
                    .contents
                    .as_ref()
                    .ok_or_else(|| anyhow!("seed file missing contents"))?;
                total_bytes = total_bytes
                    .checked_add(contents.len())
                    .ok_or_else(|| anyhow!("seed size overflow"))?;
            }
            SeedEntryKind::Symlink => {
                #[cfg(not(unix))]
                {
                    return Err(anyhow!("seed symlinks are unsupported on this platform"));
                }
                if entry.contents.is_some() {
                    return Err(anyhow!("seed symlink must not include contents"));
                }
                let target = entry
                    .target
                    .as_ref()
                    .ok_or_else(|| anyhow!("seed symlink missing target"))?;
                normalize_seed_path(target)
                    .with_context(|| format!("symlink target {target:?}"))?;
                total_bytes = total_bytes
                    .checked_add(target.len())
                    .ok_or_else(|| anyhow!("seed size overflow"))?;
            }
        }
        validate_seed_mode(entry.mode)?;
        if total_bytes > MAX_SEED_TOTAL_BYTES {
            return Err(anyhow!(
                "seed exceeds max total bytes ({MAX_SEED_TOTAL_BYTES})"
            ));
        }
    }
    Ok(())
}

fn normalize_seed_path(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("seed paths must not be empty"));
    }
    let normalized = trimmed.replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute() {
        return Err(anyhow!("seed paths must be relative"));
    }
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => cleaned.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(anyhow!("seed paths must not contain '..'"));
            }
            _ => return Err(anyhow!("seed paths must be relative")),
        }
    }
    let cleaned = cleaned.to_string_lossy().to_string();
    if cleaned.is_empty() {
        return Err(anyhow!("seed paths must not be empty"));
    }
    Ok(cleaned)
}

fn validate_seed_mode(mode: Option<u32>) -> Result<()> {
    if let Some(mode) = mode {
        #[cfg(not(unix))]
        {
            return Err(anyhow!("seed mode is unsupported on this platform"));
        }
        if mode > 0o777 {
            return Err(anyhow!("seed mode must be <= 0777"));
        }
    }
    Ok(())
}

fn apply_seed_mode(path: &Path, mode: Option<u32>) -> Result<()> {
    let Some(mode) = mode else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .with_context(|| format!("inspect {}", path.display()))?
            .permissions();
        perms.set_mode(mode);
        fs::set_permissions(path, perms)
            .with_context(|| format!("set permissions on {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        return Err(anyhow!("seed mode is unsupported on this platform"));
    }
    Ok(())
}

fn apply_seed_symlink(target: &str, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, path)
            .with_context(|| format!("create symlink {}", path.display()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        let _ = path;
        Err(anyhow!("seed symlinks are unsupported on this platform"))
    }
}

fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn exit_status_string(status: &std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        format!("{code}")
    } else {
        "terminated by signal".to_string()
    }
}

fn format_command_line(binary_name: &str, argv: &[String]) -> String {
    let mut parts = Vec::with_capacity(argv.len() + 1);
    parts.push(shell_quote(binary_name));
    for arg in argv {
        parts.push(shell_quote(arg));
    }
    parts.join(" ")
}

fn shell_quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    let safe = arg.chars().all(|ch| {
        matches!(
            ch,
            'a'..='z'
                | 'A'..='Z'
                | '0'..='9'
                | '_'
                | '-'
                | '.'
                | '/'
                | ':'
                | '@'
                | '+'
                | '='
        )
    });
    if safe {
        return arg.to_string();
    }
    let escaped = arg.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_expect() -> ScenarioExpect {
        ScenarioExpect {
            exit_code: Some(0),
            exit_signal: None,
            stdout_contains_all: Vec::new(),
            stdout_contains_any: Vec::new(),
            stdout_regex_all: Vec::new(),
            stdout_regex_any: Vec::new(),
            stderr_contains_all: Vec::new(),
            stderr_contains_any: Vec::new(),
            stderr_regex_all: Vec::new(),
            stderr_regex_any: Vec::new(),
        }
    }

    fn base_scenario() -> ScenarioSpec {
        ScenarioSpec {
            id: "scenario".to_string(),
            kind: ScenarioKind::Behavior,
            publish: false,
            scope: Vec::new(),
            argv: vec!["--help".to_string()],
            env: BTreeMap::new(),
            seed_dir: None,
            seed: None,
            cwd: None,
            timeout_seconds: None,
            net_mode: None,
            no_sandbox: None,
            no_strace: None,
            snippet_max_lines: None,
            snippet_max_bytes: None,
            coverage_tier: None,
            covers: Vec::new(),
            coverage_ignore: true,
            expect: base_expect(),
        }
    }

    fn plan_with(scenarios: Vec<ScenarioSpec>, defaults: Option<ScenarioDefaults>) -> ScenarioPlan {
        ScenarioPlan {
            schema_version: SCENARIO_PLAN_SCHEMA_VERSION,
            binary: None,
            default_env: BTreeMap::new(),
            defaults,
            coverage: None,
            verification: VerificationPlan::default(),
            scenarios,
        }
    }

    #[test]
    fn scenario_digest_stable_and_sensitive_to_env() {
        let scenario = base_scenario();
        let plan = plan_with(vec![scenario.clone()], None);
        let first = effective_scenario_config(&plan, &scenario).unwrap();
        let second = effective_scenario_config(&plan, &scenario).unwrap();
        assert_eq!(first.scenario_digest, second.scenario_digest);

        let mut scenario_changed = scenario.clone();
        scenario_changed
            .env
            .insert("NO_COLOR".to_string(), "0".to_string());
        let changed = effective_scenario_config(&plan, &scenario_changed).unwrap();
        assert_ne!(first.scenario_digest, changed.scenario_digest);
    }

    #[test]
    fn defaults_merge_and_env_precedence() {
        let mut default_env = BTreeMap::new();
        default_env.insert("LANG".to_string(), "C".to_string());
        let mut defaults_env = BTreeMap::new();
        defaults_env.insert("LANG".to_string(), "C.UTF-8".to_string());
        let defaults = ScenarioDefaults {
            env: defaults_env,
            seed_dir: Some("fixtures".to_string()),
            cwd: Some("work".to_string()),
            timeout_seconds: Some(3.0),
            net_mode: Some("off".to_string()),
            no_sandbox: Some(false),
            no_strace: Some(true),
            snippet_max_lines: Some(7),
            snippet_max_bytes: Some(77),
        };

        let mut scenario = base_scenario();
        scenario.timeout_seconds = Some(5.0);
        scenario.snippet_max_lines = Some(11);
        let mut plan = plan_with(vec![scenario.clone()], Some(defaults));
        plan.default_env = default_env;

        let config = effective_scenario_config(&plan, &scenario).unwrap();
        assert_eq!(config.timeout_seconds, Some(5.0));
        assert_eq!(config.net_mode.as_deref(), Some("off"));
        assert_eq!(config.no_sandbox, Some(false));
        assert_eq!(config.no_strace, Some(true));
        assert_eq!(config.snippet_max_lines, 11);
        assert_eq!(config.snippet_max_bytes, 77);
        assert_eq!(config.cwd.as_deref(), Some("work"));
        assert_eq!(config.seed_dir.as_deref(), Some("fixtures"));
        assert_eq!(config.env.get("LANG").map(String::as_str), Some("C.UTF-8"));

        scenario.env.insert("LANG".to_string(), "POSIX".to_string());
        let config_override = effective_scenario_config(&plan, &scenario).unwrap();
        assert_eq!(
            config_override.env.get("LANG").map(String::as_str),
            Some("POSIX")
        );
    }

    #[test]
    fn env_defaults_are_plan_owned() {
        let scenario = base_scenario();
        let mut plan = plan_with(vec![scenario.clone()], None);
        let config = effective_scenario_config(&plan, &scenario).unwrap();
        assert!(!config.env.contains_key("LC_ALL"));
        assert!(!config.env.contains_key("LANG"));

        plan.default_env
            .insert("LC_ALL".to_string(), "C".to_string());
        let config = effective_scenario_config(&plan, &scenario).unwrap();
        assert_eq!(config.env.get("LC_ALL").map(String::as_str), Some("C"));
    }

    #[test]
    fn plan_stub_includes_multiple_help_scenarios() {
        let plan: ScenarioPlan = serde_json::from_str(&plan_stub(Some("tool"))).unwrap();
        assert_eq!(plan.schema_version, SCENARIO_PLAN_SCHEMA_VERSION);
        assert_eq!(plan.binary.as_deref(), Some("tool"));
        assert_eq!(
            plan.default_env.get("LC_ALL").map(String::as_str),
            Some("C")
        );
        assert_eq!(plan.default_env.get("LANG").map(String::as_str), Some("C"));
        assert_eq!(
            plan.default_env.get("TERM").map(String::as_str),
            Some("dumb")
        );
        assert_eq!(
            plan.default_env.get("NO_COLOR").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            plan.default_env.get("PAGER").map(String::as_str),
            Some("cat")
        );
        assert_eq!(
            plan.default_env.get("GIT_PAGER").map(String::as_str),
            Some("cat")
        );
        let defaults = plan.defaults.as_ref().expect("defaults");
        assert_eq!(defaults.seed_dir.as_deref(), Some("fixtures/empty"));
        assert_eq!(defaults.cwd.as_deref(), Some("."));
        assert_eq!(defaults.timeout_seconds, Some(3.0));
        assert_eq!(defaults.net_mode.as_deref(), Some("off"));
        assert_eq!(defaults.no_sandbox, Some(false));
        assert_eq!(defaults.no_strace, Some(true));
        assert_eq!(defaults.snippet_max_lines, Some(12));
        assert_eq!(defaults.snippet_max_bytes, Some(1024));
        assert!(plan.verification.queue.is_empty());

        let expected = [
            ("help--help", "--help"),
            ("help--usage", "--usage"),
            ("help--question", "-?"),
        ];
        let ids: Vec<&str> = plan
            .scenarios
            .iter()
            .map(|scenario| scenario.id.as_str())
            .collect();
        assert_eq!(ids, expected.iter().map(|(id, _)| *id).collect::<Vec<_>>());

        for (scenario, (expected_id, expected_arg)) in plan.scenarios.iter().zip(expected.iter()) {
            assert_eq!(scenario.id, *expected_id);
            assert_eq!(scenario.kind, ScenarioKind::Help);
            assert!(!scenario.publish);
            assert!(scenario.coverage_ignore);
            assert_eq!(scenario.argv, vec![(*expected_arg).to_string()]);
            assert!(scenario.timeout_seconds.is_none());
            assert!(scenario.net_mode.is_none());
            assert!(scenario.no_sandbox.is_none());
            assert!(scenario.no_strace.is_none());
            assert!(scenario.snippet_max_lines.is_none());
            assert!(scenario.snippet_max_bytes.is_none());
            assert_eq!(scenario.expect, ScenarioExpect::default());
        }
    }

    #[test]
    fn should_run_scenario_respects_run_mode() {
        let entry = ScenarioIndexEntry {
            scenario_id: "scenario".to_string(),
            scenario_digest: "abc".to_string(),
            last_run_epoch_ms: None,
            last_pass: Some(true),
            failures: Vec::new(),
            evidence_paths: Vec::new(),
        };
        assert!(!should_run_scenario(
            ScenarioRunMode::Default,
            "abc",
            Some(&entry),
            true
        ));
        assert!(should_run_scenario(
            ScenarioRunMode::Default,
            "def",
            Some(&entry),
            true
        ));
        let failed_entry = ScenarioIndexEntry {
            last_pass: Some(false),
            ..entry.clone()
        };
        assert!(should_run_scenario(
            ScenarioRunMode::Default,
            "abc",
            Some(&failed_entry),
            true
        ));
        assert!(should_run_scenario(
            ScenarioRunMode::Default,
            "abc",
            None,
            true
        ));
        assert!(should_run_scenario(
            ScenarioRunMode::RerunAll,
            "abc",
            Some(&entry),
            true
        ));
        assert!(!should_run_scenario(
            ScenarioRunMode::RerunFailed,
            "def",
            Some(&entry),
            true
        ));
        assert!(should_run_scenario(
            ScenarioRunMode::RerunFailed,
            "abc",
            Some(&failed_entry),
            true
        ));
        assert!(should_run_scenario(
            ScenarioRunMode::Default,
            "abc",
            Some(&entry),
            false
        ));
    }
}
