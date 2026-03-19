use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunOutcome {
    pub run_index: usize,
    pub elapsed_seconds: f64,
    pub cycles: u32,
    pub timed_out: bool,
    pub crashed: bool,
    pub surfaces: HashMap<String, SurfaceOutcome>,
    /// Captured stderr from the bman process.
    #[serde(skip)]
    pub stderr: String,
    /// Raw state.json content from the run's tmpdir (preserved for post-hoc analysis).
    #[serde(skip)]
    pub state_json: Option<String>,
    /// Counts of LM-related events parsed from stderr.
    pub lm_stats: LmStats,
    /// Pipeline progress stats parsed from PROGRESS lines.
    pub progress_stats: ProgressStats,
}

/// Counts of LM-related events parsed from bman stderr.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LmStats {
    /// Number of LM retry attempts (transient failures).
    pub lm_retries: usize,
    /// Number of rate-limit / 429 / overloaded signals.
    pub rate_limits: usize,
    /// Number of timeout / channel errors.
    pub timeouts: usize,
    /// Total LM errors (retries + final failures).
    pub errors: usize,
}

impl LmStats {
    /// Parse stderr output for LM-related events.
    pub fn from_stderr(stderr: &str) -> Self {
        let mut stats = Self::default();
        for line in stderr.lines() {
            let lower = line.to_lowercase();
            if lower.contains("lm retry") || lower.contains("retrying lm") {
                stats.lm_retries += 1;
                stats.errors += 1;
            }
            if lower.contains("rate") && lower.contains("limit")
                || lower.contains("429")
                || lower.contains("overloaded")
                || lower.contains("too many requests")
                || lower.contains("rate_limit")
            {
                stats.rate_limits += 1;
            }
            if lower.contains("timeout") || lower.contains("timed out")
                || lower.contains("channel error")
            {
                stats.timeouts += 1;
                stats.errors += 1;
            }
        }
        stats
    }

    pub fn has_issues(&self) -> bool {
        self.lm_retries > 0 || self.rate_limits > 0 || self.timeouts > 0
    }
}

/// Pipeline progress stats parsed from PROGRESS lines in stderr.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProgressStats {
    /// Max prediction-blocked count observed across PROGRESS lines.
    pub prediction_blocked: usize,
    /// Max consecutive stall cycles (cycles since last verification).
    pub max_stall: u32,
    /// Per-cycle records parsed from PROGRESS lines.
    pub cycles: Vec<CycleRecord>,
    /// Surface IDs from SURFACES: line (the manifest discovered by this run).
    pub surface_manifest: Vec<String>,
    /// Extraction timing from EXTRACT_DONE line.
    #[serde(default)]
    pub extract_done: Option<ExtractDone>,
}

/// Extraction completion metrics from EXTRACT_DONE line.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractDone {
    pub chunks: usize,
    pub surfaces: usize,
    pub elapsed_ms: u64,
}

/// Per-cycle metrics parsed from a single PROGRESS line.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CycleRecord {
    pub cycle: u32,
    pub verified: usize,
    pub total: usize,
    pub lm_ms: u64,
    pub evidence_ms: u64,
    pub prompt_kb: usize,
    pub actions: usize,
    pub invalid: usize,
    pub probes: usize,
    pub tests: usize,
    pub verified_delta: usize,
    pub outputs_equal: usize,
    pub setup_failed: usize,
    pub stall: u32,
    /// Surface IDs targeted this cycle.
    #[serde(default)]
    pub targets: Vec<String>,
}

