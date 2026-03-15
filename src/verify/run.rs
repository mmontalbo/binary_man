//! Main verification loop.
//!
//! This module implements the core verification loop:
//! bootstrap → [gather pending → lm_call → apply actions → save]* → done

use super::apply::{apply_action, merge_test_result, run_test_scenario};
use super::bootstrap::bootstrap;
use super::lm::{log_prompt, log_response, parse_lm_response, LmAction, LmResponse};
use super::prompt::{build_incremental_prompt, build_prompt, build_retry_prompt};
use super::types::{Attempt, Outcome, State, Status, SurfaceCategory, VerifiedSeed};
use super::validate::{normalize_action, validate_action};
use crate::cli::ContextMode;
use crate::lm::{create_plugin, LmConfig, LmPlugin};
use anyhow::{anyhow, Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Maximum verification attempts per surface before auto-exhausting.
const MAX_ATTEMPTS: usize = 5;

/// Maximum surfaces to include in each LM batch.
const BATCH_SIZE: usize = 5;

/// Maximum cycles to spend retrying critique-demoted surfaces.
const CRITIQUE_RETRY_CYCLES: u32 = 20;

/// Result of a verification run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResult {
    /// All surfaces verified or excluded.
    Complete,
    /// Reached max_cycles limit.
    HitMaxCycles,
}

/// Default LM timeout in seconds.
const LM_TIMEOUT_SECS: u64 = 120;

/// Maximum retry attempts for LM calls.
const MAX_LM_RETRIES: usize = 3;


/// Shared coordination state for work-stealing parallel sessions.
struct SharedProgress {
    /// Surfaces resolved (verified or excluded) by any session.
    resolved: HashSet<String>,
    /// Surfaces currently being processed by a session.
    in_progress: HashSet<String>,
    /// Total attempt count per surface across all sessions.
    attempt_counts: HashMap<String, usize>,
}

/// Compute scheduling priority for a surface category.
///
/// Lower values = higher priority. Easy/fast categories run first.
fn category_priority(category: &SurfaceCategory, state: &State) -> usize {
    match category {
        SurfaceCategory::FormatChange => 0,
        SurfaceCategory::General => 1,
        SurfaceCategory::MetaEffect => 2,
        SurfaceCategory::ValueRequired => 3,
        SurfaceCategory::Modifier { base } => {
            if state
                .entries
                .iter()
                .any(|b| b.id == *base && matches!(b.status, Status::Verified))
            {
                4
            } else {
                99
            }
        }
        SurfaceCategory::TtyDependent => 5,
    }
}


/// Result from a parallel session, containing updates to merge.
#[derive(Debug)]
struct SessionUpdate {
    surface_id: String,
    status: Status,
    attempts: Vec<Attempt>,
    retried: bool,
}

/// Run the verification loop.
///
/// This is the main entry point for the simplified verification workflow.
#[allow(clippy::too_many_arguments)]
pub fn run(
    binary: &str,
    context_argv: &[String],
    pack_path: &Path,
    max_cycles: u32,
    lm_config: &LmConfig,
    verbose: bool,
    context_mode: ContextMode,
    session_size: usize,
    parallel_sessions: bool,
    with_pty: bool,
) -> Result<RunResult> {
    // Create pack directory structure
    fs::create_dir_all(pack_path.join("evidence")).context("create evidence directory")?;
    fs::create_dir_all(pack_path.join("lm_log")).context("create lm_log directory")?;

    // Load or bootstrap
    let mut state = if pack_path.join("state.json").exists() {
        if verbose {
            eprintln!("Loading existing state from {}", pack_path.display());
        }
        State::load(pack_path)?
    } else {
        if verbose {
            eprintln!("Bootstrapping new state for {}", binary);
        }
        let state = bootstrap(binary, context_argv, lm_config)?;
        if verbose {
            eprintln!("Discovered {} surfaces", state.entries.len());
        }
        state
    };

    // Save initial state
    state.save(pack_path)?;

    // Determine session chunks
    let all_surface_ids: Vec<String> = state.entries.iter().map(|e| e.id.clone()).collect();
    let chunks: Vec<Vec<String>> = if session_size > 0 {
        all_surface_ids
            .chunks(session_size)
            .map(|c| c.to_vec())
            .collect()
    } else {
        vec![all_surface_ids]
    };

    let num_sessions = chunks.len();

    // Choose parallel or sequential execution
    let result = if parallel_sessions && num_sessions > 1 {
        run_parallel_sessions(
            pack_path,
            &mut state,
            chunks,
            max_cycles,
            lm_config,
            verbose,
            context_mode,
            with_pty,
        )
    } else {
        run_sequential_sessions(
            pack_path,
            &mut state,
            chunks,
            max_cycles,
            lm_config,
            verbose,
            context_mode,
            with_pty,
        )
    };

    // Critique pass: validate verified surfaces, demoting false positives back to Pending.
    // Demoted surfaces get critique_feedback attached so the LM can adjust.
    critique_verified_surfaces(&mut state, pack_path, lm_config, verbose)?;

    // Retry critique-demoted surfaces with a single focused session.
    // These surfaces have critique_feedback set, which the prompt will include.
    let demoted_ids: Vec<String> = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Pending) && e.critique_feedback.is_some())
        .map(|e| e.id.clone())
        .collect();

    if !demoted_ids.is_empty() {
        let remaining_cycles = max_cycles.saturating_sub(state.cycle) + CRITIQUE_RETRY_CYCLES;
        if remaining_cycles > 0 {
            if verbose {
                eprintln!(
                    "\nRetrying {} critique-demoted surface(s) with feedback...",
                    demoted_ids.len()
                );
            }

            let retry_max = state.cycle + remaining_cycles;
            let mut retry_plugin = create_plugin(lm_config);
            if let Ok(()) = retry_plugin.init() {
                let _ = run_chunk(
                    pack_path,
                    &mut *retry_plugin,
                    &mut state,
                    &demoted_ids,
                    retry_max,
                    verbose,
                    context_mode,
                    with_pty,
                    None,
                );
                retry_plugin.shutdown().ok();
            }
        }
    }

    // Mark remaining Pending surfaces as Excluded
    let mut final_excluded = 0;
    for entry in &mut state.entries {
        if matches!(entry.status, Status::Pending) {
            let reason = if entry.attempts.is_empty() {
                "Never attempted".to_string()
            } else if entry.critique_feedback.is_some() {
                "Critique-demoted, not re-verified".to_string()
            } else {
                format!("Exhausted after {} attempts", entry.attempts.len())
            };
            entry.status = Status::Excluded { reason };
            final_excluded += 1;
        }
    }
    if final_excluded > 0 && verbose {
        eprintln!(
            "\nMarked {} remaining pending surface(s) as excluded",
            final_excluded,
        );
    }

    state.save(pack_path)?;

    // Determine final result
    let all_resolved = state
        .entries
        .iter()
        .all(|e| !matches!(e.status, Status::Pending));
    if all_resolved {
        Ok(RunResult::Complete)
    } else {
        Ok(result)
    }
}

