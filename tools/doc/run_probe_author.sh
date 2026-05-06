#!/usr/bin/env bash
# Usage: ./run_probe_author.sh <binary> <model> [subcommand args...]
# Example: ./run_probe_author.sh sort devstral-small-2:24b
#          ./run_probe_author.sh git devstral-small-2:24b diff
set -euo pipefail

BINARY="$1"
MODEL="$2"
shift 2
SUBCMD_ARGS=("$@")

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
BGRID="$REPO_DIR/target/release/bgrid"

# Generate discovery skeleton
DISCOVER_ARGS=("$BINARY" "${SUBCMD_ARGS[@]}")
DISCOVERY=$("$BGRID" "${DISCOVER_ARGS[@]}" 2>/dev/null || true)

if [ -z "$DISCOVERY" ]; then
    echo "ERROR: discovery produced no output for $BINARY ${SUBCMD_ARGS[*]}" >&2
    exit 1
fi

FULL_NAME="$BINARY"
if [ ${#SUBCMD_ARGS[@]} -gt 0 ]; then
    FULL_NAME="$BINARY ${SUBCMD_ARGS[*]}"
fi

# Read language docs and prompt template
LANGUAGE_MD=$(cat "$REPO_DIR/LANGUAGE.md")
TEMPLATE=$(cat "$SCRIPT_DIR/probe_authoring_prompt.md")

# Assemble initial prompt
PROMPT="${TEMPLATE//\{\{BINARY\}\}/$FULL_NAME}"
PROMPT="${PROMPT//\{\{LANGUAGE_MD\}\}/$LANGUAGE_MD}"
PROMPT="${PROMPT//\{\{DISCOVERY\}\}/$DISCOVERY}"

TMPPROBE=$(mktemp /tmp/bgrid_probe_XXXXXX.probe)
trap "rm -f $TMPPROBE $TMPPROBE.raw" EXIT

MAX_ATTEMPTS=3
for attempt in $(seq 1 $MAX_ATTEMPTS); do
    echo "--- Attempt $attempt: prompting $MODEL for $FULL_NAME probe ---" >&2

    # Generate probe, strip escape codes and markdown fences
    echo "$PROMPT" | ollama run --nowordwrap "$MODEL" 2>/dev/null \
      | sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' \
      | sed '/^```/d' > "$TMPPROBE.raw"

    # Strip leading prose: drop lines before the first probe-like line
    # (context, vary, run, from, in, #, or blank line)
    awk '/^(context |vary |run |from |in |#|$)/{found=1} found{print}' "$TMPPROBE.raw" > "$TMPPROBE"
    rm -f "$TMPPROBE.raw"

    # Validate with --dry-run
    DRYRUN=$("$BGRID" --dry-run "$BINARY" "$TMPPROBE" 2>&1 || true)

    # Check for errors and warnings
    ERRORS=$(echo "$DRYRUN" | grep -iE "^error|^thread .* panicked|line [0-9]+:" || true)
    WARNINGS=$(echo "$DRYRUN" | grep -i "^warning:" || true)

    # Extract stats
    GRID_LINE=$(echo "$DRYRUN" | grep -oP 'grid: \d+ contexts x \d+ runs = \d+ cells' || true)

    if [ -z "$ERRORS" ] && [ -n "$GRID_LINE" ]; then
        # Check for degenerate probes
        CONTEXTS=$(echo "$GRID_LINE" | grep -oP '\d+ contexts' | grep -oP '\d+')
        RUNS=$(echo "$GRID_LINE" | grep -oP '\d+ runs' | grep -oP '\d+')

        ISSUES=""
        if [ "$CONTEXTS" -lt 2 ]; then
            ISSUES="Only $CONTEXTS context(s) — add vary blocks to create perturbation variants."
        fi
        if [ "$RUNS" -lt 3 ]; then
            ISSUES="${ISSUES:+$ISSUES }Only $RUNS run(s) — add more run invocations to test different flags."
        fi

        if [ -z "$ISSUES" ] && [ -z "$WARNINGS" ]; then
            echo "--- Probe validated: $GRID_LINE ---" >&2
            cat "$TMPPROBE"
            exit 0
        fi

        # Feed warnings/issues back
        FEEDBACK="Your probe parsed but has issues:\n"
        [ -n "$WARNINGS" ] && FEEDBACK="${FEEDBACK}${WARNINGS}\n"
        [ -n "$ISSUES" ] && FEEDBACK="${FEEDBACK}${ISSUES}\n"
        FEEDBACK="${FEEDBACK}\nFix these issues. Keep the overall structure. Output only the corrected probe file."

        echo "--- Issues found, requesting correction ---" >&2
        echo "$WARNINGS $ISSUES" >&2

        PROMPT=$(printf "%s\n\n%b\n\nIMPORTANT: Output ONLY the probe file contents. No explanation, no markdown fences, no commentary." "$(cat "$TMPPROBE")" "$FEEDBACK")
    else
        if [ -z "$GRID_LINE" ] && [ -z "$ERRORS" ]; then
            ERRORS="Probe produced no parseable output."
        fi

        echo "--- Parse errors, requesting correction ---" >&2
        echo "$ERRORS" >&2

        PROMPT=$(printf "Your probe had parse errors:\n%s\n\nHere was the probe:\n%s\n\nFix the errors and output ONLY the corrected probe file. No explanation, no markdown fences, no commentary." \
            "$ERRORS" "$(cat "$TMPPROBE")")
    fi
done

# If we get here, all attempts failed — output the last attempt
echo "--- WARNING: probe still has issues after $MAX_ATTEMPTS attempts ---" >&2
cat "$TMPPROBE"
