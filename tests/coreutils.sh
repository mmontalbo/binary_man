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
TIMEOUT=60
RESULTS_DIR="tests/results"
PASS=0
FAIL=0
TOTAL=0

mkdir -p "$RESULTS_DIR"

check() {
    local binary="$1"
    local min_distinguished="$2"
    TOTAL=$((TOTAL + 1))

    local report_file="$RESULTS_DIR/$binary.report"
    local stderr_file="$RESULTS_DIR/$binary.stderr"

    # Run and save full report + stderr
    timeout "$TIMEOUT" "$BGRID" "$binary" >"$report_file" 2>"$stderr_file" || true

    local result
    result=$(grep -a "^## Observed:" "$report_file" 2>/dev/null || echo "FAILED")

    if [ "$result" = "FAILED" ]; then
        echo "FAIL  $binary: timed out or errored (see $stderr_file)"
        FAIL=$((FAIL + 1))
        return
    fi

    local distinguished denominator
    distinguished=$(echo "$result" | grep -oP '\d+(?=/)')
    denominator=$(echo "$result" | grep -oP '(?<=/)\d+')

    if [ -z "$distinguished" ] || [ -z "$denominator" ]; then
        echo "FAIL  $binary: could not parse result: $result"
        FAIL=$((FAIL + 1))
        return
    fi

    if [ "$distinguished" -ge "$min_distinguished" ]; then
        echo "PASS  $binary: $distinguished/$denominator (expected >=$min_distinguished)"
        PASS=$((PASS + 1))
    else
        echo "FAIL  $binary: $distinguished/$denominator (expected >=$min_distinguished)"
        FAIL=$((FAIL + 1))
    fi
}

echo "=== bgrid coreutils integration test ==="
echo "binary: $BGRID"
echo "results: $RESULTS_DIR/"
echo ""

START=$(date +%s)

# Expected lower bounds for observed behavior count.
# These are the minimum acceptable — improvements raise them.
check sort   20
check ls     50
check cat    10
check cut     3
check head    4
check wc      6
check uniq    9
check nl     10
check od     12
check fold    3
check fmt     6
check paste   3
check du     24
check cp     28
check rm      7
check stat    6
check df     14

# Non-coreutils tools
check sed    18
check xargs   1
check diff    1
check find    1
check grep    1

END=$(date +%s)
ELAPSED=$((END - START))

echo ""
echo "=== Results: $PASS/$TOTAL passed, $FAIL failed ($ELAPSED seconds) ==="
echo "Reports saved to $RESULTS_DIR/"

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