/// Run sessions sequentially (original behavior).
#[allow(clippy::too_many_arguments)]
fn run_sequential_sessions(
    pack_path: &Path,
    state: &mut State,
    chunks: Vec<Vec<String>>,
    max_cycles: u32,
    lm_config: &LmConfig,
    verbose: bool,
    context_mode: ContextMode,
    with_pty: bool,
) -> RunResult {
    let num_sessions = chunks.len();
    let mut result = RunResult::Complete;

    for (session_idx, chunk_ids) in chunks.into_iter().enumerate() {
        // Skip chunks where all surfaces are already resolved
        let pending_in_chunk = state
            .entries
            .iter()
            .filter(|e| chunk_ids.contains(&e.id) && matches!(e.status, Status::Pending))
            .count();

        if pending_in_chunk == 0 {
            if verbose && num_sessions > 1 {
                eprintln!(
                    "\nSession {}/{}: skipping (all {} surfaces resolved)",
                    session_idx + 1,
                    num_sessions,
                    chunk_ids.len()
                );
            }
            continue;
        }

        if verbose && num_sessions > 1 {
            eprintln!(
                "\nSession {}/{}: processing {} surfaces ({} pending)",
                session_idx + 1,
                num_sessions,
                chunk_ids.len(),
                pending_in_chunk
            );
        }

        // Create fresh LM session for this chunk
        let mut plugin = create_plugin(lm_config);
        if let Err(e) = plugin.init() {
            eprintln!("Failed to initialize LM plugin: {}", e);
            return RunResult::HitMaxCycles;
        }

        // Run verification on this chunk
        result = match run_chunk(
            pack_path,
            &mut *plugin,
            state,
            &chunk_ids,
            max_cycles,
            verbose,
            context_mode,
            with_pty,
            None, // No prior attempts for initial run
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Session error: {}", e);
                RunResult::HitMaxCycles
            }
        };

        plugin.shutdown().ok();
        let _ = state.save(pack_path);

        // Stop if we hit max cycles
        if matches!(result, RunResult::HitMaxCycles) {
            break;
        }
    }

    result
}

