#!/usr/bin/env bash
# Stateful mock LM that returns responses sequentially.
# Usage: BMAN_LM_COMMAND="./tests/mock-lm.sh /path/to/fixture" bman ...
#
# The mock reads responses from $FIXTURE_DIR/responses/001.txt, 002.txt, etc.
# State is stored in $BMAN_MOCK_STATE_DIR/.mock_cycle (defaults to current dir).

set -e

FIXTURE_DIR="$1"
if [[ -z "$FIXTURE_DIR" ]]; then
    echo "mock-lm: missing fixture dir argument" >&2
    exit 1
fi

STATE_DIR="${BMAN_MOCK_STATE_DIR:-.}"
STATE_FILE="$STATE_DIR/.mock_cycle"
CYCLE=$(cat "$STATE_FILE" 2>/dev/null || echo 1)

# Check for injected failure
ERROR_FILE="$FIXTURE_DIR/responses/$(printf '%03d' $CYCLE)_error.txt"
if [[ -f "$ERROR_FILE" ]]; then
    cat "$ERROR_FILE" >&2
    echo $((CYCLE + 1)) > "$STATE_FILE"
    exit 1
fi

# Return response and advance cycle
RESPONSE="$FIXTURE_DIR/responses/$(printf '%03d' $CYCLE).txt"
if [[ -f "$RESPONSE" ]]; then
    cat "$RESPONSE"
    echo $((CYCLE + 1)) > "$STATE_FILE"
else
    echo "mock-lm: no response for cycle $CYCLE at $RESPONSE" >&2
    exit 1
fi
