#!/usr/bin/env bash
# Integration test: verify distinguishability rates for coreutils.
# Runs bgrid on each binary, saves the full report, and checks against expected values.
#
# Usage: ./tests/coreutils.sh [bgrid-path]
#
# Reports are saved to tests/results/<binary>.report for inspection.
# Full run logs are saved to tests/runs/<timestamp>/ for post-mortem analysis.
# Expected results are lower bounds — the test passes if the actual
# distinguished count is >= the expected count.

set -euo pipefail

BGRID="${1:-./target/release/bgrid}"
TIMEOUT=${TIMEOUT:-600}
RESULTS_DIR="tests/results"
JOBS=${JOBS:-4}

# Create timestamped run directory for full diagnostic retention
RUN_ID=$(date +%Y%m%d_%H%M%S)
RUN_DIR="tests/runs/$RUN_ID"
mkdir -p "$RESULTS_DIR" "$RUN_DIR"

# Capture system state at start
{
    echo "timestamp: $(date -Iseconds)"
    echo "bgrid: $BGRID"
    echo "jobs: $JOBS"
    echo "timeout: $TIMEOUT"
    echo "load: $(cat /proc/loadavg 2>/dev/null || echo 'unknown')"
    echo "cpus: $(nproc 2>/dev/null || echo 'unknown')"
} >"$RUN_DIR/env.txt"

# Run a single binary and write result to a temp file for later collection.
run_one() {
    local binary="$1"
    local min_distinguished="$2"
    local result_file="$RESULTS_DIR/$binary.result"

    local report_file="$RESULTS_DIR/$binary.report"
    local stderr_file="$RESULTS_DIR/$binary.stderr"

    # Run and save full report + stderr
    timeout "$TIMEOUT" "$BGRID" "$binary" >"$report_file" 2>"$stderr_file" || true

    # Retain full logs in the run directory for post-mortem
    cp "$report_file" "$RUN_DIR/$binary.report" 2>/dev/null || true
    cp "$stderr_file" "$RUN_DIR/$binary.stderr" 2>/dev/null || true

    local result
    result=$(grep -a "^## Observed:" "$report_file" 2>/dev/null || echo "FAILED")

    if [ "$result" = "FAILED" ]; then
        echo "FAIL  $binary: timed out or errored" >"$result_file"
        return
    fi

    local distinguished denominator
    distinguished=$(echo "$result" | grep -oP '\d+(?=/)')
    denominator=$(echo "$result" | grep -oP '(?<=/)\d+')

    if [ -z "$distinguished" ] || [ -z "$denominator" ]; then
        echo "FAIL  $binary: could not parse result: $result" >"$result_file"
        return
    fi

    # Extract diagnostics from stderr
    local total_timeouts=0
    while IFS= read -r line; do
        local t
        t=$(echo "$line" | grep -oP '(\d+) timeouts' | grep -oP '\d+')
        if [ -n "$t" ]; then
            total_timeouts=$((total_timeouts + t))
        fi
    done < "$stderr_file"

    local round0
    round0=$(grep -oP '\[round 0\] \K.*groups.*' "$stderr_file" 2>/dev/null | head -1)

    # Build diagnostic string
    local diag=""
    if [ "$total_timeouts" -gt 0 ]; then
        diag="$diag timeouts=$total_timeouts"
    fi
    if [ -n "$round0" ]; then
        diag="$diag r0={$round0}"
    fi
    if [ -n "$diag" ]; then
        diag=" [$diag ]"
    fi

    if [ "$distinguished" -ge "$min_distinguished" ]; then
        echo "PASS  $binary: $distinguished/$denominator (expected >=$min_distinguished)$diag" >"$result_file"
    else
        echo "FAIL  $binary: $distinguished/$denominator (expected >=$min_distinguished)$diag" >"$result_file"
    fi
}

echo "=== bgrid coreutils integration test ==="
echo "binary: $BGRID"
echo "results: $RESULTS_DIR/"
echo "run log: $RUN_DIR/"
echo "parallel: $JOBS"
echo ""

START=$(date +%s)

# Expected lower bounds for observed behavior count.
# These are the minimum acceptable — improvements raise them.
CHECKS=(
    "sort 24"
    "ls 53"
    "cat 10"
    "cut 3"
    "head 5"
    "wc 6"
    "uniq 11"
    "nl 10"
    "od 16"
    "fold 3"
    "fmt 7"
    "paste 3"
    "du 25"
    "cp 27"
    "rm 10"
    "stat 6"
    "df 15"
    "sed 22"
    "xargs 14"
    "diff 34"
    "find 1"
    "grep 34"
)

# Run all checks in parallel, limited to $JOBS at a time
export -f run_one
export BGRID TIMEOUT RESULTS_DIR RUN_DIR
printf '%s\n' "${CHECKS[@]}" | xargs -P "$JOBS" -I{} bash -c 'run_one {}'

# Collect results
PASS=0
FAIL=0
TOTAL=0
SUMMARY=""

for entry in "${CHECKS[@]}"; do
    binary="${entry%% *}"
    TOTAL=$((TOTAL + 1))
    result_file="$RESULTS_DIR/$binary.result"
    if [ -f "$result_file" ]; then
        line=$(cat "$result_file")
        echo "$line"
        SUMMARY="$SUMMARY$line"$'\n'
        if grep -q "^PASS" "$result_file"; then
            PASS=$((PASS + 1))
        else
            FAIL=$((FAIL + 1))
        fi
        rm -f "$result_file"
    else
        echo "FAIL  $binary: no result file"
        SUMMARY="$SUMMARY""FAIL  $binary: no result file"$'\n'
        FAIL=$((FAIL + 1))
    fi
done

END=$(date +%s)
ELAPSED=$((END - START))

echo ""
echo "=== Results: $PASS/$TOTAL passed, $FAIL failed ($ELAPSED seconds) ==="
echo "Reports saved to $RESULTS_DIR/"
echo "Run log saved to $RUN_DIR/"

# Save summary to run directory
{
    echo "=== Results: $PASS/$TOTAL passed, $FAIL failed ($ELAPSED seconds) ==="
    echo ""
    echo "$SUMMARY"
} >"$RUN_DIR/summary.txt"

# Add tests/runs/ to gitignore if not already there
if ! grep -q 'tests/runs/' .gitignore 2>/dev/null; then
    echo 'tests/runs/' >>.gitignore
fi

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
