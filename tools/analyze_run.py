#!/usr/bin/env python3
"""Analyze a bman verify run to diagnose where effectiveness is lost.

Usage:
    python tools/analyze_run.py <pack_path>
    python tools/analyze_run.py ~/.local/share/bman/packs/git-diff

Reads state.json from a pack directory and produces a diagnostic report
showing failure modes, efficiency metrics, and actionable breakdowns.
"""

from __future__ import annotations

import json
import sys
import time
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from lib.pack import (
    get_status,
    get_outcome,
    get_category,
    build_snapshot,
    save_snapshot,
    load_history,
)


def uses_fixtures(entry: dict) -> bool:
    for a in entry.get("attempts", []):
        for cmd in a.get("seed", {}).get("setup", []):
            if any("_fixtures" in str(arg) for arg in cmd):
                return True
    return False


def seed_has_files(entry: dict) -> bool:
    """Check if any attempt used seed.files (not just setup commands)."""
    for a in entry.get("attempts", []):
        files = a.get("seed", {}).get("files", [])
        if files:
            return True
    return False


def seed_has_two_phase(entry: dict) -> bool:
    """Check if any attempt uses the commit-then-overwrite pattern."""
    for a in entry.get("attempts", []):
        setup = a.get("seed", {}).get("setup", [])
        setup_str = str(setup).lower()
        has_commit = "commit" in setup_str
        has_overwrite = (
            "printf" in setup_str
            or "cat >" in setup_str
            or "echo" in setup_str
            or "cp " in setup_str
        )
        if has_commit and has_overwrite:
            return True
    return False


