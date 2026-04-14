#!/bin/bash
# Capture the full TUI output including borders, scrolling through all pages.
# Usage: ./capture.sh [session_name]
# NOTE: Does NOT reset scroll position — captures from current state.

SESSION="${1:-inspect}"
OUT="/tmp/inspect_capture.txt"

echo "" > "$OUT"
prev=""
page=0

while true; do
  page=$((page + 1))
  current=$(tmux capture-pane -t "$SESSION" -p | md5sum)

  if [ "$current" = "$prev" ]; then
    break
  fi
  prev="$current"

  echo "━━━ PAGE $page ━━━" >> "$OUT"
  tmux capture-pane -t "$SESSION" -p >> "$OUT"
  echo "" >> "$OUT"

  tmux send-keys -t "$SESSION" PageDown
  sleep 0.3
done

echo "Captured $page pages to $OUT" >&2
cat "$OUT"
