use crate::compare;
use crate::summary::{RunOutcome, Summary};
use crate::{Args, GitInfo};

/// Print progress for a single completed run.
pub fn print_run_progress(run: &RunOutcome, idx: usize, total: usize) {
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
    eprintln!("Run {}/{}... {}", idx + 1, total, parts.join(", "));
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
