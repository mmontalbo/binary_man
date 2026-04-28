use std::collections::HashSet;

use crate::compare;
use crate::summary::{RunOutcome, Summary};
use crate::{Args, GitInfo};

/// Print progress for a single completed run.
pub fn print_run_progress(run: &RunOutcome, idx: usize, total: usize, label: &str) {
    let verified = run.surfaces.values().filter(|s| s.verified).count();
    let surfaces = run.surfaces.len();
    let mut parts = vec![
        format!("{}/{} verified", verified, surfaces),
        format!("{} cycles", run.cycles),
        format!("{:.1}s", run.elapsed_seconds),
    ];
    if run.crashed {
        parts.push("CRASHED".to_string());
    }
    if run.timed_out {
        parts.push("TIMED OUT".to_string());
    }
    if run.lm_stats.rate_limits > 0 {
        parts.push(format!("{} rate-limits", run.lm_stats.rate_limits));
    }
    if run.lm_stats.lm_retries > 0 {
        parts.push(format!("{} retries", run.lm_stats.lm_retries));
    }
    if run.progress_stats.prediction_blocked > 0 {
        parts.push(format!("{} pred-blocked", run.progress_stats.prediction_blocked));
    }
    if let Some(ref ed) = run.progress_stats.extract_done {
        parts.push(format!("extract: {}surf {:.0}s", ed.surfaces, ed.elapsed_ms as f64 / 1000.0));
    }
    eprintln!("[{}] Run {}/{}... {}", label, idx + 1, total, parts.join(", "));
}

/// Show surface manifest variance across runs (if manifests differ).
pub fn show_surface_variance(runs: &[RunOutcome]) {
    let manifests: Vec<&Vec<String>> = runs
        .iter()
        .map(|r| &r.progress_stats.surface_manifest)
        .collect();

    // Skip if no manifests were captured
    if manifests.iter().all(|m| m.is_empty()) {
        return;
    }

    let counts: Vec<usize> = manifests.iter().map(|m| m.len()).collect();
    let all_same = counts.windows(2).all(|w| w[0] == w[1])
        && manifests.windows(2).all(|w| {
            let a: HashSet<&String> = w[0].iter().collect();
            let b: HashSet<&String> = w[1].iter().collect();
            a == b
        });

    if all_same {
        eprintln!(
            "\n  Surface manifest: {} surfaces (consistent across {} runs)",
            counts[0], runs.len()
        );
    } else {
        eprintln!("\n  Surface manifest variance:");
        for (i, m) in manifests.iter().enumerate() {
            eprintln!("    Run {}: {} surfaces", i + 1, m.len());
        }
        // Show which surfaces differ
        let union: HashSet<&String> = manifests.iter().flat_map(|m| m.iter()).collect();
        let intersection: HashSet<&String> = if manifests.is_empty() {
            HashSet::new()
        } else {
            let first: HashSet<&String> = manifests[0].iter().collect();
            manifests[1..].iter().fold(first, |acc, m| {
                let s: HashSet<&String> = m.iter().collect();
                acc.intersection(&s).copied().collect()
            })
        };
        let only_some: Vec<&&String> = union.difference(&intersection).collect();
        if !only_some.is_empty() {
            eprintln!(
                "    Common: {}  Varying: {}",
                intersection.len(),
                only_some.len()
            );
            for sid in &only_some {
                let in_runs: Vec<usize> = manifests
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| m.contains(sid))
                    .map(|(i, _)| i + 1)
                    .collect();
                eprintln!(
                    "      {} (in runs: {})",
                    sid,
                    in_runs.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(",")
                );
            }
        }
    }
}

