#!/usr/bin/env bash
# Usage: ./explore.sh <binary> [subcommand args...] [-- max_rounds]
# Iterative exploration: discovery → run → follow-up → run → ... until convergence.
# Convergence = no identical groups were split by the follow-up.
#
# Examples:
#   ./explore.sh sort
#   ./explore.sh sort -- 5
#   ./explore.sh git diff
#   ./explore.sh git diff -- 3

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
BGRID="$REPO_DIR/target/release/bgrid"

# Parse args: everything before -- is the binary+subcommand, after -- is max_rounds
BINARY_ARGS=()
MAX_ROUNDS=3
while [ $# -gt 0 ]; do
    if [ "$1" = "--" ]; then
        shift
        MAX_ROUNDS="${1:-3}"
        break
    fi
    BINARY_ARGS+=("$1")
    shift
done

BINARY="${BINARY_ARGS[0]}"
if [ -z "$BINARY" ]; then
    echo "Usage: $0 <binary> [subcommand args...] [-- max_rounds]" >&2
    exit 1
fi

LABEL=$(echo "${BINARY_ARGS[*]}" | tr ' ' '_')
WORKDIR=$(mktemp -d /tmp/bgrid_explore_XXXXXX)
echo "=== Exploring ${BINARY_ARGS[*]} (max $MAX_ROUNDS rounds) ===" >&2
echo "workdir: $WORKDIR" >&2
echo "" >&2

# Round 0: discovery
echo "[round 0] Discovery..." >&2
"$BGRID" "${BINARY_ARGS[@]}" > "$WORKDIR/round_0.probe" 2>/dev/null
"$BGRID" --compact --trace "$BINARY" "$WORKDIR/round_0.probe" 2>/dev/null

r0_groups=$(grep -ac "^## group" "$WORKDIR/round_0.results" || echo 0)
r0_multi=$(grep -a "^## group" "$WORKDIR/round_0.results" | grep -cv "(1 runs)" || echo 0)
r0_runs=$(grep -ac "^run \|^  run " "$WORKDIR/round_0.results" || echo 0)
echo "[round 0] $r0_groups groups, $r0_multi identical, from discovery" >&2

for round in $(seq 1 "$MAX_ROUNDS"); do
    prev=$((round - 1))
    prev_multi=$(grep -a "^## group" "$WORKDIR/round_${prev}.results" | grep -cv "(1 runs)" || echo 0)

    if [ "$prev_multi" -eq 0 ]; then
        echo "[round $round] No identical groups to split — converged" >&2
        break
    fi

    # Generate follow-up
    echo "[round $round] Generating follow-up..." >&2
    "$SCRIPT_DIR/suggest_followup.sh" "$WORKDIR/round_${prev}.results" "$BINARY" \
        > "$WORKDIR/round_${round}.probe" 2>/dev/null

    probe_lines=$(wc -l < "$WORKDIR/round_${round}.probe")
    if [ "$probe_lines" -lt 5 ]; then
        echo "[round $round] Follow-up too small ($probe_lines lines) — converged" >&2
        break
    fi

    # Run follow-up
    echo "[round $round] Running follow-up ($probe_lines lines)..." >&2
    "$BGRID" --compact "$BINARY" "$WORKDIR/round_${round}.probe" 2>/dev/null

    if [ ! -f "$WORKDIR/round_${round}.results" ]; then
        echo "[round $round] No results — probe may have failed" >&2
        break
    fi

    new_groups=$(grep -ac "^## group" "$WORKDIR/round_${round}.results" || echo 0)
    new_multi=$(grep -a "^## group" "$WORKDIR/round_${round}.results" | grep -cv "(1 runs)" || echo 0)

    echo "[round $round] $new_groups groups, $new_multi identical (was $prev_multi)" >&2

    # Converged if identical groups didn't decrease
    if [ "$new_multi" -ge "$prev_multi" ]; then
        echo "[round $round] No further splits — converged" >&2
        break
    fi
done

# --- Characterization report ---
final=$(ls -t "$WORKDIR"/*.results 2>/dev/null | head -1)
if [ -z "$final" ]; then
    echo "No results produced" >&2
    exit 1
fi

echo "" >&2
echo "=== Characterization Report ===" >&2
echo "" >&2

# Round history
echo "Rounds:" >&2
for f in "$WORKDIR"/round_*.results; do
    [ -f "$f" ] || continue
    r=$(basename "$f" | sed 's/round_//;s/.results//')
    groups=$(grep -ac "^## group" "$f" || echo 0)
    multi=$(grep -a "^## group" "$f" | grep -cv "(1 runs)" || echo 0)
    singletons=$((groups - multi))
    echo "  round $r: $groups groups, $singletons isolated, $multi identical" >&2
done
echo "" >&2

# Final state from last results
total_groups=$(grep -ac "^## group" "$final" || echo 0)
multi_groups=$(grep -a "^## group" "$final" | grep -cv "(1 runs)" || echo 0)
singleton_groups=$((total_groups - multi_groups))

# Count total unique flags across all groups
total_flags=$(grep -a "^## group" "$final" | sed 's/^[^:]*: //' | perl -ne '
    my @runs = split /,\s+(?=")/, $_;
    for my $r (@runs) { $r =~ s/^\s+|\s+$//g; print "$r\n" if $r =~ /^"/; }
' | wc -l)

echo "Final state:" >&2
echo "  $total_flags runs in $total_groups behavioral groups" >&2
echo "  $singleton_groups isolated (unique behavior)" >&2
echo "  $multi_groups identical (equivalent or underexplored)" >&2
echo "" >&2

# List isolated flags (singleton groups)
echo "Isolated:" >&2
grep -a "^## group.*(1 runs):" "$final" | sed 's/^## group [0-9]* (1 runs): /  /' | head -20 >&2
if [ "$singleton_groups" -gt 20 ]; then
    echo "  ... and $((singleton_groups - 20)) more" >&2
fi
echo "" >&2

# List remaining identical groups with alias detection
alias_line=$(grep -a "^# Aliases:" "$final" | sed 's/^# Aliases: //')
echo "Remaining identical groups:" >&2
grep -a "^## group" "$final" | grep -v "(1 runs)" | while IFS= read -r group; do
    runs_str=$(echo "$group" | sed 's/^[^:]*: //')
    count=$(echo "$group" | grep -oP '\d+ runs' | grep -oP '\d+')

    # Check if this is just an alias pair
    is_alias="no"
    if [ "$count" -eq 2 ] && [ -n "$alias_line" ]; then
        # Extract the two flags
        flags=$(echo "$runs_str" | perl -ne '
            my @runs = split /,\s+(?=")/, $_;
            for my $r (@runs) {
                $r =~ s/^\s+|\s+$//g;
                if ($r =~ /^"(-[^"]+)"/) { print "$1\n"; }
            }
        ')
        f1=$(echo "$flags" | head -1)
        f2=$(echo "$flags" | tail -1)
        if [ -n "$f1" ] && [ -n "$f2" ]; then
            if echo "$alias_line" | grep -qF -- "$f1 = $f2"; then
                is_alias="yes"
            elif echo "$alias_line" | grep -qF -- "$f2 = $f1"; then
                is_alias="yes"
            fi
        fi
    fi

    if [ "$is_alias" = "yes" ]; then
        echo "  ALIAS ($count): $runs_str" >&2
    else
        echo "  UNEXPLAINED ($count): $runs_str" >&2
    fi
done
echo "" >&2

# Untested flags
untested=$(grep -a "^# Not tested" "$final" | sed 's/^# Not tested ([^)]*): //')
if [ -n "$untested" ]; then
    echo "Not tested: $untested" >&2
fi

echo "Results: $WORKDIR/" >&2
echo "$final"
