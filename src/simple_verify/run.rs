//! Main verification loop.
//!
//! This module implements the core verification loop:
//! bootstrap → [gather pending → lm_call → apply actions → save]* → done

use super::apply::{apply_action, merge_test_result, run_test_scenario};
use super::bootstrap::bootstrap;
use super::evidence::{
    make_output_preview, run_scenario, sanitize_id, write_evidence, OUTPUT_PREVIEW_MAX_LEN,
};
use super::lm::{log_prompt, log_response, parse_lm_response, LmAction, LmResponse};
use super::prompt::{build_incremental_prompt, build_prompt, build_retry_prompt};
use super::types::{Attempt, DiffKind, Outcome, State, Status};
use super::validate::validate_action;
use crate::cli::ContextMode;
use crate::lm::{create_plugin, LmConfig, LmPlugin};
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

/// Maximum verification attempts per surface before auto-exhausting.
const MAX_ATTEMPTS: usize = 5;

/// Maximum surfaces to include in each LM batch.
const BATCH_SIZE: usize = 5;

/// Modifier flags to probe when an option consistently produces OutputsEqual.
/// These are common CLI modifiers that change output format/verbosity.
/// Modifier flags to probe when an option consistently produces OutputsEqual.
const PROBE_MODIFIERS: &[&str] = &["-v", "-a", "-1", "--verbose"];

/// Minimum attempts with OutputsEqual before trying modifier probing.
const PROBE_THRESHOLD: usize = 3;

/// Cycles per retry pass.
const RETRY_PASS_CYCLES: u32 = 10;

/// Minimum retry passes before considering stopping.
const MIN_RETRY_PASSES: u32 = 3;

/// Stop after this many consecutive passes with no progress.
const MAX_NO_PROGRESS_PASSES: u32 = 3;

/// Maximum total retry passes regardless of progress.
const MAX_RETRY_PASSES: u32 = 5;

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

/// Consecutive parse failures before auto-resetting session.
const PARSE_ERROR_RESET_THRESHOLD: usize = 2;

