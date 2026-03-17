#!/usr/bin/env bash
# Run eval harness across the standard corpus of binaries.
#
# Usage:
#   tools/eval_corpus.sh --runs 3
#   tools/eval_corpus.sh --runs 5 --compare v0
#   tools/eval_corpus.sh --tag-baseline v0
#
# All flags are forwarded to tools/eval.py for each corpus binary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

CORPUS=(
    "ls"
    "git diff"
    "grep"
)

failed=0

for entry in "${CORPUS[@]}"; do
    # Split into binary + entry_point
    read -ra parts <<< "$entry"
    binary="${parts[0]}"
    entry_point=("${parts[@]:1}")

    echo ""
    echo "════════════════════════════════════════════════════════════"
    echo "  CORPUS: $entry"
    echo "════════════════════════════════════════════════════════════"
    echo ""

    if python3 "$SCRIPT_DIR/eval.py" "$binary" "${entry_point[@]}" "$@"; then
        echo ""
        echo "  ✓ $entry complete"
    else
        echo ""
        echo "  ✗ $entry failed (exit $?)"
        failed=$((failed + 1))
    fi
done

echo ""
echo "════════════════════════════════════════════════════════════"
echo "  CORPUS COMPLETE: ${#CORPUS[@]} binaries, $failed failed"
echo "════════════════════════════════════════════════════════════"

exit $failed
