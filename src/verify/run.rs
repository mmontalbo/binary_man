//! Unified work-stealing verification pipeline.
//!
//! Work items flow through stages in priority order:
//! Verify > ExtractChunk
//!
//! Each worker pulls the highest-priority item available, ensuring surfaces
//! push through to verification ASAP while extraction only happens when
//! workers have nothing else to do.

use super::apply::{
    apply_action, merge_probe_result, merge_test_result, run_probe_scenario, run_test_scenario,
    ScenarioContext,
};
use super::characterize::characterize_pending_surfaces;
use super::bootstrap::{
    add_surfaces_to_state, apply_batch_probe_hits, batch_probe_surfaces,
    build_extraction_prompt, build_state_from_surfaces, parse_extraction_response,
    parse_surfaces_from_help, prepare_extraction, probe_validate_surfaces, save_surface_cache,
    DiscoveredSurface,
};
use super::lm::{
    log_prompt, log_raw_response, log_response, parse_lm_response, LmAction, LmResponse,
};
use super::prompt::{build_incremental_prompt, build_prompt, build_retry_prompt};
use super::types::{Attempt, Outcome, State, Status, SurfaceCategory, SurfaceEntry};
use super::validate::{normalize_action, validate_action};
use crate::cli::ContextMode;
use crate::lm::{create_plugin, LmConfig, LmPlugin};
use anyhow::{anyhow, Context, Result};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::config::{
    BATCH_SIZE, CHECKPOINT_INTERVAL, GLOBAL_FAILURE_THRESHOLD, LM_TIMEOUT_SECS, MAX_ATTEMPTS,
    MAX_LM_RETRIES, STAGNATION_THRESHOLD,
};

/// Result of a verification run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResult {
    /// All surfaces verified or excluded.
    Complete,
    /// Reached max_cycles limit.
    HitMaxCycles,
}

/// Timing breakdown returned from execute_cycle.
#[derive(Default)]
struct CycleTiming {
    lm_call: Duration,
    evidence: Duration,
    critique: Duration,
    rechar: Duration,
    /// Prompt size in bytes (approximate token proxy).
    prompt_bytes: usize,
    /// Total actions returned by LM.
    actions_total: usize,
    /// Actions that failed validation and were skipped.
    actions_invalid: usize,
    /// Probes executed (including auto-promoted).
    probes_run: usize,
    /// Tests executed (including auto-promoted probes that became tests).
    tests_run: usize,
    /// Surfaces newly verified this cycle.
    verified_delta: usize,
    /// Tests that returned OutputsEqual.
    outcomes_equal: usize,
    /// Tests that returned SetupFailed.
    outcomes_setup_failed: usize,
}

impl std::ops::AddAssign<&CycleTiming> for CycleTiming {
    fn add_assign(&mut self, rhs: &CycleTiming) {
        self.lm_call += rhs.lm_call;
        self.evidence += rhs.evidence;
        self.critique += rhs.critique;
        self.rechar += rhs.rechar;
        self.prompt_bytes += rhs.prompt_bytes;
        self.actions_total += rhs.actions_total;
        self.actions_invalid += rhs.actions_invalid;
        self.probes_run += rhs.probes_run;
        self.tests_run += rhs.tests_run;
        self.verified_delta += rhs.verified_delta;
        self.outcomes_equal += rhs.outcomes_equal;
        self.outcomes_setup_failed += rhs.outcomes_setup_failed;
    }
}

/// Cumulative timing per worker thread.
struct WorkerTimings {
    /// Accumulated from each execute_cycle call.
    cycle: CycleTiming,
    state_clone: Duration,
    lock_wait: Duration,
    merge: Duration,
    extract: Duration,
    verify_cycles: u32,
    extract_chunks: u32,
}

impl WorkerTimings {
    fn new() -> Self {
        WorkerTimings {
            cycle: CycleTiming::default(),
            state_clone: Duration::ZERO,
            lock_wait: Duration::ZERO,
            merge: Duration::ZERO,
            extract: Duration::ZERO,
            verify_cycles: 0,
            extract_chunks: 0,
        }
    }

    fn total_wall(&self) -> Duration {
        self.cycle.lm_call
            + self.cycle.evidence
            + self.cycle.critique
            + self.cycle.rechar
            + self.state_clone
            + self.lock_wait
            + self.merge
            + self.extract
    }
}

/// Stable configuration shared across the pipeline.
///
/// Bundles the 5 params that are threaded unchanged through
/// `run_pipeline` → `run_pipeline_worker` → `execute_cycle`.
struct VerifyContext<'a> {
    pack_path: &'a Path,
    lm_config: &'a LmConfig,
    verbose: bool,
    context_mode: ContextMode,
    with_pty: bool,
}

/// Work item in the unified pipeline.
///
/// One worker is reserved for extraction while chunks remain (and other
/// workers are verifying), preventing large man pages from starving the
/// extraction pipeline. Otherwise verify takes priority.
#[derive(Debug)]
enum WorkItem {
    /// Extract surfaces from a help text chunk and probe-validate them.
    ExtractChunk {
        chunk_index: usize,
        chunk_text: String,
    },
    /// Verify a batch of surfaces (execute_cycle).
    Verify { surface_ids: Vec<String> },
}

/// Shared pipeline coordination state.
///
/// Single `Arc<Mutex<PipelineState>>` — lock held only for short queue ops.
/// All LM calls, probing, and scenario execution happen outside the lock.
struct PipelineState {
    /// The canonical verification state (grows incrementally).
    state: State,
    /// Work queue for extraction items (verify generated on demand).
    work_queue: VecDeque<WorkItem>,
    /// Surfaces currently being worked on by a worker.
    in_progress: HashSet<String>,
    /// Surfaces resolved (verified or excluded).
    resolved: HashSet<String>,
    /// Total attempt count per surface across all workers.
    attempt_counts: HashMap<String, usize>,
    /// Total non-verified outcomes per surface across all workers.
    global_failures: HashMap<String, usize>,
    /// Number of workers currently performing extraction.
    extracting_count: usize,
    /// Number of extraction chunks completed.
    chunks_completed: usize,
    /// Total number of extraction chunks.
    chunks_total: usize,
    /// Cycle number at last checkpoint save.
    last_checkpoint_cycle: u32,
    /// Whether batch probe has already been run.
    batch_probed: bool,
    /// Last global cycle that produced at least one verification.
    last_verified_cycle: u32,
}