/// Reset excluded surfaces to Pending for retry with fresh LM session.
/// Returns the count and a map of surface_id -> prior attempts for history.
fn retry_excluded_surfaces(state: &mut State, verbose: bool) -> (usize, std::collections::HashMap<String, Vec<Attempt>>) {
    let mut count = 0;
    let mut prior_attempts = std::collections::HashMap::new();
    for entry in &mut state.entries {
        if let Status::Excluded { reason } = &entry.status {
            if !entry.retried {
                if verbose {
                    eprintln!("  Queueing {} for retry (was: {})", entry.id, reason);
                }
                // Store prior attempts before clearing
                if !entry.attempts.is_empty() {
                    prior_attempts.insert(entry.id.clone(), entry.attempts.clone());
                }
                entry.status = Status::Pending;
                entry.retried = true;
                entry.attempts.clear(); // Fresh start - full workflow will run
                count += 1;
            }
        }
    }
    (count, prior_attempts)
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
        let state = bootstrap(binary, context_argv)?;
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
        )
    };

    // Progressive retry: keep retrying excluded surfaces while making progress
    let mut retry_pass = 0u32;
    let mut passes_without_progress = 0u32;

    loop {
        // Reset retried flag for surfaces that are still excluded (allow re-retry)
        for entry in &mut state.entries {
            if matches!(entry.status, Status::Excluded { .. }) {
                entry.retried = false;
            }
        }

        // Try to reset excluded surfaces for this pass
        let (retry_count, prior_attempts) = retry_excluded_surfaces(&mut state, verbose);
        if retry_count == 0 {
            break; // Nothing left to retry
        }

        // Check stopping conditions
        if retry_pass >= MAX_RETRY_PASSES {
            if verbose {
                eprintln!("\nStopping retry: reached max {} passes", MAX_RETRY_PASSES);
            }
            break;
        }
        if retry_pass >= MIN_RETRY_PASSES && passes_without_progress >= MAX_NO_PROGRESS_PASSES {
            if verbose {
                eprintln!(
                    "\nStopping retry: {} passes without progress",
                    passes_without_progress
                );
            }
            break;
        }

        retry_pass += 1;
        let verified_before = state
            .entries
            .iter()
            .filter(|e| matches!(e.status, Status::Verified))
            .count();

        if verbose {
            eprintln!(
                "\nRetry pass {}: {} surface(s) with fresh LM session",
                retry_pass, retry_count
            );
        }
        state.save(pack_path)?;

        // Collect retried surface IDs
        let retry_ids: Vec<String> = state
            .entries
            .iter()
            .filter(|e| e.retried && matches!(e.status, Status::Pending))
            .map(|e| e.id.clone())
            .collect();

        // Chunk retry surfaces by session size (same as main phase)
        let chunk_size = if session_size > 0 {
            session_size
        } else {
            retry_ids.len()
        };
        let retry_chunks: Vec<Vec<String>> = retry_ids
            .chunks(chunk_size)
            .map(|c| c.to_vec())
            .collect();
        let num_retry_chunks = retry_chunks.len();

        // Run retry chunks in parallel or sequential (same as main phase)
        if parallel_sessions && num_retry_chunks > 1 {
            run_parallel_retry_chunks(
                pack_path,
                &mut state,
                retry_chunks,
                lm_config,
                verbose,
                context_mode,
                &prior_attempts,
            );
        } else {
            // Sequential: single LM session for all retry surfaces
            let retry_max = state.cycle + RETRY_PASS_CYCLES;
            let mut retry_plugin = create_plugin(lm_config);
            retry_plugin.init().context("initialize retry LM plugin")?;

            let _retry_result = run_retry_chunk(
                pack_path,
                &mut *retry_plugin,
                &mut state,
                &retry_ids,
                retry_max,
                verbose,
                context_mode,
                &prior_attempts,
            );

            retry_plugin.shutdown().ok();
        }

        // Check if we made progress
        let verified_after = state
            .entries
            .iter()
            .filter(|e| matches!(e.status, Status::Verified))
            .count();

        if verified_after > verified_before {
            let newly_verified = verified_after - verified_before;
            if verbose {
                eprintln!("  Progress: verified {} new surface(s)", newly_verified);
            }
            passes_without_progress = 0; // Reset momentum
        } else {
            passes_without_progress += 1;
            if verbose {
                eprintln!(
                    "  No progress ({}/{})",
                    passes_without_progress, MAX_NO_PROGRESS_PASSES
                );
            }
        }
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
fn run_sequential_sessions(
    pack_path: &Path,
    state: &mut State,
    chunks: Vec<Vec<String>>,
    max_cycles: u32,
    lm_config: &LmConfig,
    verbose: bool,
    context_mode: ContextMode,
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
        result = match run_sequential_chunk(
            pack_path,
            &mut *plugin,
            state,
            &chunk_ids,
            max_cycles,
            verbose,
            context_mode,
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

/// Run sessions in parallel using thread::scope.
fn run_parallel_sessions(
    pack_path: &Path,
    state: &mut State,
    chunks: Vec<Vec<String>>,
    max_cycles: u32,
    lm_config: &LmConfig,
    verbose: bool,
    context_mode: ContextMode,
) -> RunResult {
    let num_sessions = chunks.len();

    if verbose {
        eprintln!("\nRunning {} sessions in parallel...", num_sessions);
    }

    // Each session gets its own cycle range to avoid conflicts
    let cycles_per_session = max_cycles / num_sessions as u32;

    // Run all sessions in parallel
    let session_results: Vec<(Vec<SessionUpdate>, u32)> = thread::scope(|s| {
        let handles: Vec<_> = chunks
            .into_iter()
            .enumerate()
            .map(|(session_idx, chunk_ids)| {
                // Clone state for this session
                let mut session_state = state.clone();
                // Each session starts at a different cycle offset
                let start_cycle = session_idx as u32 * cycles_per_session;
                session_state.cycle = start_cycle;

                let session_max = start_cycle + cycles_per_session;

                s.spawn(move || {
                    // Check if any pending in this chunk
                    let pending_in_chunk = session_state
                        .entries
                        .iter()
                        .filter(|e| {
                            chunk_ids.contains(&e.id) && matches!(e.status, Status::Pending)
                        })
                        .count();

                    if pending_in_chunk == 0 {
                        return (vec![], start_cycle);
                    }

                    if verbose {
                        eprintln!(
                            "  Session {}/{}: {} surfaces ({} pending), cycles {}-{}",
                            session_idx + 1,
                            num_sessions,
                            chunk_ids.len(),
                            pending_in_chunk,
                            start_cycle + 1,
                            session_max
                        );
                    }

                    // Create fresh LM session
                    let mut plugin = create_plugin(lm_config);
                    if let Err(e) = plugin.init() {
                        eprintln!("  Session {} failed to init LM: {}", session_idx + 1, e);
                        return (vec![], start_cycle);
                    }

                    // Run verification
                    let _result = run_sequential_chunk(
                        pack_path,
                        &mut *plugin,
                        &mut session_state,
                        &chunk_ids,
                        session_max,
                        verbose,
                        context_mode,
                    );

                    plugin.shutdown().ok();

                    // Collect updates for surfaces in this chunk
                    let updates: Vec<SessionUpdate> = session_state
                        .entries
                        .iter()
                        .filter(|e| chunk_ids.contains(&e.id))
                        .map(|e| SessionUpdate {
                            surface_id: e.id.clone(),
                            status: e.status.clone(),
                            attempts: e.attempts.clone(),
                            retried: e.retried,
                        })
                        .collect();

                    (updates, session_state.cycle)
                })
            })
            .collect();

        // Collect results from all threads
        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .collect()
    });

    // Merge updates back into main state
    let mut max_cycle = state.cycle;
    for (updates, session_cycle) in session_results {
        max_cycle = max_cycle.max(session_cycle);
        for update in updates {
            if let Some(entry) = state.entries.iter_mut().find(|e| e.id == update.surface_id) {
                entry.status = update.status;
                entry.attempts = update.attempts;
                entry.retried = update.retried;
            }
        }
    }
    state.cycle = max_cycle;
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
            "\nParallel sessions complete: {} verified, {} excluded, {} pending",
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

/// Run retry chunks in parallel using thread::scope.
#[allow(clippy::too_many_arguments)]
fn run_parallel_retry_chunks(
    pack_path: &Path,
    state: &mut State,
    chunks: Vec<Vec<String>>,
    lm_config: &LmConfig,
    verbose: bool,
    context_mode: ContextMode,
    prior_attempts: &std::collections::HashMap<String, Vec<Attempt>>,
) {
    let num_chunks = chunks.len();

    if verbose {
        eprintln!("  Running {} retry chunks in parallel...", num_chunks);
    }

    // Each chunk gets its own cycle range
    let cycles_per_chunk = RETRY_PASS_CYCLES / num_chunks as u32;
    let base_cycle = state.cycle;

    // Run all chunks in parallel
    let chunk_results: Vec<(Vec<SessionUpdate>, u32)> = thread::scope(|s| {
        let handles: Vec<_> = chunks
            .into_iter()
            .enumerate()
            .map(|(chunk_idx, chunk_ids)| {
                // Clone state for this chunk
                let mut chunk_state = state.clone();
                let start_cycle = base_cycle + (chunk_idx as u32 * cycles_per_chunk);
                chunk_state.cycle = start_cycle;
                let chunk_max = start_cycle + cycles_per_chunk;

                // Clone prior_attempts for this chunk
                let chunk_prior: std::collections::HashMap<String, Vec<Attempt>> = prior_attempts
                    .iter()
                    .filter(|(k, _)| chunk_ids.contains(k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();

                s.spawn(move || {
                    let mut plugin = create_plugin(lm_config);
                    if plugin.init().is_err() {
                        return (vec![], start_cycle);
                    }

                    // Run retry chunk
                    let _ = run_retry_chunk(
                        pack_path,
                        &mut *plugin,
                        &mut chunk_state,
                        &chunk_ids,
                        chunk_max,
                        verbose,
                        context_mode,
                        &chunk_prior,
                    );

                    plugin.shutdown().ok();

                    // Collect updates for surfaces in this chunk
                    let updates: Vec<SessionUpdate> = chunk_state
                        .entries
                        .iter()
                        .filter(|e| chunk_ids.contains(&e.id))
                        .map(|e| SessionUpdate {
                            surface_id: e.id.clone(),
                            status: e.status.clone(),
                            attempts: e.attempts.clone(),
                            retried: e.retried,
                        })
                        .collect();

                    (updates, chunk_state.cycle)
                })
            })
            .collect();

        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Merge results back into main state
    let mut max_cycle = state.cycle;
    for (updates, chunk_cycle) in chunk_results {
        max_cycle = max_cycle.max(chunk_cycle);
        for update in updates {
            if let Some(entry) = state.entries.iter_mut().find(|e| e.id == update.surface_id) {
                entry.status = update.status;
                entry.attempts = update.attempts;
                entry.retried = update.retried;
            }
        }
    }
    state.cycle = max_cycle;
    let _ = state.save(pack_path);
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

/// Run the sequential verification loop for a chunk of surfaces.
///
/// Only processes surfaces whose IDs are in `chunk_ids`.
#[allow(clippy::too_many_arguments)]
fn run_sequential_chunk(
    pack_path: &Path,
    plugin: &mut dyn LmPlugin,
    state: &mut State,
    chunk_ids: &[String],
    max_cycles: u32,
    verbose: bool,
    context_mode: ContextMode,
) -> Result<RunResult> {
    // Track last response for incremental mode
    let mut last_response: Option<LmResponse> = None;
    // Track consecutive parse failures for auto-reset
    let mut consecutive_parse_failures: usize = 0;
    loop {
        // Check cycle limit
        if state.cycle >= max_cycles {
            if verbose {
                eprintln!("Hit max cycles limit ({})", max_cycles);
            }
            state.save(pack_path)?;
            return Ok(RunResult::HitMaxCycles);
        }

        // Modifier probing: try combining options with modifier flags
        // when they've hit PROBE_THRESHOLD attempts with all OutputsEqual.
        // This runs BEFORE auto-exhaust, giving options a chance to be verified.
        probe_entries_with_modifiers(state, pack_path, verbose)?;

        // Auto-exhaust surfaces over attempt limit (only in this chunk)
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

        // Find pending targets with round-robin selection (only from this chunk)
        let all_pending: Vec<String> = state
            .entries
            .iter()
            .filter(|e| chunk_ids.contains(&e.id) && matches!(e.status, Status::Pending))
            .map(|e| e.id.clone())
            .collect();

        let pending_ids: Vec<String> = if all_pending.is_empty() {
            vec![]
        } else {
            // Round-robin: start at (cycle % pending_count) to ensure fairness
            let offset = (state.cycle as usize) % all_pending.len();
            all_pending
                .iter()
                .cycle()
                .skip(offset)
                .take(BATCH_SIZE.min(all_pending.len()))
                .cloned()
                .collect()
        };

        if pending_ids.is_empty() {
            if verbose {
                eprintln!("All surfaces processed - complete!");
            }
            state.save(pack_path)?;
            return Ok(RunResult::Complete);
        }

        // Increment cycle
        state.cycle += 1;

        if verbose {
            eprintln!(
                "Cycle {}: processing {} surface(s): {}",
                state.cycle,
                pending_ids.len(),
                pending_ids.join(", ")
            );
        }

        // Resolve auto mode based on plugin type
        let effective_mode = match context_mode {
            ContextMode::Auto if plugin.is_stateful() => ContextMode::Incremental,
            ContextMode::Auto => ContextMode::Full,
            other => other,
        };

        // Build prompt based on effective context mode
        let prompt = match effective_mode {
            ContextMode::Full | ContextMode::Reset => {
                // Full mode: always send complete state
                // Reset mode: also sends complete state (but resets after)
                build_prompt(state, &pending_ids)
            }
            ContextMode::Incremental if plugin.is_stateful() && last_response.is_some() => {
                // Incremental mode for stateful plugins: send only delta
                build_incremental_prompt(state, &pending_ids, last_response.as_ref())
            }
            ContextMode::Incremental | ContextMode::Auto => {
                // First cycle or non-stateful plugin: send full prompt
                build_prompt(state, &pending_ids)
            }
        };
        log_prompt(pack_path, state.cycle, &prompt)?;

        if verbose {
            eprintln!("  Invoking LM...");
        }
        let response_text = invoke_lm_with_retry(plugin, &prompt, verbose)?;

        // Parse response with graceful degradation
        let response = match parse_lm_response(&response_text) {
            Ok(r) => r,
            Err(_e) => {
                // LM may produce prose instead of JSON. Try a short follow-up reminder.
                if verbose {
                    eprintln!("  Parse error, sending JSON reminder...");
                }
                // Short follow-up that doesn't resend the whole prompt
                let reminder = "Please provide your response as valid JSON only. \
                    Format: {\"actions\": [...]}. No explanations or prose.";
                match invoke_lm_with_retry(plugin, reminder, verbose)
                    .and_then(|text| parse_lm_response(&text))
                {
                    Ok(r) => r,
                    Err(_) => {
                        if verbose {
                            eprintln!(
                                "  LM still not producing JSON, continuing with empty actions"
                            );
                        }
                        LmResponse { actions: vec![] }
                    }
                }
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

        // Handle empty response - continue to next cycle instead of giving up
        // Empty actions can mean LM parse failed (graceful degradation) or LM
        // genuinely had nothing to say. Either way, try again next cycle.
        if response.actions.is_empty() {
            consecutive_parse_failures += 1;
            if verbose {
                eprintln!("  LM returned no actions - continuing to next cycle");
            }

            // Auto-reset session if we hit too many consecutive parse failures
            if consecutive_parse_failures >= PARSE_ERROR_RESET_THRESHOLD {
                if verbose {
                    eprintln!(
                        "  Auto-resetting LM session after {} consecutive parse failures",
                        consecutive_parse_failures
                    );
                }
                plugin.reset().ok();
                last_response = None; // Clear incremental state
                consecutive_parse_failures = 0;
            } else {
                last_response = Some(response);
            }
            state.save(pack_path)?;
            continue;
        }

        // Got valid actions - reset parse failure counter
        consecutive_parse_failures = 0;

        if verbose {
            eprintln!("  LM returned {} action(s)", response.actions.len());
        }

        // Save response for incremental mode before consuming actions
        let response_for_tracking = response.clone();

        // Partition and validate actions
        let mut baselines = Vec::new();
        let mut tests = Vec::new();
        let mut excludes = Vec::new();

        for action in response.actions {
            if let Err(e) = validate_action(&action, state) {
                eprintln!("  Skipping invalid action: {}", e);
                continue;
            }
            match &action {
                LmAction::SetBaseline { .. } => baselines.push(action),
                LmAction::Test { .. } => tests.push(action),
                LmAction::Exclude { .. } => excludes.push(action),
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
            state.save(pack_path)?;
        }

        // 2. Run tests in parallel
        if !tests.is_empty() {
            if verbose {
                eprintln!("  Running {} test(s) in parallel...", tests.len());
            }

            // Extract test parameters for parallel execution
            let test_params: Vec<_> = tests
                .into_iter()
                .filter_map(|action| {
                    if let LmAction::Test {
                        surface_id,
                        args,
                        seed,
                    } = action
                    {
                        Some((surface_id, args, seed))
                    } else {
                        None
                    }
                })
                .collect();

            // Run scenarios in parallel using thread::scope
            let results: Vec<_> = thread::scope(|s| {
                let handles: Vec<_> = test_params
                    .into_iter()
                    .map(|(surface_id, args, seed)| {
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
                                args,
                                seed,
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
                        }
                    );
                }
                merge_test_result(state, result);
            }
            state.save(pack_path)?;
        }

        // 3. Apply excludes (fast, just state mutation)
        for action in excludes {
            if verbose {
                eprintln!("  Applying: {}", format_action_desc(&action));
            }
            if let Err(e) = apply_action(state, pack_path, action) {
                eprintln!("  Action failed: {}", e);
            }
        }
        state.save(pack_path)?;

        // Track response for incremental mode
        last_response = Some(response_for_tracking);

        // Report progress
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
                "  Progress: {} verified, {} excluded, {} pending",
                verified, excluded, pending
            );
        }
    }
}

/// Run the retry verification loop for a chunk of surfaces with prior attempt history.
///
/// Similar to run_sequential_chunk but uses build_retry_prompt to include
/// prior attempt history for each surface.
#[allow(clippy::too_many_arguments)]
fn run_retry_chunk(
    pack_path: &Path,
    plugin: &mut dyn LmPlugin,
    state: &mut State,
    chunk_ids: &[String],
    max_cycles: u32,
    verbose: bool,
    context_mode: ContextMode,
    prior_attempts: &std::collections::HashMap<String, Vec<Attempt>>,
) -> Result<RunResult> {
    // Track last response for incremental mode
    let mut last_response: Option<LmResponse> = None;
    // Track consecutive parse failures for auto-reset
    let mut consecutive_parse_failures: usize = 0;
    loop {
        // Check cycle limit
        if state.cycle >= max_cycles {
            if verbose {
                eprintln!("Hit max cycles limit ({})", max_cycles);
            }
            state.save(pack_path)?;
            return Ok(RunResult::HitMaxCycles);
        }

        // Modifier probing for retry surfaces too
        probe_entries_with_modifiers(state, pack_path, verbose)?;

        // Auto-exhaust surfaces over attempt limit (only in this chunk)
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

        // Find pending targets with round-robin selection (only from this chunk)
        let all_pending: Vec<String> = state
            .entries
            .iter()
            .filter(|e| chunk_ids.contains(&e.id) && matches!(e.status, Status::Pending))
            .map(|e| e.id.clone())
            .collect();

        let pending_ids: Vec<String> = if all_pending.is_empty() {
            vec![]
        } else {
            // Round-robin: start at (cycle % pending_count) to ensure fairness
            let offset = (state.cycle as usize) % all_pending.len();
            all_pending
                .iter()
                .cycle()
                .skip(offset)
                .take(BATCH_SIZE.min(all_pending.len()))
                .cloned()
                .collect()
        };

        if pending_ids.is_empty() {
            if verbose {
                eprintln!("All retry surfaces processed - complete!");
            }
            state.save(pack_path)?;
            return Ok(RunResult::Complete);
        }

        // Increment cycle
        state.cycle += 1;

        if verbose {
            eprintln!(
                "Retry cycle {}: processing {} surface(s): {}",
                state.cycle,
                pending_ids.len(),
                pending_ids.join(", ")
            );
        }

        // Resolve auto mode based on plugin type
        let effective_mode = match context_mode {
            ContextMode::Auto if plugin.is_stateful() => ContextMode::Incremental,
            ContextMode::Auto => ContextMode::Full,
            other => other,
        };

        // Build prompt - use retry prompt with prior attempt history for first cycle,
        // then switch to incremental if supported
        let prompt = match effective_mode {
            ContextMode::Full | ContextMode::Reset => {
                // Use retry prompt with prior attempt history
                build_retry_prompt(state, &pending_ids, prior_attempts)
            }
            ContextMode::Incremental if plugin.is_stateful() && last_response.is_some() => {
                // Incremental mode for stateful plugins: send only delta
                build_incremental_prompt(state, &pending_ids, last_response.as_ref())
            }
            ContextMode::Incremental | ContextMode::Auto => {
                // First cycle: use retry prompt with history
                build_retry_prompt(state, &pending_ids, prior_attempts)
            }
        };
        log_prompt(pack_path, state.cycle, &prompt)?;

        if verbose {
            eprintln!("  Invoking LM...");
        }
        let response_text = invoke_lm_with_retry(plugin, &prompt, verbose)?;

        // Parse response with graceful degradation
        let response = match parse_lm_response(&response_text) {
            Ok(r) => r,
            Err(_e) => {
                if verbose {
                    eprintln!("  Parse error, sending JSON reminder...");
                }
                let reminder = "Please provide your response as valid JSON only. \
                    Format: {\"actions\": [...]}. No explanations or prose.";
                match invoke_lm_with_retry(plugin, reminder, verbose)
                    .and_then(|text| parse_lm_response(&text))
                {
                    Ok(r) => r,
                    Err(_) => {
                        if verbose {
                            eprintln!(
                                "  LM still not producing JSON, continuing with empty actions"
                            );
                        }
                        LmResponse { actions: vec![] }
                    }
                }
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
            consecutive_parse_failures += 1;
            if verbose {
                eprintln!("  LM returned no actions - continuing to next cycle");
            }

            if consecutive_parse_failures >= PARSE_ERROR_RESET_THRESHOLD {
                if verbose {
                    eprintln!(
                        "  Auto-resetting LM session after {} consecutive parse failures",
                        consecutive_parse_failures
                    );
                }
                plugin.reset().ok();
                last_response = None;
                consecutive_parse_failures = 0;
            } else {
                last_response = Some(response);
            }
            state.save(pack_path)?;
            continue;
        }

        // Got valid actions - reset parse failure counter
        consecutive_parse_failures = 0;

        if verbose {
            eprintln!("  LM returned {} action(s)", response.actions.len());
        }

        // Save response for incremental mode before consuming actions
        let response_for_tracking = response.clone();

        // Partition and validate actions
        let mut baselines = Vec::new();
        let mut tests = Vec::new();
        let mut excludes = Vec::new();

        for action in response.actions {
            if let Err(e) = validate_action(&action, state) {
                eprintln!("  Skipping invalid action: {}", e);
                continue;
            }
            match &action {
                LmAction::SetBaseline { .. } => baselines.push(action),
                LmAction::Test { .. } => tests.push(action),
                LmAction::Exclude { .. } => excludes.push(action),
            }
        }

        // 1. Apply baselines first
        for action in baselines {
            if verbose {
                eprintln!("  Applying: {}", format_action_desc(&action));
            }
            if let Err(e) = apply_action(state, pack_path, action) {
                eprintln!("  Action failed: {}", e);
            }
            state.save(pack_path)?;
        }

        // 2. Run tests in parallel
        if !tests.is_empty() {
            if verbose {
                eprintln!("  Running {} test(s) in parallel...", tests.len());
            }

            let test_params: Vec<_> = tests
                .into_iter()
                .filter_map(|action| {
                    if let LmAction::Test {
                        surface_id,
                        args,
                        seed,
                    } = action
                    {
                        Some((surface_id, args, seed))
                    } else {
                        None
                    }
                })
                .collect();

            let results: Vec<_> = thread::scope(|s| {
                let handles: Vec<_> = test_params
                    .into_iter()
                    .map(|(surface_id, args, seed)| {
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
                                args,
                                seed,
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
                        }
                    );
                }
                merge_test_result(state, result);
            }
            state.save(pack_path)?;
        }

        // 3. Apply excludes
        for action in excludes {
            if verbose {
                eprintln!("  Applying: {}", format_action_desc(&action));
            }
            if let Err(e) = apply_action(state, pack_path, action) {
                eprintln!("  Action failed: {}", e);
            }
        }
        state.save(pack_path)?;

        // Track response for incremental mode
        last_response = Some(response_for_tracking);

        // Report progress
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
                "  Progress: {} verified, {} excluded, {} pending",
                verified, excluded, pending
            );
        }
    }
}

/// Probe entries that have hit PROBE_THRESHOLD with all OutputsEqual outcomes.
///
/// For each such entry, try combining the option with modifier flags to find
/// a context where it produces different output. This is a mechanical probe
/// with no LM calls.
fn probe_entries_with_modifiers(state: &mut State, pack_path: &Path, verbose: bool) -> Result<()> {
    // Find entries eligible for probing:
    // - Status::Pending
    // - Exactly PROBE_THRESHOLD attempts
    // - All attempts have OutputsEqual outcome
    let probe_candidates: Vec<(usize, String)> = state
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            matches!(entry.status, Status::Pending)
                && entry.attempts.len() == PROBE_THRESHOLD
                && entry
                    .attempts
                    .iter()
                    .all(|a| matches!(a.outcome, Outcome::OutputsEqual))
        })
        .map(|(idx, entry)| (idx, entry.id.clone()))
        .collect();

    if probe_candidates.is_empty() {
        return Ok(());
    }

    if verbose {
        eprintln!(
            "Probing {} option(s) with modifier flags...",
            probe_candidates.len()
        );
    }

    for (entry_idx, surface_id) in probe_candidates {
        // Get the seed from the most recent attempt
        let seed = state.entries[entry_idx]
            .attempts
            .last()
            .map(|a| a.seed.clone())
            .unwrap_or_default();

        if verbose {
            eprintln!("  Probing {} with modifiers...", surface_id);
        }

        for modifier in PROBE_MODIFIERS {
            // Skip if the modifier is the same as the option being tested
            if *modifier == surface_id {
                continue;
            }

            // Build control argv: context_argv + modifier
            let control_argv: Vec<String> = state
                .context_argv
                .iter()
                .cloned()
                .chain(std::iter::once(modifier.to_string()))
                .collect();

            // Build variant argv: context_argv + modifier + option
            let variant_argv: Vec<String> = state
                .context_argv
                .iter()
                .cloned()
                .chain(std::iter::once(modifier.to_string()))
                .chain(std::iter::once(surface_id.clone()))
                .collect();

            // Run control scenario
            let probe_id = format!(
                "{}_probe_{}_c{}",
                sanitize_id(&surface_id),
                sanitize_id(modifier),
                state.cycle
            );
            let control_id = format!("{}_control", probe_id);

            let control_evidence =
                match run_scenario(pack_path, &control_id, &state.binary, &control_argv, &seed) {
                    Ok(ev) => ev,
                    Err(_) => continue, // Skip this modifier on error
                };

            // Run variant scenario
            let variant_evidence =
                match run_scenario(pack_path, &probe_id, &state.binary, &variant_argv, &seed) {
                    Ok(ev) => ev,
                    Err(_) => continue, // Skip this modifier on error
                };

            // Compare outputs
            let stdout_differs = variant_evidence.stdout != control_evidence.stdout;
            let stderr_differs = variant_evidence.stderr != control_evidence.stderr;
            let exit_differs = variant_evidence.exit_code != control_evidence.exit_code;

            // Reject stderr-only diffs when both runs failed (likely just error message variations)
            let both_failed = control_evidence.exit_code.unwrap_or(0) != 0
                && variant_evidence.exit_code.unwrap_or(0) != 0;
            if stderr_differs && !stdout_differs && !exit_differs && both_failed {
                continue;
            }

            if stdout_differs || stderr_differs || exit_differs {
                // Found a difference! Record the attempt and mark as verified.
                let diff_kind = match (stdout_differs, stderr_differs, exit_differs) {
                    (true, false, false) => DiffKind::Stdout,
                    (false, true, false) => DiffKind::Stderr,
                    (false, false, true) => DiffKind::ExitCode,
                    _ => DiffKind::Multiple,
                };

                if verbose {
                    eprintln!(
                        "    Probe {} + {}: outputs differ! ({:?})",
                        modifier, surface_id, diff_kind
                    );
                }

                // Write evidence files
                let control_path = format!("evidence/{}.json", control_id);
                let variant_path = format!("evidence/{}.json", probe_id);
                write_evidence(pack_path, &control_path, &control_evidence)?;
                write_evidence(pack_path, &variant_path, &variant_evidence)?;

                // Capture output previews
                let stdout_preview =
                    make_output_preview(&variant_evidence.stdout, OUTPUT_PREVIEW_MAX_LEN);
                let stderr_preview =
                    make_output_preview(&variant_evidence.stderr, OUTPUT_PREVIEW_MAX_LEN);
                let control_stdout_preview =
                    make_output_preview(&control_evidence.stdout, OUTPUT_PREVIEW_MAX_LEN);

                // Record the successful probe attempt
                let entry = &mut state.entries[entry_idx];
                entry.attempts.push(Attempt {
                    cycle: state.cycle,
                    args: vec![modifier.to_string(), surface_id.clone()],
                    full_argv: variant_argv,
                    seed,
                    evidence_path: variant_path,
                    outcome: Outcome::Verified {
                        diff_kind: diff_kind.clone(),
                    },
                    stdout_preview,
                    stderr_preview,
                    control_stdout_preview,
                });

                // Mark as verified
                entry.status = Status::Verified;

                if verbose {
                    eprintln!("    Verified {} via modifier probe", surface_id);
                }

                // Break on first success - no need to try more modifiers
                break;
            }
        }
    }

    // Save state after probing
    state.save(pack_path)?;

    Ok(())
}

/// Format an action for display.
fn format_action_desc(action: &super::lm::LmAction) -> String {
    match action {
        super::lm::LmAction::SetBaseline { args, .. } => {
            format!("SetBaseline args={:?}", args)
        }
        super::lm::LmAction::Test {
            surface_id, args, ..
        } => {
            format!("Test {} args={:?}", surface_id, args)
        }
        super::lm::LmAction::Exclude { surface_id, reason } => {
            format!("Exclude {} ({})", surface_id, reason)
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
    use crate::simple_verify::types::{SurfaceEntry, STATE_SCHEMA_VERSION};

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
                    retried: false,
                },
                SurfaceEntry {
                    id: "-b".to_string(),
                    description: "B".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Pending,
                    attempts: vec![],
                    retried: false,
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
                    retried: false,
                },
            ],
            cycle: 5,
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
    fn test_retry_excluded_surfaces() {
        let mut state = State {
            schema_version: STATE_SCHEMA_VERSION,
            binary: "test".to_string(),
            context_argv: vec![],
            baseline: None,
            entries: vec![
                // Should be retried
                SurfaceEntry {
                    id: "-a".to_string(),
                    description: "A".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Excluded {
                        reason: "Exhausted after 5 attempts".to_string(),
                    },
                    attempts: vec![],
                    retried: false,
                },
                // Should be retried (fresh run might help)
                SurfaceEntry {
                    id: "-b".to_string(),
                    description: "B".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Excluded {
                        reason: "color output cannot be tested".to_string(),
                    },
                    attempts: vec![],
                    retried: false,
                },
                // Should NOT be retried - already retried
                SurfaceEntry {
                    id: "-c".to_string(),
                    description: "C".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Excluded {
                        reason: "failed again".to_string(),
                    },
                    attempts: vec![],
                    retried: true,
                },
                // Should NOT be retried - not excluded
                SurfaceEntry {
                    id: "-d".to_string(),
                    description: "D".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Pending,
                    attempts: vec![],
                    retried: false,
                },
            ],
            cycle: 10,
        };

        let (count, prior_attempts) = retry_excluded_surfaces(&mut state, false);

        assert_eq!(count, 2);
        // Prior attempts should be empty since we didn't have any attempts
        assert!(prior_attempts.is_empty());
        // -a should now be Pending and retried
        assert!(matches!(state.entries[0].status, Status::Pending));
        assert!(state.entries[0].retried);
        // -b should now be Pending and retried (fresh run might help)
        assert!(matches!(state.entries[1].status, Status::Pending));
        assert!(state.entries[1].retried);
        // -c should still be Excluded (already retried)
        assert!(matches!(state.entries[2].status, Status::Excluded { .. }));
        assert!(state.entries[2].retried);
        // -d should still be Pending
        assert!(matches!(state.entries[3].status, Status::Pending));
        assert!(!state.entries[3].retried);
    }

    #[test]
    fn test_format_action_desc() {
        use crate::simple_verify::lm::LmAction;
        use crate::simple_verify::types::Seed;

        let action = LmAction::SetBaseline {
            args: vec![],
            seed: Seed::default(),
        };
        assert!(format_action_desc(&action).contains("SetBaseline"));

        let action = LmAction::Test {
            surface_id: "--stat".to_string(),
            args: vec!["--stat".to_string()],
            seed: Seed::default(),
        };
        assert!(format_action_desc(&action).contains("Test"));
        assert!(format_action_desc(&action).contains("--stat"));

        let action = LmAction::Exclude {
            surface_id: "--gpg".to_string(),
            reason: "needs key".to_string(),
        };
        assert!(format_action_desc(&action).contains("Exclude"));
    }
}
