#!/usr/bin/env python3
"""Validate a commit message against the project template.

By default the script checks .git/COMMIT_EDITMSG. Pass --file to point at a
different message (for example the most recent commit via `git log -1 --pretty=%B`).
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


MAX_CONTEXT_LINES = 8


def _strip_trailing_blanks(lines: list[str]) -> list[str]:
    while lines and lines[-1] == "":
        lines = lines[:-1]
    return lines


def _strip_trailing_section_blanks(lines: list[str]) -> tuple[list[str], int]:
    trimmed = list(lines)
    count = 0
    while trimmed and trimmed[-1] == "":
        trimmed.pop()
        count += 1
    return trimmed, count


def _validate_context_section(lines: list[str]):
    if not lines:
        return "Context section must include at least one line"
    if lines[0] == "" or lines[-1] == "":
        return "Context section must not start or end with blank lines"
    blank_run = 0
    non_empty = 0
    for line in lines:
        if line.strip():
            blank_run = 0
            non_empty += 1
        else:
            blank_run += 1
            if blank_run > 1:
                return "Context section must not contain consecutive blank lines"
    if non_empty > MAX_CONTEXT_LINES:
        return f"Context section must be 1-{MAX_CONTEXT_LINES} lines"
    return None


def _validate_bullet_section(
    lines: list[str],
    section_label: str,
    *,
    require_colon: bool = False,
):
    if not lines:
        return f"{section_label} section must contain at least one bullet"
    bullet_count = 0
    in_bullet = False
    for line in lines:
        if line == "":
            return f"{section_label} section must not contain blank lines"
        if line.startswith("- "):
            content = line[2:]
            if not content.strip():
                return f"{section_label} bullet must include text"
            if require_colon:
                if ":" not in content:
                    return f"{section_label} bullets must use 'label: description' format"
                label, description = content.split(":", 1)
                if not label.strip() or not description.strip():
                    return f"{section_label} bullets must use 'label: description' format"
            bullet_count += 1
            in_bullet = True
        elif line.startswith("  ") or line.startswith("\t"):
            if not in_bullet:
                return f"{section_label} continuation line must follow a bullet"
            if not line.strip():
                return f"{section_label} continuation line must include text"
        else:
            return (
                f"{section_label} lines must start with '- ' or be indented continuations"
            )
    if bullet_count == 0:
        return f"{section_label} section must contain at least one bullet"
    return None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--file",
        type=Path,
        default=Path(".git/COMMIT_EDITMSG"),
        help="path to the commit message to validate (default: %(default)s)",
    )
    return parser.parse_args()


def fail(message: str) -> int:
    sys.stderr.write(f"commit lint error: {message}\n")
    return 1


def lint_message(text: str) -> int:
    lines = _strip_trailing_blanks(text.splitlines())

    if not lines:
        return fail("commit message is empty")

    subject = lines[0]
    if not subject.strip():
        return fail("subject line must not be empty")

    if len(lines) < 3 or lines[1] != "":
        return fail("expected blank line after subject")

    headers = [
        ("Context:", _validate_context_section),
        ("What this enables:", lambda lines: _validate_bullet_section(lines, "What this enables")),
        (
            "Changes (by file):",
            lambda lines: _validate_bullet_section(lines, "Changes", require_colon=True),
        ),
        ("Deferred:", lambda lines: _validate_bullet_section(lines, "Deferred")),
    ]

    idx = 2
    for i, (header, validator) in enumerate(headers):
        if idx >= len(lines) or lines[idx] != header:
            return fail(f"expected '{header}' section header")
        idx += 1
        next_header = headers[i + 1][0] if i + 1 < len(headers) else None
        if next_header:
            try:
                next_idx = lines.index(next_header, idx)
            except ValueError:
                return fail(f"expected '{next_header}' section header")
            section_lines = lines[idx:next_idx]
            section_lines, separator_blanks = _strip_trailing_section_blanks(section_lines)
            if separator_blanks != 1:
                return fail("expected single blank line between sections")
            idx = next_idx
        else:
            section_lines = lines[idx:]
            idx = len(lines)

        if not section_lines:
            return fail(f"{header} section must not be empty")

        error = validator(section_lines)
        if error:
            return fail(error)

    return 0


def main() -> int:
    args = parse_args()
    try:
        text = args.file.read_text(encoding="utf-8")
    except FileNotFoundError:
        return fail(f"cannot read commit message file: {args.file}")
    return lint_message(text)


if __name__ == "__main__":
    raise SystemExit(main())