/// Run sessions in parallel with work-stealing.
///
/// Instead of giving each session a fixed partition of surfaces, all sessions
/// pull from a shared work queue. This ensures every surface gets attempted
/// regardless of how quickly individual sessions burn through their work.
#[allow(clippy::too_many_arguments)]
fn run_parallel_sessions(
    pack_path: &Path,
    state: &mut State,
    chunks: Vec<Vec<String>>,
    max_cycles: u32,
    lm_config: &LmConfig,
    verbose: bool,
    context_mode: ContextMode,
    with_pty: bool,
) -> RunResult {
    let num_sessions = chunks.len();

    if verbose {
        eprintln!(
            "\nRunning {} sessions with work-stealing ({} surfaces, {} max cycles)...",
            num_sessions,
            state.entries.len(),
            max_cycles
        );
    }

    // Shared coordination between sessions
    let shared = Arc::new(Mutex::new(SharedProgress {
        resolved: HashSet::new(),
        in_progress: HashSet::new(),
        attempt_counts: HashMap::new(),
    }));
    let global_cycle = Arc::new(AtomicU32::new(state.cycle));
    let initial_seed_count = state.seed_bank.len();

    // Run all sessions in parallel
    let session_results: Vec<(Vec<SessionUpdate>, Vec<VerifiedSeed>)> = thread::scope(|s| {
        let handles: Vec<_> = (0..num_sessions)
            .map(|session_idx| {
                let mut session_state = state.clone();
                let shared = Arc::clone(&shared);
                let global_cycle = Arc::clone(&global_cycle);

                s.spawn(move || {
                    if verbose {
                        eprintln!("  Session {}/{}: started", session_idx + 1, num_sessions);
                    }

                    let mut plugin = create_plugin(lm_config);
                    if let Err(e) = plugin.init() {
                        eprintln!(
                            "  Session {} failed to init LM: {}",
                            session_idx + 1,
                            e
                        );
                        return (vec![], vec![]);
                    }

                    let mut last_response: Option<LmResponse> = None;

                    loop {
                        // Claim next batch from shared queue
                        let cycle = global_cycle.fetch_add(1, Ordering::SeqCst) + 1;
                        if cycle > max_cycles {
                            // Over budget — put it back
                            global_cycle.fetch_sub(1, Ordering::SeqCst);
                            if verbose {
                                eprintln!(
                                    "  S{}: hit max cycles ({})",
                                    session_idx + 1,
                                    max_cycles
                                );
                            }
                            break;
                        }

                        let pending_ids = {
                            let mut progress = shared.lock().unwrap();

                            // Find pending surfaces not resolved or in-progress
                            let mut candidates: Vec<(usize, String)> = session_state
                                .entries
                                .iter()
                                .filter(|e| {
                                    matches!(e.status, Status::Pending)
                                        && !progress.resolved.contains(&e.id)
                                        && !progress.in_progress.contains(&e.id)
                                })
                                .map(|e| {
                                    (
                                        category_priority(&e.category, &session_state),
                                        e.id.clone(),
                                    )
                                })
                                .collect();
                            candidates.sort_by_key(|(p, _)| *p);

                            let batch: Vec<String> = candidates
                                .into_iter()
                                .take(BATCH_SIZE)
                                .map(|(_, id)| id)
                                .collect();

                            if batch.is_empty() {
                                // Nothing left — give back the cycle
                                global_cycle.fetch_sub(1, Ordering::SeqCst);
                                break;
                            }

                            // Mark as in-progress
                            for id in &batch {
                                progress.in_progress.insert(id.clone());
                            }

                            batch
                        };

                        session_state.cycle = cycle;

                        if verbose {
                            eprintln!(
                                "  S{} cycle {}: {}",
                                session_idx + 1,
                                cycle,
                                pending_ids.join(", ")
                            );
                        }

                        // Execute one cycle
                        let cycle_ok = execute_cycle(
                            pack_path,
                            &mut *plugin,
                            &mut session_state,
                            &pending_ids,
                            verbose,
                            context_mode,
                            with_pty,
                            false,
                            None,
                            &mut last_response,
                        )
                        .is_ok();

                        // Publish results back to shared progress
                        {
                            let mut progress = shared.lock().unwrap();
                            for id in &pending_ids {
                                progress.in_progress.remove(id);

                                // Track total attempts across sessions
                                if let Some(entry) =
                                    session_state.entries.iter().find(|e| &e.id == id)
                                {
                                    let new_attempts = entry.attempts.len();
                                    let total = progress
                                        .attempt_counts
                                        .entry(id.clone())
                                        .or_insert(0);
                                    *total = (*total).max(new_attempts);

                                    // Mark resolved if verified, excluded, or exhausted
                                    if !matches!(entry.status, Status::Pending)
                                        || *total >= MAX_ATTEMPTS
                                    {
                                        progress.resolved.insert(id.clone());
                                    }
                                }
                            }
                        }

                        if !cycle_ok {
                            break;
                        }
                    }

                    plugin.shutdown().ok();

                    if verbose {
                        let verified = session_state
                            .entries
                            .iter()
                            .filter(|e| {
                                matches!(e.status, Status::Verified) && !e.attempts.is_empty()
                            })
                            .count();
                        eprintln!(
                            "  Session {}/{}: done ({} verified)",
                            session_idx + 1,
                            num_sessions,
                            verified
                        );
                    }

                    // Collect updates only for surfaces this session worked on
                    let updates: Vec<SessionUpdate> = session_state
                        .entries
                        .iter()
                        .filter(|e| !e.attempts.is_empty())
                        .map(|e| SessionUpdate {
                            surface_id: e.id.clone(),
                            status: e.status.clone(),
                            attempts: e.attempts.clone(),
                            retried: e.retried,
                        })
                        .collect();

                    let new_seeds: Vec<VerifiedSeed> = session_state
                        .seed_bank
                        .into_iter()
                        .skip(initial_seed_count)
                        .collect();

                    (updates, new_seeds)
                })
            })
            .collect();

        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });

    // Merge updates back into main state.
    // Prefer updates that resolved the surface over those that didn't.
    state.cycle = global_cycle.load(Ordering::SeqCst);
    for (updates, new_seeds) in session_results {
        for update in updates {
            if let Some(entry) = state.entries.iter_mut().find(|e| e.id == update.surface_id) {
                let update_resolved = !matches!(update.status, Status::Pending);
                let current_resolved = !matches!(entry.status, Status::Pending);

                if update_resolved && !current_resolved {
                    // This session resolved it — use its result
                    entry.status = update.status;
                    entry.attempts = update.attempts;
                    entry.retried = update.retried;
                } else if !current_resolved && !update_resolved {
                    // Neither resolved — merge attempts (keep the one with more)
                    if update.attempts.len() > entry.attempts.len() {
                        entry.attempts = update.attempts;
                    }
                }
                // If current is already resolved, keep it (first resolver wins)
            }
        }
        for seed in new_seeds {
            if !state
                .seed_bank
                .iter()
                .any(|s| s.surface_id == seed.surface_id)
            {
                state.seed_bank.push(seed);
            }
        }
    }

    // Mark exhausted surfaces (tracked by shared progress)
    {
        let progress = shared.lock().unwrap();
        for (surface_id, total_attempts) in &progress.attempt_counts {
            if *total_attempts >= MAX_ATTEMPTS {
                if let Some(entry) = state.entries.iter_mut().find(|e| e.id == *surface_id) {
                    if matches!(entry.status, Status::Pending) {
                        entry.status = Status::Excluded {
                            reason: format!("Exhausted after {} attempts", total_attempts),
                        };
                        if verbose {
                            eprintln!(
                                "Auto-excluded {} (exhausted after {} attempts across sessions)",
                                surface_id, total_attempts
                            );
                        }
                    }
                }
            }
        }
    }

    let _ = state.save(pack_path);

    if verbose {
        let verified = state
            .entries
            .iter()
            .filter(|e| matches!(e.status, Status::Verified))
            .count();
        let excluded = state
            .entries
            .iter()
            .filter(|e| matches!(e.status, Status::Excluded { .. }))
            .count();
        let pending = state
            .entries
            .iter()
            .filter(|e| matches!(e.status, Status::Pending))
            .count();
        eprintln!(
            "\nWork-stealing sessions complete: {} verified, {} excluded, {} pending",
            verified, excluded, pending
        );
    }

    let all_resolved = state
        .entries
        .iter()
        .all(|e| !matches!(e.status, Status::Pending));
    if all_resolved {
        RunResult::Complete
    } else {
        RunResult::HitMaxCycles
    }
}

