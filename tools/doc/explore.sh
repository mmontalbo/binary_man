#!/usr/bin/env bash
# Usage: ./explore.sh <binary> [max_rounds]
# Iterative exploration: discovery → run → follow-up → run → ... until convergence.
# Convergence = no identical groups were split by the follow-up.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
BGRID="$REPO_DIR/target/release/bgrid"

BINARY="$1"
MAX_ROUNDS="${2:-3}"

if [ -z "$BINARY" ]; then
    echo "Usage: $0 <binary> [max_rounds]" >&2
    exit 1
fi

WORKDIR=$(mktemp -d /tmp/bgrid_explore_XXXXXX)
echo "=== Exploring $BINARY (max $MAX_ROUNDS rounds) ===" >&2
echo "workdir: $WORKDIR" >&2
echo "" >&2

# Round 0: discovery
echo "[round 0] Discovery..." >&2
"$BGRID" "$BINARY" > "$WORKDIR/round_0.probe" 2>/dev/null
"$BGRID" --compact "$BINARY" "$WORKDIR/round_0.probe" 2>/dev/null

r0_groups=$(grep -ac "^## group" "$WORKDIR/round_0.results" || echo 0)
r0_multi=$(grep -a "^## group" "$WORKDIR/round_0.results" | grep -cv "(1 runs)" || echo 0)
r0_runs=$(grep -ac "^run \|^  run " "$WORKDIR/round_0.results" || echo 0)
echo "[round 0] $r0_groups groups, $r0_multi identical, from discovery" >&2

for round in $(seq 1 "$MAX_ROUNDS"); do
    prev=$((round - 1))
    prev_multi=$(grep -a "^## group" "$WORKDIR/round_${prev}.results" | grep -cv "(1 runs)" || echo 0)

    if [ "$prev_multi" -eq 0 ]; then
        echo "[round $round] No identical groups to split — converged" >&2
        break
    fi

    # Generate follow-up
    echo "[round $round] Generating follow-up..." >&2
    "$SCRIPT_DIR/suggest_followup.sh" "$WORKDIR/round_${prev}.results" "$BINARY" \
        > "$WORKDIR/round_${round}.probe" 2>/dev/null

    probe_lines=$(wc -l < "$WORKDIR/round_${round}.probe")
    if [ "$probe_lines" -lt 5 ]; then
        echo "[round $round] Follow-up too small ($probe_lines lines) — converged" >&2
        break
    fi

    # Run follow-up
    echo "[round $round] Running follow-up ($probe_lines lines)..." >&2
    "$BGRID" --compact "$BINARY" "$WORKDIR/round_${round}.probe" 2>/dev/null

    if [ ! -f "$WORKDIR/round_${round}.results" ]; then
        echo "[round $round] No results — probe may have failed" >&2
        break
    fi

    new_groups=$(grep -ac "^## group" "$WORKDIR/round_${round}.results" || echo 0)
    new_multi=$(grep -a "^## group" "$WORKDIR/round_${round}.results" | grep -cv "(1 runs)" || echo 0)

    echo "[round $round] $new_groups groups, $new_multi identical (was $prev_multi)" >&2

    # Converged if identical groups didn't decrease
    if [ "$new_multi" -ge "$prev_multi" ]; then
        echo "[round $round] No further splits — converged" >&2
        break
    fi
done

# Summary
echo "" >&2
echo "=== Exploration complete ===" >&2
echo "Results in: $WORKDIR/" >&2
ls -la "$WORKDIR"/*.results 2>/dev/null | while read -r line; do
    file=$(echo "$line" | awk '{print $NF}')
    groups=$(grep -ac "^## group" "$file" || echo 0)
    multi=$(grep -a "^## group" "$file" | grep -cv "(1 runs)" || echo 0)
    echo "  $(basename "$file"): $groups groups ($multi identical)" >&2
done

# Output the final results file path
final=$(ls -t "$WORKDIR"/*.results 2>/dev/null | head -1)
echo "$final"