impl ProgressStats {
    /// Parse PROGRESS and SURFACES lines from stderr for structured pipeline stats.
    pub fn from_stderr(stderr: &str) -> Self {
        let mut stats = Self::default();
        for line in stderr.lines() {
            if let Some(rest) = line.strip_prefix("PROGRESS:") {
                let mut rec = CycleRecord::default();
                for part in rest.split_whitespace() {
                    if let Some((key, val)) = part.split_once('=') {
                        match key {
                            "pred_blocked" => {
                                if let Ok(n) = val.parse::<usize>() {
                                    stats.prediction_blocked = stats.prediction_blocked.max(n);
                                }
                            }
                            "cycle" => { rec.cycle = val.parse().unwrap_or(0); }
                            "stall" => {
                                rec.stall = val.parse().unwrap_or(0);
                                stats.max_stall = stats.max_stall.max(rec.stall);
                            }
                            "lm_ms" => { rec.lm_ms = val.parse().unwrap_or(0); }
                            "ev_ms" => { rec.evidence_ms = val.parse().unwrap_or(0); }
                            "prompt_kb" => { rec.prompt_kb = val.parse().unwrap_or(0); }
                            "actions" => { rec.actions = val.parse().unwrap_or(0); }
                            "invalid" => { rec.invalid = val.parse().unwrap_or(0); }
                            "probes" => { rec.probes = val.parse().unwrap_or(0); }
                            "tests" => { rec.tests = val.parse().unwrap_or(0); }
                            "vdelta" => { rec.verified_delta = val.parse().unwrap_or(0); }
                            "oe" => { rec.outputs_equal = val.parse().unwrap_or(0); }
                            "sf" => { rec.setup_failed = val.parse().unwrap_or(0); }
                            "targets" => {
                                rec.targets = val.split(',')
                                    .filter(|s| !s.is_empty())
                                    .map(String::from)
                                    .collect();
                            }
                            _ => {
                                // verified=V/T
                                if key == "verified" {
                                    if let Some((v, t)) = val.split_once('/') {
                                        rec.verified = v.parse().unwrap_or(0);
                                        rec.total = t.parse().unwrap_or(0);
                                    }
                                }
                            }
                        }
                    }
                }
                stats.cycles.push(rec);
            } else if let Some(rest) = line.strip_prefix("SURFACES:") {
                // Parse surface manifest: SURFACES: count=N ids=id1,id2,...
                for part in rest.split_whitespace() {
                    if let Some((key, val)) = part.split_once('=') {
                        if key == "ids" {
                            stats.surface_manifest = val.split(',')
                                .filter(|s| !s.is_empty())
                                .map(String::from)
                                .collect();
                        }
                    }
                }
            } else if let Some(rest) = line.strip_prefix("EXTRACT_DONE:") {
                let mut ed = ExtractDone::default();
                for part in rest.split_whitespace() {
                    if let Some((key, val)) = part.split_once('=') {
                        match key {
                            "chunks" => { ed.chunks = val.parse().unwrap_or(0); }
                            "surfaces" => { ed.surfaces = val.parse().unwrap_or(0); }
                            "elapsed_ms" => { ed.elapsed_ms = val.parse().unwrap_or(0); }
                            _ => {}
                        }
                    }
                }
                stats.extract_done = Some(ed);
            }
        }
        stats
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceOutcome {
    pub verified: bool,
    pub status: String,
    pub attempts: usize,
    pub probes: usize,
    pub first_verify_cycle: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub runs: usize,
    pub total_surfaces: usize,
    pub mean_verified: f64,
    pub mean_excluded: f64,
    pub mean_cycles: f64,
    pub mean_elapsed: f64,
    pub mean_reached: f64,
    pub mean_total_attempts: f64,
    pub mean_attempts_per_cycle: f64,
    pub mean_hit_rate: f64,
    pub crashed: usize,
    pub timed_out: usize,
    pub per_surface: HashMap<String, SurfaceStats>,
    /// Aggregate LM stats across all runs.
    pub lm_stats: LmStats,
    /// Max prediction-blocked count observed across all runs.
    #[serde(default)]
    pub max_prediction_blocked: usize,
    /// Max stall length (consecutive dry cycles) across all runs.
    #[serde(default)]
    pub max_stall: u32,
    /// Mean actions per cycle across all runs.
    #[serde(default)]
    pub mean_actions_per_cycle: f64,
    /// Mean action waste rate (invalid/total) across all runs.
    #[serde(default)]
    pub mean_action_waste_rate: f64,
    /// Mean prompt size in KB per cycle across all runs.
    #[serde(default)]
    pub mean_prompt_kb: f64,
    /// Mean extraction elapsed time in ms (from EXTRACT_DONE).
    #[serde(default)]
    pub mean_extract_ms: f64,
    /// Min/max extraction surface counts across runs.
    #[serde(default)]
    pub extract_surface_min: usize,
    #[serde(default)]
    pub extract_surface_max: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceStats {
    pub verification_rate: f64,
    pub verified_count: usize,
    pub mean_attempts: f64,
    pub mean_first_verify_cycle: Option<f64>,
    pub outcomes_by_run: Vec<Option<String>>,
}

/// Aggregate per-surface verification rates across runs.
pub fn build(runs: &[RunOutcome]) -> Summary {
    let n = runs.len();
    let mut all_ids: HashSet<String> = HashSet::new();
    for r in runs {
        all_ids.extend(r.surfaces.keys().cloned());
    }

    let mut per_surface = HashMap::new();
    for sid in &all_ids {
        let mut verified_count = 0usize;
        let mut attempt_counts = Vec::new();
        let mut first_verify_cycles = Vec::new();
        let mut outcomes_by_run = Vec::new();

        for r in runs {
            match r.surfaces.get(sid) {
                Some(s) => {
                    if s.verified {
                        verified_count += 1;
                    }
                    attempt_counts.push(s.attempts);
                    if let Some(c) = s.first_verify_cycle {
                        first_verify_cycles.push(f64::from(c));
                    }
                    outcomes_by_run.push(Some(s.status.clone()));
                }
                None => outcomes_by_run.push(None),
            }
        }

        let mean_attempts = if attempt_counts.is_empty() {
            0.0
        } else {
            attempt_counts.iter().sum::<usize>() as f64 / attempt_counts.len() as f64
        };
        let mean_first_verify_cycle = if first_verify_cycles.is_empty() {
            None
        } else {
            Some(first_verify_cycles.iter().sum::<f64>() / first_verify_cycles.len() as f64)
        };

        per_surface.insert(
            sid.clone(),
            SurfaceStats {
                verification_rate: if n > 0 {
                    verified_count as f64 / n as f64
                } else {
                    0.0
                },
                verified_count,
                mean_attempts,
                mean_first_verify_cycle,
                outcomes_by_run,
            },
        );
    }

    let total_surfaces = all_ids.len();

    let verified_per_run: Vec<usize> = runs
        .iter()
        .map(|r| r.surfaces.values().filter(|s| s.verified).count())
        .collect();
    let excluded_per_run: Vec<usize> = runs
        .iter()
        .map(|r| {
            r.surfaces
                .values()
                .filter(|s| s.status == "Excluded")
                .count()
        })
        .collect();
    let reached_per_run: Vec<usize> = runs
        .iter()
        .map(|r| r.surfaces.values().filter(|s| s.attempts > 0).count())
        .collect();
    let total_attempts_per_run: Vec<usize> = runs
        .iter()
        .map(|r| r.surfaces.values().map(|s| s.attempts).sum())
        .collect();

    let cycles: Vec<f64> = runs.iter().map(|r| f64::from(r.cycles)).collect();
    let elapsed: Vec<f64> = runs.iter().map(|r| r.elapsed_seconds).collect();
    let attempts_per_cycle: Vec<f64> = runs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if r.cycles > 0 {
                total_attempts_per_run[i] as f64 / f64::from(r.cycles)
            } else {
                0.0
            }
        })
        .collect();
    let hit_rate: Vec<f64> = verified_per_run
        .iter()
        .zip(reached_per_run.iter())
        .map(|(&v, &r)| if r > 0 { v as f64 / r as f64 } else { 0.0 })
        .collect();

    let lm_stats = LmStats {
        lm_retries: runs.iter().map(|r| r.lm_stats.lm_retries).sum(),
        rate_limits: runs.iter().map(|r| r.lm_stats.rate_limits).sum(),
        timeouts: runs.iter().map(|r| r.lm_stats.timeouts).sum(),
        errors: runs.iter().map(|r| r.lm_stats.errors).sum(),
    };

    let max_prediction_blocked = runs
        .iter()
        .map(|r| r.progress_stats.prediction_blocked)
        .max()
        .unwrap_or(0);

    let max_stall = runs
        .iter()
        .map(|r| r.progress_stats.max_stall)
        .max()
        .unwrap_or(0);

    // Aggregate per-cycle action stats across all runs
    let all_cycles: Vec<&CycleRecord> = runs
        .iter()
        .flat_map(|r| r.progress_stats.cycles.iter())
        .collect();
    let total_cycle_count = all_cycles.len();
    let (mean_actions_per_cycle, mean_action_waste_rate, mean_prompt_kb) =
        if total_cycle_count > 0 {
            let total_actions: usize = all_cycles.iter().map(|c| c.actions).sum();
            let total_invalid: usize = all_cycles.iter().map(|c| c.invalid).sum();
            let total_prompt_kb: usize = all_cycles.iter().map(|c| c.prompt_kb).sum();
            (
                total_actions as f64 / total_cycle_count as f64,
                if total_actions > 0 {
                    total_invalid as f64 / total_actions as f64
                } else {
                    0.0
                },
                total_prompt_kb as f64 / total_cycle_count as f64,
            )
        } else {
            (0.0, 0.0, 0.0)
        };

    Summary {
        runs: n,
        total_surfaces,
        mean_verified: mean_usize(&verified_per_run),
        mean_excluded: mean_usize(&excluded_per_run),
        mean_cycles: mean_f64(&cycles),
        mean_elapsed: mean_f64(&elapsed),
        mean_reached: mean_usize(&reached_per_run),
        mean_total_attempts: mean_usize(&total_attempts_per_run),
        mean_attempts_per_cycle: mean_f64(&attempts_per_cycle),
        mean_hit_rate: mean_f64(&hit_rate),
        crashed: runs.iter().filter(|r| r.crashed).count(),
        timed_out: runs.iter().filter(|r| r.timed_out).count(),
        per_surface,
        lm_stats,
        max_prediction_blocked,
        max_stall,
        mean_actions_per_cycle,
        mean_action_waste_rate,
        mean_prompt_kb,
        mean_extract_ms: {
            let extract_times: Vec<f64> = runs
                .iter()
                .filter_map(|r| r.progress_stats.extract_done.as_ref())
                .map(|ed| ed.elapsed_ms as f64)
                .collect();
            mean_f64(&extract_times)
        },
        extract_surface_min: runs
            .iter()
            .filter_map(|r| r.progress_stats.extract_done.as_ref())
            .map(|ed| ed.surfaces)
            .min()
            .unwrap_or(0),
        extract_surface_max: runs
            .iter()
            .filter_map(|r| r.progress_stats.extract_done.as_ref())
            .map(|ed| ed.surfaces)
            .max()
            .unwrap_or(0),
    }
}

fn mean_usize(v: &[usize]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<usize>() as f64 / v.len() as f64
    }
}

fn mean_f64(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}
