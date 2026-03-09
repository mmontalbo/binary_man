#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Find DuckDB
DUCKDB=$(find /nix/store -name "duckdb" -type f -executable 2>/dev/null | grep -v doc | head -1)
DUCKDB="${DUCKDB:-duckdb}"

if ! command -v "$DUCKDB" &> /dev/null; then
    echo "ERROR: duckdb not found"
    exit 1
fi

# Generate fixtures
FIXTURE_DIR=$("$SCRIPT_DIR/generate_fixtures.sh")
trap "rm -rf $FIXTURE_DIR" EXIT

echo "=== SQL Behavior Verification Tests ==="
echo "Using DuckDB: $DUCKDB"
echo "Fixtures: $FIXTURE_DIR"

# Concatenate SQL files (simulating @include processing)
concat_sql() {
    local base_dir="$REPO_ROOT/queries"
    cat "$base_dir/verification_from_scenarios/00_inputs_normalization.sql"
    cat "$base_dir/verification_from_scenarios/10_behavior_assertion_eval.sql"
    cat "$base_dir/verification_from_scenarios/20_coverage_reasoning.sql"
    cat "$base_dir/verification_from_scenarios/30_rollups_output.sql"
}

failures=0

echo ""
echo "Running verification query..."
cd "$FIXTURE_DIR"
concat_sql > /tmp/test_query.sql
result=$("$DUCKDB" -csv < /tmp/test_query.sql 2>&1)

echo ""
echo "Query output (CSV):"
echo "$result"
echo ""

# Parse CSV output: surface_id is first column, delta_outcome is column 14
# Using grep to extract the relevant rows

# Test 1: baseline exit!=0 should NOT produce delta_seen
echo -n "  baseline_error -> NOT delta_seen... "
baseline_error_line=$(echo "$result" | grep "^--test-baseline-error" || true)
baseline_error_outcome=$(echo "$baseline_error_line" | cut -d',' -f14)

if echo "$baseline_error_outcome" | grep -q "delta_seen"; then
    echo "FAIL"
    echo "    Expected: NOT delta_seen (should be assertion_failed, outputs_equal, or null)"
    echo "    Got: $baseline_error_outcome"
    failures=$((failures + 1))
else
    echo "PASS (got: ${baseline_error_outcome:-null})"
fi

# Test 2: both exit=0 with different output SHOULD produce delta_seen
echo -n "  both_ok_differ -> delta_seen... "
both_ok_line=$(echo "$result" | grep "^--test-both-ok" || true)
both_ok_outcome=$(echo "$both_ok_line" | cut -d',' -f14)

if echo "$both_ok_outcome" | grep -q "delta_seen"; then
    echo "PASS"
else
    echo "FAIL"
    echo "    Expected: delta_seen"
    echo "    Got: ${both_ok_outcome:-null}"
    failures=$((failures + 1))
fi

echo ""
if [ $failures -gt 0 ]; then
    echo "=== $failures test(s) FAILED ==="
    exit 1
fi
echo "=== All tests PASSED ==="