impl PipelineState {
    /// Claim the next work item.
    ///
    /// Verify items are generated on demand from surface state.
    /// ExtractChunk items come from the explicit queue.
    ///
    /// Worker reservation: when no worker is currently extracting and chunks
    /// remain, one worker is reserved for extraction. With a single worker,
    /// extraction is prioritized to discover surfaces before verifying.
    fn claim_work(&mut self) -> Option<WorkItem> {
        // Reserve one worker for extraction when chunks remain.
        // With multiple workers this interleaves with verification;
        // with a single worker this ensures extraction isn't starved.
        if self.extracting_count == 0 && self.chunks_completed < self.chunks_total {
            if let Some(idx) = self
                .work_queue
                .iter()
                .position(|item| matches!(item, WorkItem::ExtractChunk { .. }))
            {
                self.extracting_count += 1;
                return self.work_queue.remove(idx);
            }
        }

        // Primary: Verify — surfaces ready for verification
        let mut verify_candidates: Vec<(usize, usize, String)> = self
            .state
            .entries
            .iter()
            .filter(|e| {
                matches!(e.status, Status::Pending)
                    && !self.in_progress.contains(&e.id)
                    && !self.resolved.contains(&e.id)
                    && *self.attempt_counts.get(&e.id).unwrap_or(&0) < MAX_ATTEMPTS
                    && *self.global_failures.get(&e.id).unwrap_or(&0) < GLOBAL_FAILURE_THRESHOLD
            })
            .map(|e| {
                let global_attempts = *self.attempt_counts.get(&e.id).unwrap_or(&0);
                (
                    category_priority(&e.category, &self.state),
                    global_attempts,
                    e.id.clone(),
                )
            })
            .collect();
        verify_candidates.sort_by_key(|(p, a, _)| (*p, *a));

        if !verify_candidates.is_empty() {
            // Solo promotion: surfaces with 3+ attempts get dedicated batches
            if let Some(pos) = verify_candidates.iter().position(|(_, a, _)| *a >= 3) {
                let (_, _, solo_id) = verify_candidates.remove(pos);
                self.in_progress.insert(solo_id.clone());
                return Some(WorkItem::Verify {
                    surface_ids: vec![solo_id],
                });
            }

            // Dynamic batch size: smaller batches when only hard surfaces remain
            let min_attempts = verify_candidates
                .iter()
                .map(|(_, a, _)| *a)
                .min()
                .unwrap_or(0);
            let batch_size = if min_attempts >= 2 {
                BATCH_SIZE.min(3)
            } else {
                BATCH_SIZE
            };

            let batch: Vec<String> = verify_candidates
                .into_iter()
                .take(batch_size)
                .map(|(_, _, id)| id)
                .collect();

            for id in &batch {
                self.in_progress.insert(id.clone());
            }
            return Some(WorkItem::Verify { surface_ids: batch });
        }

        // Fallback: ExtractChunk items from queue
        if let Some(idx) = self
            .work_queue
            .iter()
            .position(|item| matches!(item, WorkItem::ExtractChunk { .. }))
        {
            self.extracting_count += 1;
            return self.work_queue.remove(idx);
        }

        None
    }

    /// Check if there is any remaining work (in queue, in progress, or potential).
    fn has_remaining_work(&self) -> bool {
        if self.chunks_completed < self.chunks_total {
            return true;
        }
        if !self.work_queue.is_empty() {
            return true;
        }
        if !self.in_progress.is_empty() {
            return true;
        }
        self.state.entries.iter().any(|e| {
            matches!(e.status, Status::Pending) && !self.resolved.contains(&e.id)
        })
    }

