"""Shared helpers for reading bman pack state and history."""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path


def get_status(entry: dict) -> str:
    s = entry["status"]
    if isinstance(s, str):
        return s
    if isinstance(s, dict):
        return s.get("kind", str(s))
    return str(s)


def get_outcome(attempt: dict) -> str:
    o = attempt["outcome"]
    if isinstance(o, str):
        return o
    if isinstance(o, dict):
        return o.get("kind", str(o))
    return str(o)


def get_category(entry: dict) -> str:
    c = entry.get("category", "General")
    if isinstance(c, str):
        return c
    if isinstance(c, dict):
        kind = c.get("kind", "")
        if kind == "Modifier":
            return "Modifier"
        return kind
    return str(c)


def build_snapshot(state: dict) -> dict:
    """Build a compact run snapshot for history tracking."""
    entries = state["entries"]
    verified_ids = sorted(e["id"] for e in entries if get_status(e) == "Verified")
    excluded_ids = sorted(e["id"] for e in entries if get_status(e) == "Excluded")
    attempt_1_ids = sorted(
        e["id"] for e in entries
        if get_status(e) == "Verified" and len(e.get("attempts", [])) <= 1
    )
    total_attempts = sum(len(e.get("attempts", [])) for e in entries)
    oe_count = sum(
        1 for e in entries
        for a in e.get("attempts", [])
        if get_outcome(a) == "OutputsEqual"
    )
    return {
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "surfaces": len(entries),
        "verified": verified_ids,
        "excluded": excluded_ids,
        "attempt_1_verified": attempt_1_ids,
        "total_attempts": total_attempts,
        "outputs_equal": oe_count,
        "cycle": state.get("cycle", 0),
    }


def save_snapshot(pack_path: Path, snapshot: dict) -> None:
    """Append a snapshot to the run history file."""
    history_path = pack_path / "run_history.jsonl"
    with open(history_path, "a") as f:
        f.write(json.dumps(snapshot, separators=(",", ":")) + "\n")


def load_history(pack_path: Path) -> list[dict]:
    """Load all previous run snapshots."""
    history_path = pack_path / "run_history.jsonl"
    if not history_path.exists():
        return []
    runs = []
    for line in history_path.read_text().splitlines():
        line = line.strip()
        if line:
            runs.append(json.loads(line))
    return runs


def load_state(path: Path) -> dict:
    """Load state.json with error handling."""
    try:
        with open(path) as f:
            return json.load(f)
    except FileNotFoundError:
        print(f"Error: {path} not found", file=sys.stderr)
        raise SystemExit(1)
    except json.JSONDecodeError as exc:
        print(f"Error: {path} is not valid JSON: {exc}", file=sys.stderr)
        raise SystemExit(1)


def extract_surface_outcomes(state: dict) -> dict:
    """Per-surface summary from a completed run state.

    Returns {surface_id: {verified, status, attempts, probes,
                          outcome_trajectory, first_verify_cycle}}
    """
    results = {}
    for entry in state["entries"]:
        sid = entry["id"]
        status = get_status(entry)
        attempts = entry.get("attempts", [])
        trajectory = [get_outcome(a) for a in attempts]

        first_verify_cycle = None
        for a in attempts:
            if get_outcome(a) == "Verified":
                first_verify_cycle = a.get("cycle")
                break

        results[sid] = {
            "verified": status == "Verified",
            "status": status,
            "attempts": len(attempts),
            "probes": len(entry.get("probes", [])),
            "outcome_trajectory": trajectory,
            "first_verify_cycle": first_verify_cycle,
        }
    return results
