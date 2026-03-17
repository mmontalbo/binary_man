#!/usr/bin/env python3
"""A/B testing harness for bman verification.

Runs bman multiple times on the same binary/entry-point, collects per-surface
outcomes, and optionally compares against a tagged baseline using statistical
tests (Wilcoxon signed-rank, McNemar's).

Usage:
    tools/eval.py <binary> [entry_point...] --runs N
    tools/eval.py <binary> [entry_point...] --runs N --compare baseline_name
    tools/eval.py <binary> [entry_point...] --tag-baseline NAME
    tools/eval.py <binary> [entry_point...] --runs N --json

Examples:
    tools/eval.py ls --runs 3
    tools/eval.py git diff --runs 5 --compare v1 --max-cycles 40
    tools/eval.py git diff --tag-baseline v1
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

TOOLS_DIR = Path(__file__).resolve().parent
ROOT_DIR = TOOLS_DIR.parent
EVAL_DATA_DIR = TOOLS_DIR / "eval_data"
BASELINES_DIR = TOOLS_DIR / "baselines"

sys.path.insert(0, str(TOOLS_DIR))
from lib.pack import extract_surface_outcomes, get_status, load_state


# ── Git helpers ──────────────────────────────────────────────────────

def get_git_info() -> dict:
    """Return commit hash (7-char), subject, and dirty flag."""
    try:
        commit = subprocess.check_output(
            ["git", "rev-parse", "--short=7", "HEAD"],
            cwd=ROOT_DIR, text=True,
        ).strip()
        subject = subprocess.check_output(
            ["git", "log", "-1", "--format=%s"],
            cwd=ROOT_DIR, text=True,
        ).strip()
        dirty = bool(subprocess.check_output(
            ["git", "status", "--porcelain"],
            cwd=ROOT_DIR, text=True,
        ).strip())
    except subprocess.CalledProcessError:
        return {"commit": "unknown", "subject": "", "dirty": False}
    return {"commit": commit, "subject": subject, "dirty": dirty}


# ── Build ────────────────────────────────────────────────────────────

def build_bman() -> str:
    """Build bman in release mode, return path to binary."""
    print("Building bman (cargo build --release)...", file=sys.stderr)
    result = subprocess.run(
        ["cargo", "build", "--release"],
        cwd=ROOT_DIR,
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        print(f"Build failed:\n{result.stderr}", file=sys.stderr)
        raise SystemExit(1)
    path = ROOT_DIR / "target" / "release" / "bman"
    if not path.exists():
        print(f"Binary not found at {path}", file=sys.stderr)
        raise SystemExit(1)
    return str(path)


# ── Bootstrap ────────────────────────────────────────────────────────

def create_bootstrap_state(
    bman_bin: str,
    binary: str,
    entry_point: list[str],
    max_cycles: int,
    timeout: int,
) -> dict:
    """Run bman with --max-cycles 1 to get characterization, then strip to clean state."""
    tmpdir = tempfile.mkdtemp(prefix="bman_bootstrap_")
    try:
        cmd = [bman_bin, "--doc-pack", tmpdir, "--max-cycles", "1"]
        cmd.append(binary)
        cmd.extend(entry_point)
        print(f"Bootstrapping: {' '.join(cmd)}", file=sys.stderr)
        subprocess.run(cmd, timeout=timeout, capture_output=True, text=True)

        state_path = Path(tmpdir) / "state.json"
        if not state_path.exists():
            print("Bootstrap failed: no state.json produced", file=sys.stderr)
            raise SystemExit(1)

        state = load_state(state_path)

        # Strip per-surface run state, keep shared infrastructure
        # (seed_bank and baseline are expensive to build and help warm-start)
        for entry in state["entries"]:
            entry["attempts"] = []
            entry["probes"] = []
            entry["retried"] = False
            entry["critique_demotions"] = 0
            entry["status"] = {"kind": "Pending"}
        state["cycle"] = 0

        return state
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)


# ── Single run ───────────────────────────────────────────────────────

def run_single(
    bman_bin: str,
    bootstrap_state: dict,
    binary: str,
    entry_point: list[str],
    max_cycles: int,
    timeout: int,
    run_idx: int,
) -> dict:
    """Execute one bman run from bootstrap state, return outcome dict."""
    tmpdir = tempfile.mkdtemp(prefix=f"bman_eval_{run_idx}_")
    start = time.monotonic()
    crashed = False
    timed_out = False

    try:
        # Write bootstrap state
        state_path = Path(tmpdir) / "state.json"
        with open(state_path, "w") as f:
            json.dump(bootstrap_state, f)

        # Create subdirs bman expects
        (Path(tmpdir) / "evidence").mkdir(exist_ok=True)
        (Path(tmpdir) / "lm_log").mkdir(exist_ok=True)

        cmd = [bman_bin, "--doc-pack", tmpdir, "--max-cycles", str(max_cycles)]
        cmd.append(binary)
        cmd.extend(entry_point)

        try:
            result = subprocess.run(
                cmd, timeout=timeout, capture_output=True, text=True,
            )
            if result.returncode != 0:
                crashed = True
        except subprocess.TimeoutExpired:
            timed_out = True

        # Read final state (even on crash/timeout there may be partial results)
        elapsed = time.monotonic() - start
        if state_path.exists():
            state = load_state(state_path)
            surfaces = extract_surface_outcomes(state)
            cycle = state.get("cycle", 0)
        else:
            surfaces = {}
            cycle = 0

        complete = not crashed and not timed_out

        return {
            "run_index": run_idx,
            "elapsed_seconds": round(elapsed, 1),
            "cycle": cycle,
            "crashed": crashed,
            "timed_out": timed_out,
            "complete": complete,
            "surfaces": surfaces,
        }
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)


# ── Summary ──────────────────────────────────────────────────────────

def build_summary(runs: list[dict], meta: dict) -> dict:
    """Aggregate per-surface verification rates across runs."""
    n = len(runs)
    all_surface_ids = set()
    for r in runs:
        all_surface_ids.update(r["surfaces"].keys())

    per_surface = {}
    for sid in sorted(all_surface_ids):
        verified_count = 0
        attempt_counts = []
        first_verify_cycles = []
        outcomes_by_run = []

        for r in runs:
            s = r["surfaces"].get(sid)
            if s is None:
                outcomes_by_run.append(None)
                continue
            if s["verified"]:
                verified_count += 1
            attempt_counts.append(s["attempts"])
            if s["first_verify_cycle"] is not None:
                first_verify_cycles.append(s["first_verify_cycle"])
            outcomes_by_run.append(s["status"])

        per_surface[sid] = {
            "verification_rate": verified_count / n if n else 0.0,
            "verified_count": verified_count,
            "mean_attempts": sum(attempt_counts) / len(attempt_counts) if attempt_counts else 0.0,
            "mean_first_verify_cycle": (
                sum(first_verify_cycles) / len(first_verify_cycles)
                if first_verify_cycles else None
            ),
            "outcomes_by_run": outcomes_by_run,
        }

    # Aggregate stats
    total_surfaces = len(all_surface_ids)
    verified_per_run = [
        sum(1 for s in r["surfaces"].values() if s["verified"]) for r in runs
    ]
    excluded_per_run = [
        sum(1 for s in r["surfaces"].values() if s["status"] == "Excluded") for r in runs
    ]
    cycle_per_run = [r["cycle"] for r in runs]
    elapsed_per_run = [r["elapsed_seconds"] for r in runs]

    # Throughput metrics: surfaces reached and attempts per cycle
    reached_per_run = [
        sum(1 for s in r["surfaces"].values() if s["attempts"] > 0) for r in runs
    ]
    total_attempts_per_run = [
        sum(s["attempts"] for s in r["surfaces"].values()) for r in runs
    ]
    attempts_per_cycle_per_run = [
        (att / cyc if cyc > 0 else 0.0)
        for att, cyc in zip(total_attempts_per_run, cycle_per_run)
    ]
    hit_rate_per_run = [
        (v / r if r > 0 else 0.0)
        for v, r in zip(verified_per_run, reached_per_run)
    ]

    return {
        "meta": meta,
        "runs": n,
        "total_surfaces": total_surfaces,
        "mean_verified": sum(verified_per_run) / n if n else 0.0,
        "mean_excluded": sum(excluded_per_run) / n if n else 0.0,
        "mean_cycle": sum(cycle_per_run) / n if n else 0.0,
        "mean_elapsed": sum(elapsed_per_run) / n if n else 0.0,
        "mean_reached": sum(reached_per_run) / n if n else 0.0,
        "mean_total_attempts": sum(total_attempts_per_run) / n if n else 0.0,
        "mean_attempts_per_cycle": sum(attempts_per_cycle_per_run) / n if n else 0.0,
        "mean_hit_rate": sum(hit_rate_per_run) / n if n else 0.0,
        "crashed": sum(1 for r in runs if r["crashed"]),
        "timed_out": sum(1 for r in runs if r["timed_out"]),
        "per_surface": per_surface,
    }


# ── Statistics (stdlib only) ─────────────────────────────────────────

def _normal_cdf(x: float) -> float:
    """Standard normal CDF using math.erf."""
    return 0.5 * (1.0 + math.erf(x / math.sqrt(2.0)))


def _chi2_cdf_1df(x: float) -> float:
    """Chi-squared CDF with 1 degree of freedom."""
    if x <= 0:
        return 0.0
    return 2.0 * _normal_cdf(math.sqrt(x)) - 1.0


def wilcoxon_signed_rank(pairs: list[tuple[float, float]]) -> dict:
    """Wilcoxon signed-rank test on paired observations.

    Each pair is (baseline_value, current_value).  We test whether the
    current values are systematically different from the baseline.
    """
    diffs = [(b, c, c - b) for b, c in pairs if c != b]
    n = len(diffs)

    if n == 0:
        return {
            "W_plus": 0.0, "W_minus": 0.0, "z": 0.0,
            "p": 1.0, "n_pairs": len(pairs), "n_non_tied": 0,
        }

    # Rank absolute differences, averaging ties
    abs_diffs = [(abs(d), i) for i, (_, _, d) in enumerate(diffs)]
    abs_diffs.sort(key=lambda x: x[0])

    ranks = [0.0] * n
    i = 0
    while i < n:
        j = i
        while j < n and abs_diffs[j][0] == abs_diffs[i][0]:
            j += 1
        avg_rank = (i + 1 + j) / 2.0  # 1-indexed average
        for k in range(i, j):
            ranks[abs_diffs[k][1]] = avg_rank
        i = j

    W_plus = sum(ranks[i] for i in range(n) if diffs[i][2] > 0)
    W_minus = sum(ranks[i] for i in range(n) if diffs[i][2] < 0)

    # Normal approximation
    mean_W = n * (n + 1) / 4.0
    var_W = n * (n + 1) * (2 * n + 1) / 24.0
    if var_W == 0:
        z = 0.0
    else:
        z = (W_plus - mean_W) / math.sqrt(var_W)

    p = 2.0 * (1.0 - _normal_cdf(abs(z)))

    return {
        "W_plus": W_plus,
        "W_minus": W_minus,
        "z": round(z, 4),
        "p": round(p, 6),
        "n_pairs": len(pairs),
        "n_non_tied": n,
    }


def mcnemar_test(a_only: int, b_only: int) -> dict:
    """McNemar's test with continuity correction.

    a_only = surfaces verified only in baseline (losses).
    b_only = surfaces verified only in current (gains).
    """
    total = a_only + b_only
    if total == 0:
        return {"chi2": 0.0, "p": 1.0, "a_only": a_only, "b_only": b_only}

    chi2 = (abs(a_only - b_only) - 1) ** 2 / total
    p = 1.0 - _chi2_cdf_1df(chi2)

    return {
        "chi2": round(chi2, 4),
        "p": round(p, 6),
        "a_only": a_only,
        "b_only": b_only,
    }


# ── Comparison logic ─────────────────────────────────────────────────

def classify_flips(baseline_summary: dict, current_summary: dict) -> dict:
    """Compare per-surface verification rates between baseline and current."""
    b_surfaces = baseline_summary["per_surface"]
    c_surfaces = current_summary["per_surface"]

    common = set(b_surfaces) & set(c_surfaces)
    new_surfaces = sorted(set(c_surfaces) - set(b_surfaces))
    removed_surfaces = sorted(set(b_surfaces) - set(c_surfaces))

    stable_gains = []
    stable_losses = []
    fragile = []

    for sid in sorted(common):
        b_rate = b_surfaces[sid]["verification_rate"]
        c_rate = c_surfaces[sid]["verification_rate"]
        if b_rate == c_rate:
            continue
        if b_rate == 0.0 and c_rate == 1.0:
            stable_gains.append(sid)
        elif b_rate == 1.0 and c_rate == 0.0:
            stable_losses.append(sid)
        else:
            fragile.append({"id": sid, "baseline_rate": b_rate, "current_rate": c_rate})

    return {
        "stable_gains": stable_gains,
        "stable_losses": stable_losses,
        "fragile": fragile,
        "new_surfaces": new_surfaces,
        "removed_surfaces": removed_surfaces,
        "common_count": len(common),
    }


def compute_verdict(wilcoxon_result: dict, flips: dict, total_surfaces: int) -> dict:
    """Decision rule: p < 0.05 AND net_stable_flips >= max(5% of surfaces, 3)."""
    p = wilcoxon_result["p"]
    net_stable = len(flips["stable_gains"]) - len(flips["stable_losses"])
    threshold = max(int(total_surfaces * 0.05), 3)
    significant = p < 0.05

    if significant and net_stable >= threshold:
        verdict = "improvement"
    elif significant and net_stable <= -threshold:
        verdict = "regression"
    elif significant:
        verdict = "significant_below_threshold"
    else:
        verdict = "not_significant"

    return {
        "verdict": verdict,
        "p": p,
        "net_stable_flips": net_stable,
        "threshold": threshold,
        "significant": significant,
    }


def compute_efficiency(summary: dict) -> dict:
    """Compute efficiency metrics from a summary."""
    first_cycles = []
    for s in summary["per_surface"].values():
        if s["mean_first_verify_cycle"] is not None:
            first_cycles.append(s["mean_first_verify_cycle"])

    if first_cycles:
        first_cycles.sort()
        mid = len(first_cycles) // 2
        if len(first_cycles) % 2 == 0 and len(first_cycles) > 1:
            median_fc = (first_cycles[mid - 1] + first_cycles[mid]) / 2
        else:
            median_fc = first_cycles[mid]
    else:
        median_fc = None

    total_attempts = sum(
        s["mean_attempts"] for s in summary["per_surface"].values()
    )
    verified_attempts = sum(
        s["mean_attempts"] for s in summary["per_surface"].values()
        if s["verification_rate"] > 0
    )
    waste_ratio = (
        ((total_attempts - verified_attempts) / total_attempts * 100)
        if total_attempts > 0 else 0.0
    )

    return {
        "median_first_verify_cycle": median_fc,
        "waste_ratio_pct": round(waste_ratio, 1),
    }


# ── Baselines ────────────────────────────────────────────────────────

def _pack_name(binary: str, entry_point: list[str]) -> str:
    """Derive storage name from binary + entry point."""
    parts = [binary] + entry_point
    return "-".join(parts)


def tag_baseline(pack_name: str, name: str, commit: str) -> None:
    """Save the latest eval summary as a named baseline."""
    # Find the most recent summary for this pack and commit
    pack_dir = EVAL_DATA_DIR / pack_name
    if not pack_dir.exists():
        # Try to find any commit dir if commit is "latest"
        print(f"No eval data found for pack '{pack_name}'", file=sys.stderr)
        raise SystemExit(1)

    commit_dir = pack_dir / commit
    if not commit_dir.exists():
        # Try prefix match
        matches = [d for d in pack_dir.iterdir() if d.is_dir() and d.name.startswith(commit)]
        if len(matches) == 1:
            commit_dir = matches[0]
        elif not matches:
            print(f"No eval data for commit {commit} in {pack_dir}", file=sys.stderr)
            raise SystemExit(1)
        else:
            print(f"Ambiguous commit prefix {commit}: {[d.name for d in matches]}", file=sys.stderr)
            raise SystemExit(1)

    summary_path = commit_dir / "summary.json"
    if not summary_path.exists():
        print(f"No summary.json in {commit_dir}", file=sys.stderr)
        raise SystemExit(1)

    summary = json.loads(summary_path.read_text())

    BASELINES_DIR.mkdir(parents=True, exist_ok=True)
    baseline = {
        "name": name,
        "commit": commit_dir.name,
        "tagged_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "summary": summary,
    }
    baseline_path = BASELINES_DIR / f"{pack_name}.json"

    # Load existing baselines or create new
    if baseline_path.exists():
        data = json.loads(baseline_path.read_text())
    else:
        data = {"baselines": {}}

    data["baselines"][name] = baseline
    with open(baseline_path, "w") as f:
        json.dump(data, f, indent=2)
        f.write("\n")

    print(f"Tagged baseline '{name}' for {pack_name} @ {commit_dir.name}", file=sys.stderr)


def load_comparison_data(pack_name: str, ref: str) -> dict | None:
    """Load a baseline or prior run for comparison.

    Tries: baseline name, then commit hash, then prefix match.
    """
    # Try as baseline name
    baseline_path = BASELINES_DIR / f"{pack_name}.json"
    if baseline_path.exists():
        data = json.loads(baseline_path.read_text())
        if ref in data.get("baselines", {}):
            return data["baselines"][ref]["summary"]

    # Try as commit hash (exact or prefix)
    pack_dir = EVAL_DATA_DIR / pack_name
    if pack_dir.exists():
        commit_dir = pack_dir / ref
        if commit_dir.exists():
            summary_path = commit_dir / "summary.json"
            if summary_path.exists():
                return json.loads(summary_path.read_text())

        # Prefix match
        matches = [
            d for d in pack_dir.iterdir()
            if d.is_dir() and d.name.startswith(ref)
        ]
        if len(matches) == 1:
            summary_path = matches[0] / "summary.json"
            if summary_path.exists():
                return json.loads(summary_path.read_text())

    return None


# ── Display ──────────────────────────────────────────────────────────

def show_standalone(summary: dict, json_output: bool) -> None:
    """Display metrics for a single version (no comparison)."""
    if json_output:
        print(json.dumps(summary, indent=2))
        return

    meta = summary["meta"]
    n = summary["runs"]
    total = summary["total_surfaces"]
    eff = compute_efficiency(summary)

    print(f"\n{'═' * 60}", file=sys.stderr)
    print(f"  eval: {meta['binary']} {' '.join(meta.get('entry_point', []))}".rstrip(), file=sys.stderr)
    print(f"  commit: {meta['git']['commit']}"
          f"{'*' if meta['git']['dirty'] else ''}"
          f"  ({meta['git']['subject']})", file=sys.stderr)
    print(f"  runs: {n}  surfaces: {total}  max-cycles: {meta['max_cycles']}", file=sys.stderr)
    if meta.get("note"):
        print(f"  note: {meta['note']}", file=sys.stderr)
    print(f"{'═' * 60}", file=sys.stderr)

    mean_v = summary["mean_verified"]
    mean_x = summary["mean_excluded"]
    rate = mean_v / total * 100 if total else 0

    print(f"\n  Mean verified: {mean_v:.1f}/{total} ({rate:.1f}%)", file=sys.stderr)
    print(f"  Mean excluded: {mean_x:.1f}/{total}", file=sys.stderr)
    print(f"  Mean cycles:   {summary['mean_cycle']:.1f}", file=sys.stderr)
    print(f"  Mean elapsed:  {summary['mean_elapsed']:.0f}s", file=sys.stderr)

    # Throughput metrics
    reached = summary.get("mean_reached", 0)
    print(f"\n  Throughput:", file=sys.stderr)
    print(f"    Surfaces reached:    {reached:.1f}/{total} ({reached / total * 100:.0f}%)", file=sys.stderr)
    print(f"    Total attempts:      {summary.get('mean_total_attempts', 0):.1f}", file=sys.stderr)
    print(f"    Attempts/cycle:      {summary.get('mean_attempts_per_cycle', 0):.2f}", file=sys.stderr)
    print(f"    Hit rate (V/reached):{summary.get('mean_hit_rate', 0) * 100:5.1f}%", file=sys.stderr)
    if eff["median_first_verify_cycle"] is not None:
        print(f"    Median 1st-verify:   cycle {eff['median_first_verify_cycle']:.1f}", file=sys.stderr)
    print(f"    Waste ratio:         {eff['waste_ratio_pct']}%", file=sys.stderr)

    if summary["crashed"] or summary["timed_out"]:
        print(f"\n  Crashed: {summary['crashed']}  Timed out: {summary['timed_out']}", file=sys.stderr)

    # Show per-surface rates for surfaces that aren't always verified
    interesting = {
        sid: s for sid, s in summary["per_surface"].items()
        if s["verification_rate"] < 1.0
    }
    if interesting:
        print(f"\n  Surfaces below 100% verification rate:", file=sys.stderr)
        for sid in sorted(interesting, key=lambda x: interesting[x]["verification_rate"]):
            s = interesting[sid]
            pct = s["verification_rate"] * 100
            print(f"    {sid:40s} {pct:5.1f}% ({s['verified_count']}/{n})", file=sys.stderr)

    print(file=sys.stderr)


def show_comparison(current: dict, baseline: dict, json_output: bool) -> None:
    """Display statistical comparison between current and baseline."""
    flips = classify_flips(baseline, current)

    # Build paired data for Wilcoxon: per-surface verification rates
    b_surfaces = baseline["per_surface"]
    c_surfaces = current["per_surface"]
    common = set(b_surfaces) & set(c_surfaces)
    pairs = [(b_surfaces[s]["verification_rate"], c_surfaces[s]["verification_rate"]) for s in common]

    wilcoxon = wilcoxon_signed_rank(pairs)
    verdict = compute_verdict(wilcoxon, flips, len(common))

    # For single-run data, also compute McNemar's
    b_only = set()
    c_only = set()
    for sid in common:
        b_v = b_surfaces[sid]["verification_rate"] > 0
        c_v = c_surfaces[sid]["verification_rate"] > 0
        if b_v and not c_v:
            b_only.add(sid)
        elif c_v and not b_v:
            c_only.add(sid)
    mcnemar = mcnemar_test(len(b_only), len(c_only))

    if json_output:
        output = {
            "baseline_meta": baseline.get("meta", {}),
            "current_meta": current.get("meta", {}),
            "flips": flips,
            "wilcoxon": wilcoxon,
            "mcnemar": mcnemar,
            "verdict": verdict,
        }
        print(json.dumps(output, indent=2))
        return

    # Human display
    c_meta = current.get("meta", {})
    b_meta = baseline.get("meta", {})
    c_total = current["total_surfaces"]
    b_total = baseline["total_surfaces"]

    print(f"\n{'═' * 60}", file=sys.stderr)
    print(f"  COMPARISON", file=sys.stderr)
    print(f"{'═' * 60}", file=sys.stderr)

    print(f"\n  Baseline: {b_meta.get('git', {}).get('commit', '?')} "
          f"({b_meta.get('git', {}).get('subject', '?')[:40]})", file=sys.stderr)
    print(f"  Current:  {c_meta.get('git', {}).get('commit', '?')} "
          f"({c_meta.get('git', {}).get('subject', '?')[:40]})", file=sys.stderr)
    print(f"  Runs: baseline={baseline['runs']}, current={current['runs']}", file=sys.stderr)

    b_rate = baseline["mean_verified"] / b_total * 100 if b_total else 0
    c_rate = current["mean_verified"] / c_total * 100 if c_total else 0
    delta = c_rate - b_rate

    print(f"\n  Verification rate: {b_rate:.1f}% → {c_rate:.1f}% ({delta:+.1f}pp)", file=sys.stderr)
    print(f"  Surfaces: {flips['common_count']} common, "
          f"{len(flips['new_surfaces'])} new, "
          f"{len(flips['removed_surfaces'])} removed", file=sys.stderr)

    if flips["stable_gains"]:
        print(f"\n  Stable gains (0% → 100%):", file=sys.stderr)
        for sid in flips["stable_gains"]:
            print(f"    + {sid}", file=sys.stderr)

    if flips["stable_losses"]:
        print(f"\n  Stable losses (100% → 0%):", file=sys.stderr)
        for sid in flips["stable_losses"]:
            print(f"    - {sid}", file=sys.stderr)

    if flips["fragile"]:
        print(f"\n  Rate changes:", file=sys.stderr)
        for f in flips["fragile"]:
            b_pct = f["baseline_rate"] * 100
            c_pct = f["current_rate"] * 100
            print(f"    ~ {f['id']:40s} {b_pct:.0f}% → {c_pct:.0f}%", file=sys.stderr)

    print(f"\n  Statistics:", file=sys.stderr)
    print(f"    Wilcoxon: W+={wilcoxon['W_plus']:.0f} W-={wilcoxon['W_minus']:.0f} "
          f"z={wilcoxon['z']:.3f} p={wilcoxon['p']:.4f} "
          f"(n={wilcoxon['n_non_tied']} non-tied)", file=sys.stderr)
    print(f"    McNemar:  chi2={mcnemar['chi2']:.3f} p={mcnemar['p']:.4f} "
          f"(baseline-only={mcnemar['a_only']} current-only={mcnemar['b_only']})", file=sys.stderr)

    verdict_labels = {
        "improvement": "IMPROVEMENT",
        "regression": "REGRESSION",
        "significant_below_threshold": "SIGNIFICANT (below flip threshold)",
        "not_significant": "NOT SIGNIFICANT",
    }
    label = verdict_labels.get(verdict["verdict"], verdict["verdict"])
    print(f"\n  Verdict: {label}", file=sys.stderr)
    print(f"    net stable flips: {verdict['net_stable_flips']:+d} "
          f"(threshold: {verdict['threshold']})", file=sys.stderr)
    print(file=sys.stderr)


# ── Persistence ──────────────────────────────────────────────────────

def save_eval_data(pack_name: str, commit: str, runs: list[dict], summary: dict, meta: dict) -> Path:
    """Save run data and summary to eval_data/<pack>/<commit>/."""
    out_dir = EVAL_DATA_DIR / pack_name / commit
    out_dir.mkdir(parents=True, exist_ok=True)

    with open(out_dir / "meta.json", "w") as f:
        json.dump(meta, f, indent=2)
        f.write("\n")

    for r in runs:
        with open(out_dir / f"run_{r['run_index']}.json", "w") as f:
            json.dump(r, f, indent=2)
            f.write("\n")

    with open(out_dir / "summary.json", "w") as f:
        json.dump(summary, f, indent=2)
        f.write("\n")

    return out_dir


# ── CLI ──────────────────────────────────────────────────────────────

def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="A/B testing harness for bman verification",
        usage="tools/eval.py <binary> [entry_point...] --runs N [options]",
    )
    parser.add_argument("binary", help="Binary to test (e.g. 'ls', 'git')")
    parser.add_argument("entry_point", nargs="*", help="Entry point args (e.g. 'diff' for git diff)")
    parser.add_argument("--runs", "-n", type=int, default=0, help="Number of evaluation runs")
    parser.add_argument("--compare", help="Compare against baseline name or commit")
    parser.add_argument("--tag-baseline", help="Tag current results as a named baseline")
    parser.add_argument("--json", action="store_true", help="Output results as JSON")
    parser.add_argument("--max-cycles", type=int, default=80, help="Max cycles per run (default: 80)")
    parser.add_argument("--timeout", type=int, default=300, help="Timeout per run in seconds (default: 300)")
    parser.add_argument("--note", help="Free-text note to attach to the eval")
    parser.add_argument("--parallel", "-p", action="store_true",
                        help="Run trials in parallel (faster, but more LM load)")
    return parser.parse_args()


# ── Main ─────────────────────────────────────────────────────────────

def main() -> int:
    args = parse_args()
    git = get_git_info()
    pack_name = _pack_name(args.binary, args.entry_point)

    # Handle --tag-baseline (no runs needed)
    if args.tag_baseline:
        tag_baseline(pack_name, args.tag_baseline, git["commit"])
        return 0

    if args.runs <= 0:
        print("Error: --runs N is required (unless using --tag-baseline)", file=sys.stderr)
        return 1

    # Build
    bman_bin = build_bman()

    # Bootstrap
    print(f"\nBootstrapping state for {args.binary} {' '.join(args.entry_point)}...",
          file=sys.stderr)
    bootstrap = create_bootstrap_state(
        bman_bin, args.binary, args.entry_point, args.max_cycles, args.timeout,
    )
    n_surfaces = len(bootstrap["entries"])
    print(f"Bootstrap complete: {n_surfaces} surfaces\n", file=sys.stderr)

    # Run trials
    def _run_and_report(i: int) -> dict:
        result = run_single(
            bman_bin, bootstrap, args.binary, args.entry_point,
            args.max_cycles, args.timeout, i,
        )
        verified = sum(1 for s in result["surfaces"].values() if s["verified"])
        total = len(result["surfaces"])
        status_parts = [f"{verified}/{total} verified", f"{result['cycle']} cycles", f"{result['elapsed_seconds']}s"]
        if result["crashed"]:
            status_parts.append("CRASHED")
        if result["timed_out"]:
            status_parts.append("TIMED OUT")
        print(f"Run {i + 1}/{args.runs}... {', '.join(status_parts)}", file=sys.stderr)
        return result

    if args.parallel and args.runs > 1:
        print(f"Running {args.runs} trials in parallel...", file=sys.stderr)
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.runs) as pool:
            futures = {pool.submit(_run_and_report, i): i for i in range(args.runs)}
            runs = [None] * args.runs
            for future in concurrent.futures.as_completed(futures):
                idx = futures[future]
                runs[idx] = future.result()
    else:
        runs = []
        for i in range(args.runs):
            runs.append(_run_and_report(i))

    # Build summary
    meta = {
        "binary": args.binary,
        "entry_point": args.entry_point,
        "pack_name": pack_name,
        "git": git,
        "max_cycles": args.max_cycles,
        "timeout": args.timeout,
        "runs": args.runs,
        "note": args.note,
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S"),
    }
    summary = build_summary(runs, meta)

    # Save
    out_dir = save_eval_data(pack_name, git["commit"], runs, summary, meta)
    print(f"Results saved to {out_dir}", file=sys.stderr)

    # Display
    if args.compare:
        baseline = load_comparison_data(pack_name, args.compare)
        if baseline is None:
            print(f"Warning: comparison ref '{args.compare}' not found, showing standalone",
                  file=sys.stderr)
            show_standalone(summary, args.json)
        else:
            show_comparison(summary, baseline, args.json)
    else:
        show_standalone(summary, args.json)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