/// Invoke LM with retry logic.
///
/// Retries up to MAX_LM_RETRIES times, resetting the plugin on the second-to-last attempt.
fn invoke_lm_with_retry(plugin: &mut dyn LmPlugin, prompt: &str, verbose: bool) -> Result<String> {
    let timeout = Duration::from_secs(LM_TIMEOUT_SECS);

    for attempt in 1..=MAX_LM_RETRIES {
        match plugin.prompt(prompt, timeout) {
            Ok(response) => return Ok(response),
            Err(e) => {
                if attempt < MAX_LM_RETRIES {
                    if verbose {
                        eprintln!("  LM retry {}/{} ({})", attempt, MAX_LM_RETRIES - 1, e);
                    }
                    if attempt == MAX_LM_RETRIES - 1 {
                        if verbose {
                            eprintln!("  Resetting LM session...");
                        }
                        plugin.reset().ok();
                    }
                } else {
                    return Err(e);
                }
            }
        }
    }
    Err(anyhow!("LM invocation failed after retries"))
}

/// Execute a single verification cycle for the given pending surfaces.
///
/// Builds the prompt, invokes the LM, parses the response, and executes
/// the resulting test actions. Updates `state` in place.
#[allow(clippy::too_many_arguments)]
fn execute_cycle(
    pack_path: &Path,
    plugin: &mut dyn LmPlugin,
    state: &mut State,
    pending_ids: &[String],
    verbose: bool,
    context_mode: ContextMode,
    with_pty: bool,
    _is_retry: bool,
    prior_attempts: Option<&std::collections::HashMap<String, Vec<Attempt>>>,
    last_response: &mut Option<LmResponse>,
) -> Result<()> {
    // Resolve auto mode based on plugin type
    let effective_mode = match context_mode {
        ContextMode::Auto if plugin.is_stateful() => ContextMode::Incremental,
        ContextMode::Auto => ContextMode::Full,
        other => other,
    };

    // Build prompt based on effective context mode
    let prompt = match effective_mode {
        ContextMode::Full | ContextMode::Reset => {
            if let Some(prior) = prior_attempts {
                build_retry_prompt(state, pending_ids, prior)
            } else {
                build_prompt(state, pending_ids)
            }
        }
        ContextMode::Incremental if plugin.is_stateful() && last_response.is_some() => {
            build_incremental_prompt(state, pending_ids, last_response.as_ref())
        }
        ContextMode::Incremental | ContextMode::Auto => {
            if let Some(prior) = prior_attempts {
                build_retry_prompt(state, pending_ids, prior)
            } else {
                build_prompt(state, pending_ids)
            }
        }
    };
    log_prompt(pack_path, state.cycle, &prompt)?;

    if verbose {
        eprintln!("  Invoking LM...");
    }
    let response_text = invoke_lm_with_retry(plugin, &prompt, verbose)?;

    // Parse response — on failure, reset session immediately rather than
    // sending reminders into a poisoned conversation.
    let response = match parse_lm_response(&response_text) {
        Ok(r) => r,
        Err(_e) => {
            if verbose {
                eprintln!("  Parse error, resetting LM session");
            }
            plugin.reset().ok();
            *last_response = None;
            return Ok(());
        }
    };
    log_response(pack_path, state.cycle, &response)?;

    // Reset mode: reset plugin after each successful call
    if matches!(effective_mode, ContextMode::Reset) && plugin.is_stateful() {
        if verbose {
            eprintln!("  Resetting LM session (context_mode=reset)");
        }
        plugin.reset()?;
    }

    // Handle empty response
    if response.actions.is_empty() {
        if verbose {
            eprintln!("  LM returned no actions, resetting session");
        }
        plugin.reset().ok();
        *last_response = None;
        return Ok(());
    }

    if verbose {
        eprintln!("  LM returned {} action(s)", response.actions.len());
    }

    // Save response for incremental mode before consuming actions
    let response_for_tracking = response.clone();

    // Partition and validate actions
    let mut baselines = Vec::new();
    let mut tests = Vec::new();

    for action in response.actions {
        // Normalize action to handle --option=value and -Uvalue formats
        let action = normalize_action(action, state);

        if let Err(e) = validate_action(&action, state) {
            eprintln!("  Skipping invalid action: {}", e);
            continue;
        }
        match &action {
            LmAction::SetBaseline { .. } => baselines.push(action),
            LmAction::Test { .. } => tests.push(action),
        }
    }

    // 1. Apply baselines first (must complete before tests)
    for action in baselines {
        if verbose {
            eprintln!("  Applying: {}", format_action_desc(&action));
        }
        if let Err(e) = apply_action(state, pack_path, action) {
            eprintln!("  Action failed: {}", e);
        }
    }

    // 2. Run tests in parallel
    if !tests.is_empty() {
        if verbose {
            eprintln!("  Running {} test(s) in parallel...", tests.len());
        }

        // Extract test parameters for parallel execution.
        // Auto-enable PTY for TTY-dependent surfaces.
        let test_params: Vec<_> = tests
            .into_iter()
            .filter_map(|action| {
                if let LmAction::Test {
                    surface_id,
                    extra_args,
                    seed,
                    prediction,
                } = action
                {
                    let surface_pty = with_pty
                        || state.entries.iter().any(|e| {
                            e.id == surface_id
                                && matches!(e.category, SurfaceCategory::TtyDependent)
                        });
                    Some((surface_id, extra_args, seed, prediction, surface_pty))
                } else {
                    None
                }
            })
            .collect();

        // Run scenarios in parallel using thread::scope
        let results: Vec<_> = thread::scope(|s| {
            let handles: Vec<_> = test_params
                .into_iter()
                .map(|(surface_id, extra_args, seed, prediction, surface_pty)| {
                    let binary = &state.binary;
                    let context_argv = &state.context_argv;
                    let cycle = state.cycle;
                    s.spawn(move || {
                        run_test_scenario(
                            pack_path,
                            binary,
                            context_argv,
                            cycle,
                            &surface_id,
                            extra_args,
                            seed,
                            surface_pty,
                            prediction,
                        )
                    })
                })
                .collect();

            handles
                .into_iter()
                .filter_map(|h| match h.join() {
                    Ok(Ok(result)) => Some(result),
                    Ok(Err(e)) => {
                        eprintln!("  Test scenario failed: {}", e);
                        None
                    }
                    Err(_) => {
                        eprintln!("  Test thread panicked");
                        None
                    }
                })
                .collect()
        });

        // Merge results into state (sequential, fast)
        for result in results {
            if verbose {
                eprintln!(
                    "  {} → {:?}",
                    result.surface_id,
                    match &result.outcome {
                        Outcome::Verified { diff_kind } =>
                            format!("Verified ({:?})", diff_kind),
                        Outcome::OutputsEqual => "OutputsEqual".to_string(),
                        Outcome::SetupFailed { .. } => "SetupFailed".to_string(),
                        Outcome::Crashed { .. } => "Crashed".to_string(),
                        Outcome::ExecutionError { .. } => "ExecutionError".to_string(),
                        Outcome::OptionError { .. } => "OptionError".to_string(),
                    }
                );
            }
            merge_test_result(state, result);
        }
    }

    // Track response for incremental mode
    *last_response = Some(response_for_tracking);

    Ok(())
}

