#!/usr/bin/env bash
# Usage: ./check_coverage.sh <results-file> <doc-file>
# Checks that generated docs cover all observed flags and don't hallucinate.

RESULTS="$1"
DOC="$2"

# Extract flags from results: "--flag" or "-X" patterns inside quotes
# Use -a to handle binary files (null bytes from test output)
results_flags=$(grep -aoP '"--?[a-zA-Z][-a-zA-Z0-9]*(?:=[^"]*)?"' "$RESULTS" \
  | tr -d '"' | sed 's/=.*//' | sort -u)

# Extract flags from doc: --flag or -X patterns
# Filter out permission strings (-rw-r--r--, -rwxr-xr-x)
doc_flags=$(grep -oP '(?<![a-zA-Z])--?[a-zA-Z][-a-zA-Z0-9]*' "$DOC" \
  | sed 's/=.*//' \
  | grep -vP '^-r[w-][x-]' \
  | sort -u)

echo "=== Coverage Check ==="

# Missing: in results but not in doc
missing=0
while IFS= read -r flag; do
  [ -z "$flag" ] && continue
  if ! echo "$doc_flags" | grep -qxF -- "$flag"; then
    [ $missing -eq 0 ] && echo "" && echo "FAIL: Flags observed but missing from docs:"
    echo "  $flag"
    missing=$((missing + 1))
  fi
done <<< "$results_flags"
[ $missing -eq 0 ] && echo "PASS: All observed flags mentioned in docs"

# Phantom: in doc but not in results
phantom=0
while IFS= read -r flag; do
  [ -z "$flag" ] && continue
  if ! echo "$results_flags" | grep -qxF -- "$flag"; then
    [ $phantom -eq 0 ] && echo "" && echo "WARN: Flags in docs but not in results:"
    echo "  $flag"
    phantom=$((phantom + 1))
  fi
done <<< "$doc_flags"
[ $phantom -eq 0 ] && echo "PASS: No phantom flags in docs"

# Exit codes
results_exits=$(grep -aoP 'exit[: {]+[0-9]+' "$RESULTS" | grep -oP '[0-9]+' | sort -u)
doc_exits=$(grep -oP '[Ee]xit[^a-zA-Z]*[0-9]+' "$DOC" | grep -oP '[0-9]+' | sort -u)

echo ""
missing_exits=0
while IFS= read -r code; do
  [ -z "$code" ] && continue
  if ! echo "$doc_exits" | grep -qxF -- "$code"; then
    [ $missing_exits -eq 0 ] && echo "WARN: Exit codes observed but not documented:"
    echo "  $code"
    missing_exits=$((missing_exits + 1))
  fi
done <<< "$results_exits"
[ $missing_exits -eq 0 ] && echo "PASS: All observed exit codes documented"

# Summary
n_results=$(echo "$results_flags" | grep -c . 2>/dev/null || echo 0)
n_doc=$(echo "$doc_flags" | grep -c . 2>/dev/null || echo 0)
echo ""
echo "=== Summary ==="
echo "Results flags: $n_results"
echo "Doc flags:     $n_doc"
echo "Missing:       $missing"
echo "Phantom:       $phantom"

# Exit non-zero if missing flags
exit $missing
