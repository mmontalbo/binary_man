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
    pub verbose: bool,
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

/// Run scenarios and return an examples report snapshot.
pub fn run_scenarios(args: &RunScenariosArgs<'_>) -> Result<ExamplesReport> {
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

    let scenarios_index_path = args
        .run_root
        .join("inventory")
        .join("scenarios")
        .join("index.json");
    let index_state = load_scenario_index_state(&scenarios_index_path, &plan, args.verbose);
    let has_existing_index = index_state.existing.is_some();
    let mut index_entries = index_state.entries;
    let mut index_changed = index_state.changed;

    let mut previous_outcomes = load_previous_outcomes(args.run_root, args.verbose);
    let cache_ready = previous_outcomes.available && has_existing_index;
    if args.verbose && !cache_ready {
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

    let scenarios = plan
        .scenarios
        .iter()
        .filter(|scenario| match args.kind_filter {
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
            args.run_mode,
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

        if args.verbose {
            eprintln!("running scenario {} {}", args.binary_name, scenario.id);
        }

        let run_argv0 = args.binary_name.to_string();
        let materialized_seed = if let Some(seed) = scenario.seed.as_ref() {
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
    }

    write_scenario_index_if_needed(
        args.staging_root,
        index_entries,
        has_existing_index,
        index_changed,
    )?;

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

    Ok(ExamplesReport {
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
    })
}