def analyze(state: dict) -> None:
    entries = state["entries"]
    total = len(entries)
    verified = [e for e in entries if get_status(e) == "Verified"]
    excluded = [e for e in entries if get_status(e) == "Excluded"]
    pending = [e for e in entries if get_status(e) == "Pending"]

    # ── Header ──────────────────────────────────────────────
    binary = state["binary"]
    ctx = " ".join(state.get("context_argv", []))
    command = f"{binary} {ctx}".strip()
    print(f"{'═' * 70}")
    print(f"  bman verify analysis: {command}")
    print(f"  cycles: {state['cycle']}  surfaces: {total}")
    print(f"  verified: {len(verified)}  excluded: {len(excluded)}  pending: {len(pending)}")
    rate = len(verified) * 100 // total if total else 0
    print(f"  verification rate: {rate}%")
    print(f"{'═' * 70}")

    # ── 1. Failure mode distribution ────────────────────────
    print("\n┌─ FAILURE MODES (excluded surfaces) ──────────────────")
    if excluded:
        last_outcomes = Counter()
        for e in excluded:
            if e["attempts"]:
                last_outcomes[get_outcome(e["attempts"][-1])] += 1
            else:
                last_outcomes["(no attempts)"] += 1
        for outcome, count in last_outcomes.most_common():
            pct = count * 100 // len(excluded)
            bar = "█" * (pct // 2)
            print(f"│  {outcome:20s} {count:3d}/{len(excluded)} ({pct:2d}%) {bar}")
    else:
        print("│  (none)")
    print("│")

    # Trajectory patterns for excluded
    if excluded:
        trajectory_counter = Counter()
        for e in excluded:
            outcomes = tuple(get_outcome(a) for a in e["attempts"])
            trajectory_counter[outcomes] += 1
        print("│  Common failure trajectories:")
        for traj, count in trajectory_counter.most_common(5):
            label = " → ".join(traj) if traj else "(no attempts)"
            print(f"│    {count}× {label}")
    print("└──────────────────────────────────────────────────────")

    # ── 2. Outcome distribution (all attempts) ──────────────
    print("\n┌─ OUTCOME DISTRIBUTION (all attempts) ────────────────")
    all_outcomes = Counter()
    for e in entries:
        for a in e["attempts"]:
            all_outcomes[get_outcome(a)] += 1
    total_attempts = sum(all_outcomes.values())
    for outcome, count in all_outcomes.most_common():
        pct = count * 100 // total_attempts if total_attempts else 0
        bar = "█" * (pct // 2)
        print(f"│  {outcome:20s} {count:3d}/{total_attempts} ({pct:2d}%) {bar}")
    print("└──────────────────────────────────────────────────────")

    # ── 3. Attempt efficiency ───────────────────────────────
    print("\n┌─ ATTEMPT EFFICIENCY ─────────────────────────────────")
    v_attempts = sum(len(e["attempts"]) for e in verified)
    x_attempts = sum(len(e["attempts"]) for e in excluded)
    print(f"│  Attempts on verified:  {v_attempts:3d} ({v_attempts / len(verified):.1f} avg)" if verified else "│  Attempts on verified:    0")
    print(f"│  Attempts on excluded:  {x_attempts:3d} ({x_attempts / len(excluded):.1f} avg)" if excluded else "│  Attempts on excluded:    0")
    if total_attempts:
        waste_pct = x_attempts * 100 // total_attempts
        print(f"│  Waste ratio:           {waste_pct}% of all attempts spent on surfaces that were never verified")
    print("│")

    # Attempts-to-verify distribution
    if verified:
        attempt_dist = Counter(len(e["attempts"]) for e in verified)
        print("│  Attempts to verify:")
        cumulative = 0
        for n in sorted(attempt_dist):
            cumulative += attempt_dist[n]
            pct = cumulative * 100 // len(verified)
            print(f"│    {n} attempt(s): {attempt_dist[n]:3d}  (cumulative: {pct}%)")
    print("│")

    # Outcome by attempt number
    by_pos: dict[int, Counter] = {}
    for e in entries:
        for i, a in enumerate(e["attempts"]):
            pos = i + 1
            if pos not in by_pos:
                by_pos[pos] = Counter()
            by_pos[pos][get_outcome(a)] += 1
    print("│  Outcome by attempt number:")
    for pos in sorted(by_pos):
        total_at_pos = sum(by_pos[pos].values())
        v_at_pos = by_pos[pos].get("Verified", 0)
        oe_at_pos = by_pos[pos].get("OutputsEqual", 0)
        other = total_at_pos - v_at_pos - oe_at_pos
        print(f"│    #{pos}: {total_at_pos:3d} total  Verified={v_at_pos}  OutputsEqual={oe_at_pos}  other={other}")
    print("└──────────────────────────────────────────────────────")

    # ── 4. Prediction accuracy ──────────────────────────────
    print("\n┌─ PREDICTION ACCURACY ────────────────────────────────")
    pred_by_outcome: dict[str, Counter] = {}
    for e in entries:
        for a in e["attempts"]:
            o = get_outcome(a)
            pm = a.get("prediction_matched")
            if o not in pred_by_outcome:
                pred_by_outcome[o] = Counter()
            if pm is True:
                pred_by_outcome[o]["hit"] += 1
            elif pm is False:
                pred_by_outcome[o]["miss"] += 1
            else:
                pred_by_outcome[o]["none"] += 1
    for o in ["Verified", "OutputsEqual", "OptionError", "SetupFailed", "Crashed"]:
        if o not in pred_by_outcome:
            continue
        c = pred_by_outcome[o]
        total_pred = c["hit"] + c["miss"]
        if total_pred == 0:
            continue
        hit_rate = c["hit"] * 100 // total_pred
        print(f"│  {o:20s}  hit={c['hit']:3d}  miss={c['miss']:3d}  accuracy={hit_rate}%")
    print("│")
    # Insight
    oe_miss = pred_by_outcome.get("OutputsEqual", Counter()).get("miss", 0)
    oe_total = sum(pred_by_outcome.get("OutputsEqual", Counter()).values())
    if oe_miss > 0:
        print(f"│  ⚠ {oe_miss}/{oe_total} OutputsEqual had wrong predictions —")
        print(f"│    the LM predicted a diff but the seed didn't produce one.")
        print(f"│    This is the primary signal of seed quality problems.")
    print("└──────────────────────────────────────────────────────")

    # ── 5. Feature usage & effectiveness ────────────────────
    print("\n┌─ FEATURE USAGE & EFFECTIVENESS ──────────────────────")
    # Characterization
    has_char = [e for e in entries if e.get("characterization")]
    char_verified = [e for e in has_char if get_status(e) == "Verified"]
    no_char = [e for e in entries if not e.get("characterization")]
    no_char_verified = [e for e in no_char if get_status(e) == "Verified"]
    print(f"│  Characterization:")
    if has_char:
        char_rate = len(char_verified) * 100 // len(has_char) if has_char else 0
        no_char_rate = len(no_char_verified) * 100 // len(no_char) if no_char else 0
        print(f"│    With:    {len(char_verified)}/{len(has_char)} verified ({char_rate}%)")
        print(f"│    Without: {len(no_char_verified)}/{len(no_char)} verified ({no_char_rate}%)")
        # Re-characterization
        rechar = [e for e in has_char if e["characterization"].get("revision", 0) > 0]
        if rechar:
            rechar_v = sum(1 for e in rechar if get_status(e) == "Verified")
            print(f"│    Re-characterized: {len(rechar)} ({rechar_v} verified)")
    else:
        print(f"│    (not used in this run)")
    print("│")

    # Probes
    has_probes = [e for e in entries if e.get("probes")]
    if has_probes:
        total_probes = sum(len(e["probes"]) for e in has_probes)
        differ_probes = sum(
            sum(1 for p in e["probes"] if p.get("outputs_differ"))
            for e in has_probes
        )
        probes_verified = [e for e in has_probes if get_status(e) == "Verified"]
        print(f"│  Probes:")
        print(f"│    Surfaces probed:  {len(has_probes)}")
        print(f"│    Total probes:     {total_probes}")
        print(f"│    Outputs differed: {differ_probes} ({differ_probes * 100 // total_probes}%)" if total_probes else "")
        print(f"│    Probed → verified: {len(probes_verified)}/{len(has_probes)}")
    else:
        print(f"│  Probes: (not used in this run)")
    print("│")

    # Critique
    has_critique = [e for e in entries if e.get("critique_feedback")]
    if has_critique:
        crit_verified = [e for e in has_critique if get_status(e) == "Verified"]
        print(f"│  Critique:")
        print(f"│    Surfaces critiqued: {len(has_critique)}")
        print(f"│    Survived re-verify: {len(crit_verified)}/{len(has_critique)}")
    else:
        print(f"│  Critique: (not used in this run)")
    print("│")

    # Fixture usage
    v_fixture = sum(1 for e in verified if uses_fixtures(e))
    x_fixture = sum(1 for e in excluded if uses_fixtures(e))
    print(f"│  Fixtures:")
    print(f"│    Verified using fixtures:  {v_fixture}/{len(verified)}")
    print(f"│    Excluded using fixtures:  {x_fixture}/{len(excluded)}")
    print("│")

    # Seed patterns
    v_two_phase = sum(1 for e in verified if seed_has_two_phase(e))
    x_two_phase = sum(1 for e in excluded if seed_has_two_phase(e))
    v_has_files = sum(1 for e in verified if seed_has_files(e))
    x_has_files = sum(1 for e in excluded if seed_has_files(e))
    print(f"│  Seed patterns:")
    print(f"│    Two-phase (commit+overwrite): verified={v_two_phase} excluded={x_two_phase}")
    print(f"│    Uses seed.files:              verified={v_has_files} excluded={x_has_files}")
    print("└──────────────────────────────────────────────────────")

    # ── 6. Excluded surface detail ──────────────────────────
    print("\n┌─ EXCLUDED SURFACES (detail) ─────────────────────────")
    for e in sorted(excluded, key=lambda x: x["id"]):
        outcomes = [get_outcome(a) for a in e["attempts"]]
        cat = get_category(e)
        trajectory = " → ".join(outcomes) if outcomes else "(no attempts)"
        print(f"│  {e['id']}")
        print(f"│    category: {cat}  attempts: {len(outcomes)}  trajectory: {trajectory}")
        if e.get("characterization"):
            c = e["characterization"]
            print(f"│    characterization: \"{c.get('trigger', '?')}\" (rev {c.get('revision', 0)})")
        # Show what seeds were tried (abbreviated)
        for i, a in enumerate(e["attempts"][:2]):
            setup = a.get("seed", {}).get("setup", [])
            # Find the interesting part (not git init/config boilerplate)
            interesting = []
            for cmd in setup:
                cmd_str = " ".join(str(x) for x in cmd[:3])
                if "git init" in cmd_str or "git config" in cmd_str:
                    continue
                interesting.append(cmd_str)
            if interesting:
                print(f"│    seed #{i+1}: {'; '.join(interesting[:3])}")
        if len(e["attempts"]) > 2:
            print(f"│    ... +{len(e['attempts']) - 2} more")
    print("└──────────────────────────────────────────────────────")

    # ── 7. Stability estimate ─────────────────────────────
    print("\n┌─ STABILITY ESTIMATE ──────────────────────────────────")
    # Classify each surface into a stability tier
    stable_verified = []   # attempt-1 success
    fragile_verified = []  # needed 2+ attempts
    fragile_excluded = []  # excluded after attempts (might flip)
    structural_excluded = []  # never attempted or structural barrier

    for e in entries:
        status = get_status(e)
        attempts = e.get("attempts", [])
        if status == "Verified":
            if len(attempts) <= 1:
                stable_verified.append(e["id"])
            else:
                fragile_verified.append(e["id"])
        elif status == "Excluded":
            if not attempts:
                structural_excluded.append(e["id"])
            else:
                fragile_excluded.append(e["id"])

    floor = len(stable_verified)
    likely = floor + len(fragile_verified)
    variance_band = len(fragile_verified) + len(fragile_excluded)
    ceiling = total - len(structural_excluded)

    print(f"│  Stable verified (attempt 1):  {floor:3d}  ({floor * 100 // total}%)")
    print(f"│  Fragile verified (attempt 2+): {len(fragile_verified):3d}")
    print(f"│  Fragile excluded (had attempts):{len(fragile_excluded):3d}")
    print(f"│  Structural excluded (0 attempts):{len(structural_excluded):3d}")
    print(f"│")
    print(f"│  Estimated range: {floor}–{ceiling} verified ({floor * 100 // total}%–{ceiling * 100 // total}%)")
    print(f"│  Variance band:   {variance_band} surfaces ({variance_band * 100 // total}% of total)")
    if fragile_excluded:
        print(f"│")
        print(f"│  Fragile excluded (could flip on re-run):")
        for sid in sorted(fragile_excluded):
            e = next(x for x in entries if x["id"] == sid)
            outcomes = [get_outcome(a) for a in e["attempts"]]
            print(f"│    {sid}  [{' → '.join(outcomes)}]")
    print("└──────────────────────────────────────────────────────")

    # ── 8. Diagnosis ────────────────────────────────────────
    print("\n┌─ DIAGNOSIS ──────────────────────────────────────────")

    # OutputsEqual dominance
    oe_count = all_outcomes.get("OutputsEqual", 0)
    if total_attempts and oe_count * 100 // total_attempts > 40:
        print(f"│  ⚠ SEED QUALITY: {oe_count}/{total_attempts} ({oe_count * 100 // total_attempts}%) of all attempts")
        print(f"│    produce OutputsEqual. The LM constructs seeds that don't exercise")
        print(f"│    the option's effect. Root causes:")
        print(f"│    - Seed content too simple (no structural tension)")
        print(f"│    - Wrong seed setup pattern (files not committed before overwrite)")
        print(f"│    - LM doesn't understand what the option needs")

    # Prediction-outcome mismatch
    oe_misses = pred_by_outcome.get("OutputsEqual", Counter()).get("miss", 0)
    if oe_misses > 10:
        print(f"│")
        print(f"│  ⚠ PREDICTION CALIBRATION: {oe_misses} times the LM predicted a diff")
        print(f"│    but got OutputsEqual. The LM thinks its seed should work but it")
        print(f"│    doesn't — suggests understanding gap, not random guessing.")

    # Attempt waste
    if total_attempts and x_attempts * 100 // total_attempts > 40:
        print(f"│")
        print(f"│  ⚠ ATTEMPT WASTE: {x_attempts}/{total_attempts} ({x_attempts * 100 // total_attempts}%) of attempts")
        print(f"│    spent on surfaces that were never verified. Early detection of")
        print(f"│    hopeless surfaces would reclaim this budget.")

    # Stagnation at later attempts
    if 6 in by_pos:
        late_oe = by_pos.get(6, Counter()).get("OutputsEqual", 0)
        late_total = sum(by_pos.get(6, Counter()).values())
        if late_total > 0 and late_oe * 100 // late_total > 80:
            print(f"│")
            print(f"│  ⚠ STAGNATION: attempt #6+ is {late_oe}/{late_total} OutputsEqual.")
            print(f"│    The LM isn't learning from failures — later attempts repeat")
            print(f"│    the same mistakes. Re-characterization or probe evidence could")
            print(f"│    break the cycle.")

    # Modifier surfaces
    modifier_excluded = [e for e in excluded if get_category(e) == "Modifier"]
    if modifier_excluded:
        bases = [
            e.get("category", {}).get("base", "?")
            if isinstance(e.get("category"), dict) else "?"
            for e in modifier_excluded
        ]
        base_status = {}
        for e in modifier_excluded:
            cat = e.get("category", {})
            base = cat.get("base") if isinstance(cat, dict) else None
            if not base:
                continue
            base_entry = next((x for x in entries if x["id"] == base), None)
            if base_entry:
                base_status[e["id"]] = (base, get_status(base_entry))
        print(f"│")
        print(f"│  ⚠ MODIFIER SURFACES: {len(modifier_excluded)} modifier(s) excluded.")
        for mod_id, (base, bs) in base_status.items():
            print(f"│    {mod_id} (modifies {base}, base is {bs})")
        print(f"│    Modifiers need their base option verified first. If the base is")
        print(f"│    also excluded, the modifier can't succeed.")

    print("└──────────────────────────────────────────────────────")


def show_cross_run_variance(history: list[dict]) -> None:
    """Show variance analysis across multiple runs."""
    if len(history) < 2:
        return

    print(f"\n┌─ CROSS-RUN VARIANCE ({len(history)} runs) ─────────────────────")

    # Verification rates
    rates = [len(r["verified"]) * 100 // r["surfaces"] for r in history]
    verified_counts = [len(r["verified"]) for r in history]
    attempt_1_counts = [len(r["attempt_1_verified"]) for r in history]
    print(f"│  Verification rate:  min={min(rates)}%  max={max(rates)}%  spread={max(rates) - min(rates)}pp")
    print(f"│  Verified count:     min={min(verified_counts)}  max={max(verified_counts)}  spread={max(verified_counts) - min(verified_counts)}")
    print(f"│  Attempt-1 verified: min={min(attempt_1_counts)}  max={max(attempt_1_counts)}  spread={max(attempt_1_counts) - min(attempt_1_counts)}")
    print(f"│")

    # Per-run summary table
    print(f"│  Run history:")
    print(f"│  {'#':>3}  {'Date':10}  {'Rate':>5}  {'Verified':>8}  {'Att-1':>5}  {'OE':>4}  {'Attempts':>8}")
    for i, r in enumerate(history):
        rate = len(r["verified"]) * 100 // r["surfaces"]
        print(
            f"│  {i + 1:3d}  {r['timestamp'][:10]}  {rate:4d}%  "
            f"{len(r['verified']):8d}  {len(r['attempt_1_verified']):5d}  "
            f"{r['outputs_equal']:4d}  {r['total_attempts']:8d}"
        )
    print(f"│")

    # Surface stability across runs
    all_surfaces = set()
    for r in history:
        all_surfaces.update(r["verified"])
        all_surfaces.update(r["excluded"])

    always_verified = set(history[0]["verified"])
    always_excluded = set(history[0]["excluded"])
    for r in history[1:]:
        always_verified &= set(r["verified"])
        always_excluded &= set(r["excluded"])

    ever_verified = set()
    ever_excluded = set()
    for r in history:
        ever_verified |= set(r["verified"])
        ever_excluded |= set(r["excluded"])

    flippers = ever_verified & ever_excluded

    print(f"│  Surface stability:")
    print(f"│    Always verified:    {len(always_verified):3d}  (stable core)")
    print(f"│    Always excluded:    {len(always_excluded):3d}  (structurally hard)")
    print(f"│    Flipped across runs:{len(flippers):3d}  (LM-variance dependent)")
    if flippers:
        print(f"│")
        print(f"│  Volatile surfaces (verified in some runs, excluded in others):")
        for sid in sorted(flippers):
            v_count = sum(1 for r in history if sid in r["verified"])
            print(f"│    {sid}  verified {v_count}/{len(history)} runs")
    print("└──────────────────────────────────────────────────────")


def main() -> int:
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <pack_path> [--no-save]", file=sys.stderr)
        print(f"  e.g.: {sys.argv[0]} ~/.local/share/bman/packs/git-diff", file=sys.stderr)
        return 1

    pack_path = Path(sys.argv[1])
    no_save = "--no-save" in sys.argv
    state_path = pack_path / "state.json"
    if not state_path.exists():
        print(f"Error: {state_path} not found", file=sys.stderr)
        return 1

    with open(state_path) as f:
        state = json.load(f)

    analyze(state)

    # Cross-run variance (show before saving so current run isn't double-counted)
    history = load_history(pack_path)
    snapshot = build_snapshot(state)

    # Deduplicate: skip save if latest snapshot has identical verified/excluded sets
    is_dup = False
    if history:
        last = history[-1]
        if (sorted(last["verified"]) == sorted(snapshot["verified"])
                and sorted(last["excluded"]) == sorted(snapshot["excluded"])):
            is_dup = True

    if history:
        combined = history if is_dup else history + [snapshot]
        show_cross_run_variance(combined)

    if not no_save and not is_dup:
        save_snapshot(pack_path, snapshot)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
