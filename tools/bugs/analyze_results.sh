#!/usr/bin/env bash
# Usage: ./analyze_results.sh <results-file>
# Reads bgrid results and flags anomalies by severity.
# No LM needed — purely rule-based analysis.
#
# Anomaly classes:
#   SIGNAL    — process killed by signal (exit > 128)
#   OOM       — out of memory / resource exhaustion
#   INCONSISTENT — flag accepts -1 but rejects 0, or vice versa
#   SILENT_ACCEPT — negative/overflow value accepted without error
#   CONTRADICTORY — opposing flags produce silent empty output
#   CORRUPT   — output structure anomaly
#   SENSITIVE — accesses credential/sensitive files

RESULTS="$1"

if [ ! -f "$RESULTS" ]; then
    echo "Usage: $0 <results-file>" >&2
    exit 1
fi

echo "=== Bug Analysis: $RESULTS ==="
echo ""

found=0

# --- SIGNAL: exit > 128 ---
signals=$(grep -a "exit.*(" "$RESULTS" | grep -oP 'exit \d+ \([A-Z]+\)' | sort -u)
if [ -n "$signals" ]; then
    echo "## SIGNAL — process killed"
    grep -a -B2 "exit.*(" "$RESULTS" | grep -E "^run |^## group|exit.*\(" | head -20
    echo ""
    found=$((found + 1))
fi

# --- OOM / resource exhaustion ---
oom=$(grep -ai "out of memory\|cannot allocate\|calloc failed\|malloc failed" "$RESULTS")
if [ -n "$oom" ]; then
    echo "## OOM — resource exhaustion"
    grep -a -B3 -i "out of memory\|cannot allocate\|calloc failed\|malloc failed" "$RESULTS" | head -20
    echo ""
    found=$((found + 1))
fi

# --- SENSITIVE file access ---
sensitive=$(grep -a "SENSITIVE:" "$RESULTS")
if [ -n "$sensitive" ]; then
    echo "## SENSITIVE — credential/config file access"
    grep -a "SENSITIVE:" "$RESULTS" | head -10
    echo ""
    found=$((found + 1))
fi

# --- NETWORK attempts ---
network=$(grep -a "NET:" "$RESULTS")
if [ -n "$network" ]; then
    echo "## NETWORK — connection attempts"
    grep -a "NET:" "$RESULTS" | head -10
    echo ""
    found=$((found + 1))
fi

# --- SILENT_ACCEPT: negative values accepted (exit 0) ---
# Look for runs with -1 or -2147483648 that exit 0
neg_accepted=$(grep -aP 'run.*"-?1".*|run.*"=-1"' "$RESULTS" | grep "exit 0" | head -20)
overflow_accepted=$(grep -aP 'run.*"=2147483647".*|run.*"=2147483648"' "$RESULTS" | grep "exit 0" | head -10)

if [ -n "$neg_accepted" ]; then
    echo "## SILENT_ACCEPT — negative values accepted"
    echo "$neg_accepted"
    echo ""
    found=$((found + 1))
fi

if [ -n "$overflow_accepted" ]; then
    echo "## OVERFLOW_ACCEPT — large values accepted"
    echo "$overflow_accepted"
    echo ""
    found=$((found + 1))
fi

# --- INCONSISTENT validation ---
# Find flags where -1 exits differently from 0
# Extract flag=value runs and their exit codes
echo "## VALIDATION CONSISTENCY"
prev_flag=""
prev_val=""
prev_exit=""
inconsistencies=0

grep -aP '^(run |## group).*"=(-?[0-9]+)"' "$RESULTS" | while IFS= read -r line; do
    flag=$(echo "$line" | grep -oP '"--?[a-zA-Z][-a-zA-Z0-9]*=' | head -1 | tr -d '"' | sed 's/=$//')
    val=$(echo "$line" | grep -oP '=(-?[0-9]+)"' | head -1 | sed 's/[="]//g')
    exit_code=$(echo "$line" | grep -oP 'exit \d+' | head -1 | awk '{print $2}')

    if [ -z "$flag" ] || [ -z "$exit_code" ]; then continue; fi

    if [ "$flag" = "$prev_flag" ] && [ "$prev_val" != "$val" ]; then
        if [ "$prev_exit" != "$exit_code" ]; then
            # One value accepted, another rejected — check if inconsistent
            if { [ "$val" = "-1" ] && [ "$exit_code" = "0" ] && [ "$prev_exit" != "0" ]; } || \
               { [ "$prev_val" = "-1" ] && [ "$prev_exit" = "0" ] && [ "$exit_code" != "0" ]; } || \
               { [ "$val" = "0" ] && [ "$exit_code" != "0" ] && [ "$prev_val" = "-1" ] && [ "$prev_exit" = "0" ]; }; then
                echo "  $flag: val=$prev_val→exit $prev_exit, val=$val→exit $exit_code (negative accepted, zero/positive rejected?)"
                inconsistencies=$((inconsistencies + 1))
            fi
        fi
    fi
    prev_flag="$flag"
    prev_val="$val"
    prev_exit="$exit_code"
done

echo ""

# --- CONTRADICTORY: --X --no-X producing empty output ---
contradictory=$(grep -aP 'run.*"--[a-z].*"--no-' "$RESULTS" | grep "stdout empty" | head -10)
if [ -n "$contradictory" ]; then
    echo "## CONTRADICTORY — opposing flags, empty output"
    echo "$contradictory"
    echo ""
    found=$((found + 1))
fi

# --- Summary ---
echo "=== Summary ==="
total_runs=$(grep -ac '^run \|^## group' "$RESULTS" | head -1)
echo "Total runs/groups: $total_runs"
echo "Anomaly classes found: $found"

# Count by type
sig_count=$(grep -ac "SIGNAL" "$RESULTS" || echo 0)
net_count=$(grep -ac "NET:" "$RESULTS" || echo 0)
sens_count=$(grep -ac "SENSITIVE:" "$RESULTS" || echo 0)
exit2_count=$(grep -ac "exit 2" "$RESULTS" || echo 0)
exit128_count=$(grep -ac "exit 128" "$RESULTS" || echo 0)

echo "  Signals: $sig_count"
echo "  Network attempts: $net_count"
echo "  Sensitive file access: $sens_count"
echo "  Usage errors (exit 2): $exit2_count"
echo "  Fatal errors (exit 128): $exit128_count"