/// Display metrics for a single version (no comparison).
pub fn show_standalone(current: &Summary, git: &GitInfo, args: &Args, json: bool) {
    if json {
        let output = serde_json::json!({
            "git": { "commit": git.commit, "subject": git.subject, "dirty": git.dirty },
            "binary": args.binary,
            "entry_point": args.entry_point,
            "max_cycles": args.max_cycles,
            "summary": current,
        });
        let _ = serde_json::to_writer_pretty(std::io::stdout(), &output);
        println!();
        return;
    }

    let total = current.total_surfaces;
    let rate = if total > 0 {
        current.mean_verified / total as f64 * 100.0
    } else {
        0.0
    };

    eprintln!("\n{}", "=".repeat(60));
    eprintln!(
        "  eval: {} {}",
        args.binary,
        args.entry_point.join(" ")
    );
    eprintln!(
        "  commit: {}{}  ({})",
        git.commit,
        if git.dirty { "*" } else { "" },
        git.subject
    );
    eprintln!(
        "  runs: {}  surfaces: {}  max-cycles: {}",
        current.runs, total, args.max_cycles
    );
    if let Some(ref note) = args.note {
        eprintln!("  note: {}", note);
    }
    eprintln!("{}", "=".repeat(60));

    eprintln!(
        "\n  Mean verified: {:.1}/{} ({:.1}%)",
        current.mean_verified, total, rate
    );
    eprintln!("  Mean excluded: {:.1}/{}", current.mean_excluded, total);
    eprintln!("  Mean cycles:   {:.1}", current.mean_cycles);
    eprintln!("  Mean elapsed:  {:.0}s", current.mean_elapsed);

    // Throughput
    let reached_pct = if total > 0 {
        current.mean_reached / total as f64 * 100.0
    } else {
        0.0
    };
    eprintln!("\n  Throughput:");
    eprintln!(
        "    Surfaces reached:    {:.1}/{} ({:.0}%)",
        current.mean_reached, total, reached_pct
    );
    eprintln!(
        "    Total attempts:      {:.1}",
        current.mean_total_attempts
    );
    eprintln!(
        "    Attempts/cycle:      {:.2}",
        current.mean_attempts_per_cycle
    );
    eprintln!(
        "    Hit rate (V/reached):{:5.1}%",
        current.mean_hit_rate * 100.0
    );

    let eff = compute_efficiency(current);
    if let Some(median) = eff.median_first_verify_cycle {
        eprintln!("    Median 1st-verify:   cycle {:.1}", median);
    }
    eprintln!("    Waste ratio:         {:.1}%", eff.waste_ratio_pct);

    if current.crashed > 0 || current.timed_out > 0 {
        eprintln!(
            "\n  Crashed: {}  Timed out: {}",
            current.crashed, current.timed_out
        );
    }

    // Pipeline stats
    if current.max_prediction_blocked > 0 || current.max_stall > 0
        || current.mean_actions_per_cycle > 0.0
    {
        eprintln!("\n  Pipeline stats:");
        if current.max_prediction_blocked > 0 {
            eprintln!(
                "    Prediction blocked:  {} (max across runs)",
                current.max_prediction_blocked
            );
        }
        if current.max_stall > 0 {
            eprintln!(
                "    Max stall:           {} cycles (longest dry streak)",
                current.max_stall
            );
        }
        if current.mean_actions_per_cycle > 0.0 {
            eprintln!(
                "    Actions/cycle:       {:.1} avg ({:.0}% waste)",
                current.mean_actions_per_cycle,
                current.mean_action_waste_rate * 100.0
            );
        }
        if current.mean_prompt_kb > 0.0 {
            eprintln!(
                "    Prompt size:         {:.0}KB avg/cycle",
                current.mean_prompt_kb
            );
        }
    }

    // Extraction stats
    if current.mean_extract_ms > 0.0 {
        eprintln!("\n  Extraction:");
        eprintln!(
            "    Mean time:           {:.1}s",
            current.mean_extract_ms / 1000.0
        );
        if current.extract_surface_min != current.extract_surface_max {
            eprintln!(
                "    Surface count:       {}-{} (variance across runs)",
                current.extract_surface_min, current.extract_surface_max
            );
        } else if current.extract_surface_min > 0 {
            eprintln!(
                "    Surface count:       {} (consistent)",
                current.extract_surface_min
            );
        }
    }

    // LM stats
    let lm = &current.lm_stats;
    if lm.has_issues() {
        eprintln!("\n  LM issues (across {} runs):", current.runs);
        if lm.rate_limits > 0 {
            eprintln!("    Rate limits:  {}", lm.rate_limits);
        }
        if lm.lm_retries > 0 {
            eprintln!("    LM retries:   {}", lm.lm_retries);
        }
        if lm.timeouts > 0 {
            eprintln!("    Timeouts:     {}", lm.timeouts);
        }
    }

    // Surfaces below 100%
    let mut interesting: Vec<(&String, &crate::summary::SurfaceStats)> = current
        .per_surface
        .iter()
        .filter(|(_, s)| s.verification_rate < 1.0)
        .collect();

    if !interesting.is_empty() {
        interesting.sort_by(|a, b| {
            a.1.verification_rate
                .partial_cmp(&b.1.verification_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        eprintln!("\n  Surfaces below 100% verification rate:");
        for (sid, s) in &interesting {
            eprintln!(
                "    {:40} {:5.1}% ({}/{})",
                sid,
                s.verification_rate * 100.0,
                s.verified_count,
                current.runs
            );
        }
    }

    eprintln!();
}

/// Display statistical comparison between current and baseline.
pub fn show_comparison(current: &Summary, baseline: &Summary, json: bool) {
    let result = compare::compare(baseline, current);

    if json {
        let _ = serde_json::to_writer_pretty(std::io::stdout(), &result);
        println!();
        return;
    }

    let flips = &result.flips;
    let wilcoxon = &result.wilcoxon;
    let mcnemar = &result.mcnemar;
    let verdict = &result.verdict;

    eprintln!("\n{}", "=".repeat(60));
    eprintln!("  COMPARISON");
    eprintln!("{}", "=".repeat(60));
    eprintln!(
        "\n  Runs: baseline={}, current={}",
        baseline.runs, current.runs
    );

    let b_total = baseline.total_surfaces;
    let c_total = current.total_surfaces;
    let b_rate = if b_total > 0 {
        baseline.mean_verified / b_total as f64 * 100.0
    } else {
        0.0
    };
    let c_rate = if c_total > 0 {
        current.mean_verified / c_total as f64 * 100.0
    } else {
        0.0
    };

    eprintln!(
        "\n  Verification rate: {:.1}% -> {:.1}% ({:+.1}pp)",
        b_rate,
        c_rate,
        c_rate - b_rate
    );
    eprintln!(
        "  Surfaces: {} common, {} new, {} removed",
        flips.common_count,
        flips.new_surfaces.len(),
        flips.removed_surfaces.len()
    );

    if !flips.stable_gains.is_empty() {
        eprintln!("\n  Stable gains (0% -> 100%):");
        for sid in &flips.stable_gains {
            eprintln!("    + {}", sid);
        }
    }

    if !flips.stable_losses.is_empty() {
        eprintln!("\n  Stable losses (100% -> 0%):");
        for sid in &flips.stable_losses {
            eprintln!("    - {}", sid);
        }
    }

    if !flips.fragile.is_empty() {
        eprintln!("\n  Rate changes:");
        for f in &flips.fragile {
            eprintln!(
                "    ~ {:40} {:.0}% -> {:.0}%",
                f.id,
                f.baseline_rate * 100.0,
                f.current_rate * 100.0
            );
        }
    }

    eprintln!("\n  Statistics:");
    eprintln!(
        "    Wilcoxon: W+={:.0} W-={:.0} z={:.3} p={:.4} (n={} non-tied)",
        wilcoxon.w_plus, wilcoxon.w_minus, wilcoxon.z, wilcoxon.p, wilcoxon.n_non_tied
    );
    eprintln!(
        "    McNemar:  chi2={:.3} p={:.4} (baseline-only={} current-only={})",
        mcnemar.chi2, mcnemar.p, mcnemar.a_only, mcnemar.b_only
    );

    let label = match verdict.verdict.as_str() {
        "improvement" => "IMPROVEMENT",
        "regression" => "REGRESSION",
        "significant_below_threshold" => "SIGNIFICANT (below flip threshold)",
        "not_significant" => "NOT SIGNIFICANT",
        other => other,
    };
    eprintln!("\n  Verdict: {}", label);
    eprintln!(
        "    net stable flips: {:+} (threshold: {})",
        verdict.net_stable_flips, verdict.threshold
    );
    eprintln!();
}

struct Efficiency {
    median_first_verify_cycle: Option<f64>,
    waste_ratio_pct: f64,
}

fn compute_efficiency(summary: &Summary) -> Efficiency {
    let mut first_cycles: Vec<f64> = summary
        .per_surface
        .values()
        .filter_map(|s| s.mean_first_verify_cycle)
        .collect();

    let median = if first_cycles.is_empty() {
        None
    } else {
        first_cycles.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = first_cycles.len() / 2;
        if first_cycles.len().is_multiple_of(2) && first_cycles.len() > 1 {
            Some((first_cycles[mid - 1] + first_cycles[mid]) / 2.0)
        } else {
            Some(first_cycles[mid])
        }
    };

    let total_attempts: f64 = summary.per_surface.values().map(|s| s.mean_attempts).sum();
    let verified_attempts: f64 = summary
        .per_surface
        .values()
        .filter(|s| s.verification_rate > 0.0)
        .map(|s| s.mean_attempts)
        .sum();

    let waste_ratio = if total_attempts > 0.0 {
        (total_attempts - verified_attempts) / total_attempts * 100.0
    } else {
        0.0
    };

    Efficiency {
        median_first_verify_cycle: median,
        waste_ratio_pct: (waste_ratio * 10.0).round() / 10.0,
    }
}
