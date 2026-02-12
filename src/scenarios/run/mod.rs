//! Scenario execution engine.
//!
//! Runs scenarios deterministically and emits evidence blobs without interpreting
//! meaning beyond pack-owned expectations.
mod cache;
mod exec;
mod validate;

use super::config::{effective_scenario_config, ScenarioRunConfig};
use super::evidence::{
    load_scenario_index_state, read_runs_index, write_scenario_index_if_needed, ExamplesReport,
    ScenarioIndexEntry, ScenarioOutcome,
};
use super::seed::materialize_inline_seed;
use super::{ScenarioKind, ScenarioRunMode, ScenarioSpec};
use crate::util::display_path;
use anyhow::{anyhow, Context, Result};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use cache::{load_previous_outcomes, should_run_scenario};
use exec::{
    build_failed_execution, build_run_kv_args, build_success_execution, invoke_binary_lens_run,
    warm_lens_flake,
};

/// Inputs needed to execute a batch of scenarios.
pub struct RunScenariosArgs<'a> {
    pub pack_root: &'a Path,
    pub run_root: &'a Path,
    pub binary_name: &'a str,
    pub scenarios_path: &'a Path,
    pub lens_flake: &'a str,
    pub display_root: Option<&'a Path>,
    pub staging_root: Option<&'a Path>,
    pub kind_filter: Option<ScenarioKind>,
    pub run_mode: ScenarioRunMode,
    pub forced_rerun_scenario_ids: Vec<String>,
    pub extra_scenarios: Vec<ScenarioSpec>,
    pub auto_run_limit: Option<usize>,
    pub auto_progress: Option<AutoVerificationProgress>,
    pub verbose: bool,
}

pub struct AutoVerificationProgress {
    pub remaining_total: Option<usize>,
    pub remaining_by_kind: Vec<AutoVerificationKindProgress>,
    pub max_new_runs_per_apply: usize,
}

pub struct AutoVerificationKindProgress {
    pub kind: String,
    pub remaining_count: usize,
}

pub struct RunScenariosResult {
    pub report: ExamplesReport,
    pub executed_forced_rerun_scenario_ids: Vec<String>,
}

pub(super) struct ScenarioRunContext<'a> {
    pub(super) scenario: &'a ScenarioSpec,
    pub(super) run_config: &'a ScenarioRunConfig,
    pub(super) run_argv0: &'a str,
    pub(super) run_seed_dir: Option<&'a str>,
    pub(super) duration_ms: u128,
}

pub(super) struct ScenarioExecution {
    pub(super) outcome: Option<ScenarioOutcome>,
    pub(super) index_entry: ScenarioIndexEntry,
}

/// Tracks auto-verification execution state during scenario runs.
struct AutoVerificationState {
    runs_used: usize,
    total: usize,
    skipped_cache: usize,
    skipped_limit: usize,
    run_limit: usize,
}

impl AutoVerificationState {
    fn new(limit: Option<usize>) -> Self {
        Self {
            runs_used: 0,
            total: 0,
            skipped_cache: 0,
            skipped_limit: 0,
            run_limit: limit.unwrap_or(usize::MAX),
        }
    }

    fn at_limit(&self) -> bool {
        self.runs_used >= self.run_limit
    }

    fn record_run(&mut self) {
        self.runs_used += 1;
    }
}

/// Log cache state when incomplete.
fn log_incomplete_cache(previous_available: bool, has_existing_index: bool) {
    let report_state = if previous_available { "present" } else { "missing" };
    let index_status = if has_existing_index { "present" } else { "missing" };
    eprintln!(
        "note: scenario cache incomplete (report {report_state}, index {index_status}); rerunning all scenarios"
    );
}

