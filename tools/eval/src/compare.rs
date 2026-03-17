use std::collections::HashSet;

use serde::Serialize;

use crate::stats::{self, McNemarResult, WilcoxonResult};
use crate::summary::Summary;

#[derive(Debug, Clone, Serialize)]
pub struct Flips {
    pub stable_gains: Vec<String>,
    pub stable_losses: Vec<String>,
    pub fragile: Vec<FragileFlip>,
    pub new_surfaces: Vec<String>,
    pub removed_surfaces: Vec<String>,
    pub common_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FragileFlip {
    pub id: String,
    pub baseline_rate: f64,
    pub current_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub verdict: String,
    pub p: f64,
    pub net_stable_flips: i64,
    pub threshold: usize,
    pub significant: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComparisonResult {
    pub flips: Flips,
    pub wilcoxon: WilcoxonResult,
    pub mcnemar: McNemarResult,
    pub verdict: Verdict,
}

/// Compare per-surface verification rates between baseline and current.
pub fn classify_flips(baseline: &Summary, current: &Summary) -> Flips {
    let b_keys: HashSet<&str> = baseline.per_surface.keys().map(String::as_str).collect();
    let c_keys: HashSet<&str> = current.per_surface.keys().map(String::as_str).collect();

    let mut common: Vec<&str> = b_keys.intersection(&c_keys).copied().collect();
    common.sort();

    let mut new_surfaces: Vec<String> = c_keys
        .difference(&b_keys)
        .map(|s| (*s).to_string())
        .collect();
    new_surfaces.sort();

    let mut removed_surfaces: Vec<String> = b_keys
        .difference(&c_keys)
        .map(|s| (*s).to_string())
        .collect();
    removed_surfaces.sort();

    let mut stable_gains = Vec::new();
    let mut stable_losses = Vec::new();
    let mut fragile = Vec::new();

    for sid in &common {
        let b_rate = baseline.per_surface[*sid].verification_rate;
        let c_rate = current.per_surface[*sid].verification_rate;

        if (b_rate - c_rate).abs() < f64::EPSILON {
            continue;
        }
        if b_rate == 0.0 && c_rate == 1.0 {
            stable_gains.push(sid.to_string());
        } else if b_rate == 1.0 && c_rate == 0.0 {
            stable_losses.push(sid.to_string());
        } else {
            fragile.push(FragileFlip {
                id: sid.to_string(),
                baseline_rate: b_rate,
                current_rate: c_rate,
            });
        }
    }

    Flips {
        stable_gains,
        stable_losses,
        fragile,
        new_surfaces,
        removed_surfaces,
        common_count: common.len(),
    }
}

/// Decision rule: p < 0.05 AND net_stable_flips >= max(5% of surfaces, 3).
pub fn compute_verdict(
    wilcoxon: &WilcoxonResult,
    flips: &Flips,
    total_surfaces: usize,
) -> Verdict {
    let p = wilcoxon.p;
    let net_stable = flips.stable_gains.len() as i64 - flips.stable_losses.len() as i64;
    let threshold = std::cmp::max((total_surfaces as f64 * 0.05) as usize, 3);
    let significant = p < 0.05;

    let verdict = if significant && net_stable >= threshold as i64 {
        "improvement"
    } else if significant && net_stable <= -(threshold as i64) {
        "regression"
    } else if significant {
        "significant_below_threshold"
    } else {
        "not_significant"
    };

    Verdict {
        verdict: verdict.to_string(),
        p,
        net_stable_flips: net_stable,
        threshold,
        significant,
    }
}

/// Run full comparison analysis between baseline and current summaries.
pub fn compare(baseline: &Summary, current: &Summary) -> ComparisonResult {
    let flips = classify_flips(baseline, current);

    // Build paired data for Wilcoxon: per-surface verification rates
    let b_keys: HashSet<&str> = baseline.per_surface.keys().map(String::as_str).collect();
    let c_keys: HashSet<&str> = current.per_surface.keys().map(String::as_str).collect();
    let common: Vec<&str> = b_keys.intersection(&c_keys).copied().collect();

    let pairs: Vec<(f64, f64)> = common
        .iter()
        .map(|sid| {
            (
                baseline.per_surface[*sid].verification_rate,
                current.per_surface[*sid].verification_rate,
            )
        })
        .collect();

    let wilcoxon = stats::wilcoxon_signed_rank(&pairs);
    let verdict = compute_verdict(&wilcoxon, &flips, common.len());

    // McNemar's: surfaces verified in one but not the other
    let mut b_only = 0usize;
    let mut c_only = 0usize;
    for sid in &common {
        let b_v = baseline.per_surface[*sid].verification_rate > 0.0;
        let c_v = current.per_surface[*sid].verification_rate > 0.0;
        if b_v && !c_v {
            b_only += 1;
        } else if c_v && !b_v {
            c_only += 1;
        }
    }
    let mcnemar = stats::mcnemar_test(b_only, c_only);

    ComparisonResult {
        flips,
        wilcoxon,
        mcnemar,
        verdict,
    }
}