/// Run the verification loop for a chunk of surfaces.
///
/// Only processes surfaces whose IDs are in `chunk_ids`.
/// If `prior_attempts` is Some, uses retry prompts with attempt history.
/// Otherwise, uses standard prompts.
#[allow(clippy::too_many_arguments)]
fn run_chunk(
    pack_path: &Path,
    plugin: &mut dyn LmPlugin,
    state: &mut State,
    chunk_ids: &[String],
    max_cycles: u32,
    verbose: bool,
    context_mode: ContextMode,
    with_pty: bool,
    prior_attempts: Option<&std::collections::HashMap<String, Vec<Attempt>>>,
) -> Result<RunResult> {
    let is_retry = prior_attempts.is_some();
    let mut last_response: Option<LmResponse> = None;

    loop {
        if state.cycle >= max_cycles {
            if verbose {
                eprintln!("Hit max cycles limit ({})", max_cycles);
            }
            state.save(pack_path)?;
            return Ok(RunResult::HitMaxCycles);
        }

        // Auto-exhaust surfaces over attempt limit
        for entry in &mut state.entries {
            if chunk_ids.contains(&entry.id)
                && matches!(entry.status, Status::Pending)
                && entry.attempts.len() >= MAX_ATTEMPTS
            {
                entry.status = Status::Excluded {
                    reason: format!("Exhausted after {} attempts", MAX_ATTEMPTS),
                };
                if verbose {
                    eprintln!("Auto-excluded {} (exhausted attempts)", entry.id);
                }
            }
        }

        // Find pending targets, sorted by category priority
        let mut all_pending: Vec<(usize, String)> = state
            .entries
            .iter()
            .filter(|e| chunk_ids.contains(&e.id) && matches!(e.status, Status::Pending))
            .map(|e| (category_priority(&e.category, state), e.id.clone()))
            .collect();
        all_pending.sort_by_key(|(p, _)| *p);

        // Only reduce batch size during explicit retry passes
        let batch_size = if is_retry { 2 } else { BATCH_SIZE };

        let pending_ids: Vec<String> = all_pending
            .into_iter()
            .take(batch_size)
            .map(|(_, id)| id)
            .collect();

        if pending_ids.is_empty() {
            if verbose {
                eprintln!(
                    "{} - complete!",
                    if is_retry {
                        "All retry surfaces processed"
                    } else {
                        "All surfaces processed"
                    }
                );
            }
            state.save(pack_path)?;
            return Ok(RunResult::Complete);
        }

        state.cycle += 1;

        if verbose {
            eprintln!(
                "{} {}: processing {} surface(s): {}",
                if is_retry { "Retry cycle" } else { "Cycle" },
                state.cycle,
                pending_ids.len(),
                pending_ids.join(", ")
            );
        }

        execute_cycle(
            pack_path,
            plugin,
            state,
            &pending_ids,
            verbose,
            context_mode,
            with_pty,
            is_retry,
            prior_attempts,
            &mut last_response,
        )?;

        state.save(pack_path)?;

        // Report progress
        if verbose {
            let verified = state
                .entries
                .iter()
                .filter(|e| chunk_ids.contains(&e.id) && matches!(e.status, Status::Verified))
                .count();
            let excluded = state
                .entries
                .iter()
                .filter(|e| {
                    chunk_ids.contains(&e.id) && matches!(e.status, Status::Excluded { .. })
                })
                .count();
            let pending = state
                .entries
                .iter()
                .filter(|e| chunk_ids.contains(&e.id) && matches!(e.status, Status::Pending))
                .count();
            eprintln!(
                "  Progress: {}/{} verified, {} excluded, {} pending",
                verified,
                chunk_ids.len(),
                excluded,
                pending
            );
        }
    }
}

