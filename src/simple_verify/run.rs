//! Main verification loop.
//!
//! This module implements the core verification loop:
//! bootstrap → [gather pending → lm_call → apply actions → save]* → done

use super::apply::{apply_action, merge_test_result, run_test_scenario};
use super::bootstrap::bootstrap;
use super::lm::{log_prompt, log_response, parse_lm_response, LmAction, LmResponse};
use super::prompt::{build_incremental_prompt, build_prompt, build_retry_prompt};
use super::types::{Attempt, Outcome, State, Status};
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
fn retry_excluded_surfaces(
    state: &mut State,
    verbose: bool,
) -> (usize, std::collections::HashMap<String, Vec<Attempt>>) {
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

    // Critique pass: validate verified surfaces before retry phase
    critique_verified_surfaces(&mut state, pack_path, lm_config, verbose)?;

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
        let retry_chunks: Vec<Vec<String>> =
            retry_ids.chunks(chunk_size).map(|c| c.to_vec()).collect();
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
                with_pty,
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
                with_pty,
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

    // Mark any remaining Pending surfaces as Excluded (retry exhausted)
    let mut final_excluded = 0;
    for entry in &mut state.entries {
        if matches!(entry.status, Status::Pending) {
            entry.status = Status::Excluded {
                reason: "Exhausted all retry passes".to_string(),
            };
            final_excluded += 1;
        }
    }
    if final_excluded > 0 && verbose {
        eprintln!(
            "\nMarked {} remaining pending surface(s) as excluded",
            final_excluded
        );
    }

    state.save(pack_path)?;

    // Determine final result - all surfaces should now be resolved
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
        result = match run_sequential_chunk(
            pack_path,
            &mut *plugin,
            state,
            &chunk_ids,
            max_cycles,
            verbose,
            context_mode,
            with_pty,
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
                        with_pty,
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
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
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
    with_pty: bool,
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
                        with_pty,
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
    with_pty: bool,
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

        for action in response.actions {
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
                                with_pty,
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
            state.save(pack_path)?;
        }

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
    with_pty: bool,
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

        for action in response.actions {
            if let Err(e) = validate_action(&action, state) {
                eprintln!("  Skipping invalid action: {}", e);
                continue;
            }
            match &action {
                LmAction::SetBaseline { .. } => baselines.push(action),
                LmAction::Test { .. } => tests.push(action),
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
                                with_pty,
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
                            Outcome::OptionError { .. } => "OptionError".to_string(),
                        }
                    );
                }
                merge_test_result(state, result);
            }
            state.save(pack_path)?;
        }

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
    // Collect verified surfaces that need critique
    let verified_ids: Vec<String> = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Verified))
        .map(|e| e.id.clone())
        .collect();

    if verified_ids.is_empty() {
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
                        entry.status = Status::Pending;
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
        super::lm::LmAction::SetBaseline { args, .. } => {
            format!("SetBaseline args={:?}", args)
        }
        super::lm::LmAction::Test {
            surface_id, args, ..
        } => {
            format!("Test {} args={:?}", surface_id, args)
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
    }
}
