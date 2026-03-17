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
    /// Counts of LM-related events parsed from stderr.
    pub lm_stats: LmStats,
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
