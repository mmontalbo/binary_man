use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct WilcoxonResult {
    pub w_plus: f64,
    pub w_minus: f64,
    pub z: f64,
    pub p: f64,
    pub n_pairs: usize,
    pub n_non_tied: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct McNemarResult {
    pub chi2: f64,
    pub p: f64,
    pub a_only: usize,
    pub b_only: usize,
}

/// Wilcoxon signed-rank test on paired observations.
///
/// Each pair is `(baseline_value, current_value)`. Tests whether current
/// values are systematically different from the baseline.
pub fn wilcoxon_signed_rank(pairs: &[(f64, f64)]) -> WilcoxonResult {
    let diffs: Vec<f64> = pairs
        .iter()
        .map(|&(b, c)| c - b)
        .filter(|d| d.abs() > f64::EPSILON)
        .collect();

    let n = diffs.len();
    if n == 0 {
        return WilcoxonResult {
            w_plus: 0.0,
            w_minus: 0.0,
            z: 0.0,
            p: 1.0,
            n_pairs: pairs.len(),
            n_non_tied: 0,
        };
    }

    // Rank absolute differences with tie averaging
    let mut abs_indexed: Vec<(f64, usize)> =
        diffs.iter().enumerate().map(|(i, d)| (d.abs(), i)).collect();
    abs_indexed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut ranks = vec![0.0f64; n];
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && (abs_indexed[j].0 - abs_indexed[i].0).abs() < f64::EPSILON {
            j += 1;
        }
        let avg_rank = (i + 1 + j) as f64 / 2.0; // 1-indexed
        for item in abs_indexed.iter().take(j).skip(i) {
            ranks[item.1] = avg_rank;
        }
        i = j;
    }

    let w_plus: f64 = (0..n)
        .filter(|&i| diffs[i] > 0.0)
        .map(|i| ranks[i])
        .sum();
    let w_minus: f64 = (0..n)
        .filter(|&i| diffs[i] < 0.0)
        .map(|i| ranks[i])
        .sum();

    // Normal approximation
    let nf = n as f64;
    let mean_w = nf * (nf + 1.0) / 4.0;
    let var_w = nf * (nf + 1.0) * (2.0 * nf + 1.0) / 24.0;
    let z = if var_w == 0.0 {
        0.0
    } else {
        (w_plus - mean_w) / var_w.sqrt()
    };
    let p = 2.0 * (1.0 - normal_cdf(z.abs()));

    WilcoxonResult {
        w_plus,
        w_minus,
        z: round4(z),
        p: round6(p),
        n_pairs: pairs.len(),
        n_non_tied: n,
    }
}

/// McNemar's test with continuity correction.
///
/// `a_only` = surfaces verified only in baseline (losses).
/// `b_only` = surfaces verified only in current (gains).
pub fn mcnemar_test(a_only: usize, b_only: usize) -> McNemarResult {
    let total = a_only + b_only;
    if total == 0 {
        return McNemarResult {
            chi2: 0.0,
            p: 1.0,
            a_only,
            b_only,
        };
    }

    let diff = (a_only as f64 - b_only as f64).abs() - 1.0;
    let chi2 = if diff > 0.0 {
        diff * diff / total as f64
    } else {
        0.0
    };
    let p = 1.0 - chi2_cdf_1df(chi2);

    McNemarResult {
        chi2: round4(chi2),
        p: round6(p),
        a_only,
        b_only,
    }
}

/// Standard normal CDF.
fn normal_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// Chi-squared CDF with 1 degree of freedom.
fn chi2_cdf_1df(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    2.0 * normal_cdf(x.sqrt()) - 1.0
}

/// Error function (Abramowitz and Stegun approximation, ~1.5e-7 accuracy).
fn erf(x: f64) -> f64 {
    let sign = x.signum();
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * (-x * x).exp();
    sign * y
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

fn round6(v: f64) -> f64 {
    (v * 1000000.0).round() / 1000000.0
}
