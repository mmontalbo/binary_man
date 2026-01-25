use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use crate::enrich;
use crate::surface;

const DEFAULT_SNIPPET_MAX_BYTES: usize = 4096;
const DEFAULT_SNIPPET_MAX_LINES: usize = 60;

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct ScenarioCatalog {
    pub schema: Option<SchemaRef>,
    pub binary: Option<String>,
    #[serde(default)]
    pub default_env: BTreeMap<String, String>,
    #[serde(default)]
    pub coverage: Option<CoverageNotes>,
    #[serde(default)]
    pub scenarios: Vec<ScenarioSpec>,
}

#[derive(Debug, Deserialize)]
pub struct SchemaRef {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct CoverageNotes {
    #[serde(default)]
    pub blocked: Vec<CoverageBlocked>,
}

#[derive(Debug, Deserialize)]
pub struct CoverageBlocked {
    #[serde(default)]
    pub option_ids: Vec<String>,
    pub reason: String,
    #[serde(default)]
    pub details: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ScenarioSpec {
    pub id: String,
    #[serde(default = "default_true")]
    pub publish: bool,
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
    pub snippet_max_lines: Option<usize>,
    pub snippet_max_bytes: Option<usize>,
    #[serde(default)]
    pub coverage_tier: Option<String>,
    #[serde(default)]
    pub covers_options: Vec<String>,
    #[serde(default)]
    pub coverage_ignore: bool,
    pub expect: ScenarioExpect,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
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

#[derive(Debug, Deserialize, Serialize)]
pub struct CoverageLedger {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: String,
    pub scenarios_path: String,
    pub validation_source: String,
    pub options_total: usize,
    pub behavior_count: usize,
    pub rejected_count: usize,
    pub acceptance_count: usize,
    pub blocked_count: usize,
    pub uncovered_count: usize,
    pub options: Vec<CoverageOptionEntry>,
    pub unknown_options: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CoverageOptionEntry {
    pub option_id: String,
    pub aliases: Vec<String>,
    pub status: String,
    pub behavior_scenarios: Vec<String>,
    pub rejection_scenarios: Vec<String>,
    pub acceptance_scenarios: Vec<String>,
    pub blocked_reason: Option<String>,
    pub blocked_details: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<enrich::EvidenceRef>,
}

pub fn load_catalog(path: &Path) -> Result<ScenarioCatalog> {
    let bytes =
        fs::read(path).with_context(|| format!("read scenarios catalog {}", path.display()))?;
    let catalog: ScenarioCatalog =
        serde_json::from_slice(&bytes).context("parse scenarios catalog JSON")?;
    if let Some(schema) = catalog.schema.as_ref() {
        if schema.name != "binary_man_scenarios" {
            return Err(anyhow!(
                "unexpected scenarios schema name {:?} (expected \"binary_man_scenarios\")",
                schema.name
            ));
        }
    }
    if catalog.scenarios.is_empty() {
        return Err(anyhow!("scenarios catalog contains no scenarios"));
    }
    if let Some(coverage) = catalog.coverage.as_ref() {
        for blocked in &coverage.blocked {
            if blocked.option_ids.is_empty() {
                return Err(anyhow!("coverage.blocked entries must include option_ids"));
            }
            if blocked.reason.trim().is_empty() {
                return Err(anyhow!("coverage.blocked reason must not be empty"));
            }
        }
    }
    Ok(catalog)
}

pub fn run_scenarios(
    pack_root: &Path,
    run_root: &Path,
    binary_name: &str,
    scenarios_path: &Path,
    lens_flake: &str,
    display_root: Option<&Path>,
    verbose: bool,
) -> Result<ExamplesReport> {
    let catalog = load_catalog(scenarios_path)?;
    if let Some(catalog_binary) = catalog.binary.as_deref() {
        if catalog_binary != binary_name {
            return Err(anyhow!(
                "scenarios catalog binary {:?} does not match pack binary {:?}",
                catalog_binary,
                binary_name
            ));
        }
    }

    let pack_root = pack_root
        .canonicalize()
        .with_context(|| format!("resolve pack root {}", pack_root.display()))?;

    let mut outcomes = Vec::new();
    let mut run_ids = Vec::new();

    for scenario in catalog.scenarios {
        if verbose {
            eprintln!("running scenario {} {}", binary_name, scenario.id);
        }

        validate_scenario_spec(&scenario)
            .with_context(|| format!("validate scenario {}", scenario.id))?;

        let env = merge_env(&catalog.default_env, &scenario.env);
        let seed_dir = scenario.seed_dir.clone();
        let cwd = scenario.cwd.clone();
        let snippet_max_lines = scenario
            .snippet_max_lines
            .unwrap_or(DEFAULT_SNIPPET_MAX_LINES);
        let snippet_max_bytes = scenario
            .snippet_max_bytes
            .unwrap_or(DEFAULT_SNIPPET_MAX_BYTES);
        let run_argv0 = binary_name.to_string();
        let run_kv_args = build_run_kv_args(&scenario, &run_argv0)?;
        let before = read_runs_index(&pack_root).context("read runs index (before)")?;

        let status = invoke_binary_lens_run(
            &pack_root,
            run_root,
            lens_flake,
            &run_kv_args,
            &scenario.argv,
            &env,
        )
        .with_context(|| format!("invoke binary_lens for scenario {}", scenario.id))?;
        if !status.success() {
            let argv = scenario.argv.clone();
            let command_line = format_command_line(binary_name, &argv);
            let failures = vec![format!(
                "binary_lens run failed with status {}",
                exit_status_string(&status)
            )];
            outcomes.push(ScenarioOutcome {
                scenario_id: scenario.id,
                publish: scenario.publish,
                argv,
                env,
                seed_dir,
                cwd,
                timeout_seconds: scenario.timeout_seconds,
                net_mode: scenario.net_mode.clone(),
                no_sandbox: scenario.no_sandbox,
                no_strace: scenario.no_strace,
                snippet_max_lines,
                snippet_max_bytes,
                run_argv0,
                expected: scenario.expect,
                run_id: None,
                manifest_ref: None,
                stdout_ref: None,
                stderr_ref: None,
                observed_exit_code: None,
                observed_exit_signal: None,
                observed_timed_out: false,
                pass: false,
                failures,
                command_line,
                stdout_snippet: String::new(),
                stderr_snippet: String::new(),
            });
            continue;
        }

        let after = read_runs_index(&pack_root).context("read runs index (after)")?;
        let (run_id, entry) = resolve_new_run(&before, &after)
            .with_context(|| format!("resolve new run for scenario {}", scenario.id))?;
        run_ids.push(run_id.clone());

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
        let stdout_snippet =
            bounded_snippet(stdout_text.as_ref(), snippet_max_lines, snippet_max_bytes);
        let stderr_snippet =
            bounded_snippet(stderr_text.as_ref(), snippet_max_lines, snippet_max_bytes);

        if verbose && !pass {
            eprintln!("scenario {} failed: {}", scenario.id, failures.join("; "));
        }

        outcomes.push(ScenarioOutcome {
            scenario_id: scenario.id,
            publish: scenario.publish,
            argv: scenario.argv,
            env,
            seed_dir,
            cwd,
            timeout_seconds: scenario.timeout_seconds,
            net_mode: scenario.net_mode,
            no_sandbox: scenario.no_sandbox,
            no_strace: scenario.no_strace,
            snippet_max_lines,
            snippet_max_bytes,
            run_argv0,
            expected: scenario.expect,
            run_id: Some(run_id),
            manifest_ref: Some(manifest_ref),
            stdout_ref: Some(stdout_ref),
            stderr_ref: Some(stderr_ref),
            observed_exit_code,
            observed_exit_signal,
            observed_timed_out,
            pass,
            failures,
            command_line,
            stdout_snippet,
            stderr_snippet,
        });
    }

    let pass_count = outcomes.iter().filter(|outcome| outcome.pass).count();
    let fail_count = outcomes.len() - pass_count;
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

pub fn build_coverage_ledger(
    binary_name: &str,
    surface: &surface::SurfaceInventory,
    doc_pack_root: &Path,
    scenarios_path: &Path,
    display_root: Option<&Path>,
) -> Result<CoverageLedger> {
    let catalog = load_catalog(scenarios_path)?;
    if let Some(catalog_binary) = catalog.binary.as_deref() {
        if catalog_binary != binary_name {
            return Err(anyhow!(
                "scenarios catalog binary {:?} does not match pack binary {:?}",
                catalog_binary,
                binary_name
            ));
        }
    }

    let surface_path = doc_pack_root.join("inventory").join("surface.json");
    let surface_evidence = enrich::evidence_from_path(doc_pack_root, &surface_path)?;
    let catalog_evidence = enrich::evidence_from_path(doc_pack_root, scenarios_path)?;
    let mut options: BTreeMap<String, CoverageState> = BTreeMap::new();
    for item in surface.items.iter().filter(|item| is_surface_item_kind(&item.kind)) {
        let aliases = if item.display != item.id {
            vec![item.display.clone()]
        } else {
            Vec::new()
        };
        options.insert(
            item.id.clone(),
            CoverageState {
                aliases,
                evidence: item.evidence.clone(),
                ..CoverageState::default()
            },
        );
    }

    let mut warnings = Vec::new();
    let mut unknown_options = BTreeSet::new();
    let mut blocked_map: HashMap<String, BlockedInfo> = HashMap::new();
    if let Some(coverage) = catalog.coverage.as_ref() {
        for blocked in &coverage.blocked {
            for option_id in &blocked.option_ids {
                let normalized = normalize_surface_id(option_id);
                if normalized.is_empty() {
                    continue;
                }
                let entry = blocked_map.entry(normalized).or_insert(BlockedInfo {
                    reason: blocked.reason.clone(),
                    details: blocked.details.clone(),
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
            }
        }
    }

    for scenario in &catalog.scenarios {
        validate_scenario_spec(scenario)
            .with_context(|| format!("validate scenario {}", scenario.id))?;
        if scenario.coverage_ignore {
            continue;
        }
        if scenario.covers_options.is_empty() {
            warnings.push(format!(
                "scenario {:?} missing covers_options for coverage",
                scenario.id
            ));
            continue;
        }
        let tier = coverage_tier(scenario);
        let option_ids = scenario_surface_ids(scenario);
        for option_id in option_ids {
            match options.get_mut(&option_id) {
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
                    unknown_options.insert(option_id);
                }
            }
        }
    }

    for (option_id, blocked) in blocked_map {
        match options.get_mut(&option_id) {
            Some(entry) => {
                if !entry.behavior_scenarios.is_empty() {
                    warnings.push(format!(
                        "option {:?} marked blocked but has behavior coverage",
                        option_id
                    ));
                }
                entry.blocked = Some(blocked);
            }
            None => {
                warnings.push(format!(
                    "blocked option {:?} not found in surface inventory",
                    option_id
                ));
                unknown_options.insert(option_id);
            }
        }
    }

    let mut entries = Vec::new();
    let mut behavior_count = 0;
    let mut rejected_count = 0;
    let mut acceptance_count = 0;
    let mut blocked_count = 0;
    let mut uncovered_count = 0;

    for (option_id, entry) in options {
        let behavior_scenarios: Vec<String> = entry.behavior_scenarios.into_iter().collect();
        let rejection_scenarios: Vec<String> = entry.rejection_scenarios.into_iter().collect();
        let acceptance_scenarios: Vec<String> = entry.acceptance_scenarios.into_iter().collect();
        let (blocked_reason, blocked_details) = match entry.blocked.as_ref() {
            Some(blocked) => {
                blocked_count += 1;
                (Some(blocked.reason.clone()), blocked.details.clone())
            }
            None => (None, None),
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
        } else {
            uncovered_count += 1;
            "uncovered"
        };

        let mut evidence = entry.evidence.clone();
        evidence.push(surface_evidence.clone());
        evidence.push(catalog_evidence.clone());
        let evidence = dedup_evidence(evidence);

        entries.push(CoverageOptionEntry {
            option_id,
            aliases: entry.aliases,
            status: status.to_string(),
            behavior_scenarios,
            rejection_scenarios,
            acceptance_scenarios,
            blocked_reason,
            blocked_details,
            evidence,
        });
    }

    let generated_at_epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("compute timestamp")?
        .as_millis();

    Ok(CoverageLedger {
        schema_version: 1,
        generated_at_epoch_ms,
        binary_name: binary_name.to_string(),
        scenarios_path: display_path(scenarios_path, display_root),
        validation_source: "catalog".to_string(),
        options_total: entries.len(),
        behavior_count,
        rejected_count,
        acceptance_count,
        blocked_count,
        uncovered_count,
        options: entries,
        unknown_options: unknown_options.into_iter().collect(),
        warnings,
    })
}

#[derive(Debug, Clone)]
struct BlockedInfo {
    reason: String,
    details: Option<String>,
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
    for token in &scenario.covers_options {
        let normalized = normalize_surface_id(token);
        if !normalized.is_empty() {
            ids.insert(normalized);
        }
    }
    ids.into_iter().collect()
}

fn normalize_surface_id(token: &str) -> String {
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

fn dedup_evidence(entries: Vec<enrich::EvidenceRef>) -> Vec<enrich::EvidenceRef> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for entry in entries {
        if seen.insert(entry.path.clone()) {
            deduped.push(entry);
        }
    }
    deduped
}

fn display_path(path: &Path, base: Option<&Path>) -> String {
    if let Some(base) = base {
        if let Ok(relative) = path.strip_prefix(base) {
            return relative.display().to_string();
        }
    }
    path.display().to_string()
}

fn read_runs_index(pack_root: &Path) -> Result<Vec<RunIndexEntry>> {
    let index_path = pack_root.join("runs").join("index.json");
    if !index_path.is_file() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(&index_path).with_context(|| format!("read {}", index_path.display()))?;
    let index: RunsIndex = serde_json::from_slice(&bytes).context("parse runs index JSON")?;
    Ok(index.runs)
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

fn bounded_snippet(text: &str, max_lines: usize, max_bytes: usize) -> String {
    let marker = "\n[... output truncated ...]\n";
    if max_lines == 0 || max_bytes == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut truncated = false;
    let mut lines = 0usize;

    for chunk in text.split_inclusive('\n') {
        if lines >= max_lines {
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
        lines += 1;
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

fn validate_scenario_spec(scenario: &ScenarioSpec) -> Result<()> {
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
    for option_id in &scenario.covers_options {
        if option_id.trim().is_empty() {
            return Err(anyhow!("covers_options entries must not be empty"));
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

fn build_run_kv_args(scenario: &ScenarioSpec, run_argv0: &str) -> Result<Vec<String>> {
    let mut args = vec![String::from("run=1"), format!("run_argv0={run_argv0}")];

    if let Some(seed_dir) = scenario.seed_dir.as_deref() {
        args.push(format!("run_seed_dir={seed_dir}"));
    }
    if let Some(cwd) = scenario.cwd.as_deref() {
        args.push(format!("run_cwd={cwd}"));
    }
    if let Some(timeout_seconds) = scenario.timeout_seconds {
        args.push(format!("run_timeout_seconds={timeout_seconds}"));
    }
    if let Some(net_mode) = scenario.net_mode.as_deref() {
        args.push(format!("run_net={net_mode}"));
    }
    if let Some(no_sandbox) = scenario.no_sandbox {
        args.push(format!("run_no_sandbox={}", if no_sandbox { 1 } else { 0 }));
    }
    if let Some(no_strace) = scenario.no_strace {
        args.push(format!("run_no_strace={}", if no_strace { 1 } else { 0 }));
    }

    Ok(args)
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
    let safe = arg.chars().all(|ch| match ch {
        'a'..='z' | 'A'..='Z' | '0'..='9' => true,
        '_' | '-' | '.' | '/' | ':' | '@' | '+' | '=' => true,
        _ => false,
    });
    if safe {
        return arg.to_string();
    }
    let escaped = arg.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}
