#!/usr/bin/env bash
# Usage: ./run_interpret.sh <binary> <results-file> <model>
# Example: ./run_interpret.sh sort /tmp/sort.results devstral-small-2:24b
set -euo pipefail

BINARY="$1"
RESULTS_FILE="$2"
MODEL="$3"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

RESULTS=$(cat "$RESULTS_FILE")
TEMPLATE=$(cat "$SCRIPT_DIR/result_interpretation_prompt.md")

PROMPT="${TEMPLATE//\{\{BINARY\}\}/$BINARY}"
PROMPT="${PROMPT//\{\{RESULTS\}\}/$RESULTS}"

echo "--- Prompting $MODEL to interpret $RESULTS_FILE ---" >&2
echo "$PROMPT" | ollama run --nowordwrap "$MODEL" 2>/dev/null \
  | sed 's/\x1b\[[0-9;]*[a-zA-Z]//g'
