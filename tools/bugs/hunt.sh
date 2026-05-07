#!/usr/bin/env bash
# Usage: ./hunt.sh <binary> [subcommand args...]
# Full bug-hunting pipeline: discover → generate boundary probe → run → analyze
#
# Example:
#   ./hunt.sh sort
#   ./hunt.sh git diff

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
BGRID="$REPO_DIR/target/release/bgrid"

BINARY="$1"
shift
SUBCMD_ARGS=("$@")

LABEL="${BINARY}$([ ${#SUBCMD_ARGS[@]} -gt 0 ] && echo "_${SUBCMD_ARGS[*]}" | tr ' ' '_' || true)"
PROBE="/tmp/bgrid_hunt_${LABEL}.probe"
RESULTS="/tmp/bgrid_hunt_${LABEL}.results"

echo "=== Bug Hunt: $BINARY ${SUBCMD_ARGS[*]} ===" >&2
echo "" >&2

# Step 1: Generate boundary probe
echo "[1/3] Generating boundary probe..." >&2
"$SCRIPT_DIR/generate_boundary_probe.sh" "$BINARY" "${SUBCMD_ARGS[@]}" > "$PROBE"
probe_lines=$(wc -l < "$PROBE")
echo "  wrote $PROBE ($probe_lines lines)" >&2

# Step 2: Run probe
echo "[2/3] Running probe..." >&2
"$BGRID" --trace "$BINARY" "$PROBE" 2>&1 | grep -E "contexts|cells|SIGNAL|SETUP FAILED" >&2
echo "  wrote $RESULTS" >&2

# Step 3: Analyze
echo "[3/3] Analyzing results..." >&2
echo "" >&2
"$SCRIPT_DIR/analyze_results.sh" "$RESULTS"