/// Maximum surfaces to include in each critique batch.
const CRITIQUE_BATCH_SIZE: usize = 10;

/// Critique verified surfaces to validate they demonstrate documented behavior.
///
/// This pass reviews all Verified surfaces and can:
/// - ACCEPT: Confirm the surface is correctly verified
/// - DEMOTE: Return surface to Pending for retry (outputs differed but didn't demonstrate behavior)
///
/// Batches are processed in parallel for faster throughput.
fn critique_verified_surfaces(
    state: &mut State,
    pack_path: &Path,
    lm_config: &LmConfig,
    verbose: bool,
) -> Result<()> {
    // Collect ALL verified surfaces for critique - predictions inform but don't bypass
    let verified_ids: Vec<String> = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Verified))
        .map(|e| e.id.clone())
        .collect();

    if verified_ids.is_empty() {
        if verbose {
            eprintln!("\nNo surfaces need critique");
        }
        state.save(pack_path)?;
        return Ok(());
    }

    if verbose {
        eprintln!(
            "\nCritique pass: reviewing {} verified surface(s) in parallel...",
            verified_ids.len()
        );
    }

    // Prepare batches with their prompts (needs state access, so done before parallel section)
    let batches: Vec<(Vec<String>, String)> = verified_ids
        .chunks(CRITIQUE_BATCH_SIZE)
        .map(|batch| {
            let batch_ids: Vec<String> = batch.to_vec();
            let prompt = build_critique_prompt(state, &batch_ids, pack_path);
            (batch_ids, prompt)
        })
        .collect();

    // Process batches in parallel
    let all_judgments: Vec<Vec<(String, CritiqueAction)>> = thread::scope(|s| {
        let handles: Vec<_> = batches
            .into_iter()
            .map(|(batch_ids, prompt)| {
                s.spawn(move || -> Vec<(String, CritiqueAction)> {
                    // Create fresh LM session for this batch
                    let mut plugin = create_plugin(lm_config);
                    if let Err(e) = plugin.init() {
                        if verbose {
                            eprintln!("  Critique batch init failed: {}", e);
                        }
                        return vec![];
                    }

                    // Invoke LM
                    let response_text = match invoke_lm_with_retry(&mut *plugin, &prompt, verbose) {
                        Ok(text) => text,
                        Err(e) => {
                            if verbose {
                                eprintln!(
                                    "  Critique LM failed for batch {:?}: {}",
                                    &batch_ids[..batch_ids.len().min(3)],
                                    e
                                );
                            }
                            plugin.shutdown().ok();
                            return vec![];
                        }
                    };

                    plugin.shutdown().ok();

                    // Parse critique response
                    parse_critique_response(&response_text)
                })
            })
            .collect();

        // Collect results from all threads
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });

    // Apply all judgments sequentially
    let mut demoted_count = 0;
    for judgments in all_judgments {
        for (surface_id, action) in judgments {
            if let Some(entry) = state.entries.iter_mut().find(|e| e.id == surface_id) {
                match action {
                    CritiqueAction::Accept => {
                        if verbose {
                            eprintln!("  {} → ACCEPT", surface_id);
                        }
                    }
                    CritiqueAction::Demote { reason } => {
                        // Set back to Pending with critique feedback.
                        // Keep attempts intact so the LM sees what was tried.
                        entry.status = Status::Pending;
                        entry.critique_feedback = Some(reason.clone());
                        demoted_count += 1;
                        if verbose {
                            eprintln!("  {} → DEMOTE ({})", surface_id, reason);
                        }
                    }
                }
            }
        }
    }

    if verbose {
        eprintln!(
            "Critique complete: {} demoted, {} confirmed",
            demoted_count,
            verified_ids.len() - demoted_count
        );
    }

    state.save(pack_path)?;
    Ok(())
}

/// Action from critique LM.
#[derive(Debug, Clone)]
enum CritiqueAction {
    /// Surface correctly demonstrates documented behavior.
    Accept,
    /// Surface should be retried - outputs differed but didn't demonstrate behavior.
    Demote { reason: String },
}

/// Maximum chars for each output in critique prompt.
const CRITIQUE_OUTPUT_MAX_LEN: usize = 1500;

