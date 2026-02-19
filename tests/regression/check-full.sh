#!/usr/bin/env bash
# Full regression check: regenerate packs with live LM and compare to baselines.
# Run before merging prompt PRs.
# Runtime: ~3min/binary, ~20min for all
#
# Usage:
#   ./check-full.sh          # Check all benchmarks
#   ./check-full.sh echo     # Check single binary

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BASELINES_DIR="$REPO_ROOT/tests/baselines"
BMAN="${REPO_ROOT}/target/release/bman"

ALL_BENCHMARKS="echo sort touch wc cp git-config"

# Timeouts per binary (seconds)
declare -A TIMEOUTS=(
  [echo]=60
  [sort]=180
  [touch]=120
  [wc]=60
  [cp]=300
  [git-config]=300
)

# Use provided binary or all benchmarks
if [ $# -gt 0 ]; then
  BENCHMARKS="$1"
else
  BENCHMARKS="$ALL_BENCHMARKS"
fi

echo "Building bman..."
cargo build --release --manifest-path="$REPO_ROOT/Cargo.toml" --quiet

echo "Full regression check for: $BENCHMARKS"
echo ""

PASS=true
for bin in $BENCHMARKS; do
  baseline_file="$BASELINES_DIR/$bin.json"
  pack="/tmp/bman-regress-$bin"
  timeout_secs="${TIMEOUTS[$bin]:-180}"

  if [ ! -f "$baseline_file" ]; then
    echo "SKIP: $bin (no baseline at $baseline_file)"
    continue
  fi

  # Convert git-config to "git config" for command
  cmd=$(echo "$bin" | tr '-' ' ')

  echo "=== $bin ==="
  rm -rf "$pack"

  if ! BMAN_LM_COMMAND="claude -p --model haiku" \
       timeout "$timeout_secs" "$BMAN" --doc-pack "$pack" \
       --max-cycles 10 --verbose $cmd; then
    echo "  FAIL: bman execution failed or timed out"
    PASS=false
    echo ""
    continue
  fi

  # Extract baseline metrics
  baseline=$(cat "$baseline_file")
  b_verified=$(echo "$baseline" | jq '.requirements[] | select(.id=="verification") | .behavior_verified_count')
  b_complete=$(echo "$baseline" | jq '.requirements[] | select(.id=="verification") | .status == "met"')
  b_excluded=$(echo "$baseline" | jq '.requirements[] | select(.id=="verification") | .verification.behavior_excluded_count // 0')

  # Extract current metrics
  current=$("$BMAN" status --doc-pack "$pack" --json)
  c_verified=$(echo "$current" | jq '.requirements[] | select(.id=="verification") | .behavior_verified_count')
  c_complete=$(echo "$current" | jq '.requirements[] | select(.id=="verification") | .status == "met"')
  c_excluded=$(echo "$current" | jq '.requirements[] | select(.id=="verification") | .verification.behavior_excluded_count // 0')

  # Check for regressions
  if [ "$c_verified" -lt "$b_verified" ]; then
    echo "  FAIL: verified regressed $b_verified → $c_verified"
    PASS=false
  elif [ "$b_complete" = "true" ] && [ "$c_complete" = "false" ]; then
    echo "  FAIL: completeness regressed (was complete, now incomplete)"
    PASS=false
  elif [ "$c_excluded" -gt "$b_excluded" ]; then
    echo "  WARN: excluded increased $b_excluded → $c_excluded (review manually)"
    echo "  OK: verified=$c_verified complete=$c_complete"
  else
    echo "  OK: verified=$c_verified complete=$c_complete"
  fi
  echo ""
done

if $PASS; then
  echo "Regression check passed"
  exit 0
else
  echo "Regression check FAILED"
  exit 1
fi