/// Run scenarios and return an examples report snapshot.
pub fn run_scenarios(args: &RunScenariosArgs<'_>) -> Result<RunScenariosResult> {
    let plan = super::load_plan(args.scenarios_path, args.run_root)?;
    if let Some(plan_binary) = plan.binary.as_deref() {
        if plan_binary != args.binary_name {
            return Err(anyhow!(
                "scenarios plan binary {:?} does not match pack binary {:?}",
                plan_binary,
                args.binary_name
            ));
        }
    }

    let pack_root = args
        .pack_root
        .canonicalize()
        .with_context(|| format!("resolve pack root {}", args.pack_root.display()))?;

    // Pre-warm the lens flake to ensure Nix store is populated before scenario loop.
    // This moves the one-time fetch/build cost out of per-scenario execution.
    warm_lens_flake(args.lens_flake, args.verbose)?;

    let scenarios_index_path = args
        .run_root
        .join("inventory")
        .join("scenarios")
        .join("index.json");
    let mut scenarios: Vec<ScenarioSpec> = plan.scenarios.clone();
    scenarios.extend(args.extra_scenarios.iter().cloned());
    let forced_ids = normalize_forced_rerun_ids(&args.forced_rerun_scenario_ids);
    let known_scenario_ids: BTreeSet<String> = scenarios
        .iter()
        .map(|scenario| scenario.id.clone())
        .collect();
    let (forced_rerun_ids, unknown_forced_ids) =
        split_forced_rerun_ids(&forced_ids, &known_scenario_ids);
    for scenario_id in unknown_forced_ids {
        eprintln!("warning: ignored --rerun-scenario-id {scenario_id}: no matching scenario id");
    }
    let has_auto = scenarios
        .iter()
        .any(|scenario| scenario.id.starts_with(super::AUTO_VERIFY_SCENARIO_PREFIX));
    let retain_ids: BTreeSet<String> = scenarios
        .iter()
        .map(|scenario| scenario.id.clone())
        .collect();
    let index_state = load_scenario_index_state(&scenarios_index_path, &retain_ids, args.verbose);
    let has_existing_index = index_state.existing.is_some();
    let mut index_entries = index_state.entries;
    let mut index_changed = index_state.changed;

    let mut previous_outcomes = load_previous_outcomes(args.run_root, args.verbose);
    let cache_ready = previous_outcomes.available && has_existing_index;
    if args.verbose && !cache_ready {
        log_incomplete_cache(previous_outcomes.available, has_existing_index);
    }
    let mut outcomes = Vec::new();
    let mut executed_forced_rerun_ids = BTreeSet::new();

    if has_auto && args.auto_run_limit.is_some() {
        if let Some(progress) = args.auto_progress.as_ref() {
            emit_auto_progress_header(progress);
        }
    }

    let scenarios = scenarios.iter().filter(|scenario| match args.kind_filter {
        Some(kind) => scenario.kind == kind,
        None => true,
    });

    let mut auto_state = AutoVerificationState::new(args.auto_run_limit);

    for scenario in scenarios {
        let run_config = effective_scenario_config(&plan, scenario)?;
        let reportable = scenario.publish;
        let has_index_entry = index_entries.contains_key(&scenario.id);
        let has_previous_outcome =
            cache_ready && previous_outcomes.outcomes.contains_key(&scenario.id);
        let allow_index_cache = !reportable && has_index_entry;
        let is_forced_rerun = forced_rerun_ids.contains(&scenario.id);
        // Skip when we can reuse prior outcomes for reportable scenarios, or when
        // non-reportable scenarios already have indexed evidence.
        let should_run = should_run_scenario(
            args.run_mode,
            &run_config.scenario_digest,
            index_entries.get(&scenario.id),
            has_previous_outcome || allow_index_cache,
            is_forced_rerun,
        );

        let is_auto = scenario.id.starts_with(super::AUTO_VERIFY_SCENARIO_PREFIX);
        if is_auto {
            auto_state.total += 1;
        }
        if !should_run {
            if is_auto {
                auto_state.skipped_cache += 1;
            }
            if reportable {
                if let Some(outcome) = previous_outcomes.outcomes.remove(&scenario.id) {
                    outcomes.push(outcome);
                }
            }
            continue;
        }

        if is_auto && auto_state.at_limit() {
            auto_state.skipped_limit += 1;
            continue;
        }

        if args.verbose {
            eprintln!("running scenario {} {}", args.binary_name, scenario.id);
        }
        if is_forced_rerun {
            executed_forced_rerun_ids.insert(scenario.id.clone());
        }

        let run_argv0 = args.binary_name.to_string();
        let materialized_seed = if let Some(seed) = run_config.seed.as_ref() {
            let staging_root = args.staging_root.ok_or_else(|| {
                anyhow!(
                    "inline seed requires a staging root for scenario {}",
                    scenario.id
                )
            })?;
            Some(materialize_inline_seed(
                staging_root,
                args.run_root,
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
            args.run_root,
            args.lens_flake,
            &run_kv_args,
            &scenario.argv,
            &run_config.env,
        )
        .with_context(|| format!("invoke binary_lens for scenario {}", scenario.id))?;
        let duration_ms = started.elapsed().as_millis();
        let context = ScenarioRunContext {
            scenario,
            run_config: &run_config,
            run_argv0: &run_argv0,
            run_seed_dir,
            duration_ms,
        };
        let execution = if !status.success() {
            build_failed_execution(&context, &status)?
        } else {
            let after = read_runs_index(&pack_root).context("read runs index (after)")?;
            build_success_execution(
                &pack_root,
                args.staging_root,
                &context,
                &before,
                &after,
                args.verbose,
            )?
        };
        if let Some(outcome) = execution.outcome {
            outcomes.push(outcome);
        }
        index_entries.insert(scenario.id.clone(), execution.index_entry);
        index_changed = true;
        if is_auto {
            auto_state.record_run();
            if args.auto_run_limit.is_some() && auto_state.runs_used.is_multiple_of(25) {
                eprintln!("auto verification progress: {} runs executed", auto_state.runs_used);
            }
        }
    }

    write_scenario_index_if_needed(
        args.staging_root,
        index_entries,
        has_existing_index,
        index_changed,
    )?;

    if has_auto && args.auto_run_limit.is_some() && auto_state.total > 0 {
        let mut summary = format!("auto verification: ran {}", auto_state.runs_used);
        if auto_state.skipped_cache > 0 || auto_state.skipped_limit > 0 {
            summary.push_str(&format!(
                " (skipped {} cached, {} limit)",
                auto_state.skipped_cache, auto_state.skipped_limit
            ));
        }
        summary.push_str("; see bman status --json");
        eprintln!("{summary}");
    }

    let pass_count = outcomes.iter().filter(|outcome| outcome.pass).count();
    let fail_count = outcomes.len() - pass_count;
    if args.verbose {
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

    Ok(RunScenariosResult {
        report: ExamplesReport {
            schema_version: 1,
            generated_at_epoch_ms,
            binary_name: args.binary_name.to_string(),
            pack_root: display_path(&pack_root, args.display_root),
            scenarios_path: display_path(args.scenarios_path, args.display_root),
            scenario_count: outcomes.len(),
            pass_count,
            fail_count,
            run_ids,
            scenarios: outcomes,
        },
        executed_forced_rerun_scenario_ids: executed_forced_rerun_ids.into_iter().collect(),
    })
}

fn normalize_forced_rerun_ids(raw: &[String]) -> Vec<String> {
    let mut ids = raw
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn split_forced_rerun_ids(
    forced_ids: &[String],
    known_ids: &BTreeSet<String>,
) -> (BTreeSet<String>, Vec<String>) {
    let mut known = BTreeSet::new();
    let mut unknown = Vec::new();
    for scenario_id in forced_ids {
        if known_ids.contains(scenario_id) {
            known.insert(scenario_id.clone());
        } else {
            unknown.push(scenario_id.clone());
        }
    }
    (known, unknown)
}

fn emit_auto_progress_header(progress: &AutoVerificationProgress) {
    let by_kind = format_auto_remaining_by_kind(&progress.remaining_by_kind);
    match progress.remaining_total {
        Some(remaining_total) => {
            if by_kind.is_empty() {
                eprintln!("auto verification remaining: {remaining_total}");
            } else {
                eprintln!("auto verification remaining: {remaining_total} ({by_kind})");
            }
            eprintln!(
                "auto verification batch: will run up to {} new scenarios this apply (max_new_runs_per_apply={})",
                progress.max_new_runs_per_apply,
                progress.max_new_runs_per_apply
            );
            if remaining_total > progress.max_new_runs_per_apply {
                eprintln!(
                    "hint: set scenarios/plan.json.verification.policy.max_new_runs_per_apply >= {remaining_total} to finish in one apply"
                );
            }
        }
        None => {
            eprintln!("auto verification remaining: unknown (plan missing verification_plan)");
            eprintln!(
                "auto verification batch: will run up to {} new scenarios this apply (max_new_runs_per_apply={})",
                progress.max_new_runs_per_apply,
                progress.max_new_runs_per_apply
            );
        }
    }
}

fn format_auto_remaining_by_kind(groups: &[AutoVerificationKindProgress]) -> String {
    let parts: Vec<String> = groups
        .iter()
        .map(|group| format!("{} {}", group.kind, group.remaining_count))
        .collect();
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_rerun_ids_are_trimmed_sorted_and_deduped() {
        let ids = vec![
            " verify_b ".to_string(),
            "".to_string(),
            "verify_a".to_string(),
            "verify_b".to_string(),
        ];
        assert_eq!(
            normalize_forced_rerun_ids(&ids),
            vec!["verify_a".to_string(), "verify_b".to_string()]
        );
    }

    #[test]
    fn split_forced_rerun_ids_reports_unknown_ids() {
        let forced_ids = vec![
            "verify_a".to_string(),
            "unknown".to_string(),
            "verify_b".to_string(),
        ];
        let known_ids = BTreeSet::from(["verify_a".to_string(), "verify_b".to_string()]);
        let (known, unknown) = split_forced_rerun_ids(&forced_ids, &known_ids);
        assert_eq!(
            known,
            BTreeSet::from(["verify_a".to_string(), "verify_b".to_string()])
        );
        assert_eq!(unknown, vec!["unknown".to_string()]);
    }
}
