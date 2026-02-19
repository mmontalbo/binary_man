#!/usr/bin/env bash
# Quick sanity check: re-evaluate existing packs without LM calls.
# Catches code changes that break scenario evaluation.
# Runtime: ~10s

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BMAN="${REPO_ROOT}/target/release/bman"

BENCHMARKS="echo sort touch wc cp git-config"

echo "Building bman..."
cargo build --release --manifest-path="$REPO_ROOT/Cargo.toml" --quiet

echo "Sanity check: re-evaluating existing packs"
echo ""

PASS=true
for bin in $BENCHMARKS; do
  pack="/tmp/bman-regress-$bin"

  if [ ! -d "$pack" ]; then
    echo "SKIP: $bin (no pack at $pack)"
    continue
  fi

  if "$BMAN" status --doc-pack "$pack" --force >/dev/null 2>&1; then
    echo "OK: $bin"
  else
    echo "FAIL: $bin (status evaluation failed)"
    PASS=false
  fi
done

echo ""
if $PASS; then
  echo "Sanity check passed"
  exit 0
else
  echo "Sanity check FAILED"
  exit 1
fi
