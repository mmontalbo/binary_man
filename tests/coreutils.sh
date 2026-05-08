#!/usr/bin/env bash
# Integration test: verify distinguishability rates for coreutils.
# Runs bgrid on each binary and checks the result against expected values.
#
# Usage: ./tests/coreutils.sh [bgrid-path]
#
# Expected results are lower bounds — the test passes if the actual
# distinguished count is >= the expected count. This allows improvements
# without breaking the test.

set -euo pipefail

BGRID="${1:-./target/release/bgrid}"
TIMEOUT=60
PASS=0
FAIL=0
TOTAL=0

check() {
    local binary="$1"
    local min_distinguished="$2"
    local max_denominator="$3"
    TOTAL=$((TOTAL + 1))

    local result
    result=$(timeout "$TIMEOUT" "$BGRID" "$binary" 2>/dev/null | grep "^## Distinguished:" || echo "FAILED")

    if [ "$result" = "FAILED" ]; then
        echo "FAIL  $binary: timed out or errored"
        FAIL=$((FAIL + 1))
        return
    fi

    # Parse "## Distinguished: N/M flags"
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
echo ""

START=$(date +%s)

# Expected: (binary, min_distinguished, max_denominator)
# These are lower bounds — improvements raise them, regressions lower them.
check sort   29 29
check ls     50 51
check cat    10 12
check cut     9 12
check head    4  6
check wc      7  9
check uniq   10 11
check nl     10 13
check od     19 19
check fold    3  5
check fmt     6  9
check paste   3  5
check du     25 28
check cp     18 30
check rm      9  9
check stat    7  8
check df     15 17

END=$(date +%s)
ELAPSED=$((END - START))

echo ""
echo "=== Results: $PASS/$TOTAL passed, $FAIL failed ($ELAPSED seconds) ==="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
