#!/usr/bin/env bash
# Integration test: verify distinguishability rates for coreutils.
# Runs bgrid on each binary, saves the full report, and checks against expected values.
#
# Usage: ./tests/coreutils.sh [bgrid-path]
#
# Reports are saved to tests/results/<binary>.report for inspection.
# Expected results are lower bounds — the test passes if the actual
# distinguished count is >= the expected count.

set -euo pipefail

BGRID="${1:-./target/release/bgrid}"
TIMEOUT=${TIMEOUT:-180}
RESULTS_DIR="tests/results"
JOBS=${JOBS:-4}

mkdir -p "$RESULTS_DIR"

# Run a single binary and write result to a temp file for later collection.
run_one() {
    local binary="$1"
    local min_distinguished="$2"
    local result_file="$RESULTS_DIR/$binary.result"

    local report_file="$RESULTS_DIR/$binary.report"
    local stderr_file="$RESULTS_DIR/$binary.stderr"

    # Run and save full report + stderr
    timeout "$TIMEOUT" "$BGRID" "$binary" >"$report_file" 2>"$stderr_file" || true

    local result
    result=$(grep -a "^## Observed:" "$report_file" 2>/dev/null || echo "FAILED")

    if [ "$result" = "FAILED" ]; then
        echo "FAIL  $binary: timed out or errored (see $stderr_file)" >"$result_file"
        return
    fi

    local distinguished denominator
    distinguished=$(echo "$result" | grep -oP '\d+(?=/)')
    denominator=$(echo "$result" | grep -oP '(?<=/)\d+')

    if [ -z "$distinguished" ] || [ -z "$denominator" ]; then
        echo "FAIL  $binary: could not parse result: $result" >"$result_file"
        return
    fi

    if [ "$distinguished" -ge "$min_distinguished" ]; then
        echo "PASS  $binary: $distinguished/$denominator (expected >=$min_distinguished)" >"$result_file"
    else
        echo "FAIL  $binary: $distinguished/$denominator (expected >=$min_distinguished)" >"$result_file"
    fi
}

echo "=== bgrid coreutils integration test ==="
echo "binary: $BGRID"
echo "results: $RESULTS_DIR/"
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
    "head 7"
    "wc 6"
    "uniq 11"
    "nl 10"
    "od 16"
    "fold 3"
    "fmt 7"
    "paste 3"
    "du 25"
    "cp 30"
    "rm 10"
    "stat 6"
    "df 15"
    "sed 22"
    "xargs 14"
    "diff 36"
    "find 1"
    "grep 34"
)

# Run all checks in parallel, limited to $JOBS at a time
export -f run_one
export BGRID TIMEOUT RESULTS_DIR
printf '%s\n' "${CHECKS[@]}" | xargs -P "$JOBS" -I{} bash -c 'run_one {}'

# Collect results
PASS=0
FAIL=0
TOTAL=0

for entry in "${CHECKS[@]}"; do
    binary="${entry%% *}"
    TOTAL=$((TOTAL + 1))
    result_file="$RESULTS_DIR/$binary.result"
    if [ -f "$result_file" ]; then
        cat "$result_file"
        if grep -q "^PASS" "$result_file"; then
            PASS=$((PASS + 1))
        else
            FAIL=$((FAIL + 1))
        fi
        rm -f "$result_file"
    else
        echo "FAIL  $binary: no result file"
        FAIL=$((FAIL + 1))
    fi
done

END=$(date +%s)
ELAPSED=$((END - START))

echo ""
echo "=== Results: $PASS/$TOTAL passed, $FAIL failed ($ELAPSED seconds) ==="
echo "Reports saved to $RESULTS_DIR/"

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