    /// Merge worker results back into canonical state.
    ///
    /// Returns true if any surface was newly verified this cycle.
    fn merge_worker_results(
        &mut self,
        surface_ids: &[String],
        worker_state: &State,
        cycle: u32,
    ) -> bool {
        let mut any_verified = false;

        for id in surface_ids {
            self.in_progress.remove(id);

            let Some(worker_entry) =
                worker_state.entries.iter().find(|e| &e.id == id)
            else {
                continue;
            };

            // Update attempt/failure counts
            let prev_attempts =
                *self.attempt_counts.get(id).unwrap_or(&0);
            let new_attempts = worker_entry.attempts.len();
            *self.attempt_counts.entry(id.clone()).or_insert(0) =
                prev_attempts.max(new_attempts);

            let new_failures = worker_entry
                .attempts
                .iter()
                .skip(prev_attempts)
                .filter(|a| !matches!(a.outcome, Outcome::Verified { .. }))
                .count();
            if new_failures > 0 {
                *self.global_failures.entry(id.clone()).or_insert(0) +=
                    new_failures;
            }

            // Merge into canonical state
            let total = *self.attempt_counts.get(id).unwrap_or(&0);
            let failures =
                *self.global_failures.get(id).unwrap_or(&0);
            let mut mark_resolved = false;

            if let Some(state_entry) =
                self.state.entries.iter_mut().find(|e| &e.id == id)
            {
                let worker_resolved =
                    !matches!(worker_entry.status, Status::Pending);
                let state_resolved =
                    !matches!(state_entry.status, Status::Pending);

                if worker_resolved && !state_resolved {
                    state_entry.status = worker_entry.status.clone();
                    state_entry.attempts =
                        worker_entry.attempts.clone();
                    state_entry.probes =
                        worker_entry.probes.clone();
                    state_entry.retried = worker_entry.retried;
                    state_entry.critique_demotions =
                        worker_entry.critique_demotions;
                    state_entry.critique_feedback =
                        worker_entry.critique_feedback.clone();
                    mark_resolved = true;
                } else if !state_resolved {
                    if worker_entry.attempts.len()
                        > state_entry.attempts.len()
                    {
                        state_entry.attempts =
                            worker_entry.attempts.clone();
                    }
                    if worker_entry.probes.len()
                        > state_entry.probes.len()
                    {
                        state_entry.probes =
                            worker_entry.probes.clone();
                    }
                    // Merge critique state (take highest demotion count)
                    if worker_entry.critique_demotions
                        > state_entry.critique_demotions
                    {
                        state_entry.critique_demotions =
                            worker_entry.critique_demotions;
                        state_entry.critique_feedback =
                            worker_entry.critique_feedback.clone();
                    }
                    // Post-merge auto-exclude (adds stagnation check
                    // the old inline code was missing)
                    if try_auto_exclude(state_entry, total, failures) {
                        mark_resolved = true;
                    }
                }

                // Merge characterization updates from rechar
                if let Some(ref wc) = worker_entry.characterization {
                    if state_entry
                        .characterization
                        .as_ref()
                        .is_none_or(|sc| sc.revision < wc.revision)
                    {
                        state_entry.characterization =
                            Some(wc.clone());
                    }
                }

                if matches!(worker_entry.status, Status::Verified) {
                    any_verified = true;
                }
            }

            if mark_resolved {
                self.resolved.insert(id.clone());
            }
        }

        // Merge seed bank
        for seed in &worker_state.seed_bank {
            if !self
                .state
                .seed_bank
                .iter()
                .any(|s| s.surface_id == seed.surface_id)
            {
                self.state.seed_bank.push(seed.clone());
            }
        }

        if any_verified {
            self.last_verified_cycle = cycle;
        }

        any_verified
    }
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

/// Try to auto-exclude a surface based on global attempt/failure counts and stagnation.
///
/// Returns true if the surface was excluded. Call sites:
/// - Pre-execution (worker snapshot): quick-reject before LM call
/// - Post-merge (canonical state): final check after merging worker results
fn try_auto_exclude(
    entry: &mut SurfaceEntry,
    global_attempts: usize,
    global_failures: usize,
) -> bool {
    if !matches!(entry.status, Status::Pending) || entry.has_verified_attempt() {
        return false;
    }
    if global_attempts >= MAX_ATTEMPTS {
        entry.status = Status::Excluded {
            reason: format!("Exhausted after {} attempts", global_attempts),
        };
        return true;
    }
    if entry.is_stagnant() {
        entry.status = Status::Excluded {
            reason: format!(
                "Stagnant ({} consecutive OutputsEqual)",
                STAGNATION_THRESHOLD,
            ),
        };
        return true;
    }
    if global_failures >= GLOBAL_FAILURE_THRESHOLD {
        entry.status = Status::Excluded {
            reason: format!("Globally hopeless ({} failures)", global_failures),
        };
        return true;
    }
    false
}

/// Emit a compact progress line for eval harness streaming.
fn emit_progress(
    ps: &PipelineState,
    cycle: u32,
    cycle_timing: Option<&CycleTiming>,
    surface_ids: &[String],
) {
    let v = ps
        .state
        .entries
        .iter()
        .filter(|e| matches!(e.status, Status::Verified))
        .count();
    let t = ps.state.entries.len();
    let ch = ps.chunks_completed;
    let ct = ps.chunks_total;
    // Count pred_blocked surfaces and break down by channel match
    let mut pb = 0u32;
    let mut pb_chan_ok = 0u32; // right channel, wrong content
    let mut pb_chan_miss = 0u32; // wrong channel entirely
    for e in &ps.state.entries {
        if !matches!(e.status, Status::Pending) {
            continue;
        }
        let blocked = e.attempts.iter().any(|a| {
            matches!(a.outcome, Outcome::Verified { .. })
                && a.prediction_matched == Some(false)
        });
        if blocked {
            pb += 1;
            // Check channel match on the most recent pred_blocked attempt
            if let Some(a) = e.attempts.iter().rev().find(|a| {
                matches!(a.outcome, Outcome::Verified { .. })
                    && a.prediction_matched == Some(false)
            }) {
                match a.prediction_channel_matched {
                    Some(true) => pb_chan_ok += 1,
                    Some(false) => pb_chan_miss += 1,
                    None => {} // legacy data, no channel info
                }
            }
        }
    }
    let stall_len = if ps.last_verified_cycle > 0 {
        cycle.saturating_sub(ps.last_verified_cycle)
    } else {
        0
    };
    // Per-cycle action accounting from the most recent cycle
    let (ct_lm_ms, ct_ev_ms, ct_prompt_kb, ct_actions, ct_invalid, ct_probes, ct_tests,
        ct_vdelta, ct_oe, ct_sf) = if let Some(ctr) = cycle_timing
    {
        (
            ctr.lm_call.as_millis() as u64,
            ctr.evidence.as_millis() as u64,
            ctr.prompt_bytes / 1024,
            ctr.actions_total,
            ctr.actions_invalid,
            ctr.probes_run,
            ctr.tests_run,
            ctr.verified_delta,
            ctr.outcomes_equal,
            ctr.outcomes_setup_failed,
        )
    } else {
        (0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    };
    let targets_str = surface_ids.join(",");
    eprintln!(
        "PROGRESS: cycle={} verified={}/{} chunks={}/{} pred_blocked={} \
         pb_chan_ok={} pb_chan_miss={} \
         stall={} lm_ms={} ev_ms={} prompt_kb={} actions={} invalid={} \
         probes={} tests={} vdelta={} oe={} sf={} targets={}",
        cycle, v, t, ch, ct, pb, pb_chan_ok, pb_chan_miss, stall_len, ct_lm_ms, ct_ev_ms,
        ct_prompt_kb, ct_actions, ct_invalid, ct_probes, ct_tests, ct_vdelta, ct_oe, ct_sf,
        targets_str,
    );
}

/// Run the verification loop.
///
/// This is the main entry point. Uses the unified pipeline where extraction,
/// characterization, and verification flow through a single priority queue.
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

    // Load existing state or prepare fresh extraction
    let (mut state, prep) = if pack_path.join("state.json").exists() {
        if verbose {
            eprintln!("Loading existing state from {}", pack_path.display());
        }
        (State::load(pack_path)?, None)
    } else {
        if verbose {
            eprintln!("Bootstrapping new state for {}", binary);
        }
        let prep = prepare_extraction(binary, context_argv, Some(pack_path), verbose)?;
        let state = if let Some(ref cached) = prep.cached_surfaces {
            // Cache hit — build full state from cached surfaces
            build_state_from_surfaces(binary, context_argv, cached.clone(), &prep.help_outputs)?
        } else {
            // No cache — seed state from regex extraction.
            // LM extraction chunks will add more surfaces via the pipeline.
            let mut regex_surfaces = Vec::new();
            for output in &prep.help_outputs {
                for surface in parse_surfaces_from_help(output) {
                    regex_surfaces.push(surface);
                }
            }
            build_state_from_surfaces(binary, context_argv, regex_surfaces, &prep.help_outputs)?
        };

        if verbose {
            eprintln!("Discovered {} surfaces", state.entries.len());
        }
        (state, Some(prep))
    };

    // Batch probe: auto-verify surfaces that show differing output mechanically
    let mut batch_verified_ids = Vec::new();
    if state.seed_bank.is_empty() && state.cycle == 0 {
        eprintln!(
            "PROGRESS: phase=batch_probe surfaces={}",
            state.entries.len()
        );
        let hits = batch_probe_surfaces(&state, pack_path, verbose);
        batch_verified_ids = apply_batch_probe_hits(&mut state, hits, verbose);
    }

    // Upfront characterization: analyze pending surfaces before pipeline starts.
    // Produces trigger + expected_diff for each uncharacterized surface so the
    // verification LM has a specification to build seeds against.
    let uncharacterized = state
        .entries
        .iter()
        .filter(|e| {
            matches!(e.status, Status::Pending)
                && e.characterization.is_none()
                && e.attempts.is_empty()
        })
        .count();
    if uncharacterized > 0 {
        eprintln!("PROGRESS: phase=characterize pending={}", uncharacterized);
        characterize_pending_surfaces(&mut state, pack_path, lm_config, verbose)?;
        eprintln!("PROGRESS: phase=characterize_done");
    }

    // Critique batch-probe-verified surfaces
    if !batch_verified_ids.is_empty() {
        eprintln!("PROGRESS: phase=batch_critique surfaces={}", batch_verified_ids.len());
        super::critique::critique_surfaces(&mut state, pack_path, lm_config, verbose, &batch_verified_ids)?;
        eprintln!("PROGRESS: phase=batch_critique_done");
    }

    // Stamp experiment params for traceability
    state.experiment_params = Some(super::config::experiment_params());

    // Save initial state
    state.save(pack_path)?;

    // Determine number of workers
    let num_workers = if parallel_sessions {
        let total = state.entries.len().max(10);
        let base = if session_size > 0 {
            total.div_ceil(session_size).clamp(1, 8)
        } else {
            1
        };
        // When extraction chunks remain, use chunks+1 workers so all
        // chunks extract in parallel while one worker verifies.
        if let Some(p) = prep.as_ref() {
            if p.cached_surfaces.is_none() && !p.chunks.is_empty() {
                base.max(p.chunks.len() + 1).min(8)
            } else {
                base
            }
        } else {
            base
        }
    } else {
        1
    };

    // Run unified pipeline
    let ctx = VerifyContext {
        pack_path,
        lm_config,
        verbose,
        context_mode,
        with_pty,
    };
    let result = run_pipeline(&mut state, prep, max_cycles, num_workers, &ctx);

    // Mark remaining Pending surfaces as Excluded (or recover verified ones)
    let mut final_excluded = 0;
    let mut final_recovered = 0;
    for entry in &mut state.entries {
        if matches!(entry.status, Status::Pending) {
            if entry.has_verified_attempt() {
                entry.status = Status::Verified;
                final_recovered += 1;
            } else {
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
    }
    if verbose && (final_excluded > 0 || final_recovered > 0) {
        if final_recovered > 0 {
            eprintln!(
                "\nRecovered {} pending surface(s) to Verified (had verified attempt)",
                final_recovered,
            );
        }
        if final_excluded > 0 {
            eprintln!(
                "\nMarked {} remaining pending surface(s) as excluded",
                final_excluded,
            );
        }
    }

    state.save(pack_path)?;

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

/// Run the unified work-stealing pipeline.
///
/// All work items (extraction, characterization, verification) flow through
/// a single priority queue. Workers pull the highest-priority item available.
fn run_pipeline(
    state: &mut State,
    prep: Option<super::bootstrap::ExtractionPrep>,
    max_cycles: u32,
    num_workers: usize,
    ctx: &VerifyContext<'_>,
) -> RunResult {
    let binary = state.binary.clone();
    let context_argv = state.context_argv.clone();

    // Initialize pipeline state
    let chunks_total = prep.as_ref().map_or(0, |p| p.chunks.len());
    let mut pipeline_state = PipelineState {
        state: state.clone(),
        work_queue: VecDeque::new(),
        in_progress: HashSet::new(),
        resolved: HashSet::new(),
        attempt_counts: HashMap::new(),
        global_failures: HashMap::new(),
        extracting_count: 0,
        chunks_completed: 0,
        chunks_total,
        last_checkpoint_cycle: state.cycle,
        batch_probed: !state.seed_bank.is_empty(),
        last_verified_cycle: 0,
    };

    // Pre-populate resolved set from existing state (resumed runs)
    for entry in &pipeline_state.state.entries {
        if !matches!(entry.status, Status::Pending) {
            pipeline_state.resolved.insert(entry.id.clone());
        }
    }

    // Seed extraction chunks into the work queue
    let should_save_cache;
    if let Some(ref prep) = prep {
        if prep.cached_surfaces.is_some() {
            pipeline_state.chunks_completed = pipeline_state.chunks_total;
            should_save_cache = false;
        } else {
            for (idx, chunk_text) in prep.chunks.iter().enumerate() {
                pipeline_state.work_queue.push_back(WorkItem::ExtractChunk {
                    chunk_index: idx,
                    chunk_text: chunk_text.clone(),
                });
            }
            should_save_cache = true;
        }
    } else {
        pipeline_state.chunks_completed = pipeline_state.chunks_total;
        should_save_cache = false;
    }

    if ctx.verbose {
        let pending = pipeline_state
            .state
            .entries
            .iter()
            .filter(|e| matches!(e.status, Status::Pending))
            .count();
        eprintln!(
            "\nPipeline: {} worker(s), {} surfaces ({} pending), {} extraction chunk(s), {} max cycles",
            num_workers,
            pipeline_state.state.entries.len(),
            pending,
            pipeline_state.chunks_total,
            max_cycles,
        );
    }

    // Quick exit if nothing to do
    if !pipeline_state.has_remaining_work() {
        *state = pipeline_state.state;
        return RunResult::Complete;
    }

    let pipeline = Arc::new((Mutex::new(pipeline_state), Condvar::new()));
    let global_cycle = Arc::new(AtomicU32::new(state.cycle));

    // Spawn workers
    let pipeline_start = Instant::now();
    let all_timings: Vec<WorkerTimings> = thread::scope(|s| {
        let handles: Vec<_> = (0..num_workers)
            .map(|worker_idx| {
                let pipeline = Arc::clone(&pipeline);
                let global_cycle = Arc::clone(&global_cycle);
                let binary = binary.clone();
                let context_argv = context_argv.clone();

                s.spawn(move || {
                    run_pipeline_worker(
                        worker_idx,
                        &pipeline,
                        &global_cycle,
                        &binary,
                        &context_argv,
                        max_cycles,
                        ctx,
                    )
                })
            })
            .collect();

        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .collect()
    });
    let pipeline_wall = pipeline_start.elapsed();

    // Extract final state
    {
        let ps = pipeline.0.lock().unwrap();
        *state = ps.state.clone();
    }

    // Save surface cache if we did fresh extraction
    if should_save_cache {
        if let Some(ref prep) = prep {
            let surfaces: Vec<DiscoveredSurface> = state
                .entries
                .iter()
                .map(|e| DiscoveredSurface {
                    id: e.id.clone(),
                    description: e.description.clone(),
                    context: e.context.clone(),
                    value_hint: e.value_hint.clone(),
                })
                .collect();
            save_surface_cache(ctx.pack_path, &prep.help_hash, &surfaces);
        }
    }

    if ctx.verbose {
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
            "\nPipeline complete: {} verified, {} excluded, {} pending",
            verified, excluded, pending
        );

        // Print timing breakdown
        print_timing_summary(&all_timings, pipeline_wall);
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

/// Print aggregated timing breakdown to stderr.
fn print_timing_summary(worker_timings: &[WorkerTimings], wall: Duration) {
    fn fmt_dur(d: Duration) -> String {
        let s = d.as_secs_f64();
        if s < 0.1 {
            format!("{:.1}ms", s * 1000.0)
        } else {
            format!("{:.1}s", s)
        }
    }

    fn pct(part: Duration, total: Duration) -> String {
        if total.is_zero() {
            return "—".to_string();
        }
        format!("{:.0}%", part.as_secs_f64() / total.as_secs_f64() * 100.0)
    }

    // Aggregate across workers
    let mut total = WorkerTimings::new();
    let mut total_cycles: u32 = 0;
    let mut total_extracts: u32 = 0;
    for wt in worker_timings {
        total.cycle += &wt.cycle;
        total.state_clone += wt.state_clone;
        total.lock_wait += wt.lock_wait;
        total.merge += wt.merge;
        total.extract += wt.extract;
        total_cycles += wt.verify_cycles;
        total_extracts += wt.extract_chunks;
    }

    let accounted = total.total_wall();

    eprintln!("\n  Timing breakdown (wall: {}):", fmt_dur(wall));
    eprintln!(
        "    LM calls (verify):   {:>8}  {}",
        fmt_dur(total.cycle.lm_call),
        pct(total.cycle.lm_call, wall)
    );
    eprintln!(
        "    Evidence execution:  {:>8}  {}",
        fmt_dur(total.cycle.evidence),
        pct(total.cycle.evidence, wall)
    );
    eprintln!(
        "    Critique:            {:>8}  {}",
        fmt_dur(total.cycle.critique),
        pct(total.cycle.critique, wall)
    );
    eprintln!(
        "    Recharacterize (LM): {:>8}  {}",
        fmt_dur(total.cycle.rechar),
        pct(total.cycle.rechar, wall)
    );
    eprintln!(
        "    Extract (LM):        {:>8}  {}",
        fmt_dur(total.extract),
        pct(total.extract, wall)
    );
    eprintln!(
        "    State clone:         {:>8}  {}",
        fmt_dur(total.state_clone),
        pct(total.state_clone, wall)
    );
    eprintln!(
        "    Lock wait:           {:>8}  {}",
        fmt_dur(total.lock_wait),
        pct(total.lock_wait, wall)
    );
    eprintln!(
        "    Merge:               {:>8}  {}",
        fmt_dur(total.merge),
        pct(total.merge, wall)
    );
    let unaccounted = if wall > accounted {
        wall - accounted
    } else {
        Duration::ZERO
    };
    eprintln!(
        "    Unaccounted:         {:>8}  {}",
        fmt_dur(unaccounted),
        pct(unaccounted, wall)
    );
    eprintln!(
        "    Work items: {} verify, {} extract",
        total_cycles, total_extracts
    );
}

/// Run a single pipeline worker.
///
/// Each worker has its own LM plugin. One worker is reserved for extraction
/// while chunks remain; otherwise verify takes priority.
fn run_pipeline_worker(
    worker_idx: usize,
    pipeline: &(Mutex<PipelineState>, Condvar),
    global_cycle: &AtomicU32,
    binary: &str,
    context_argv: &[String],
    max_cycles: u32,
    ctx: &VerifyContext<'_>,
) -> WorkerTimings {
    let mut timings = WorkerTimings::new();
    let (lock, condvar) = pipeline;
    let w = worker_idx + 1;

    let mut plugin = create_plugin(ctx.lm_config);
    if let Err(e) = plugin.init() {
        eprintln!("  W{}: failed to init LM: {}", w, e);
        return timings;
    }

    let mut last_response: Option<LmResponse> = None;
    let mut last_verify_cycle: u32 = 0;
    let mut stall_resets: u32 = 0;

    loop {
        // Claim work under lock (with condvar wait if nothing available)
        let lock_t0 = Instant::now();
        let work = {
            let mut ps = lock.lock().unwrap();
            loop {
                if let Some(item) = ps.claim_work() {
                    break Some(item);
                }
                if !ps.has_remaining_work() {
                    break None;
                }
                let (new_ps, _) = condvar
                    .wait_timeout(ps, Duration::from_secs(5))
                    .unwrap();
                ps = new_ps;
            }
        };

        timings.lock_wait += lock_t0.elapsed();

        let work = match work {
            Some(w) => w,
            None => break,
        };

        match work {
            WorkItem::ExtractChunk {
                chunk_index,
                chunk_text,
            } => {
                if ctx.verbose {
                    eprintln!("  W{}: extracting chunk {}", w, chunk_index);
                }

                // Build prompt and call LM (outside lock)
                let ext_t0 = Instant::now();
                let prompt = build_extraction_prompt(binary, context_argv, &chunk_text);
                let response = invoke_lm_with_retry(&mut *plugin, &prompt, ctx.verbose);

                let mut surfaces = match response {
                    Ok(text) => parse_extraction_response(&text).unwrap_or_default(),
                    Err(e) => {
                        if ctx.verbose {
                            eprintln!(
                                "  W{}: extraction chunk {} failed: {}",
                                w, chunk_index, e
                            );
                        }
                        vec![]
                    }
                };

                // Probe-validate (outside lock)
                if !surfaces.is_empty() {
                    if ctx.verbose {
                        eprintln!(
                            "  W{}: probe-validating {} candidates",
                            w,
                            surfaces.len()
                        );
                    }
                    surfaces =
                        probe_validate_surfaces(binary, context_argv, surfaces, ctx.verbose);
                }

                timings.extract += ext_t0.elapsed();
                timings.extract_chunks += 1;

                // Add to shared state (under lock)
                let should_batch_probe;
                {
                    let merge_t0 = Instant::now();
                    let mut ps = lock.lock().unwrap();
                    let before = ps.state.entries.len();
                    add_surfaces_to_state(&mut ps.state, surfaces);
                    ps.chunks_completed += 1;
                    ps.extracting_count -= 1;
                    let added = ps.state.entries.len() - before;
                    if ctx.verbose && added > 0 {
                        eprintln!(
                            "  W{}: chunk {} added {} surfaces ({}/{})",
                            w, chunk_index, added, ps.chunks_completed, ps.chunks_total,
                        );
                    }
                    // Trigger batch probe after extraction adds surfaces
                    should_batch_probe = added > 0
                        && !ps.batch_probed
                        && ps.state.seed_bank.is_empty();
                    if should_batch_probe {
                        ps.batch_probed = true;
                    }
                    condvar.notify_all();
                    timings.merge += merge_t0.elapsed();
                }

                // Run batch probe outside the lock (I/O heavy)
                if should_batch_probe {
                    let state_snapshot = lock.lock().unwrap().state.clone();
                    let hits = batch_probe_surfaces(&state_snapshot, ctx.pack_path, ctx.verbose);
                    if !hits.is_empty() {
                        let mut ps = lock.lock().unwrap();
                        apply_batch_probe_hits(&mut ps.state, hits, ctx.verbose);
                        condvar.notify_all();
                    }
                }
            }

            WorkItem::Verify { surface_ids } => {
                // Claim cycle number
                let cycle = global_cycle.fetch_add(1, Ordering::SeqCst) + 1;
                if cycle > max_cycles {
                    global_cycle.fetch_sub(1, Ordering::SeqCst);
                    let mut ps = lock.lock().unwrap();
                    for id in &surface_ids {
                        ps.in_progress.remove(id);
                    }
                    condvar.notify_all();
                    if ctx.verbose {
                        eprintln!("  W{}: hit max cycles ({})", w, max_cycles);
                    }
                    break;
                }

                if ctx.verbose {
                    eprintln!(
                        "  W{} cycle {}: {}",
                        w, cycle,
                        surface_ids.join(", ")
                    );
                }

                // Snapshot state for this cycle
                let clone_t0 = Instant::now();
                let (mut worker_state, stall_cycles) = {
                    let ps = lock.lock().unwrap();
                    let sc = if ps.last_verified_cycle > 0 {
                        cycle.saturating_sub(ps.last_verified_cycle)
                    } else {
                        0
                    };
                    (ps.state.clone(), sc)
                };
                timings.state_clone += clone_t0.elapsed();
                worker_state.cycle = cycle;

                // Pre-stagnation recharacterization (outside lock)
                let rechar_t0 = Instant::now();
                {
                    let rechar_candidates: Vec<String> = worker_state
                        .entries
                        .iter()
                        .filter(|e| {
                            surface_ids.contains(&e.id)
                                && matches!(e.status, Status::Pending)
                                && e.needs_recharacterization()
                        })
                        .map(|e| e.id.clone())
                        .collect();
                    for id in &rechar_candidates {
                        super::characterize::recharacterize_surface(
                            &mut worker_state,
                            ctx.pack_path,
                            ctx.lm_config,
                            ctx.verbose,
                            id,
                        )
                        .ok();
                    }
                }

                timings.cycle.rechar += rechar_t0.elapsed();

                // Auto-exhaust check (peek at shared counters)
                {
                    let ps = lock.lock().unwrap();
                    for entry in &mut worker_state.entries {
                        if surface_ids.contains(&entry.id) {
                            let ga = *ps.attempt_counts.get(&entry.id).unwrap_or(&0);
                            let gf = *ps.global_failures.get(&entry.id).unwrap_or(&0);
                            try_auto_exclude(entry, ga, gf);
                        }
                    }
                }

                // Filter to still-pending surfaces
                let active_ids: Vec<String> = surface_ids
                    .iter()
                    .filter(|id| {
                        worker_state
                            .entries
                            .iter()
                            .any(|e| &e.id == *id && matches!(e.status, Status::Pending))
                    })
                    .cloned()
                    .collect();

                let mut cycle_timing: Option<CycleTiming> = None;
                if !active_ids.is_empty() {
                    // Execute verification cycle (outside lock)
                    if let Ok(ct) = execute_cycle(
                        &mut *plugin,
                        &mut worker_state,
                        &active_ids,
                        None,
                        &mut last_response,
                        stall_cycles,
                        ctx,
                    ) {
                        timings.cycle += &ct;
                        cycle_timing = Some(ct);
                    }
                    timings.verify_cycles += 1;
                }

                // Publish results back to shared state (under lock)
                {
                    let merge_t0 = Instant::now();
                    let mut ps = lock.lock().unwrap();
                    let any_verified =
                        ps.merge_worker_results(&surface_ids, &worker_state, cycle);

                    if any_verified {
                        last_verify_cycle = cycle;
                    }

                    // Periodic checkpoint
                    if cycle.saturating_sub(ps.last_checkpoint_cycle)
                        >= CHECKPOINT_INTERVAL
                    {
                        ps.last_checkpoint_cycle = cycle;
                        ps.state.cycle = cycle;
                        if let Err(e) = ps.state.save(ctx.pack_path) {
                            if ctx.verbose {
                                eprintln!("  Checkpoint save failed: {}", e);
                            }
                        } else if ctx.verbose {
                            let verified = ps
                                .state
                                .entries
                                .iter()
                                .filter(|e| matches!(e.status, Status::Verified))
                                .count();
                            eprintln!(
                                "  Checkpoint at cycle {}: {} verified",
                                cycle, verified
                            );
                        }
                    }

                    if cycle.is_multiple_of(5) || any_verified {
                        emit_progress(&ps, cycle, cycle_timing.as_ref(), &surface_ids);
                    }

                    condvar.notify_all();
                    timings.merge += merge_t0.elapsed();
                }

                // Stall detection
                if last_verify_cycle > 0 && cycle - last_verify_cycle >= 10 {
                    stall_resets += 1;
                    if stall_resets >= 2 {
                        if ctx.verbose {
                            eprintln!(
                                "  W{}: winding down ({} resets with no progress)",
                                w, stall_resets,
                            );
                        }
                        break;
                    }
                    if ctx.verbose {
                        eprintln!(
                            "  W{}: stalled, resetting LM (reset {}/2)",
                            w, stall_resets,
                        );
                    }
                    plugin.reset().ok();
                    last_response = None;
                    last_verify_cycle = cycle;
                }
            }
        }
    }

    plugin.shutdown().ok();
    if ctx.verbose {
        eprintln!("  W{}: done", w);
    }
    timings
}

/// Run a list of independent scenarios in parallel, collecting successful results.
///
/// Each param is moved into a spawned thread and passed to `run_fn`.
/// Failures and panics are logged with `label` and filtered out.
fn run_scenarios_parallel<P: Send, R: Send>(
    params: Vec<P>,
    run_fn: impl Fn(P) -> Result<R> + Send + Sync,
    label: &str,
) -> Vec<R> {
    thread::scope(|s| {
        let run_fn = &run_fn;
        let handles: Vec<_> = params
            .into_iter()
            .map(|p| s.spawn(move || run_fn(p)))
            .collect();
        handles
            .into_iter()
            .filter_map(|h| match h.join() {
                Ok(Ok(r)) => Some(r),
                Ok(Err(e)) => {
                    eprintln!("  {} failed: {}", label, e);
                    None
                }
                Err(_) => {
                    eprintln!("  {} panicked", label);
                    None
                }
            })
            .collect()
    })
}

/// Run batches of LM prompts in parallel, each with its own plugin instance.
///
/// Each batch gets a fresh LM plugin. On success, the response text is passed to
/// `parse_fn` along with the batch IDs to produce typed results. Failed batches
/// produce empty results.
pub(crate) fn run_parallel_lm_batches<T: Send>(
    batches: Vec<(Vec<String>, String)>,
    lm_config: &LmConfig,
    verbose: bool,
    label: &str,
    parse_fn: impl Fn(&str, &[String]) -> Vec<T> + Send + Sync,
) -> Vec<Vec<T>> {
    thread::scope(|s| {
        let parse_fn = &parse_fn;
        let handles: Vec<_> = batches
            .into_iter()
            .map(|(batch_ids, prompt)| {
                s.spawn(move || -> Vec<T> {
                    let mut plugin = create_plugin(lm_config);
                    if let Err(e) = plugin.init() {
                        if verbose {
                            eprintln!("  {} batch init failed: {}", label, e);
                        }
                        return vec![];
                    }

                    let response_text = match invoke_lm_with_retry(&mut *plugin, &prompt, verbose) {
                        Ok(text) => text,
                        Err(e) => {
                            if verbose {
                                eprintln!(
                                    "  {} LM failed for batch {:?}: {}",
                                    label,
                                    &batch_ids[..batch_ids.len().min(3)],
                                    e
                                );
                            }
                            plugin.shutdown().ok();
                            return vec![];
                        }
                    };

                    plugin.shutdown().ok();
                    parse_fn(&response_text, &batch_ids)
                })
            })
            .collect();

        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
}

/// Invoke LM with retry logic.
///
/// Retries up to MAX_LM_RETRIES times, resetting the plugin on the second-to-last attempt.
pub(crate) fn invoke_lm_with_retry(
    plugin: &mut dyn LmPlugin,
    prompt: &str,
    verbose: bool,
) -> Result<String> {
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

/// Run probe scenarios in parallel, auto-promote differing probes to formal tests.
/// Returns surface IDs newly verified via auto-promotion.
fn execute_probes(
    state: &mut State,
    probes: Vec<LmAction>,
    ctx: &VerifyContext<'_>,
    timing: &mut CycleTiming,
) -> Vec<String> {
    let mut newly_verified = Vec::new();
    if probes.is_empty() {
        return newly_verified;
    }

    if ctx.verbose {
        eprintln!("  Running {} probe(s) in parallel...", probes.len());
    }

    let probe_params: Vec<_> = probes
        .into_iter()
        .filter_map(|action| {
            if let LmAction::Probe {
                surface_id,
                extra_args,
                seed,
            } = action
            {
                let vh = state
                    .find_entry(&surface_id)
                    .and_then(|e| e.value_hint.clone());
                Some((surface_id, extra_args, seed, vh))
            } else {
                None
            }
        })
        .collect();

    let probe_results = {
        let sc = ScenarioContext {
            pack_path: ctx.pack_path,
            binary: &state.binary,
            context_argv: &state.context_argv,
            cycle: state.cycle,
            with_pty: ctx.with_pty,
        };
        run_scenarios_parallel(
            probe_params,
            |(surface_id, extra_args, seed, vh)| {
                run_probe_scenario(&sc, &surface_id, extra_args, seed, vh.as_deref())
            },
            "Probe",
        )
    };

    timing.probes_run = probe_results.len();
    let mut auto_promote = Vec::new();
    for result in probe_results {
        if ctx.verbose {
            let status = if result.setup_failed {
                "SetupFailed".to_string()
            } else if result.outputs_differ {
                "DIFFER (auto-promote)".to_string()
            } else {
                match result.exit_code {
                    Some(0) => "identical".to_string(),
                    Some(c) => format!("exit {}", c),
                    None => "NoExit".to_string(),
                }
            };
            eprintln!("  Probe {} → {}", result.surface_id, status);
        }
        if result.outputs_differ && !result.setup_failed {
            let vh = state
                .find_entry(&result.surface_id)
                .and_then(|e| e.value_hint.clone());
            auto_promote.push((
                result.surface_id.clone(),
                result.extra_args.clone(),
                result.seed.clone(),
                vh,
            ));
        }
        merge_probe_result(state, result);
    }

    if !auto_promote.is_empty() {
        if ctx.verbose {
            eprintln!(
                "  Auto-promoting {} probe(s) to tests...",
                auto_promote.len()
            );
        }
        let promote_results = {
            let sc = ScenarioContext {
                pack_path: ctx.pack_path,
                binary: &state.binary,
                context_argv: &state.context_argv,
                cycle: state.cycle,
                with_pty: ctx.with_pty,
            };
            run_scenarios_parallel(
                auto_promote,
                |(surface_id, extra_args, seed, vh)| {
                    run_test_scenario(
                        &sc,
                        &surface_id,
                        extra_args,
                        seed,
                        sc.with_pty,
                        None,
                        vh.as_deref(),
                    )
                },
                "Auto-promote",
            )
        };

        timing.tests_run += promote_results.len();
        for result in promote_results {
            let is_verified = matches!(result.outcome, Outcome::Verified { .. });
            if ctx.verbose {
                eprintln!(
                    "  Auto-promoted {} → {:?}",
                    result.surface_id,
                    if is_verified {
                        "Verified"
                    } else {
                        "not verified"
                    }
                );
            }
            if is_verified {
                newly_verified.push(result.surface_id.clone());
            }
            merge_test_result(state, result);
        }
    }

    newly_verified
}

/// Run test scenarios in parallel and merge results.
/// Returns (newly_verified_ids, newly_equal_ids).
fn execute_tests(
    state: &mut State,
    tests: Vec<LmAction>,
    ctx: &VerifyContext<'_>,
    timing: &mut CycleTiming,
) -> (Vec<String>, Vec<String>) {
    if ctx.verbose {
        eprintln!("  Running {} test(s) in parallel...", tests.len());
    }

    let test_params: Vec<_> = tests
        .into_iter()
        .filter_map(|action| {
            if let LmAction::Test {
                surface_id,
                extra_args,
                seed,
                prediction,
                ..
            } = action
            {
                let surface_pty = ctx.with_pty
                    || state.entries.iter().any(|e| {
                        e.id == surface_id
                            && matches!(e.category, SurfaceCategory::TtyDependent)
                    });
                let vh = state
                    .find_entry(&surface_id)
                    .and_then(|e| e.value_hint.clone());
                Some((surface_id, extra_args, seed, prediction, surface_pty, vh))
            } else {
                None
            }
        })
        .collect();

    let results = {
        let sc = ScenarioContext {
            pack_path: ctx.pack_path,
            binary: &state.binary,
            context_argv: &state.context_argv,
            cycle: state.cycle,
            with_pty: ctx.with_pty,
        };
        run_scenarios_parallel(
            test_params,
            |(surface_id, extra_args, seed, prediction, surface_pty, vh)| {
                run_test_scenario(
                    &sc,
                    &surface_id,
                    extra_args,
                    seed,
                    surface_pty,
                    prediction,
                    vh.as_deref(),
                )
            },
            "Test",
        )
    };

    timing.tests_run += results.len();
    let mut newly_verified = Vec::new();
    let mut newly_equal = Vec::new();
    for result in results {
        let is_verified = matches!(result.outcome, Outcome::Verified { .. });
        let is_equal = matches!(result.outcome, Outcome::OutputsEqual);
        if is_equal {
            timing.outcomes_equal += 1;
        }
        if matches!(result.outcome, Outcome::SetupFailed { .. }) {
            timing.outcomes_setup_failed += 1;
        }
        if ctx.verbose {
            eprintln!(
                "  {} → {:?}",
                result.surface_id,
                match &result.outcome {
                    Outcome::Verified { diff_kind } => format!("Verified ({:?})", diff_kind),
                    Outcome::OutputsEqual => "OutputsEqual".to_string(),
                    Outcome::SetupFailed { .. } => "SetupFailed".to_string(),
                    Outcome::Crashed { .. } => "Crashed".to_string(),
                    Outcome::ExecutionError { .. } => "ExecutionError".to_string(),
                    Outcome::OptionError { .. } => "OptionError".to_string(),
                }
            );
        }
        if is_verified {
            newly_verified.push(result.surface_id.clone());
        }
        if is_equal {
            newly_equal.push(result.surface_id.clone());
        }
        merge_test_result(state, result);
    }

    (newly_verified, newly_equal)
}

/// Execute a single verification cycle for the given pending surfaces.
///
/// Builds the prompt, invokes the LM, parses the response, and executes
/// the resulting test actions. Updates `state` in place.
fn execute_cycle(
    plugin: &mut dyn LmPlugin,
    state: &mut State,
    pending_ids: &[String],
    prior_attempts: Option<&std::collections::HashMap<String, Vec<Attempt>>>,
    last_response: &mut Option<LmResponse>,
    stall_cycles: u32,
    ctx: &VerifyContext<'_>,
) -> Result<CycleTiming> {
    let mut timing = CycleTiming::default();
    // Resolve auto mode based on plugin type
    let effective_mode = match ctx.context_mode {
        ContextMode::Auto if plugin.is_stateful() => ContextMode::Incremental,
        ContextMode::Auto => ContextMode::Full,
        other => other,
    };

    // Build prompt based on effective context mode
    let mut prompt = match effective_mode {
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

    // Inject stall hint when the pipeline has gone multiple cycles without
    // any verification (global counter, not per-worker).
    if stall_cycles >= 3 {
        prompt.push_str(&format!(
            "\n## Stall Warning\nLast {} cycles produced zero new verifications. \
             Focus on untried surfaces and consider different seed strategies.\n\n",
            stall_cycles
        ));
    }

    timing.prompt_bytes = prompt.len();
    log_prompt(ctx.pack_path, state.cycle, &prompt)?;

    if ctx.verbose {
        eprintln!("  Invoking LM...");
    }
    let t0 = Instant::now();
    let response_text = invoke_lm_with_retry(plugin, &prompt, ctx.verbose)?;
    timing.lm_call += t0.elapsed();

    // Parse response — on failure, log raw text and reset session immediately
    // rather than sending reminders into a poisoned conversation.
    let response = match parse_lm_response(&response_text) {
        Ok(r) => r,
        Err(e) => {
            if ctx.verbose {
                eprintln!("  Parse error: {}", e);
            }
            log_raw_response(ctx.pack_path, state.cycle, &response_text).ok();
            plugin.reset().ok();
            *last_response = None;
            return Ok(timing);
        }
    };
    log_raw_response(ctx.pack_path, state.cycle, &response_text).ok();
    log_response(ctx.pack_path, state.cycle, &response)?;

    // Reset mode: reset plugin after each successful call
    if matches!(effective_mode, ContextMode::Reset) && plugin.is_stateful() {
        if ctx.verbose {
            eprintln!("  Resetting LM session (context_mode=reset)");
        }
        plugin.reset()?;
    }

    // Handle empty response
    if response.actions.is_empty() {
        if ctx.verbose {
            eprintln!("  LM returned no actions, resetting session");
        }
        plugin.reset().ok();
        *last_response = None;
        return Ok(timing);
    }

    if ctx.verbose {
        eprintln!("  LM returned {} action(s)", response.actions.len());
    }

    // Partition and validate actions
    timing.actions_total = response.actions.len();
    let mut baselines = Vec::new();
    let mut probes = Vec::new();
    let mut tests = Vec::new();
    let mut valid_actions = Vec::new();

    for action in response.actions {
        // Normalize action to handle --option=value and -Uvalue formats
        let action = normalize_action(action, state);

        if let Err(e) = validate_action(&action, state, Some(pending_ids)) {
            eprintln!("  Skipping invalid action: {}", e);
            timing.actions_invalid += 1;
            continue;
        }
        valid_actions.push(action.clone());
        match &action {
            LmAction::SetBaseline { .. } => baselines.push(action),
            LmAction::Probe { .. } => probes.push(action),
            LmAction::Test { .. } => tests.push(action),
        }
    }

    // Build clean response for incremental prompt tracking (exclude rejected actions)
    let response_for_tracking = LmResponse {
        analysis: response.analysis.clone(),
        actions: valid_actions,
    };

    // 1. Apply baselines first (must complete before probes/tests)
    for action in baselines {
        if ctx.verbose {
            eprintln!("  Applying: {}", format_action_desc(&action));
        }
        if let Err(e) = apply_action(state, ctx.pack_path, action) {
            eprintln!("  Action failed: {}", e);
        }
    }

    // 2. Run probes in parallel (bilateral comparison + auto-promote)
    let evidence_start = Instant::now();
    let mut newly_verified = execute_probes(state, probes, ctx, &mut timing);

    // 3. Run tests in parallel
    if !tests.is_empty() {
        let (test_verified, newly_equal) = execute_tests(state, tests, ctx, &mut timing);
        newly_verified.extend(test_verified);
        timing.evidence += evidence_start.elapsed();

        // Inline critique: immediately review newly verified surfaces.
        if !newly_verified.is_empty() {
            let t0 = Instant::now();
            super::critique::critique_surfaces(
                state,
                ctx.pack_path,
                ctx.lm_config,
                ctx.verbose,
                &newly_verified,
            )?;
            timing.critique += t0.elapsed();
        }

        for surface_id in &newly_equal {
            let needs_rechar = state
                .find_entry(surface_id)
                .is_some_and(|e| e.needs_recharacterization());
            if needs_rechar {
                let t0 = Instant::now();
                super::characterize::recharacterize_surface(
                    state, ctx.pack_path, ctx.lm_config, ctx.verbose, surface_id,
                )?;
                timing.rechar += t0.elapsed();
            }
        }
    }

    // Track verified count
    timing.verified_delta = newly_verified.len();

    // Track response for incremental mode
    *last_response = Some(response_for_tracking);

    Ok(timing)
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
        super::lm::LmAction::Probe {
            surface_id,
            extra_args,
            ..
        } => {
            if extra_args.is_empty() {
                format!("Probe {}", surface_id)
            } else {
                format!("Probe {} +{:?}", surface_id, extra_args)
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
                    probes: vec![],
                    attempts: vec![],
                    category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                    critique_demotions: 0,
                    characterization: None,
                },
                SurfaceEntry {
                    id: "-b".to_string(),
                    description: "B".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Pending,
                    probes: vec![],
                    attempts: vec![],
                    category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                    critique_demotions: 0,
                    characterization: None,
                },
                SurfaceEntry {
                    id: "-c".to_string(),
                    description: "C".to_string(),
                    context: None,
                    value_hint: None,
                    status: Status::Excluded {
                        reason: "test".to_string(),
                    },
                    probes: vec![],
                    attempts: vec![],
                    category: SurfaceCategory::General,
                    retried: false,
                    critique_feedback: None,
                    critique_demotions: 0,
                    characterization: None,
                },
            ],
            cycle: 5,
            seed_bank: vec![],
            help_preamble: String::new(),
            examples_section: String::new(),
            experiment_params: None,
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
