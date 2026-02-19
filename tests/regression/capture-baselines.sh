#!/usr/bin/env bash
# Capture regression baselines for benchmark binaries.
# Run once after prompts are stable, commit results to tests/baselines/.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BASELINES_DIR="$REPO_ROOT/tests/baselines"
BMAN="${REPO_ROOT}/target/release/bman"

BENCHMARKS="echo sort touch wc cp git-config"

# Timeouts per binary (seconds)
declare -A TIMEOUTS=(
  [echo]=60
  [sort]=180
  [touch]=120
  [wc]=60
  [cp]=300
  [git-config]=300
)

mkdir -p "$BASELINES_DIR"

echo "Building bman..."
cargo build --release --manifest-path="$REPO_ROOT/Cargo.toml" --quiet

echo "Capturing baselines for: $BENCHMARKS"
echo ""

for bin in $BENCHMARKS; do
  pack="/tmp/bman-regress-$bin"
  timeout_secs="${TIMEOUTS[$bin]:-180}"

  # Convert git-config to "git config" for command
  cmd=$(echo "$bin" | tr '-' ' ')

  echo "=== $bin (timeout: ${timeout_secs}s) ==="
  rm -rf "$pack"

  if BMAN_LM_COMMAND="claude -p --model haiku" \
     timeout "$timeout_secs" "$BMAN" --doc-pack "$pack" \
     --max-cycles 10 --verbose $cmd; then
    "$BMAN" status --doc-pack "$pack" --json > "$BASELINES_DIR/$bin.json"

    # Extract key metrics for display
    verified=$(jq '.requirements[] | select(.id=="verification") | .behavior_verified_count' "$BASELINES_DIR/$bin.json")
    complete=$(jq '.requirements[] | select(.id=="verification") | .status == "met"' "$BASELINES_DIR/$bin.json")
    echo "  Captured: verified=$verified complete=$complete"
  else
    echo "  FAILED: timeout or error"
  fi
  echo ""
done

echo "Baselines written to $BASELINES_DIR/"
echo "Run: git add tests/baselines/*.json"
