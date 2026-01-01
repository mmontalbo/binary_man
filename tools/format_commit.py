#!/usr/bin/env python3
"""Format a commit message that follows the project template.

Usage:
    python tools/format_commit.py "subject line" \
        --context "why this commit exists" \
        --enable "capability enabled" \
        --change "path: what + why" \
        --deferred "deferred work" \
        [--context "additional context line"] \
        [--enable "capability enabled"] \
        [--change "path: what + why"] \
        [--deferred "deferred work"] [...]

Use --write <path> to write the formatted commit message to a file (for example
Git's .git/COMMIT_EDITMSG). Without --write, the formatted message is printed
to stdout. Pass --commit to invoke `git commit` with the formatted message,
and repeat --commit-arg to forward custom arguments (for example --commit-arg
--amend).
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path


MAX_CONTEXT_LINES = 8


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("subject", help="commit subject line")
    parser.add_argument(
        "--context",
        action="append",
        required=True,
        metavar="TEXT",
        help=f"context for why the commit exists (repeatable, max {MAX_CONTEXT_LINES} lines)",
    )
    parser.add_argument(
        "--enable",
        action="append",
        required=True,
        metavar="TEXT",
        help="capability enabled by the change (repeatable)",
    )
    parser.add_argument(
        "--change",
        action="append",
        required=True,
        metavar="TEXT",
        help="change summary in 'path: what + why' form (repeatable)",
    )
    parser.add_argument(
        "--deferred",
        action="append",
        required=True,
        metavar="TEXT",
        help="deferred follow-up work (repeatable)",
    )
    parser.add_argument(
        "--write",
        type=Path,
        metavar="PATH",
        help="write the formatted message to PATH instead of stdout",
    )
    parser.add_argument(
        "--commit",
        action="store_true",
        help="run `git commit` with the formatted message",
    )
    parser.add_argument(
        "--commit-arg",
        action="append",
        default=[],
        metavar="ARG",
        help="additional argument to pass to `git commit` (repeatable)",
    )
    return parser.parse_args()


def _is_deleted_in_git(root: Path, path: str) -> bool:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "ls-files", "--deleted", "--", path],
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError:
        return False
    return path in result.stdout.splitlines()


def _split_context_lines(entries: list[str]) -> list[str]:
    lines = []
    for entry in entries:
        for raw_line in entry.splitlines():
            lines.append(raw_line.rstrip())
    while lines and not lines[0].strip():
        lines.pop(0)
    while lines and not lines[-1].strip():
        lines.pop()
    if not lines:
        raise ValueError("--context entries must be non-empty")

    cleaned = []
    blank_run = 0
    non_empty = 0
    for line in lines:
        if not line.strip():
            blank_run += 1
            if blank_run > 1:
                raise ValueError("context must not contain consecutive blank lines")
            cleaned.append("")
        else:
            blank_run = 0
            non_empty += 1
            cleaned.append(line.strip())
    if non_empty > MAX_CONTEXT_LINES:
        raise ValueError(f"context must be 1-{MAX_CONTEXT_LINES} lines")
    return cleaned


def _split_item_lines(item: str, label: str) -> list[str]:
    lines = item.splitlines()
    while lines and not lines[0].strip():
        lines.pop(0)
    while lines and not lines[-1].strip():
        lines.pop()
    if not lines:
        raise ValueError(f"{label} must be non-empty")

    first = lines[0].strip()
    if first.startswith("- "):
        first = first[2:].strip()
    if not first:
        raise ValueError(f"{label} must be non-empty")

    cleaned = [first]
    for line in lines[1:]:
        if not line.strip():
            raise ValueError(f"{label} must not contain blank lines")
        cleaned.append(line.strip())
    return cleaned


def _format_bullet_lines(lines: list[str], indent: str = "  ") -> list[str]:
    formatted = [f"- {lines[0]}"]
    for line in lines[1:]:
        formatted.append(f"{indent}{line}")
    return formatted


def _extract_path_candidate(label: str) -> str:
    if " (" in label and label.endswith(")"):
        return label.split(" (", 1)[0].strip()
    return label


def _should_validate_path(candidate: str, root: Path) -> bool:
    if " " in candidate or "\t" in candidate:
        return False
    if "/" in candidate or candidate.startswith(".") or "." in candidate:
        return True
    return (root / candidate).exists()


def _validate_path(root: Path, path: str) -> None:
    file_path = (root / path).resolve()
    if file_path == root:
        raise ValueError("change label must not be the repo root; point at a file or directory")
    try:
        file_path.relative_to(root)
    except ValueError as exc:
        raise ValueError(f"change label path must be within repo: {path}") from exc
    if not file_path.exists():
        if not _is_deleted_in_git(root, path):
            raise ValueError(f"change label path does not exist: {path}")


def format_message(
    subject: str,
    context: list[str],
    enables: list[str],
    changes: list[str],
    deferred: list[str],
) -> str:
    root = Path(__file__).resolve().parent.parent
    subject = subject.strip()
    if not subject:
        raise ValueError("subject must not be empty")

    if not context:
        raise ValueError("at least one --context entry is required")
    context_lines = _split_context_lines(context)

    if not enables:
        raise ValueError("at least one --enable entry is required")
    enable_lines = []
    for item in enables:
        item_lines = _split_item_lines(item, "--enable entry")
        enable_lines.extend(_format_bullet_lines(item_lines))

    if not changes:
        raise ValueError("at least one --change entry is required")
    change_lines = []
    for item in changes:
        item_lines = _split_item_lines(item, "--change entry")
        if ":" not in item_lines[0]:
            raise ValueError(f"--change entry must include 'path: description' (got {item!r})")
        label, description = item_lines[0].split(":", 1)
        label = label.strip()
        description = description.strip()
        if not label:
            raise ValueError(f"--change entry missing label before colon (got {item!r})")
        if not description:
            raise ValueError(f"--change entry missing description after colon (got {item!r})")
        path_candidate = _extract_path_candidate(label)
        if _should_validate_path(path_candidate, root):
            _validate_path(root, path_candidate)
        item_lines[0] = f"{label}: {description}"
        change_lines.extend(_format_bullet_lines(item_lines))

    if not deferred:
        raise ValueError("at least one --deferred entry is required")
    deferred_lines = []
    for item in deferred:
        item_lines = _split_item_lines(item, "--deferred entry")
        deferred_lines.extend(_format_bullet_lines(item_lines))

    lines = [subject, "", "Context:"]
    lines.extend(context_lines)
    lines.extend(["", "What this enables:"])
    lines.extend(enable_lines)
    lines.extend(["", "Changes (by file):"])
    lines.extend(change_lines)
    lines.extend(["", "Deferred:"])
    lines.extend(deferred_lines)
    return "\n".join(lines) + "\n"


def run_git_commit(message: str, extra_args: list[str]) -> int:
    with tempfile.NamedTemporaryFile("w", delete=False, encoding="utf-8") as handle:
        handle.write(message)
        temp_path = Path(handle.name)
    try:
        result = subprocess.run(
            ["git", "commit", "-F", str(temp_path), *extra_args],
            check=False,
        )
        if result.returncode != 0:
            sys.stderr.write(
                f"`git commit` failed with exit code {result.returncode}\n"
            )
        return result.returncode
    finally:
        try:
            temp_path.unlink()
        except OSError:
            pass


def main() -> int:
    args = parse_args()
    message = format_message(
        args.subject,
        args.context,
        args.enable,
        args.change,
        args.deferred,
    )
    if args.write:
        args.write.write_text(message, encoding="utf-8")
    else:
        sys.stdout.write(message)
    if args.commit:
        return run_git_commit(message, args.commit_arg)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