/// Build a critique prompt for a batch of verified surfaces.
///
/// Reads full evidence files to provide better context than truncated previews.
fn build_critique_prompt(state: &State, surface_ids: &[String], pack_path: &Path) -> String {
    let mut prompt = String::new();

    prompt.push_str("# Critique Task\n\n");
    prompt.push_str("Review these verified CLI option tests. Each was marked 'verified' because its output differed from the control run.\n\n");
    prompt.push_str("Your job: Determine if the output difference actually demonstrates the documented behavior.\n\n");
    prompt.push_str("## Actions\n\n");
    prompt.push_str("- **ACCEPT**: The diff clearly shows the option working as documented\n");
    prompt.push_str("- **DEMOTE**: The diff exists but doesn't demonstrate the behavior (e.g., error message, unrelated change)\n\n");

    prompt.push_str("## Surfaces to Review\n\n");

    for surface_id in surface_ids {
        if let Some(entry) = state.entries.iter().find(|e| e.id == *surface_id) {
            prompt.push_str(&format!("### {}\n\n", surface_id));
            prompt.push_str(&format!("**Description**: {}\n\n", entry.description));

            // Include the most recent attempt's evidence
            if let Some(attempt) = entry.attempts.last() {
                prompt.push_str(&format!("**Args**: {:?}\n\n", attempt.args));

                // Try to read full evidence files for better context
                let evidence = read_evidence_outputs(pack_path, &attempt.evidence_path);

                // Show exit code difference if present
                if evidence.control_exit_code != evidence.option_exit_code {
                    prompt.push_str(&format!(
                        "**Exit codes**: control={:?}, option={:?}\n\n",
                        evidence.control_exit_code, evidence.option_exit_code
                    ));
                }

                // Show unified diff if we have both outputs
                if !evidence.control_stdout.is_empty() && !evidence.option_stdout.is_empty() {
                    let diff =
                        compute_unified_diff(&evidence.control_stdout, &evidence.option_stdout);
                    if !diff.is_empty() {
                        prompt.push_str("**Diff (control vs option)**:\n```diff\n");
                        prompt.push_str(&truncate_string(&diff, CRITIQUE_OUTPUT_MAX_LEN));
                        prompt.push_str("\n```\n\n");
                    }
                }

                // Also show raw outputs for context
                if !evidence.control_stdout.is_empty() {
                    prompt.push_str("**Control stdout** (truncated):\n```\n");
                    prompt.push_str(&truncate_string(&evidence.control_stdout, 800));
                    prompt.push_str("\n```\n\n");
                }
                if !evidence.option_stdout.is_empty() {
                    prompt.push_str("**Option stdout** (truncated):\n```\n");
                    prompt.push_str(&truncate_string(&evidence.option_stdout, 800));
                    prompt.push_str("\n```\n\n");
                }
                if !evidence.option_stderr.is_empty() {
                    prompt.push_str("**Option stderr**:\n```\n");
                    prompt.push_str(&truncate_string(&evidence.option_stderr, 400));
                    prompt.push_str("\n```\n\n");
                }

                prompt.push_str(&format!("**Outcome**: {:?}\n\n", attempt.outcome));

                // Include prediction info to help inform critique
                match attempt.prediction_matched {
                    Some(true) => {
                        prompt.push_str("**Prediction**: MATCHED ✓ (LM predicted this behavior, recommend ACCEPT)\n\n");
                    }
                    Some(false) => {
                        prompt.push_str("**Prediction**: FAILED ✗ (LM predicted different behavior, recommend DEMOTE)\n\n");
                    }
                    None => {
                        prompt.push_str("**Prediction**: None (no prediction made)\n\n");
                    }
                }
            }
        }
    }

    prompt.push_str("## Response Format\n\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"judgments\": [\n");
    prompt.push_str("    {\"surface_id\": \"--option\", \"action\": \"ACCEPT\"},\n");
    prompt.push_str("    {\"surface_id\": \"--other\", \"action\": \"DEMOTE\", \"reason\": \"error message, not behavior\"}\n");
    prompt.push_str("  ]\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n");

    prompt
}

/// Evidence outputs read from files.
struct EvidenceOutputs {
    control_stdout: String,
    option_stdout: String,
    option_stderr: String,
    control_exit_code: Option<i64>,
    option_exit_code: Option<i64>,
}

/// Read stdout/stderr/exit_code from evidence files.
fn read_evidence_outputs(pack_path: &Path, evidence_path: &str) -> EvidenceOutputs {
    // Option evidence path is like "evidence/foo_c5.json"
    // Control evidence path is like "evidence/foo_c5_control.json"
    let option_path = pack_path.join(evidence_path);
    let control_path_str = evidence_path.replace(".json", "_control.json");
    let control_path = pack_path.join(&control_path_str);

    let mut result = EvidenceOutputs {
        control_stdout: String::new(),
        option_stdout: String::new(),
        option_stderr: String::new(),
        control_exit_code: None,
        option_exit_code: None,
    };

    // Read option evidence
    if let Ok(content) = fs::read_to_string(&option_path) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            result.option_stdout = json
                .get("stdout")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            result.option_stderr = json
                .get("stderr")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            result.option_exit_code = json.get("exit_code").and_then(|v| v.as_i64());
        }
    }

    // Read control evidence
    if let Ok(content) = fs::read_to_string(&control_path) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            result.control_stdout = json
                .get("stdout")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            result.control_exit_code = json.get("exit_code").and_then(|v| v.as_i64());
        }
    }

    result
}

/// Compute a simple unified diff between two strings.
fn compute_unified_diff(control: &str, option: &str) -> String {
    let control_lines: Vec<&str> = control.lines().collect();
    let option_lines: Vec<&str> = option.lines().collect();

    let mut diff = String::new();
    let max_lines = control_lines.len().max(option_lines.len()).min(100);

    for i in 0..max_lines {
        let ctrl = control_lines.get(i).copied().unwrap_or("");
        let opt = option_lines.get(i).copied().unwrap_or("");

        if ctrl != opt {
            if !ctrl.is_empty() {
                diff.push_str(&format!("-{}\n", ctrl));
            }
            if !opt.is_empty() {
                diff.push_str(&format!("+{}\n", opt));
            }
        } else if !ctrl.is_empty() {
            // Show some context around differences
            diff.push_str(&format!(" {}\n", ctrl));
        }
    }

    // Trim excessive unchanged lines, keep lines around changes
    compress_diff_context(&diff, 3)
}

/// Compress diff to show only N lines of context around changes.
fn compress_diff_context(diff: &str, context_lines: usize) -> String {
    let lines: Vec<&str> = diff.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    // Find which lines are changes (start with + or -)
    let is_change: Vec<bool> = lines
        .iter()
        .map(|l| l.starts_with('+') || l.starts_with('-'))
        .collect();

    // Mark lines to keep (changes and context around them)
    let mut keep = vec![false; lines.len()];
    for (i, &is_ch) in is_change.iter().enumerate() {
        if is_ch {
            let start = i.saturating_sub(context_lines);
            let end = (i + context_lines + 1).min(lines.len());
            for k in &mut keep[start..end] {
                *k = true;
            }
        }
    }

    // Build result with "..." for skipped sections
    let mut result = String::new();
    let mut in_skip = false;
    for (i, &line) in lines.iter().enumerate() {
        if keep[i] {
            if in_skip {
                result.push_str("...\n");
                in_skip = false;
            }
            result.push_str(line);
            result.push('\n');
        } else {
            in_skip = true;
        }
    }

    result
}

/// Truncate string to max length, adding "..." if truncated.
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

/// Parse critique response from LM.
fn parse_critique_response(response: &str) -> Vec<(String, CritiqueAction)> {
    let mut results = Vec::new();

    // Try to extract JSON from response
    let json_str = if let Some(start) = response.find('{') {
        if let Some(end) = response.rfind('}') {
            &response[start..=end]
        } else {
            return results;
        }
    } else {
        return results;
    };

    // Parse JSON
    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return results,
    };

    // Extract judgments array
    if let Some(judgments) = parsed.get("judgments").and_then(|j| j.as_array()) {
        for judgment in judgments {
            let surface_id = judgment
                .get("surface_id")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());

            let action_str = judgment.get("action").and_then(|a| a.as_str());

            if let (Some(id), Some(action)) = (surface_id, action_str) {
                let critique_action = match action.to_uppercase().as_str() {
                    "ACCEPT" => CritiqueAction::Accept,
                    "DEMOTE" => {
                        let reason = judgment
                            .get("reason")
                            .and_then(|r| r.as_str())
                            .unwrap_or("demoted by critique")
                            .to_string();
                        CritiqueAction::Demote { reason }
                    }
                    _ => continue,
                };
                results.push((id, critique_action));
            }
        }
    }

    results
}

/// Format an action for display.
fn format_action_desc(action: &super::lm::LmAction) -> String {
    match action {
        super::lm::LmAction::SetBaseline { .. } => "SetBaseline".to_string(),
        super::lm::LmAction::Test {
            surface_id,
            extra_args,
            ..
        } => {
            if extra_args.is_empty() {
                format!("Test {}", surface_id)
            } else {
                format!("Test {} +{:?}", surface_id, extra_args)
            }
        }
    }
}

/// Get a summary of the current state.
pub fn get_summary(state: &State) -> Summary {
    let total = state.entries.len();
    let verified = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Verified))
        .count();
    let excluded = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Excluded { .. }))
        .count();
    let pending = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Pending))
        .count();

    Summary {
        binary: state.binary.clone(),
        context_argv: state.context_argv.clone(),
        cycle: state.cycle,
        total,
        verified,
        excluded,
        pending,
        has_baseline: state.baseline.is_some(),
    }
}

/// Summary of verification state.
#[derive(Debug, Clone)]
pub struct Summary {
    pub binary: String,
    pub context_argv: Vec<String>,
    pub cycle: u32,
    pub total: usize,
    pub verified: usize,
    pub excluded: usize,
    pub pending: usize,
    pub has_baseline: bool,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let context = if self.context_argv.is_empty() {
            String::new()
        } else {
            format!(" {}", self.context_argv.join(" "))
        };
        writeln!(f, "Binary: {}{}", self.binary, context)?;
        writeln!(f, "Cycle: {}", self.cycle)?;
        writeln!(
            f,
            "Baseline: {}",
            if self.has_baseline { "yes" } else { "no" }
        )?;
        writeln!(f, "Surfaces: {} total", self.total)?;
        writeln!(f, "  Verified: {}", self.verified)?;
        writeln!(f, "  Excluded: {}", self.excluded)?;
        writeln!(f, "  Pending: {}", self.pending)?;
        let pct = if self.total > 0 {
            (self.verified * 100) / self.total
        } else {
            0
        };
        write!(f, "Verification rate: {}%", pct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::types::{SurfaceEntry, STATE_SCHEMA_VERSION};

    #[test]
    fn test_get_summary() {
        let state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "test".to_string(),
            context_argv: vec!["sub".to_string()],
            baseline: None,
            entries: vec![
                SurfaceEntry {
                    id: "-a".to_string(),
                    description: "A".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Verified,
                    attempts: vec![],
                category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                },
                SurfaceEntry {
                    id: "-b".to_string(),
                    description: "B".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Pending,
                    attempts: vec![],
                category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                },
                SurfaceEntry {
                    id: "-c".to_string(),
                    description: "C".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Excluded {
                        reason: "test".to_string(),
                    },
                    attempts: vec![],
                category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                },
            ],
            cycle: 5,
            seed_bank: vec![],
            help_preamble: String::new(),
        };

        let summary = get_summary(&state);

        assert_eq!(summary.binary, "test");
        assert_eq!(summary.context_argv, vec!["sub"]);
        assert_eq!(summary.cycle, 5);
        assert_eq!(summary.total, 3);
        assert_eq!(summary.verified, 1);
        assert_eq!(summary.excluded, 1);
        assert_eq!(summary.pending, 1);
        assert!(!summary.has_baseline);
    }

    #[test]
    fn test_format_action_desc() {
        use crate::verify::lm::LmAction;
        use crate::verify::types::Seed;

        let action = LmAction::SetBaseline {
            seed: Seed::default(),
        };
        assert_eq!(format_action_desc(&action), "SetBaseline");

        let action = LmAction::Test {
            surface_id: "--stat".to_string(),
            extra_args: vec![],
            seed: Seed::default(),
            prediction: None,
        };
        assert_eq!(format_action_desc(&action), "Test --stat");

        let action = LmAction::Test {
            surface_id: "--stat".to_string(),
            extra_args: vec!["--numstat".to_string()],
            seed: Seed::default(),
            prediction: None,
        };
        assert!(format_action_desc(&action).contains("Test --stat"));
        assert!(format_action_desc(&action).contains("--numstat"));
    }
}
